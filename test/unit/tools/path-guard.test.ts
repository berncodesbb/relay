import { afterEach, describe, expect, it } from 'vitest';
import { mkdir, mkdtemp, rm, symlink, writeFile } from 'node:fs/promises';
import { realpathSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { isWithinWorkspace, PathEscapeError, resolveWorkspacePath } from '../../../src/tools/path-guard.js';
import { cleanupWorkspaces, makeWorkspace } from './test-kit.js';

const extraDirs: string[] = [];

afterEach(async () => {
  await cleanupWorkspaces();
  for (const dir of extraDirs.splice(0)) {
    await rm(dir, { recursive: true, force: true });
  }
});

describe('resolveWorkspacePath', () => {
  it('rejects absolute paths', () => {
    const ws = '/definitely/not/real';
    expect(() => resolveWorkspacePath(ws, '/etc/passwd')).toThrow(PathEscapeError);
    expect(() => resolveWorkspacePath(ws, '/etc/passwd')).toThrow(/absolute paths are not allowed/);
  });

  it('rejects parent traversal', () => {
    expect(() => resolveWorkspacePath('/definitely/not/real', '../outside.txt')).toThrow(PathEscapeError);
    expect(() => resolveWorkspacePath('/definitely/not/real', 'a/../../outside.txt')).toThrow(PathEscapeError);
    expect(() => resolveWorkspacePath('/definitely/not/real', 'a\\..\\..\\b.txt')).toThrow(PathEscapeError);
  });

  it('rejects home shortcuts and empty paths', () => {
    expect(() => resolveWorkspacePath('/definitely/not/real', '~')).toThrow(PathEscapeError);
    expect(() => resolveWorkspacePath('/definitely/not/real', '~/.ssh/id_rsa')).toThrow(PathEscapeError);
    expect(() => resolveWorkspacePath('/definitely/not/real', '')).toThrow(PathEscapeError);
  });

  it('rejects a symlink inside the workspace that points outside', async () => {
    const ws = await makeWorkspace();
    const outside = await mkdtemp(path.join(tmpdir(), 'relay-outside-'));
    extraDirs.push(outside);
    await writeFile(path.join(outside, 'secret.txt'), 'secret');
    await symlink(outside, path.join(ws, 'escape'));

    expect(() => resolveWorkspacePath(ws, 'escape/secret.txt')).toThrow(PathEscapeError);
    expect(() => resolveWorkspacePath(ws, 'escape/secret.txt')).toThrow(/symlink/);
  });

  it('rejects deep symlink chains that leave the workspace', async () => {
    const ws = await makeWorkspace();
    const outside = await mkdtemp(path.join(tmpdir(), 'relay-outside-'));
    extraDirs.push(outside);
    await symlink(outside, path.join(ws, 'hop-two'));
    await symlink(path.join(ws, 'hop-two'), path.join(ws, 'hop-one'));

    expect(() => resolveWorkspacePath(ws, 'hop-one/file.txt')).toThrow(PathEscapeError);
  });

  it('allows symlinks that stay inside the workspace', async () => {
    const ws = await makeWorkspace();
    const realDir = path.join(ws, 'real');
    await mkdir(realDir);
    await symlink(realDir, path.join(ws, 'alias'));

    const resolved = resolveWorkspacePath(ws, 'alias/file.txt');
    expect(resolved).toBe(path.join(realpathSync(realDir), 'file.txt'));
    expect(isWithinWorkspace(realpathSync(ws), resolved)).toBe(true);
  });

  it('resolves nonexistent nested paths inside the workspace', async () => {
    const ws = await makeWorkspace();
    const resolved = resolveWorkspacePath(ws, 'a/b/c.txt');
    expect(resolved).toBe(path.join(realpathSync(ws), 'a', 'b', 'c.txt'));
    expect(isWithinWorkspace(realpathSync(ws), resolved)).toBe(true);
  });

  it('normalizes "./" and trailing separators', async () => {
    const ws = await makeWorkspace();
    const resolved = resolveWorkspacePath(ws, './src/');
    expect(resolved).toBe(path.join(realpathSync(ws), 'src'));
  });

  it('works when the workspace root itself is behind a symlink', async () => {
    const parent = await makeWorkspace();
    const realRoot = path.join(parent, 'real-root');
    await mkdir(realRoot);
    const linkedRoot = path.join(parent, 'link-root');
    await symlink(realRoot, linkedRoot);
    const realRootResolved = realpathSync(realRoot);

    expect(resolveWorkspacePath(linkedRoot, 'notes.txt')).toBe(path.join(realRootResolved, 'notes.txt'));
    expect(resolveWorkspacePath(linkedRoot, 'deep/nested/notes.txt')).toBe(
      path.join(realRootResolved, 'deep', 'nested', 'notes.txt'),
    );
  });

  it('throws when the workspace root does not exist', () => {
    expect(() => resolveWorkspacePath(path.join(tmpdir(), 'relay-missing-root-xyz'), 'a.txt')).toThrow(
      PathEscapeError,
    );
  });
});

describe('isWithinWorkspace', () => {
  it('accepts the root itself and paths inside it', () => {
    expect(isWithinWorkspace('/tmp/ws', '/tmp/ws')).toBe(true);
    expect(isWithinWorkspace('/tmp/ws', '/tmp/ws/a/b.txt')).toBe(true);
  });

  it('rejects siblings with a shared prefix', () => {
    expect(isWithinWorkspace('/tmp/ws', '/tmp/ws2/file.txt')).toBe(false);
    expect(isWithinWorkspace('/tmp/ws', '/tmp')).toBe(false);
    expect(isWithinWorkspace('/tmp/ws', '/etc/passwd')).toBe(false);
  });

  it('does not reject files merely named with leading dots', () => {
    expect(isWithinWorkspace('/tmp/ws', '/tmp/ws/..hidden')).toBe(true);
  });
});
