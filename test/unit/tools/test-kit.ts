/**
 * Shared helpers for tool unit tests: throwaway workspaces, tool contexts,
 * argument validation, and small tool settings.
 */

import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import type { RegisteredTool, ToolContext, ToolResult } from '../../../src/tools/registry.js';
import type { ToolSettings } from '../../../src/config/schema.js';

const workspaces: string[] = [];

/** Create a throwaway workspace directory under the OS temp dir. */
export async function makeWorkspace(): Promise<string> {
  const dir = await mkdtemp(path.join(tmpdir(), 'relay-tools-'));
  workspaces.push(dir);
  return dir;
}

/** Remove every workspace created by {@link makeWorkspace}. */
export async function cleanupWorkspaces(): Promise<void> {
  while (workspaces.length > 0) {
    const dir = workspaces.pop();
    if (dir !== undefined) {
      await rm(dir, { recursive: true, force: true });
    }
  }
}

/** Build a tool context rooted at `root`. */
export function makeContext(root: string, signal?: AbortSignal): ToolContext {
  return {
    workspaceRoot: root,
    signal: signal ?? new AbortController().signal,
    log: () => {
      // Tests intentionally discard tool log lines.
    },
  };
}

/** Small tool settings for tests, with optional overrides. */
export function testSettings(overrides: Partial<ToolSettings> = {}): ToolSettings {
  return {
    commandTimeoutMs: 10_000,
    maxSearchResults: 200,
    maxFileBytes: 1_000_000,
    ...overrides,
  };
}

/**
 * Validate arguments through the tool's schema (as the runtime does) and
 * execute. Schema failures surface as an error result naming the issue.
 */
export async function callTool(tool: RegisteredTool, args: unknown, context: ToolContext): Promise<ToolResult> {
  const parsed = tool.argsSchema.safeParse(args);
  if (!parsed.success) {
    const issues = parsed.error.issues.map((issue) => `${issue.path.join('.') || '(root)'}: ${issue.message}`);
    return { ok: false, outcome: 'error', output: `invalid arguments: ${issues.join('; ')}`, affectedFiles: [] };
  }
  return tool.execute(parsed.data, context);
}
