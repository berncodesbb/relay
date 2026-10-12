/**
 * Filesystem tools: bounded reads, directory inventory, and create-only writes.
 *
 * Reads never return binary blobs and always pass through redaction and
 * output bounds. Writes only create NEW files (overwrites are refused so
 * user changes survive) and are atomic via write-temp-then-link.
 */

import { link, mkdir, open, readdir, rename, rm, stat, writeFile as writeTextFile } from 'node:fs/promises';
import { randomBytes } from 'node:crypto';
import path from 'node:path';
import type { ToolContext, ToolDefinition, ToolRegistry, ToolResult } from './registry.js';
import { normalizeWorkspacePath, PathEscapeError, resolveWorkspacePath } from './path-guard.js';
import { boundOutput, redactSecrets } from './redact.js';
import { boundedString, commonArgs, relativePathArg } from './schemas.js';
import type { ToolSettings } from '../config/schema.js';
import { z } from 'zod';

const READ_TIMEOUT_MS = 10_000;
const WRITE_TIMEOUT_MS = 10_000;
const MAX_OUTPUT_CHARS = 30_000;
const MAX_LIST_ENTRIES = 2_000;

/** Directory names never listed or descended into. */
const SKIPPED_DIR_NAMES: readonly string[] = ['node_modules', '.git', 'dist', '.relay', 'coverage'];

function fail(output: string): ToolResult {
  return { ok: false, outcome: 'error', output, affectedFiles: [] };
}

