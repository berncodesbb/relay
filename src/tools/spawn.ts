/**
 * Internal bounded subprocess execution shared by process and git tools.
 *
 * Always spawns with an argument array (never a shell string), detaches the
 * child into its own process group so the whole tree can be terminated on
 * timeout or cancellation, bounds collected stdout/stderr memory, and
 * restricts inherited environment variables to an explicit allowlist.
 */

import { spawn, type ChildProcess } from 'node:child_process';
import process from 'node:process';
import { envNameLooksSecret } from './redact.js';

/** Environment variable names children always inherit. */
export const BASE_ENV_NAMES: readonly string[] = ['PATH', 'HOME', 'LANG', 'TERM', 'TMPDIR', 'CI', 'TZ', 'SHELL'];

/** Environment variable name prefixes children always inherit (e.g. LC_ALL). */
export const BASE_ENV_PREFIXES: readonly string[] = ['LC_'];

/** Default per-stream character cap for collected child output. */
export const DEFAULT_STREAM_CAP_CHARS = 200_000;

export type BoundedSpawnRequest = {
  readonly command: string;
  readonly args: readonly string[];
  readonly cwd: string;
  readonly timeoutMs: number;
  readonly signal: AbortSignal;
  /** Names of extra environment variables to inherit on top of the allowlist. */
  readonly extraEnv?: readonly string[] | undefined;
  readonly maxStreamChars?: number | undefined;
};

export type BoundedSpawnResult = {
  readonly exitCode: number | null;
  readonly termSignal: string | null;
  readonly stdout: string;
  readonly stderr: string;
  readonly durationMs: number;
  readonly timedOut: boolean;
  readonly canceled: boolean;
  readonly spawnError: string | null;
  /** Inherited variable names, for recording (names only, never values). */
  readonly inheritedEnv: readonly string[];
  /** Extra requested names refused because they look secret-bearing. */
  readonly refusedEnv: readonly string[];
};

export type BoundedEnv = {
  readonly env: NodeJS.ProcessEnv;
  readonly inherited: readonly string[];
  readonly refused: readonly string[];
};

/** Build a minimal child environment from the allowlist plus named extras. */
export function buildBoundedEnv(extraNames: readonly string[] = []): BoundedEnv {
  const extra = new Set(extraNames);
  const env: NodeJS.ProcessEnv = {};
  const inherited: string[] = [];
  const refused: string[] = [];
  for (const name of Object.keys(process.env)) {
    if (envNameLooksSecret(name)) {
      if (extra.has(name)) {
        refused.push(name);
      }
      continue;
    }
    const allowed = BASE_ENV_NAMES.includes(name) || BASE_ENV_PREFIXES.some((prefix) => name.startsWith(prefix)) || extra.has(name);
    if (!allowed) {
      continue;
    }
    const value = process.env[name];
    if (value === undefined) {
      continue;
    }
    env[name] = value;
    inherited.push(name);
  }
  return { env, inherited, refused };
}

/** Bounded byte collector that keeps the head and the tail of a stream. */
class HeadTailBuffer {
  static readonly HEAD_LIMIT = 1_000_000;
  static readonly TAIL_LIMIT = 1_000_000;
  private head: Buffer = Buffer.alloc(0);
  private tail: Buffer = Buffer.alloc(0);

  push(chunk: Buffer): void {
    let data = chunk;
    if (this.head.length < HeadTailBuffer.HEAD_LIMIT) {
      const take = Math.min(HeadTailBuffer.HEAD_LIMIT - this.head.length, data.length);
      this.head = Buffer.concat([this.head, data.subarray(0, take)]);
      data = data.subarray(take);
    }
    if (data.length === 0) {
      return;
    }
    const merged = Buffer.concat([this.tail, data]);
    if (merged.length > HeadTailBuffer.TAIL_LIMIT) {
      this.tail = merged.subarray(merged.length - HeadTailBuffer.TAIL_LIMIT);
    } else {
      this.tail = merged;
    }
  }

