/**
 * Workspace text search tool.
 *
 * Walks the workspace without following symlinks, skipping vendored/build
 * directories and oversized files, and matches per line: literal substring by
 * default, or a caller-supplied regular expression. Output is always redacted
 * and bounded, with a match/file summary line.
 */

import { readdir, readFile, stat } from 'node:fs/promises';
import path from 'node:path';
import type { ToolDefinition, ToolRegistry, ToolResult } from './registry.js';
import { normalizeWorkspacePath, PathEscapeError, resolveWorkspacePath } from './path-guard.js';
import { boundOutput, redactSecrets } from './redact.js';
import { boundedString } from './schemas.js';
import type { ToolSettings } from '../config/schema.js';
import { z } from 'zod';

const SEARCH_TIMEOUT_MS = 30_000;
const SEARCH_DEADLINE_MS = 25_000;
const MAX_OUTPUT_CHARS = 30_000;

const SKIPPED_DIR_NAMES: readonly string[] = ['node_modules', '.git', 'dist', '.relay', 'coverage'];

function fail(output: string): ToolResult {
  return { ok: false, outcome: 'error', output, affectedFiles: [] };
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

/** Convert a simple wildcard glob ('*.ts') to an anchored regular expression. */
function globToRegExp(glob: string): RegExp {
  const escaped = glob.replace(/[.+^${}()|[\]\\]/g, '\\$&').replace(/\*/g, '[^/]*').replace(/\?/g, '[^/]');
  return new RegExp(`^${escaped}$`);
}

export function registerSearchTools(registry: ToolRegistry, settings: ToolSettings): void {
  const searchTextArgs = z.object({
    query: boundedString(500).describe('Literal substring to find, or a regular expression when regex is true.'),
    regex: z.boolean().default(false).describe('Interpret `query` as a JavaScript regular expression.'),
    includeGlob: z
      .string()
      .min(1)
      .max(200)
      .optional()
      .describe("Simple filename filter such as '*.ts' (wildcards, no traversal)."),
    maxResults: z.number().int().min(1).max(10_000).optional().describe('Stop after this many matching lines.'),
  });
  const searchText: ToolDefinition<typeof searchTextArgs> = {
    name: 'search-text',
    title: 'Search workspace text',
    description:
      'Search all text files under the workspace root for a literal string (or regular expression with regex=true). Returns `path:line: text` matches and a summary.',
    capability: 'read',
    argsSchema: searchTextArgs,
    timeoutMs: SEARCH_TIMEOUT_MS,
    maxOutputChars: MAX_OUTPUT_CHARS,
    async execute(args, context): Promise<ToolResult> {
      const maxResults = Math.min(args.maxResults ?? settings.maxSearchResults, settings.maxSearchResults);

      let matcher: (line: string) => boolean;
      if (args.regex) {
        let pattern: RegExp;
        try {
          pattern = new RegExp(args.query);
        } catch (error) {
          return fail(
            `invalid regular expression /${args.query}/: ${errorMessage(error)}; fix the pattern or pass regex=false for a literal search`,
          );
        }
        matcher = (line) => pattern.test(line);
      } else {
        const needle = args.query;
        matcher = (line) => line.includes(needle);
      }

      let nameFilter: ((relativePath: string) => boolean) | null = null;
      if (args.includeGlob !== undefined) {
        if (args.includeGlob.includes('..') || args.includeGlob.startsWith('/')) {
          return fail(`invalid includeGlob "${args.includeGlob}": use a simple pattern like '*.ts'`);
        }
        const pattern = globToRegExp(args.includeGlob);
        const matchesPath = args.includeGlob.includes('/');
        nameFilter = (relativePath) =>
          matchesPath ? pattern.test(relativePath) : pattern.test(path.basename(relativePath));
      }

      let root: string;
      try {
        root = resolveWorkspacePath(context.workspaceRoot, '.');
      } catch (error) {
        return fail(error instanceof PathEscapeError ? error.message : `cannot resolve workspace: ${errorMessage(error)}`);
      }

      const files: string[] = [];
      const deadline = Date.now() + SEARCH_DEADLINE_MS;
      let aborted = false;
      let deadlineHit = false;

      const collect = async (dir: string, prefix: string): Promise<void> => {
        if (aborted || files.length >= 5_000) {
          return;
        }
        if (context.signal.aborted) {
          aborted = true;
          return;
        }
        if (Date.now() > deadline) {
          deadlineHit = true;
          return;
        }
        let entries;
        try {
          entries = await readdir(dir, { withFileTypes: true });
        } catch {
          return;
        }
        for (const entry of entries) {
          if (context.signal.aborted) {
            aborted = true;
            return;
          }
          if (entry.isDirectory()) {
            if (!SKIPPED_DIR_NAMES.includes(entry.name)) {
              await collect(path.join(dir, entry.name), prefix === '' ? entry.name : `${prefix}/${entry.name}`);
            }
            continue;
          }
          if (entry.isSymbolicLink()) {
            continue;
          }
          if (!entry.isFile()) {
            continue;
          }
          const relative = prefix === '' ? entry.name : `${prefix}/${entry.name}`;
          if (nameFilter !== null && !nameFilter(relative)) {
            continue;
          }
          files.push(relative);
        }
      };

      await collect(root, '');

      if (aborted) {
        return fail('search-text canceled');
      }

      const matchedFiles = new Set<string>();
      const out: string[] = [];
      let matchCount = 0;
      let stoppedAtLimit = false;

      for (const relative of files) {
        if (context.signal.aborted) {
          return fail('search-text canceled');
        }
        const absolute = path.join(root, relative);
        let info;
        try {
          info = await stat(absolute);
        } catch {
          continue;
        }
        if (!info.isFile() || info.size > settings.maxFileBytes) {
          continue;
        }
        let content: string;
        try {
          content = await readFile(absolute, 'utf8');
        } catch {
          continue;
        }
        if (content.includes('\0')) {
          continue;
        }
        const lines = content.split('\n');
        for (let index = 0; index < lines.length; index += 1) {
          const line = lines[index] as string;
          if (!matcher(line)) {
            continue;
          }
          out.push(`${relative}:${index + 1}: ${line.trim()}`);
          matchedFiles.add(relative);
          matchCount += 1;
          if (matchCount >= maxResults) {
            stoppedAtLimit = true;
            break;
          }
        }
        if (stoppedAtLimit) {
          break;
        }
      }

      if (context.signal.aborted) {
        return fail('search-text canceled');
      }

      const summary = `${matchCount} match${matchCount === 1 ? '' : 'es'} in ${matchedFiles.size} file${
        matchedFiles.size === 1 ? '' : 's'
      }${stoppedAtLimit ? ' (stopped at limit)' : ''}${deadlineHit ? ' (stopped early: scan deadline reached)' : ''}`;
      const body = out.length === 0 ? `no matches for ${args.regex ? `/${args.query}/` : JSON.stringify(args.query)}` : out.join('\n');
      const output = `${body}\n${summary}`;
      return {
        ok: true,
        outcome: 'ok',
        output: boundOutput(redactSecrets(output), MAX_OUTPUT_CHARS),
        data: { matches: matchCount, files: matchedFiles.size, stoppedAtLimit },
        affectedFiles: [],
      };
    },
  };

  registry.register(searchText);
}
