import { afterEach, describe, expect, it } from 'vitest';
import { mkdir, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { ToolRegistry } from '../../../src/tools/registry.js';
import { registerProcessTools } from '../../../src/tools/process.js';
import { callTool, cleanupWorkspaces, makeContext, makeWorkspace, testSettings } from './test-kit.js';

function processTools(commandTimeoutMs = 10_000) {
  const registry = new ToolRegistry();
  registerProcessTools(registry, testSettings({ commandTimeoutMs }));
  return registry;
}

afterEach(async () => {
  await cleanupWorkspaces();
});

describe('run-command', () => {
  it('runs node and reports exit code and stdout', async () => {
    const ws = await makeWorkspace();
    const result = await callTool(
      processTools().get('run-command'),
      { command: 'node', args: ['-e', 'console.log("hello relay")'] },
      makeContext(ws),
    );
    expect(result.ok).toBe(true);
    expect(result.output).toContain('exit code: 0');
    expect(result.output).toContain('hello relay');
    expect(result.output).toContain('--- stderr ---');
    expect(result.affectedFiles).toEqual([]);
  });

  it('passes arguments with spaces and metacharacters verbatim (no shell)', async () => {
    const ws = await makeWorkspace();
    await writeFile(path.join(ws, 'show-args.mjs'), 'console.log(JSON.stringify(process.argv.slice(2)));\n');
    const tricky = ['a b;c', 'x|y', '$HOME', '`id`', '"quoted"', "single'quote"];
    const result = await callTool(
      processTools().get('run-command'),
      { command: 'node', args: ['show-args.mjs', ...tricky] },
      makeContext(ws),
    );
    expect(result.ok).toBe(true);
    const stdout = result.output.split('--- stdout ---')[1]?.split('--- stderr ---')[0]?.trim() ?? '';
    expect(stdout).toBe(JSON.stringify(tricky));
  });

  it('rejects shell strings as the command', async () => {
    const ws = await makeWorkspace();
    const result = await callTool(
      processTools().get('run-command'),
      { command: 'echo hi; rm -rf /', args: [] },
      makeContext(ws),
    );
    expect(result.ok).toBe(false);
    expect(result.output).toMatch(/invalid arguments/);
    expect(result.output).toMatch(/shell/);
  });

  it('rejects an empty command', async () => {
    const ws = await makeWorkspace();
    const result = await callTool(processTools().get('run-command'), { command: '', args: [] }, makeContext(ws));
    expect(result.ok).toBe(false);
    expect(result.output).toMatch(/invalid arguments/);
  });

  it('kills the process group on timeout', async () => {
    const ws = await makeWorkspace();
    const result = await callTool(
      processTools(500).get('run-command'),
      { command: 'node', args: ['-e', 'setTimeout(() => {}, 60000)'] },
      makeContext(ws),
    );
    expect(result.ok).toBe(false);
    expect(result.outcome).toBe('error');
    expect(result.output).toMatch(/timed out after 500ms/);
    const durationText = result.output.match(/duration: (\d+)ms/)?.[1];
    expect(durationText === undefined ? Number.MAX_SAFE_INTEGER : Number(durationText)).toBeLessThan(10_000);
  });

  it('kills the process group promptly on abort', async () => {
    const ws = await makeWorkspace();
    const controller = new AbortController();
    setTimeout(() => controller.abort(), 150);
    const started = Date.now();
    const result = await callTool(
      processTools().get('run-command'),
      { command: 'node', args: ['-e', 'setTimeout(() => {}, 60000)'] },
      makeContext(ws, controller.signal),
    );
    expect(result.ok).toBe(false);
    expect(result.output).toMatch(/canceled/);
    expect(Date.now() - started).toBeLessThan(10_000);
  });

  it('does not inherit non-allowlisted environment variables', async () => {
    const ws = await makeWorkspace();
    process.env.RELAY_UNLISTED_VAR = 'leaky-value-123';
    try {
      const result = await callTool(
        processTools().get('run-command'),
        { command: 'node', args: ['-e', 'console.log(process.env.RELAY_UNLISTED_VAR ?? "absent")'] },
        makeContext(ws),
      );
      expect(result.ok).toBe(true);
      expect(result.output).toContain('absent');
      expect(result.output).not.toContain('leaky-value-123');
    } finally {
      delete process.env.RELAY_UNLISTED_VAR;
    }
  });

  it('inherits base variables like PATH and HOME', async () => {
    const ws = await makeWorkspace();
    const result = await callTool(
      processTools().get('run-command'),
      {
        command: 'node',
        args: ['-e', 'console.log(process.env.PATH && process.env.HOME ? "env-ok" : "env-missing")'],
      },
      makeContext(ws),
    );
    expect(result.ok).toBe(true);
    expect(result.output).toContain('env-ok');
  });

  it('inherits explicitly named extra variables and records only names', async () => {
    const ws = await makeWorkspace();
    process.env.RELAY_UNLISTED_VAR = 'visible-when-named';
    try {
      const result = await callTool(
        processTools().get('run-command'),
        {
          command: 'node',
          args: ['-e', 'console.log(process.env.RELAY_UNLISTED_VAR ?? "absent")'],
          env: ['RELAY_UNLISTED_VAR'],
        },
        makeContext(ws),
      );
      expect(result.ok).toBe(true);
      expect(result.output).toContain('visible-when-named');
      expect(result.output).toContain('extra env inherited: RELAY_UNLISTED_VAR');
    } finally {
      delete process.env.RELAY_UNLISTED_VAR;
    }
  });

  it('refuses to inherit secret-looking variables even when named', async () => {
    const ws = await makeWorkspace();
    process.env.RELAY_TEST_API_KEY = 'should-never-cross';
    try {
      const result = await callTool(
        processTools().get('run-command'),
        { command: 'node', args: ['-e', 'console.log(1)'], env: ['RELAY_TEST_API_KEY'] },
        makeContext(ws),
      );
      expect(result.ok).toBe(false);
      expect(result.output).toMatch(/refusing to inherit secret-looking/);
      expect(result.output).toContain('RELAY_TEST_API_KEY');
      expect(result.output).not.toContain('should-never-cross');
    } finally {
      delete process.env.RELAY_TEST_API_KEY;
    }
  });

  it('bounds oversized stdout', async () => {
    const ws = await makeWorkspace();
    const result = await callTool(
      processTools().get('run-command'),
      { command: 'node', args: ['-e', 'console.log("z".repeat(300000))'] },
      makeContext(ws),
    );
    expect(result.ok).toBe(true);
    expect(result.output.length).toBeLessThanOrEqual(40_000);
    expect(result.output).toMatch(/truncated/);
  });

  it('reports non-zero exit codes as errors', async () => {
    const ws = await makeWorkspace();
    const result = await callTool(
      processTools().get('run-command'),
      { command: 'node', args: ['-e', 'process.exit(3)'] },
      makeContext(ws),
    );
    expect(result.ok).toBe(false);
    expect(result.output).toContain('exit code: 3');
  });

  it('reports a missing executable clearly', async () => {
    const ws = await makeWorkspace();
    const result = await callTool(
      processTools().get('run-command'),
      { command: 'definitely-not-a-real-binary-xyz', args: ['--flag'] },
      makeContext(ws),
    );
    expect(result.ok).toBe(false);
    expect(result.output).toMatch(/failed to start/);
  });

  it('honors a workspace-relative cwd', async () => {
    const ws = await makeWorkspace();
    await mkdir(path.join(ws, 'inner'));
    const result = await callTool(
      processTools().get('run-command'),
      { command: 'node', args: ['-e', 'console.log(process.cwd())'], cwd: 'inner' },
      makeContext(ws),
    );
    expect(result.ok).toBe(true);
    expect(result.output).toMatch(/inner/);
  });
});
