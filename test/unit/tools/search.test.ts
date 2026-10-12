import { afterEach, describe, expect, it } from 'vitest';
import { mkdir, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { ToolRegistry } from '../../../src/tools/registry.js';
import { registerSearchTools } from '../../../src/tools/search.js';
import { callTool, cleanupWorkspaces, makeContext, makeWorkspace, testSettings } from './test-kit.js';

function searchTools(maxSearchResults = 200, maxFileBytes = 1_000_000) {
  const registry = new ToolRegistry();
  registerSearchTools(registry, testSettings({ maxSearchResults, maxFileBytes }));
  return registry;
}

afterEach(async () => {
  await cleanupWorkspaces();
});

async function seedWorkspace(ws: string): Promise<void> {
  await mkdir(path.join(ws, 'node_modules/pkg'), { recursive: true });
  await mkdir(path.join(ws, 'sub'));
  await writeFile(path.join(ws, 'a.ts'), 'import x\nconst needle = 1\nexport { x }\n');
  await writeFile(path.join(ws, 'b.md'), '# doc\nneedle here\nand needle again\n');
  await writeFile(path.join(ws, 'sub/c.ts'), 'NEEDLE uppercase\n');
  await writeFile(path.join(ws, 'node_modules/pkg/vendor.ts'), 'needle hidden\n');
}

describe('search-text', () => {
  it('finds literal matches with file and line numbers', async () => {
    const ws = await makeWorkspace();
    await seedWorkspace(ws);
    const registry = searchTools();
    const result = await callTool(registry.get('search-text'), { query: 'needle' }, makeContext(ws));
    expect(result.ok).toBe(true);
    expect(result.output).toContain('a.ts:2: const needle = 1');
    expect(result.output).toContain('b.md:2: needle here');
    expect(result.output).toContain('b.md:3: and needle again');
    expect(result.output).toMatch(/3 matches in 2 files/);
    expect(result.output).not.toContain('node_modules');
    expect(result.output).not.toContain('NEEDLE uppercase');
  });

  it('skips vendored directories entirely', async () => {
    const ws = await makeWorkspace();
    await seedWorkspace(ws);
    const registry = searchTools();
    const result = await callTool(registry.get('search-text'), { query: 'hidden' }, makeContext(ws));
    expect(result.ok).toBe(true);
    expect(result.output).toMatch(/0 matches in 0 files/);
  });

  it('supports regular expression mode and literal mode treats regex syntax as text', async () => {
    const ws = await makeWorkspace();
    await seedWorkspace(ws);
    const registry = searchTools();
    const regex = await callTool(registry.get('search-text'), { query: 'nee[d]+le', regex: true }, makeContext(ws));
    expect(regex.ok).toBe(true);
    expect(regex.output).toMatch(/3 matches in 2 files/);
    const literal = await callTool(registry.get('search-text'), { query: 'nee[d]+le' }, makeContext(ws));
    expect(literal.ok).toBe(true);
    expect(literal.output).toMatch(/0 matches in 0 files/);
  });

  it('reports invalid regular expressions with an actionable message', async () => {
    const ws = await makeWorkspace();
    const registry = searchTools();
    const result = await callTool(registry.get('search-text'), { query: 'a(b', regex: true }, makeContext(ws));
    expect(result.ok).toBe(false);
    expect(result.outcome).toBe('error');
    expect(result.output).toMatch(/invalid regular expression/);
    expect(result.output).toMatch(/regex=false/);
  });

  it('stops at the configured result limit', async () => {
    const ws = await makeWorkspace();
    await seedWorkspace(ws);
    const registry = searchTools(2);
    const result = await callTool(registry.get('search-text'), { query: 'needle' }, makeContext(ws));
    expect(result.ok).toBe(true);
    expect(result.output).toMatch(/2 matches in \d+ files \(stopped at limit\)/);
    const lines = result.output.split('\n').filter((line) => /^\S+:\d+:/.test(line));
    expect(lines.length).toBe(2);
  });

  it('honors includeGlob filters', async () => {
    const ws = await makeWorkspace();
    await seedWorkspace(ws);
    const registry = searchTools();
    const result = await callTool(registry.get('search-text'), { query: 'needle', includeGlob: '*.md' }, makeContext(ws));
    expect(result.ok).toBe(true);
    expect(result.output).toContain('b.md:2:');
    expect(result.output).not.toContain('a.ts');
    expect(result.output).toMatch(/2 matches in 1 file/);
  });

  it('skips files larger than the byte limit', async () => {
    const ws = await makeWorkspace();
    await seedWorkspace(ws);
    await writeFile(path.join(ws, 'huge.log'), `needle\n${'y'.repeat(5_000)}\n`);
    const registry = searchTools(200, 1_000);
    const result = await callTool(registry.get('search-text'), { query: 'needle' }, makeContext(ws));
    expect(result.ok).toBe(true);
    expect(result.output).not.toContain('huge.log');
  });
});
