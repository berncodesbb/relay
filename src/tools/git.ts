/**
 * Read-only Git tools plus repository instruction discovery.
 *
 * Git is spawned exactly like any other child process: argument arrays, a
 * bounded environment, timeouts, and group termination. No mutating Git
 * operation (add/commit/push/checkout/reset/stash/clean) is registered, so
 * user worktree state can never be rewritten by a tool.
 */

import { readFile } from 'node:fs/promises';
import path from 'node:path';
import type { ToolContext, ToolDefinition, ToolRegistry, ToolResult } from './registry.js';
import { normalizeWorkspacePath } from './path-guard.js';
import { boundOutput, redactSecrets } from './redact.js';
import { spawnBounded, type BoundedSpawnResult } from './spawn.js';
import { relativePathArg } from './schemas.js';
import { z } from 'zod';

const GIT_TIMEOUT_MS = 20_000;
const MAX_OUTPUT_CHARS = 30_000;
const INSTRUCTION_FILES: readonly string[] = ['CLAUDE.md', 'AGENTS.md', 'README.md'];
const INSTRUCTION_FILE_CHAR_LIMIT = 20_000;

function fail(output: string): ToolResult {
  return { ok: false, outcome: 'error', output, affectedFiles: [] };
}

function done(output: string): ToolResult {
  return { ok: true, outcome: 'ok', output, affectedFiles: [] };
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

/**
 * Run git with an argument array in the workspace. Not registered as a tool;
 * exported so future read-only helpers share identical safety rails.
 */
export async function runGit(args: readonly string[], context: ToolContext): Promise<BoundedSpawnResult> {
  return spawnBounded({
    command: 'git',
    args,
    cwd: context.workspaceRoot,
    timeoutMs: GIT_TIMEOUT_MS,
    signal: context.signal,
  });
}

function notARepository(stderr: string): boolean {
  return /not a git repository/i.test(stderr) || /must be run in a work tree/i.test(stderr);
}

function gitFailure(context: ToolContext, result: BoundedSpawnResult): ToolResult | null {
  if (result.canceled) {
    return fail('git command canceled');
  }
  if (result.timedOut) {
    return fail(`git command timed out after ${GIT_TIMEOUT_MS}ms`);
  }
  if (result.spawnError !== null) {
    return fail(`git failed to start: ${result.spawnError} (is git installed and on PATH?)`);
  }
  if (result.exitCode === 0) {
    return null;
  }
  if (/does not have any commits/i.test(result.stderr) || /does not have a commit/i.test(result.stderr)) {
    return fail('repository has no commits yet (history is empty)');
  }
  if (notARepository(result.stderr)) {
    return fail(
      `${path.basename(context.workspaceRoot)} is not a git repository (no .git found in it or any parent). Initialize one with "git init" or point Relay at a repository root.`,
    );
  }
  return fail(`git exited with code ${result.exitCode ?? 'unknown'}:\n${result.stderr.trim()}`);
}

export function registerGitTools(registry: ToolRegistry): void {
  const noArgs = z.object({}).strict();
  const gitStatus: ToolDefinition<typeof noArgs> = {
    name: 'git-status',
    title: 'Git status',
    description: 'Show `git status --porcelain=v1 -b` plus a one-line change summary. Read-only.',
    capability: 'read',
    argsSchema: noArgs,
    timeoutMs: GIT_TIMEOUT_MS,
    maxOutputChars: MAX_OUTPUT_CHARS,
    async execute(_args, context): Promise<ToolResult> {
      const result = await runGit(['status', '--porcelain=v1', '-b'], context);
      const failure = gitFailure(context, result);
      if (failure !== null) {
        return failure;
      }
      const lines = result.stdout.split('\n').filter((line) => line.length > 0);
      const entries = lines.filter((line) => !line.startsWith('##'));
      const untracked = entries.filter((line) => line.startsWith('??')).length;
      const modified = entries.filter((line) => /^[ MARC][MD]/.test(line) || /^M/.test(line)).length;
      const added = entries.filter((line) => /^A/.test(line)).length;
      const deleted = entries.filter((line) => /^[AMD]?D/.test(line) && !line.startsWith('??')).length;
      const summary = `${entries.length} changed ${entries.length === 1 ? 'entry' : 'entries'} (${added} added, ${modified} modified, ${deleted} deleted, ${untracked} untracked)`;
      return done(boundOutput(redactSecrets(`${result.stdout.trim()}\n${summary}`), MAX_OUTPUT_CHARS));
    },
  };

  const gitDiffArgs = z.object({
    path: relativePathArg.optional().describe('Limit the diff to this workspace-relative path.'),
    staged: z.boolean().default(false).describe('Show staged (cached) changes instead of the working tree.'),
  });
  const gitDiff: ToolDefinition<typeof gitDiffArgs> = {
    name: 'git-diff',
    title: 'Git diff',
    description: 'Show `git diff` (unified) for the whole tree or one workspace-relative path; pass staged=true for --cached. Read-only.',
    capability: 'read',
    argsSchema: gitDiffArgs,
    timeoutMs: GIT_TIMEOUT_MS,
    maxOutputChars: MAX_OUTPUT_CHARS,
    async execute(args, context): Promise<ToolResult> {
      const gitArgs = ['diff', '--no-color'];
      if (args.staged) {
        gitArgs.push('--cached');
      }
      if (args.path !== undefined) {
        gitArgs.push('--', normalizeWorkspacePath(args.path));
      }
      const result = await runGit(gitArgs, context);
      const failure = gitFailure(context, result);
      if (failure !== null) {
        return failure;
      }
      const body = result.stdout.trim();
      const output = body === '' ? 'no changes' : `${body}\n(${body.split('\n').length} diff lines)`;
      return done(boundOutput(redactSecrets(output), MAX_OUTPUT_CHARS));
    },
  };

  const gitLogArgs = z.object({
    limit: z.number().int().min(1).max(200).default(20).describe('Number of commits to show.'),
  });
  const gitLog: ToolDefinition<typeof gitLogArgs> = {
    name: 'git-log',
    title: 'Git log',
    description: 'Show the most recent commits as `git log --oneline -n <limit>`. Read-only.',
    capability: 'read',
    argsSchema: gitLogArgs,
    timeoutMs: GIT_TIMEOUT_MS,
    maxOutputChars: MAX_OUTPUT_CHARS,
    async execute(args, context): Promise<ToolResult> {
      const result = await runGit(['log', '--oneline', '--no-color', '-n', String(args.limit)], context);
      const failure = gitFailure(context, result);
      if (failure !== null) {
        return failure;
      }
      const body = result.stdout.trim();
      if (body === '') {
        return done('no commits yet');
      }
      const count = body.split('\n').length;
      return done(boundOutput(redactSecrets(`${body}\n(${count} commits)`), MAX_OUTPUT_CHARS));
    },
  };

  const repoInstructions: ToolDefinition<typeof noArgs> = {
    name: 'repo-instructions',
    title: 'Repository instructions',
    description:
      'Read repository instruction files from the workspace root in priority order: CLAUDE.md, AGENTS.md, README.md (first 20000 chars each).',
    capability: 'read',
    argsSchema: noArgs,
    timeoutMs: GIT_TIMEOUT_MS,
    maxOutputChars: 60_000,
    async execute(_args, context): Promise<ToolResult> {
      const sections: string[] = [];
      for (const name of INSTRUCTION_FILES) {
        if (context.signal.aborted) {
          return fail('repo-instructions canceled');
        }
        const absolute = path.join(context.workspaceRoot, name);
        let content: string;
        try {
          content = await readFile(absolute, 'utf8');
        } catch {
          continue;
        }
        const header =
          content.length > INSTRUCTION_FILE_CHAR_LIMIT
            ? `# ${name} (first ${INSTRUCTION_FILE_CHAR_LIMIT} of ${content.length} chars)`
            : `# ${name} (${content.length} chars)`;
        sections.push(`${header}\n${content.slice(0, INSTRUCTION_FILE_CHAR_LIMIT)}`);
      }
      if (sections.length === 0) {
        return done(
          `no repository instruction files found at the workspace root (looked for ${INSTRUCTION_FILES.join(', ')})`,
        );
      }
      return done(boundOutput(redactSecrets(sections.join('\n\n')), 60_000));
    },
  };

  registry.register(gitStatus);
  registry.register(gitDiff);
  registry.register(gitLog);
  registry.register(repoInstructions);
}