function done(output: string, affectedFiles: readonly string[] = []): ToolResult {
  return { ok: true, outcome: 'ok', output, affectedFiles };
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function canceled(context: ToolContext): boolean {
  if (context.signal.aborted) {
    return true;
  }
  return false;
}

export function registerFilesystemTools(registry: ToolRegistry, settings: ToolSettings): void {
  const readFileArgs = z.object({
    path: commonArgs.path,
    maxBytes: z.number().int().min(1).max(20_000_000).optional().describe('Read at most this many bytes.'),
  });
  const readFile: ToolDefinition<typeof readFileArgs> = {
    name: 'read-file',
    title: 'Read file',
    description:
      'Read a text file inside the workspace. Returns up to maxBytes with a size header; binary content is refused.',
    capability: 'read',
    argsSchema: readFileArgs,
    timeoutMs: READ_TIMEOUT_MS,
    maxOutputChars: MAX_OUTPUT_CHARS,
    async execute(args, context): Promise<ToolResult> {
      if (canceled(context)) {
        return fail('read-file canceled before it started');
      }
      const rel = normalizeWorkspacePath(args.path);
      let absolute: string;
      try {
        absolute = resolveWorkspacePath(context.workspaceRoot, rel);
      } catch (error) {
        return fail(error instanceof PathEscapeError ? error.message : `cannot resolve "${rel}": ${errorMessage(error)}`);
      }
      let info;
      try {
        info = await stat(absolute);
      } catch {
        return fail(`file not found: ${rel}`);
      }
      if (!info.isFile()) {
        return fail(`not a regular file: ${rel}`);
      }
      const maxBytes = Math.min(args.maxBytes ?? settings.maxFileBytes, settings.maxFileBytes);
      let handle;
      try {
        handle = await open(absolute, 'r');
      } catch (error) {
        return fail(`cannot open ${rel}: ${errorMessage(error)}`);
      }
      try {
        const buffer = Buffer.alloc(maxBytes);
        const { bytesRead } = await handle.read(buffer, 0, maxBytes, 0);
        const slice = buffer.subarray(0, bytesRead);
        const nulIndex = slice.indexOf(0);
        if (nulIndex !== -1) {
          return fail(`refusing to read ${rel}: binary content (NUL byte at offset ${nulIndex}); only text files are readable`);
        }
        const content = slice.toString('utf8');
        const header = `# ${rel} (first ${bytesRead} of ${info.size} bytes)`;
        return done(boundOutput(redactSecrets(`${header}\n${content}`), MAX_OUTPUT_CHARS));
      } finally {
        await handle.close();
      }
    },
  };

  const listDirArgs = z.object({
    path: relativePathArg.default('.').describe('Workspace-relative directory path; defaults to the workspace root.'),
    depth: z.number().int().min(1).max(4).default(1).describe('How many directory levels to list.'),
  });
  const listDir: ToolDefinition<typeof listDirArgs> = {
    name: 'list-dir',
    title: 'List directory',
    description:
      'List workspace directories as a tree (directories first), up to `depth` levels. Skips node_modules, .git, dist, .relay, and coverage.',
    capability: 'read',
    argsSchema: listDirArgs,
    timeoutMs: READ_TIMEOUT_MS,
    maxOutputChars: MAX_OUTPUT_CHARS,
    async execute(args, context): Promise<ToolResult> {
      if (canceled(context)) {
        return fail('list-dir canceled before it started');
      }
      const rel = normalizeWorkspacePath(args.path);
      let absolute: string;
      try {
        absolute = resolveWorkspacePath(context.workspaceRoot, rel);
      } catch (error) {
        return fail(error instanceof PathEscapeError ? error.message : `cannot resolve "${rel}": ${errorMessage(error)}`);
      }
      let info;
      try {
        info = await stat(absolute);
      } catch {
        return fail(`directory not found: ${rel}`);
      }
      if (!info.isDirectory()) {
        return fail(`not a directory: ${rel}`);
      }
      const lines: string[] = [];
      let truncated = false;
      let aborted = false;

      const walk = async (dir: string, remaining: number, indent: string): Promise<void> => {
        if (aborted || truncated) {
          return;
        }
        if (context.signal.aborted) {
          aborted = true;
          return;
        }
        let entries;
        try {
          entries = await readdir(dir, { withFileTypes: true });
        } catch (error) {
          lines.push(`${indent}(unreadable: ${errorMessage(error)})`);
          return;
        }
        entries.sort((a, b) => a.name.localeCompare(b.name));
        const dirs = entries.filter((entry) => entry.isDirectory() && !SKIPPED_DIR_NAMES.includes(entry.name));
        const others = entries.filter(
          (entry) => !entry.isDirectory() || SKIPPED_DIR_NAMES.includes(entry.name),
        );
        for (const entry of [...dirs, ...others]) {
          if (lines.length >= MAX_LIST_ENTRIES) {
            truncated = true;
            return;
          }
          const label = entry.isSymbolicLink()
            ? `${entry.name} (symlink)`
            : entry.isDirectory()
              ? `${entry.name}/`
              : entry.name;
          lines.push(`${indent}${label}`);
          if (entry.isDirectory() && !SKIPPED_DIR_NAMES.includes(entry.name) && remaining > 1) {
            await walk(path.join(dir, entry.name), remaining - 1, `${indent}  `);
          }
        }
      };

      await walk(absolute, args.depth, '');
      if (aborted) {
        return fail('list-dir canceled');
      }
      if (truncated) {
        lines.push('… (listing stopped at entry limit)');
      }
      if (lines.length === 0) {
        return done(`# ${rel} (empty directory)`);
      }
      return done(boundOutput(redactSecrets(`# ${rel}\n${lines.join('\n')}`), MAX_OUTPUT_CHARS));
    },
  };

  const writeFileArgs = z.object({
    path: commonArgs.path,
    content: boundedString(2_000_000).describe('Full file content to write.'),
  });
  const writeFileTool: ToolDefinition<typeof writeFileArgs> = {
    name: 'write-file',
    title: 'Write new file',
    description:
      'Create a NEW text file inside the workspace (with parent directories). Refuses to overwrite existing files; use apply-patch to modify them.',
    capability: 'write',
    argsSchema: writeFileArgs,
    timeoutMs: WRITE_TIMEOUT_MS,
    maxOutputChars: MAX_OUTPUT_CHARS,
    async execute(args, context): Promise<ToolResult> {
      if (canceled(context)) {
        return fail('write-file canceled before it started');
      }
      const rel = normalizeWorkspacePath(args.path);
      let absolute: string;
      try {
        absolute = resolveWorkspacePath(context.workspaceRoot, rel);
      } catch (error) {
        return fail(error instanceof PathEscapeError ? error.message : `cannot resolve "${rel}": ${errorMessage(error)}`);
      }
      const existing = await stat(absolute).catch(() => null);
      if (existing !== null) {
        return fail(`refusing to overwrite existing file ${rel}; use apply-patch to modify it`);
      }
      const parent = path.dirname(absolute);
      try {
        await mkdir(parent, { recursive: true });
        const temp = path.join(parent, `.relay-tmp-${process.pid}-${randomBytes(6).toString('hex')}`);
        await writeTextFile(temp, args.content, 'utf8');
        try {
          await link(temp, absolute);
        } catch (error) {
          const code = (error as NodeJS.ErrnoException).code;
          await rm(temp, { force: true });
          if (code === 'EEXIST') {
            return fail(`refusing to overwrite existing file ${rel}; use apply-patch to modify it`);
          }
          if (code !== undefined && code !== 'EPERM' && code !== 'EXDEV' && code !== 'ENOTSUP' && code !== 'EINVAL') {
            return fail(`cannot create ${rel}: ${errorMessage(error)}`);
          }
          // Filesystems without hardlink support fall back to rename; the
          // stat above plus rename keeps the create-only window minimal.
          const race = await stat(absolute).catch(() => null);
          if (race !== null) {
            return fail(`refusing to overwrite existing file ${rel}; use apply-patch to modify it`);
          }
          await rename(temp, absolute);
        } finally {
          await rm(temp, { force: true });
        }
      } catch (error) {
        return fail(`cannot write ${rel}: ${errorMessage(error)}`);
      }
      if (context.signal.aborted) {
        return fail(`write-file canceled (file ${rel} was already created)`);
      }
      context.log(`write-file created ${rel} (${Buffer.byteLength(args.content, 'utf8')} bytes)`);
      return done(`created ${rel} (${Buffer.byteLength(args.content, 'utf8')} bytes)`, [rel]);
    },
  };

  registry.register(readFile);
  registry.register(listDir);
  registry.register(writeFileTool);
}
