/**
 * Unified-diff patch application tool.
 *
 * The entire patch is parsed and applied in memory first: every context line
 * must match the current file exactly or nothing is written (atomic across all
 * files). File deletion, traversal, and absolute-path headers are rejected,
 * and a re-stat guard aborts when a target changed while the patch was being
 * prepared, so unrelated user edits are never clobbered.
 */

import { linkSync, mkdirSync, readFileSync, renameSync, statSync, unlinkSync, writeFileSync } from 'node:fs';
import { randomBytes } from 'node:crypto';
import process from 'node:process';
import path from 'node:path';
import type { ToolDefinition, ToolRegistry, ToolResult } from './registry.js';
import { normalizeWorkspacePath, PathEscapeError, resolveWorkspacePath } from './path-guard.js';
import { boundOutput, redactSecrets } from './redact.js';
import { boundedString, relativePathArg } from './schemas.js';
import { z } from 'zod';

const PATCH_TIMEOUT_MS = 30_000;
const MAX_OUTPUT_CHARS = 30_000;

class PatchFormatError extends Error {
  constructor(message: string) {
    super(message);
    this.name = 'PatchFormatError';
  }
}

function fail(output: string): ToolResult {
  return { ok: false, outcome: 'error', output, affectedFiles: [] };
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function quote(text: string): string {
  const shortened = text.length > 120 ? `${text.slice(0, 117)}...` : text;
  return JSON.stringify(shortened);
}

type PatchBodyLine = { readonly op: ' ' | '+' | '-' | '\\'; readonly text: string };

type ParsedHunk = {
  readonly lineNo: number;
  readonly oldStart: number;
  readonly oldCount: number | null;
  readonly body: readonly PatchBodyLine[];
};

type ParsedFile = {
  readonly relPath: string;
  readonly isNew: boolean;
  readonly hunks: readonly ParsedHunk[];
  readonly headerLineNo: number;
};

const HUNK_HEADER = /^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@/;

/** Strip timestamps and quotes from a diff file token. */
function parseFileToken(raw: string): string {
  let token = raw;
  const tab = token.indexOf('\t');
  if (tab !== -1) {
    token = token.slice(0, tab);
  }
  if (token.length >= 2 && token.startsWith('"') && token.endsWith('"')) {
    token = token.slice(1, -1);
  }
  return token;
}

function stripPrefix(token: string, prefix: 'a' | 'b'): string {
  return token.startsWith(`${prefix}/`) ? token.slice(2) : token;
}

export function parsePatch(patch: string): readonly ParsedFile[] {
  const rawLines = patch.split('\n');
  if (rawLines.length > 0 && rawLines[rawLines.length - 1] === '') {
    rawLines.pop();
  }
  const files: ParsedFile[] = [];
  const seen = new Set<string>();
  let index = 0;

  const SKIP_PREFIXES: readonly string[] = [
    'diff ',
    'index ',
    'new file mode ',
    'old mode ',
    'new mode ',
    'similarity index ',
    'rename ',
    'copy ',
    'Binary files ',
  ];

  while (index < rawLines.length) {
    const line = rawLines[index] as string;
    if (line === '') {
      index += 1;
      continue;
    }
    if (line.startsWith('deleted file mode ')) {
      throw new PatchFormatError('patch deletes a file, which is not supported (refusing)');
    }
    if (SKIP_PREFIXES.some((prefix) => line.startsWith(prefix))) {
      index += 1;
      continue;
    }
    if (!line.startsWith('--- ')) {
      throw new PatchFormatError(
        `patch line ${index + 1}: unexpected ${quote(line)}; expected a "--- a/<path>" file header`,
      );
    }

    const headerLineNo = index + 1;
    const oldToken = parseFileToken(line.slice(4));
    const plusLine: string | undefined = rawLines[index + 1];
    if (plusLine === undefined || !plusLine.startsWith('+++ ')) {
      throw new PatchFormatError(
        `patch line ${headerLineNo + 1}: expected "+++ b/<path>" after the "--- " header, found ${quote(
          plusLine ?? '<end of patch>',
        )}`,
      );
    }
    const newToken = parseFileToken(plusLine.slice(4));

    if (newToken === '/dev/null') {
      throw new PatchFormatError(
        `patch line ${headerLineNo + 1}: file deletion is not supported ("+++ /dev/null" for ${quote(oldToken)})`,
      );
    }
    const relPath = normalizeWorkspacePath(stripPrefix(newToken, 'b'));
    const oldPath = oldToken === '/dev/null' ? null : normalizeWorkspacePath(stripPrefix(oldToken, 'a'));
    if (relPath.length === 0) {
      throw new PatchFormatError(`patch line ${headerLineNo + 1}: empty target path`);
    }
    const pathCheck = relativePathArg.safeParse(relPath);
    if (!pathCheck.success) {
      throw new PatchFormatError(
        `patch line ${headerLineNo + 1}: target path ${quote(relPath)} is not a safe workspace-relative path (${
          pathCheck.error.issues[0]?.message ?? 'rejected'
        })`,
      );
    }
    if (oldPath !== null && oldPath !== relPath) {
      throw new PatchFormatError(
        `patch line ${headerLineNo + 1}: rename patches are not supported (${quote(oldToken)} -> ${quote(newToken)})`,
      );
    }
    if (seen.has(relPath)) {
      throw new PatchFormatError(`patch contains more than one block for ${quote(relPath)}; merge them into one`);
    }
    seen.add(relPath);

    index += 2;
    const hunks: ParsedHunk[] = [];
    while (index < rawLines.length) {
      const candidate = rawLines[index] as string;
      if (!candidate.startsWith('@@')) {
        break;
      }
      const match = HUNK_HEADER.exec(candidate);
      if (match === null) {
        throw new PatchFormatError(`patch line ${index + 1}: malformed hunk header ${quote(candidate)}`);
      }
      const hunkLineNo = index + 1;
      const oldStart = Number(match[1]);
      const oldCount = match[2] === undefined ? null : Number(match[2]);
      const newCount = match[4] === undefined ? null : Number(match[4]);
      const expectedOld = oldCount ?? 1;
      const expectedNew = newCount ?? 1;
      index += 1;
      const body: PatchBodyLine[] = [];
      let needOld = expectedOld;
      let needNew = expectedNew;
      while (index < rawLines.length) {
        const bodyLine = rawLines[index] as string;
        const op = bodyLine[0] as string;
        if (op === '\\') {
          // "\ No newline at end of file" markers attach to the previous line.
          body.push({ op: '\\', text: bodyLine.slice(1) });
          index += 1;
          continue;
        }
        if (needOld === 0 && needNew === 0) {
          break;
        }
        if (op === ' ') {
          if (needOld === 0 || needNew === 0) {
            throw new PatchFormatError(
              `patch line ${index + 1}: unexpected context line in hunk for ${quote(relPath)} (header declares ${expectedOld} old and ${expectedNew} new line(s))`,
            );
          }
          body.push({ op: ' ', text: bodyLine.slice(1) });
          needOld -= 1;
          needNew -= 1;
        } else if (op === '-') {
          if (needOld === 0) {
            throw new PatchFormatError(
              `patch line ${index + 1}: more removed lines than the hunk header for ${quote(relPath)} declares (${expectedOld})`,
            );
          }
          body.push({ op: '-', text: bodyLine.slice(1) });
          needOld -= 1;
        } else if (op === '+') {
          if (needNew === 0) {
            throw new PatchFormatError(
              `patch line ${index + 1}: more added lines than the hunk header for ${quote(relPath)} declares (${expectedNew})`,
            );
          }
          body.push({ op: '+', text: bodyLine.slice(1) });
          needNew -= 1;
        } else {
          break;
        }
        index += 1;
      }
      if (needOld > 0 || needNew > 0) {
        throw new PatchFormatError(
          `patch line ${hunkLineNo}: hunk for ${quote(relPath)} declares ${expectedOld} old and ${expectedNew} new line(s) but its body is incomplete`,
        );
      }
      if (body.length === 0) {
        throw new PatchFormatError(`patch line ${hunkLineNo}: hunk for ${quote(relPath)} has no body lines`);
      }
      hunks.push({ lineNo: hunkLineNo, oldStart, oldCount, body });
    }

    if (hunks.length === 0) {
      throw new PatchFormatError(
        `patch line ${headerLineNo}: file block for ${quote(relPath)} has no hunks (mode-only changes are not supported)`,
      );
    }
    files.push({ relPath, isNew: oldToken === '/dev/null', hunks, headerLineNo });
  }

  if (files.length === 0) {
    throw new PatchFormatError('patch contains no file blocks (expected "--- a/<path>" / "+++ b/<path>" headers)');
  }
  return files;
}

type PreparedFile = {
  readonly relPath: string;
  readonly absPath: string;
  readonly isNew: boolean;
  readonly added: number;
  readonly removed: number;
  readonly newContent: string;
  /** Content before the patch (null for new files); used to roll back a failed multi-file commit. */
  readonly originalContent: string | null;
  readonly statBefore: { readonly mtimeMs: number; readonly size: number } | null;
  /** Staged temp file, set during the two-phase commit. */
  tempPath: string | null;
};

/**
 * Write every file's new content to a temp file first, then publish them all.
 * If any publish fails mid-way, already-published files are rolled back to
 * their original content (best effort), so an I/O failure cannot leave a
 * half-applied multi-file patch.
 */
function stageWrite(absPath: string, content: string): string {
  const parent = path.dirname(absPath);
  mkdirSync(parent, { recursive: true });
  const temp = path.join(parent, `.relay-patch-${process.pid}-${randomBytes(6).toString('hex')}`);
  writeFileSync(temp, content, 'utf8');
  return temp;
}

/** Publish one staged temp file (create-only hard link for new files). */
function commitStaged(absPath: string, temp: string, isNew: boolean): void {
  if (isNew) {
    linkSync(temp, absPath);
    unlinkSync(temp);
  } else {
    renameSync(temp, absPath);
  }
}

function cleanupTemp(temp: string | null): void {
  if (temp === null) return;
  try {
    unlinkSync(temp);
  } catch {
    // Best-effort: the file may already have been published or removed.
  }
}

/** Best-effort rollback of files already published by a failed commit loop. */
function rollbackPublished(file: PreparedFile): void {
  try {
    if (file.isNew) {
      unlinkSync(file.absPath);
      return;
    }
    if (file.originalContent !== null) {
      const restore = stageWrite(file.absPath, file.originalContent);
      commitStaged(file.absPath, restore, false);
    }
  } catch {
    // Rollback is best-effort; the failure is reported with the tool result.
  }
}

export function registerPatchTools(registry: ToolRegistry): void {
  const applyPatchArgs = z.object({
    patch: boundedString(200_000).describe('Unified diff text with "--- a/<path>" / "+++ b/<path>" headers and @@ hunks.'),
  });
  const applyPatch: ToolDefinition<typeof applyPatchArgs> = {
    name: 'apply-patch',
    title: 'Apply unified diff',
    description:
      'Apply a unified diff inside the workspace. Context lines must match exactly; on any conflict nothing is written. Creates new files via "--- /dev/null". Never deletes files.',
    capability: 'write',
    argsSchema: applyPatchArgs,
    timeoutMs: PATCH_TIMEOUT_MS,
    maxOutputChars: MAX_OUTPUT_CHARS,
    async execute(args, context): Promise<ToolResult> {
      if (context.signal.aborted) {
        return fail('apply-patch canceled before it started');
      }
      let files: readonly ParsedFile[];
      try {
        files = parsePatch(args.patch);
      } catch (error) {
        return fail(error instanceof PatchFormatError ? error.message : `cannot parse patch: ${errorMessage(error)}`);
      }

      const prepared: PreparedFile[] = [];
      try {
        for (const file of files) {
          const absPath = resolveWorkspacePath(context.workspaceRoot, file.relPath);
          let original: string;
          let statBefore: { mtimeMs: number; size: number } | null = null;
          if (file.isNew) {
            const existing = statSync(absPath, { throwIfNoEntry: false });
            if (existing !== undefined) {
              return fail(
                `cannot create "${file.relPath}" with this patch: the file already exists (patch line ${file.headerLineNo})`,
              );
            }
            original = '';
          } else {
            const info = statSync(absPath, { throwIfNoEntry: false });
            if (info === undefined) {
              return fail(
                `conflict: patch modifies "${file.relPath}" but that file does not exist (patch line ${file.headerLineNo}); use "--- /dev/null" to create it`,
              );
            }
            statBefore = { mtimeMs: info.mtimeMs, size: info.size };
            original = readFileSync(absPath, 'utf8');
          }

          const normalized = original.replace(/\r\n/g, '\n');
          const endedWithNewline = file.isNew ? true : normalized.length > 0 && normalized.endsWith('\n');
          const lines: string[] =
            normalized.length === 0 ? [] : normalized.slice(0, endedWithNewline ? -1 : undefined).split('\n');
          let delta = 0;
          let added = 0;
          let removed = 0;
          let noNewlineAtEnd = false;

          for (let hunkIndex = 0; hunkIndex < file.hunks.length; hunkIndex += 1) {
            const hunk = file.hunks[hunkIndex] as ParsedHunk;
            const start = Math.max(0, hunk.oldStart - 1 + delta);
            const emitted: string[] = [];
            let consumed = 0;
            let conflict: { expected: string; found: string | null; atLine: number } | null = null;

            for (const bodyLine of hunk.body) {
              if (bodyLine.op === '\\') {
                noNewlineAtEnd = true;
                continue;
              }
              if (bodyLine.op === '+') {
                emitted.push(bodyLine.text);
                added += 1;
                continue;
              }
              const actual = start + consumed < lines.length ? (lines[start + consumed] as string) : undefined;
              if (actual !== bodyLine.text) {
                conflict = { expected: bodyLine.text, found: actual ?? null, atLine: start + consumed + 1 };
                break;
              }
              if (bodyLine.op === ' ') {
                emitted.push(bodyLine.text);
              } else {
                removed += 1;
              }
              consumed += 1;
            }

            if (conflict !== null) {
              return fail(
                `conflict in "${file.relPath}" hunk ${hunkIndex + 1} (patch line ${hunk.lineNo}) at file line ${
                  conflict.atLine
                }: patch expected ${quote(conflict.expected)} but found ${
                  conflict.found === null ? '<end of file>' : quote(conflict.found)
                }. Nothing was written; re-read the file and regenerate the patch against current content.`,
              );
            }

            lines.splice(start, consumed, ...emitted);
            delta += emitted.length - consumed;
          }

          const newContent =
            lines.length === 0 ? '' : `${lines.join('\n')}${noNewlineAtEnd ? '' : endedWithNewline ? '\n' : ''}`;
          prepared.push({
            relPath: file.relPath,
            absPath,
            isNew: file.isNew,
            added,
            removed,
            newContent,
            originalContent: file.isNew ? null : original,
            statBefore,
            tempPath: null,
          });
        }

        // Concurrency guard: refuse to write if any target changed since read.
        for (const file of prepared) {
          if (file.statBefore === null) {
            continue;
          }
          const now = statSync(file.absPath, { throwIfNoEntry: false });
          if (now === undefined || now.mtimeMs !== file.statBefore.mtimeMs || now.size !== file.statBefore.size) {
            return fail(
              `file changed while patch was prepared: "${file.relPath}" (size or modification time differs from when the patch read it); re-read the file and re-apply`,
            );
          }
        }
        if (context.signal.aborted) {
          return fail('apply-patch canceled before any file was written');
        }

        // Phase 1: stage every file's new content next to its target.
        for (const file of prepared) {
          file.tempPath = stageWrite(file.absPath, file.newContent);
        }
        // Phase 2: publish. A failure rolls back what was already published.
        const published: PreparedFile[] = [];
        try {
          for (const file of prepared) {
            commitStaged(file.absPath, file.tempPath as string, file.isNew);
            file.tempPath = null;
            published.push(file);
          }
        } catch (error) {
          for (const file of published.reverse()) {
            rollbackPublished(file);
          }
          for (const file of prepared) {
            cleanupTemp(file.tempPath);
          }
          throw error;
        }
      } catch (error) {
        for (const file of prepared) {
          cleanupTemp(file.tempPath);
        }
        if (error instanceof PathEscapeError) {
          return fail(error.message);
        }
        return fail(`apply-patch failed: ${errorMessage(error)}`);
      }

      const totalAdded = prepared.reduce((sum, file) => sum + file.added, 0);
      const totalRemoved = prepared.reduce((sum, file) => sum + file.removed, 0);
      const details = prepared
        .map((file) =>
          file.isNew
            ? `  ${file.relPath}: created (+${file.added} lines)`
            : `  ${file.relPath}: +${file.added} -${file.removed} lines`,
        )
        .join('\n');
      const output = `applied patch to ${prepared.length} file${prepared.length === 1 ? '' : 's'}:\n${details}\ntotal: +${totalAdded} -${totalRemoved} lines`;
      context.log(`apply-patch changed ${prepared.length} file(s)`);
      return {
        ok: true,
        outcome: 'ok',
        output: boundOutput(redactSecrets(output), MAX_OUTPUT_CHARS),
        data: {
          files: prepared.map((file) => ({
            path: file.relPath,
            created: file.isNew,
            added: file.added,
            removed: file.removed,
          })),
        },
        affectedFiles: prepared.map((file) => file.relPath),
      };
    },
  };

  registry.register(applyPatch);
}
