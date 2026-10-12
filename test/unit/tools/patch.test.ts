import { afterEach, describe, expect, it } from 'vitest';
import { readFile, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { ToolRegistry } from '../../../src/tools/registry.js';
import { registerPatchTools } from '../../../src/tools/patch.js';
import { callTool, cleanupWorkspaces, makeContext, makeWorkspace } from './test-kit.js';

function patchTools() {
  const registry = new ToolRegistry();
  registerPatchTools(registry);
  return registry;
}

const APPLY = 'apply-patch';

afterEach(async () => {
  await cleanupWorkspaces();
});

describe('apply-patch', () => {
  it('applies a simple modification and reports changed lines', async () => {
    const ws = await makeWorkspace();
    await writeFile(path.join(ws, 'a.txt'), 'one\ntwo\nthree\n');
    const result = await callTool(
      patchTools().get(APPLY),
      { patch: '--- a/a.txt\n+++ b/a.txt\n@@ -1,3 +1,3 @@\n one\n-two\n+TWO\n three\n' },
      makeContext(ws),
    );
    expect(result.ok).toBe(true);
    expect(result.affectedFiles).toEqual(['a.txt']);
    expect(result.output).toContain('+1 -1 lines');
    expect(await readFile(path.join(ws, 'a.txt'), 'utf8')).toBe('one\nTWO\nthree\n');
  });

  it('creates new files via /dev/null headers', async () => {
    const ws = await makeWorkspace();
    const result = await callTool(
      patchTools().get(APPLY),
      { patch: '--- /dev/null\n+++ b/docs/created.md\n@@ -0,0 +1,2 @@\n+# Title\n+Body\n' },
      makeContext(ws),
    );
    expect(result.ok).toBe(true);
    expect(result.affectedFiles).toEqual(['docs/created.md']);
    expect(await readFile(path.join(ws, 'docs/created.md'), 'utf8')).toBe('# Title\nBody\n');
  });

  it('applies multiple hunks in one file with offset bookkeeping', async () => {
    const ws = await makeWorkspace();
    await writeFile(path.join(ws, 'multi.txt'), 'a\nb\nc\nd\ne\nf\n');
    const patch = [
      '--- a/multi.txt',
      '+++ b/multi.txt',
      '@@ -1,2 +1,3 @@',
      ' a',
      '+a2',
      ' b',
      '@@ -5,2 +6,2 @@',
      ' e',
      '-f',
      '+F',
    ].join('\n');
    const result = await callTool(patchTools().get(APPLY), { patch }, makeContext(ws));
    expect(result.ok).toBe(true);
    expect(await readFile(path.join(ws, 'multi.txt'), 'utf8')).toBe('a\na2\nb\nc\nd\ne\nF\n');
  });

  it('writes nothing when any hunk conflicts (atomic across files)', async () => {
    const ws = await makeWorkspace();
    const goodBefore = 'alpha\nbeta\n';
    const badBefore = 'x\ny\n';
    await writeFile(path.join(ws, 'good.txt'), goodBefore);
    await writeFile(path.join(ws, 'bad.txt'), badBefore);
    const patch = [
      '--- a/good.txt',
      '+++ b/good.txt',
      '@@ -1,2 +1,2 @@',
      ' alpha',
      '-beta',
      '+BETA',
      '--- a/bad.txt',
      '+++ b/bad.txt',
      '@@ -1,2 +1,2 @@',
      ' x',
      '-not-present',
      '+changed',
    ].join('\n');
    const result = await callTool(patchTools().get(APPLY), { patch }, makeContext(ws));
    expect(result.ok).toBe(false);
    expect(result.affectedFiles).toEqual([]);
    expect(result.output).toContain('"bad.txt" hunk 1');
    expect(await readFile(path.join(ws, 'good.txt'), 'utf8')).toBe(goodBefore);
    expect(await readFile(path.join(ws, 'bad.txt'), 'utf8')).toBe(badBefore);
  });

  it('names file, hunk, and expected content in conflict messages', async () => {
    const ws = await makeWorkspace();
    await writeFile(path.join(ws, 'a.txt'), 'one\ntwo\n');
    const result = await callTool(
      patchTools().get(APPLY),
      { patch: '--- a/a.txt\n+++ b/a.txt\n@@ -1,2 +1,2 @@\n one\n-three\n+TWO\n' },
      makeContext(ws),
    );
    expect(result.ok).toBe(false);
    expect(result.output).toContain('"a.txt" hunk 1');
    expect(result.output).toContain('"three"');
    expect(result.output).toContain('"two"');
    expect(result.output).toMatch(/Nothing was written/);
  });

  it('rejects traversal and absolute target paths', async () => {
    const ws = await makeWorkspace();
    const traversal = await callTool(
      patchTools().get(APPLY),
      { patch: '--- a/x.txt\n+++ b/../evil.txt\n@@ -1 +1 @@\n-a\n+b\n' },
      makeContext(ws),
    );
    expect(traversal.ok).toBe(false);
    expect(traversal.output).toMatch(/not a safe workspace-relative path/);
    const absolute = await callTool(
      patchTools().get(APPLY),
      { patch: '--- a/x.txt\n+++ /etc/passwd\n@@ -1 +1 @@\n-a\n+b\n' },
      makeContext(ws),
    );
    expect(absolute.ok).toBe(false);
    expect(absolute.output).toMatch(/not a safe workspace-relative path/);
  });

  it('rejects file deletion headers', async () => {
    const ws = await makeWorkspace();
    await writeFile(path.join(ws, 'victim.txt'), 'bye\n');
    const result = await callTool(
      patchTools().get(APPLY),
      { patch: '--- a/victim.txt\n+++ /dev/null\n@@ -1 +0,0 @@\n-bye\n' },
      makeContext(ws),
    );
    expect(result.ok).toBe(false);
    expect(result.output).toMatch(/deletion is not supported/);
    expect(await readFile(path.join(ws, 'victim.txt'), 'utf8')).toBe('bye\n');
  });

  it('rejects malformed hunk headers with a line number', async () => {
    const ws = await makeWorkspace();
    const result = await callTool(
      patchTools().get(APPLY),
      { patch: '--- a/a.txt\n+++ b/a.txt\n@@ nonsense @@\n-a\n+b\n' },
      makeContext(ws),
    );
    expect(result.ok).toBe(false);
    expect(result.output).toMatch(/patch line 3: malformed hunk header/);
  });

  it('rejects hunks whose declared line counts do not match the body', async () => {
    const ws = await makeWorkspace();
    const result = await callTool(
      patchTools().get(APPLY),
      { patch: '--- a/a.txt\n+++ b/a.txt\n@@ -1,3 +1,2 @@\n one\n-two\n+TWO\n' },
      makeContext(ws),
    );
    expect(result.ok).toBe(false);
    expect(result.output).toMatch(/declares 3 old and 2 new line/);
  });

  it('conflicts when a patched file does not exist', async () => {
    const ws = await makeWorkspace();
    const result = await callTool(
      patchTools().get(APPLY),
      { patch: '--- a/ghost.txt\n+++ b/ghost.txt\n@@ -1 +1 @@\n-old\n+new\n' },
      makeContext(ws),
    );
    expect(result.ok).toBe(false);
    expect(result.output).toMatch(/does not exist/);
    expect(result.output).toMatch(/dev\/null/);
  });

  it('refuses to create a file that already exists', async () => {
    const ws = await makeWorkspace();
    await writeFile(path.join(ws, 'there.txt'), 'already here\n');
    const result = await callTool(
      patchTools().get(APPLY),
      { patch: '--- /dev/null\n+++ b/there.txt\n@@ -0,0 +1,1 @@\n+new\n' },
      makeContext(ws),
    );
    expect(result.ok).toBe(false);
    expect(result.output).toMatch(/already exists/);
    expect(await readFile(path.join(ws, 'there.txt'), 'utf8')).toBe('already here\n');
  });

  it('honors the no-newline-at-end marker', async () => {
    const ws = await makeWorkspace();
    await writeFile(path.join(ws, 'noeol.txt'), 'end');
    const patch = '--- a/noeol.txt\n+++ b/noeol.txt\n@@ -1 +1 @@\n-end\n+END\n\\ No newline at end of file\n';
    const result = await callTool(patchTools().get(APPLY), { patch }, makeContext(ws));
    expect(result.ok).toBe(true);
    expect(await readFile(path.join(ws, 'noeol.txt'), 'utf8')).toBe('END');
  });

  it('rejects patches with no file blocks', async () => {
    const ws = await makeWorkspace();
    const result = await callTool(patchTools().get(APPLY), { patch: 'just some text\n' }, makeContext(ws));
    expect(result.ok).toBe(false);
    expect(result.output).toMatch(/no file blocks|unexpected/);
  });
});
