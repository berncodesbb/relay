import { afterEach, describe, expect, it } from 'vitest';
import { mkdir, mkdtemp, readFile, rm, symlink, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { ToolRegistry } from '../../../src/tools/registry.js';
import { registerFilesystemTools } from '../../../src/tools/filesystem.js';
import { callTool, cleanupWorkspaces, makeContext, makeWorkspace, testSettings } from './test-kit.js';

const extraDirs: string[] = [];

function fileTools(maxFileBytes = 1_000_000) {
  const registry = new ToolRegistry();
  registerFilesystemTools(registry, testSettings({ maxFileBytes }));
  return registry;
}

afterEach(async () => {
  await cleanupWorkspaces();
  for (const dir of extraDirs.splice(0)) {
    await rm(dir, { recursive: true, force: true });
  }
});

describe('read-file', () => {
  it('reads file content with a size header', async () => {
    const ws = await makeWorkspace();
    await writeFile(path.join(ws, 'notes.txt'), 'hello\nworld\n');
    const registry = fileTools();
    const result = await callTool(registry.get('read-file'), { path: 'notes.txt' }, makeContext(ws));
    expect(result.ok).toBe(true);
    expect(result.output.startsWith('# notes.txt (first 12 of 12 bytes)')).toBe(true);
    expect(result.output).toContain('hello\nworld');
    expect(result.affectedFiles).toEqual([]);
  });

  it('caps reads at the configured byte limit', async () => {
    const ws = await makeWorkspace();
    await writeFile(path.join(ws, 'big.txt'), 'x'.repeat(200));
    const registry = fileTools(64);
    const result = await callTool(registry.get('read-file'), { path: 'big.txt' }, makeContext(ws));
    expect(result.ok).toBe(true);
    expect(result.output.startsWith('# big.txt (first 64 of 200 bytes)')).toBe(true);
    expect(result.output).not.toContain('x'.repeat(65));
  });

  it('refuses binary content', async () => {
    const ws = await makeWorkspace();
    await writeFile(path.join(ws, 'blob.bin'), Buffer.from([0x61, 0x62, 0x00, 0x63]));
    const registry = fileTools();
    const result = await callTool(registry.get('read-file'), { path: 'blob.bin' }, makeContext(ws));
    expect(result.ok).toBe(false);
    expect(result.outcome).toBe('error');
    expect(result.output).toMatch(/binary content/);
  });

  it('redacts secret-looking content', async () => {
    const ws = await makeWorkspace();
    await writeFile(path.join(ws, 'config.txt'), 'token=sk-abcdefghijklmnop1234567890\n');
    const registry = fileTools();
    const result = await callTool(registry.get('read-file'), { path: 'config.txt' }, makeContext(ws));
    expect(result.ok).toBe(true);
    expect(result.output).toContain('[redacted:api-key]');
    expect(result.output).not.toContain('sk-abcdefghijklmnop1234567890');
  });

  it('errors for missing files and directories', async () => {
    const ws = await makeWorkspace();
    await mkdir(path.join(ws, 'pkg'));
    const registry = fileTools();
    const missing = await callTool(registry.get('read-file'), { path: 'nope.txt' }, makeContext(ws));
    expect(missing.ok).toBe(false);
    expect(missing.output).toMatch(/file not found/);
    const dir = await callTool(registry.get('read-file'), { path: 'pkg' }, makeContext(ws));
    expect(dir.ok).toBe(false);
    expect(dir.output).toMatch(/not a regular file/);
  });

  it('rejects absolute and traversing paths at validation', async () => {
    const ws = await makeWorkspace();
    const registry = fileTools();
    const absolute = await callTool(registry.get('read-file'), { path: '/etc/passwd' }, makeContext(ws));
    expect(absolute.ok).toBe(false);
    expect(absolute.output).toMatch(/invalid arguments/);
    const traversal = await callTool(registry.get('read-file'), { path: '../ws/notes.txt' }, makeContext(ws));
    expect(traversal.ok).toBe(false);
    expect(traversal.output).toMatch(/invalid arguments/);
  });

  it('refuses to read through a symlink that leaves the workspace', async () => {
    const ws = await makeWorkspace();
    const outside = await mkdtemp(path.join(tmpdir(), 'relay-outside-'));
    extraDirs.push(outside);
    await writeFile(path.join(outside, 'secret.txt'), 'secret');
    await symlink(outside, path.join(ws, 'escape'));
    const registry = fileTools();
    const result = await callTool(registry.get('read-file'), { path: 'escape/secret.txt' }, makeContext(ws));
    expect(result.ok).toBe(false);
    expect(result.output).toMatch(/symlink/);
  });
});

describe('write-file', () => {
  it('creates a new file with parent directories', async () => {
    const ws = await makeWorkspace();
    const registry = fileTools();
    const result = await callTool(
      registry.get('write-file'),
      { path: 'src/deep/new.ts', content: 'export const x = 1;\n' },
      makeContext(ws),
    );
    expect(result.ok).toBe(true);
    expect(result.affectedFiles).toEqual(['src/deep/new.ts']);
    expect(await readFile(path.join(ws, 'src/deep/new.ts'), 'utf8')).toBe('export const x = 1;\n');
  });

  it('refuses to overwrite an existing file', async () => {
    const ws = await makeWorkspace();
    await writeFile(path.join(ws, 'existing.txt'), 'user content\n');
    const registry = fileTools();
    const result = await callTool(
      registry.get('write-file'),
      { path: 'existing.txt', content: 'replacement\n' },
      makeContext(ws),
    );
    expect(result.ok).toBe(false);
    expect(result.output).toBe('refusing to overwrite existing file existing.txt; use apply-patch to modify it');
    expect(await readFile(path.join(ws, 'existing.txt'), 'utf8')).toBe('user content\n');
  });

  it('refuses paths that leave the workspace', async () => {
    const ws = await makeWorkspace();
    const registry = fileTools();
    const traversal = await callTool(
      registry.get('write-file'),
      { path: '../evil.txt', content: 'nope\n' },
      makeContext(ws),
    );
    expect(traversal.ok).toBe(false);
    expect(traversal.output).toMatch(/invalid arguments/);
    const absolute = await callTool(
      registry.get('write-file'),
      { path: '/tmp/relay-absolute-evil.txt', content: 'nope\n' },
      makeContext(ws),
    );
    expect(absolute.ok).toBe(false);
  });

  it('refuses to create files through an escaping symlink and leaves nothing behind', async () => {
    const ws = await makeWorkspace();
    const outside = await mkdtemp(path.join(tmpdir(), 'relay-outside-'));
    extraDirs.push(outside);
    await symlink(outside, path.join(ws, 'escape'));
    const registry = fileTools();
    const result = await callTool(
      registry.get('write-file'),
      { path: 'escape/new.txt', content: 'nope\n' },
      makeContext(ws),
    );
    expect(result.ok).toBe(false);
    expect(result.output).toMatch(/symlink/);
    await expect(readFile(path.join(outside, 'new.txt'), 'utf8')).rejects.toThrow();
  });
});

describe('list-dir', () => {
  it('lists directories first and skips vendored directories', async () => {
    const ws = await makeWorkspace();
    await mkdir(path.join(ws, 'src/sub'), { recursive: true });
    await mkdir(path.join(ws, 'docs'));
    await mkdir(path.join(ws, 'node_modules/pkg'), { recursive: true });
    await writeFile(path.join(ws, 'root.txt'), 'x');
    await writeFile(path.join(ws, 'src/a.ts'), 'x');
    await writeFile(path.join(ws, 'src/sub/b.ts'), 'x');
    await writeFile(path.join(ws, 'docs/readme.md'), 'x');
    await writeFile(path.join(ws, 'node_modules/pkg/skip.js'), 'x');

    const registry = fileTools();
    const result = await callTool(registry.get('list-dir'), { path: '.', depth: 2 }, makeContext(ws));
    expect(result.ok).toBe(true);
    expect(result.output).toContain('src/');
    expect(result.output).toContain('docs/');
    expect(result.output).toContain('root.txt');
    expect(result.output).toContain('sub/');
    // Vendored directories are listed but never descended into.
    expect(result.output).toContain('node_modules/');
    expect(result.output).not.toContain('pkg/');
    expect(result.output).not.toContain('skip.js');
    const dirsFirst = result.output.indexOf('src/');
    expect(dirsFirst).toBeGreaterThan(-1);
    expect(dirsFirst).toBeLessThan(result.output.indexOf('root.txt'));
  });

  it('marks symlinks without following them', async () => {
    const ws = await makeWorkspace();
    const target = path.join(ws, 'real');
    await mkdir(target);
    await symlink(target, path.join(ws, 'alias'));
    const registry = fileTools();
    const result = await callTool(registry.get('list-dir'), { depth: 1 }, makeContext(ws));
    expect(result.ok).toBe(true);
    expect(result.output).toContain('alias (symlink)');
    expect(result.output).toContain('real/');
  });

  it('errors for missing directories', async () => {
    const ws = await makeWorkspace();
    const registry = fileTools();
    const result = await callTool(registry.get('list-dir'), { path: 'missing' }, makeContext(ws));
    expect(result.ok).toBe(false);
    expect(result.output).toMatch(/not found/);
  });
});
