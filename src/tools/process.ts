/**
 * Bounded subprocess tool.
 *
 * Runs a single executable with an argument array — never a shell string —
 * in the workspace (or a workspace-relative subdirectory), with an explicit
 * environment allowlist, separate stream-collected stdout/stderr, a timeout
 * that terminates the whole process group, and abort support.
 */

import path from 'node:path';
import type { ToolDefinition, ToolRegistry, ToolResult } from './registry.js';
import { PathEscapeError, resolveWorkspacePath } from './path-guard.js';
import { boundOutput, envNameLooksSecret, redactSecrets, sanitizeEnvForRecord } from './redact.js';
import { spawnBounded } from './spawn.js';
import type { ToolSettings } from '../config/schema.js';
import { z } from 'zod';

const MAX_OUTPUT_CHARS = 40_000;

/** Shell metacharacters forbidden in a bare command name. */
const COMMAND_METACHARS = /[;|&$`><\n\r]/;

function fail(output: string): ToolResult {
  return { ok: false, outcome: 'error', output, affectedFiles: [] };
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function displayArg(value: string): string {
  return /[\s"'\\]/.test(value) ? JSON.stringify(value) : value;
}

const commandArg = z
  .string()
  .min(1)
  .max(500)
  .refine(
    (value) => {
      if (value.includes('\0')) {
        return false;
      }
      if (path.isAbsolute(value)) {
        return true;
      }
      return !COMMAND_METACHARS.test(value);
    },
    {
      message:
        'command must be an executable name or absolute path, not a shell string (metacharacters ; | & $ ` > < and newlines are rejected); pass arguments in `args`',
    },
  );

export function registerProcessTools(registry: ToolRegistry, settings: ToolSettings): void {
  const runCommandArgs = z.object({
      command: commandArg.describe('Executable name or absolute path. Never a shell string.'),
      args: z
        .array(z.string().min(0).max(20_000).refine((value) => !value.includes('\0'), { message: 'arguments must not contain NUL bytes' }))
        .max(200)
        .default([])
        .describe('Argument array passed verbatim (no shell interpretation).'),
      cwd: z.string().min(1).max(1000).optional().describe('Workspace-relative working directory; defaults to the workspace root.'),
      timeoutMs: z.number().int().min(100).max(600_000).optional().describe('Kill the process group after this long (capped by settings).'),
      env: z
        .array(z.string().regex(/^[A-Za-z_][A-Za-z0-9_]*$/, 'env entries must be variable names only'))
        .max(50)
        .optional()
        .describe('Names of extra environment variables to inherit (names only; secret-shaped names are refused).'),
  });
  const runCommand: ToolDefinition<typeof runCommandArgs> = {
    name: 'run-command',
    title: 'Run command',
    description:
      'Run an executable with an argument array inside the workspace (no shell). Bounded env inheritance, separate stdout/stderr, timeout and cancellation kill the whole process group.',
    capability: 'execute',
    argsSchema: runCommandArgs,
    timeoutMs: settings.commandTimeoutMs,
    maxOutputChars: MAX_OUTPUT_CHARS,
    async execute(args, context): Promise<ToolResult> {
      if (context.signal.aborted) {
        return fail('run-command canceled before it started');
      }

      let cwd = context.workspaceRoot;
      if (args.cwd !== undefined) {
        try {
          cwd = resolveWorkspacePath(context.workspaceRoot, args.cwd);
        } catch (error) {
          return fail(
            error instanceof PathEscapeError ? error.message : `cannot resolve cwd "${args.cwd}": ${errorMessage(error)}`,
          );
        }
      }

      const timeoutMs = Math.min(args.timeoutMs ?? settings.commandTimeoutMs, settings.commandTimeoutMs);
      const extraEnv = sanitizeEnvForRecord(args.env ?? []);
      const refused = extraEnv.filter((name) => envNameLooksSecret(name));
      if (refused.length > 0) {
        return fail(
          `refusing to inherit secret-looking environment variables: ${refused.join(
            ', ',
          )}. Secret-shaped names are never passed to child processes; set a non-secret variable instead.`,
        );
      }
      const display = [args.command, ...args.args].map(displayArg).join(' ');

      context.log(`run-command: ${args.command} (${args.args.length} args, timeout ${timeoutMs}ms)`);

      const result = await spawnBounded({
        command: args.command,
        args: args.args,
        cwd,
        timeoutMs,
        signal: context.signal,
        extraEnv,
      });

      const envNote =
        extraEnv.length === 0
          ? ''
          : `\nextra env inherited: ${extraEnv.join(', ')}`;
      const statusLine = result.spawnError !== null
        ? `failed to start: ${result.spawnError}`
        : result.canceled
          ? 'canceled before completion'
          : result.timedOut
            ? `timed out after ${timeoutMs}ms`
            : `exit code: ${result.exitCode ?? 'unknown'}${result.termSignal !== null ? ` (signal ${result.termSignal})` : ''}`;

      const body = [
        `$ ${display} (cwd: ${path.relative(context.workspaceRoot, cwd) || '.'})`,
        statusLine,
        `duration: ${result.durationMs}ms${envNote}`,
        '--- stdout ---',
        result.stdout.length === 0 ? '(empty)' : result.stdout,
        '--- stderr ---',
        result.stderr.length === 0 ? '(empty)' : result.stderr,
      ].join('\n');
      const output = boundOutput(redactSecrets(body), MAX_OUTPUT_CHARS);

      const ok = !result.canceled && !result.timedOut && result.spawnError === null && result.exitCode === 0;
      return {
        ok,
        outcome: ok ? 'ok' : 'error',
        output,
        data: {
          exitCode: result.exitCode,
          termSignal: result.termSignal,
          durationMs: result.durationMs,
          timedOut: result.timedOut,
          canceled: result.canceled,
          envInherited: sanitizeEnvForRecord(result.inheritedEnv),
        },
        affectedFiles: [],
      };
    },
  };

  registry.register(runCommand);
}
