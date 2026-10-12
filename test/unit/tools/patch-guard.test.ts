import { afterEach, describe, expect, it, vi } from 'vitest';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { ToolRegistry } from '../../../src/tools/registry.js';
import { registerPatchTools } from '../../../src/tools/patch.js';

/**
 * The concurrency guard compares the target's stat between the patch read and
 * the pre-write re-check. This suite intercepts node:fs statSync so the second
 * observation reports a changed file, exactly as if the user had edited it
 * while the patch was being prepared.
 */
const guard = vi.hoisted(() => ({ armed: false, observations: new Map<string, number>() }));

vi.mock('node:fs', async (importOriginal) => {
  const actual = await importOriginal<typeof import('node:fs')>();
  const realStat = actual.statSync;
  return {
    ...actual,
    statSync: ((target: unknown, options?: unknown) => {
      const real = (realStat as unknown as (t: unknown, o?: unknown) => { mtimeMs: number; size: number })(
        target,
        options,
      );
      if (!guard.armed) {
        return real;
      }
      const key = String(target);
      const seen = guard.observations.get(key) ?? 0;
      guard.observations.set(key, seen + 1);
      // Each observation reports a different mtime than the last, simulating
      // a user edit landing between the patch's read and the pre-write check.
      // (Existence checks in the path guard do not look at mtime.)
      return { ...real, mtimeMs: real.mtimeMs + 1_000 * (seen + 1) };
    }) as typeof actual.statSync,
  };
});

const workspaces: string[] = [];

afterEach(async () => {
  guard.armed = false;
  guard.observations.clear();
  for (const dir of workspaces.splice(0)) {
    await rm(dir, { recursive: true, force: true });
  }
});

describe('apply-patch concurrency guard', () => {
  it('aborts when the target changed after the patch read it', async () => {
    const ws = await mkdtemp(path.join(tmpdir(), 'relay-patch-guard-'));
    workspaces.push(ws);
    await writeFile(path.join(ws, 'watched.txt'), 'alpha\nbeta\n');

    const registry = new ToolRegistry();
    registerPatchTools(registry);
    const patch = '--- a/watched.txt\n+++ b/watched.txt\n@@ -1,2 +1,2 @@\n alpha\n-beta\n+BETA\n';

    guard.armed = true;
    const result = await registry.get('apply-patch').execute(
      { patch },
      { workspaceRoot: ws, signal: new AbortController().signal, log: () => {} },
    );

    expect(result.ok).toBe(false);
    expect(result.outcome).toBe('error');
    expect(result.affectedFiles).toEqual([]);
    expect(result.output).toContain('file changed while patch was prepared');
    expect(await readFile(path.join(ws, 'watched.txt'), 'utf8')).toBe('alpha\nbeta\n');
  });

  it('still applies cleanly when stats are unchanged', async () => {
    const ws = await mkdtemp(path.join(tmpdir(), 'relay-patch-guard-'));
    workspaces.push(ws);
    await writeFile(path.join(ws, 'stable.txt'), 'alpha\nbeta\n');

    const registry = new ToolRegistry();
    registerPatchTools(registry);
    const patch = '--- a/stable.txt\n+++ b/stable.txt\n@@ -1,2 +1,2 @@\n alpha\n-beta\n+BETA\n';

    guard.armed = false;
    const result = await registry.get('apply-patch').execute(
      { patch },
      { workspaceRoot: ws, signal: new AbortController().signal, log: () => {} },
    );
    expect(result.ok).toBe(true);
    expect(await readFile(path.join(ws, 'stable.txt'), 'utf8')).toBe('alpha\nBETA\n');
  });
});
