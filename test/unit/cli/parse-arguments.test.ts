import { describe, expect, it } from 'vitest';
import { parseArguments, USAGE } from '../../../src/cli/parse-arguments.js';

describe('parseArguments', () => {
  it('parses help and version', () => {
    expect(parseArguments(['--help'])).toMatchObject({ ok: true, value: { command: 'help' } });
    expect(parseArguments(['-h'])).toMatchObject({ ok: true, value: { command: 'help' } });
    expect(parseArguments(['--version'])).toMatchObject({ ok: true, value: { command: 'version' } });
    expect(parseArguments(['-V'])).toMatchObject({ ok: true, value: { command: 'version' } });
  });

  it('rejects missing and unknown commands with usage', () => {
    expect(parseArguments([])).toMatchObject({ ok: false, usage: true });
    expect(parseArguments(['frobnicate'])).toMatchObject({ ok: false, usage: true, error: expect.stringContaining('frobnicate') });
  });

  it('rejects short options other than -h/-V', () => {
    const result = parseArguments(['status', '-x']);
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.error).toContain('-x');
  });

  it('parses init with a goal flag or positional', () => {
    expect(parseArguments(['init', '--goal', 'ship a CLI'])).toMatchObject({
      ok: true,
      value: { command: 'init', goal: 'ship a CLI' },
    });
    expect(parseArguments(['init', 'ship', 'a', 'CLI'])).toMatchObject({
      ok: true,
      value: { command: 'init', goal: 'ship a CLI' },
    });
    expect(parseArguments(['init'])).toMatchObject({ ok: true, value: { command: 'init', goal: undefined } });
  });

  it('parses run with message, auto bound, and provider overrides', () => {
    const result = parseArguments(['run', 'build a timer app', '--auto', 'until:planning', '--provider', 'fake', '--no-interactive']);
    expect(result).toMatchObject({
      ok: true,
      value: {
        command: 'run',
        message: 'build a timer app',
        auto: 'until:planning',
        provider: 'fake',
        interactive: false,
      },
    });
  });

  it('supports --flag=value form and absolute workspaces', () => {
    const result = parseArguments(['--workspace=/tmp/x', 'run', '--json']);
    expect(result).toMatchObject({
      ok: true,
      workspace: '/tmp/x',
      value: { command: 'run', json: true },
    });
  });

  it('resolves relative workspaces against cwd', () => {
    const result = parseArguments(['--workspace', 'sub/dir', 'status']);
    expect(result.ok).toBe(true);
    if (result.ok) expect(result.workspace.endsWith('sub/dir')).toBe(true);
  });

  it('validates inspect, mode, and config arguments', () => {
    expect(parseArguments(['inspect'])).toMatchObject({ ok: false });
    expect(parseArguments(['inspect', 'a', 'b'])).toMatchObject({ ok: false });
    expect(parseArguments(['inspect', 'prd'])).toMatchObject({ ok: true, value: { command: 'inspect', target: 'prd' } });
    expect(parseArguments(['mode'])).toMatchObject({ ok: false });
    expect(parseArguments(['mode', 'planning'])).toMatchObject({ ok: true, value: { command: 'mode', mode: 'planning' } });
    expect(parseArguments(['config', 'frobnicate'])).toMatchObject({ ok: false });
    expect(parseArguments(['config', 'get'])).toMatchObject({ ok: false });
    expect(parseArguments(['config', 'get', 'autonomy'])).toMatchObject({
      ok: true,
      value: { command: 'config', action: 'get', key: 'autonomy' },
    });
    expect(parseArguments(['config'])).toMatchObject({ ok: true, value: { command: 'config', action: 'show' } });
  });

  it('parses pause reason and optional auto bound', () => {
    expect(parseArguments(['pause', 'reviewing', 'prd'])).toMatchObject({
      ok: true,
      value: { command: 'pause', reason: 'reviewing prd' },
    });
    expect(parseArguments(['auto'])).toMatchObject({ ok: true, value: { command: 'auto', bound: undefined } });
    expect(parseArguments(['auto', 'off'])).toMatchObject({ ok: true, value: { command: 'auto', bound: 'off' } });
  });

  it('usage text documents every command', () => {
    for (const command of ['init', 'run', 'status', 'inspect', 'mode', 'auto', 'pause', 'why', 'config']) {
      expect(USAGE).toContain(`relay ${command}`);
    }
  });
});