  toStringUtf8(): string {
    return Buffer.concat([this.head, this.tail]).toString('utf8');
  }
}

/** Cap decoded stream text, keeping head and tail around a truncation marker. */
export function capStreamText(text: string, maxChars: number): string {
  if (maxChars < 64) {
    return text.slice(0, Math.max(0, maxChars));
  }
  if (text.length <= maxChars) {
    return text;
  }
  const budget = maxChars - 32;
  const headChars = Math.floor(budget * 0.75);
  const tailChars = budget - headChars;
  const dropped = text.length - headChars - tailChars;
  const marker = `\n…[${dropped} chars truncated]…\n`;
  return `${text.slice(0, headChars)}${marker}${text.slice(text.length - tailChars)}`;
}

function killGroup(child: ChildProcess, signal: NodeJS.Signals): void {
  if (child.pid !== undefined && child.pid > 0) {
    try {
      process.kill(-child.pid, signal);
      return;
    } catch {
      // Process group already gone; fall through to direct kill.
    }
  }
  try {
    child.kill(signal);
  } catch {
    // Child already exited.
  }
}

/**
 * Spawn a command with bounded output, timeout, abort, and process-group
 * termination. Never resolves with a thrown error; failures are reported in
 * the result (spawnError / timedOut / canceled).
 */
export function spawnBounded(request: BoundedSpawnRequest): Promise<BoundedSpawnResult> {
  const startedAt = Date.now();
  const maxStreamChars = request.maxStreamChars ?? DEFAULT_STREAM_CAP_CHARS;
  const { env, inherited, refused } = buildBoundedEnv(request.extraEnv);

  return new Promise<BoundedSpawnResult>((resolve) => {
    const stdout = new HeadTailBuffer();
    const stderr = new HeadTailBuffer();
    let settled = false;
    let timedOut = false;
    let canceled = false;
    let spawnError: string | null = null;
    let escalateTimer: NodeJS.Timeout | null = null;
    let timeoutTimer: NodeJS.Timeout | null = null;

    const child = spawn(request.command, request.args, {
      cwd: request.cwd,
      env,
      stdio: ['ignore', 'pipe', 'pipe'],
      detached: true,
    });

    const onAbort = (): void => {
      canceled = true;
      killGroup(child, 'SIGTERM');
      escalateTimer = setTimeout(() => killGroup(child, 'SIGKILL'), 2_000);
    };

    const finish = (): void => {
      if (settled) {
        return;
      }
      settled = true;
      if (timeoutTimer !== null) clearTimeout(timeoutTimer);
      if (escalateTimer !== null) clearTimeout(escalateTimer);
      request.signal.removeEventListener('abort', onAbort);
      resolve({
        exitCode: child.exitCode,
        termSignal: child.signalCode,
        stdout: capStreamText(stdout.toStringUtf8(), maxStreamChars),
        stderr: capStreamText(stderr.toStringUtf8(), maxStreamChars),
        durationMs: Date.now() - startedAt,
        timedOut,
        canceled,
        spawnError,
        inheritedEnv: inherited,
        refusedEnv: refused,
      });
    };

    child.stdout?.on('data', (chunk: Buffer) => stdout.push(chunk));
    child.stderr?.on('data', (chunk: Buffer) => stderr.push(chunk));
    child.on('error', (error: NodeJS.ErrnoException) => {
      spawnError = error.message;
      finish();
    });
    child.on('close', () => finish());

    timeoutTimer = setTimeout(() => {
      timedOut = true;
      killGroup(child, 'SIGTERM');
      escalateTimer = setTimeout(() => killGroup(child, 'SIGKILL'), 2_000);
    }, request.timeoutMs);

    if (request.signal.aborted) {
      onAbort();
    } else {
      request.signal.addEventListener('abort', onAbort, { once: true });
    }
  });
}
