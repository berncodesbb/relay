import { afterEach, describe, expect, it } from 'vitest';
import { spawnSync } from 'node:child_process';
import { mkdir, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { ToolRegistry } from '../../../src/tools/registry.js';
import { registerGitTools } from '../../../src/tools/git.js';
import { callTool, cleanupWorkspaces, makeContext, makeWorkspace } from './test-kit.js';

function gitTools() {
  const registry = new ToolRegistry();
  registerGitTools(registry);
  return registry;
}

function runGit(cwd: string, args: string[]): void {
  const result = spawnSync('git', args, { cwd, encoding: 'utf8' });
  if (result.status !== 0) {
    throw new Error(`git ${args.join(' ')} failed (${result.status}): ${result.stderr}`);
  }
}

async function makeRepo(): Promise<string> {
  const ws = await makeWorkspace();
  runGit(ws, ['init', '-b', 'main']);
  runGit(ws, ['config', 'user.email', 'relay-tests@example.com']);
  runGit(ws, ['config', 'user.name', 'Relay Test']);
  await writeFile(path.join(ws, 'a.txt'), 'one\n');
  await writeFile(path.join(ws, 'b.txt'), 'beta\n');
  runGit(ws, ['add', '.']);
  runGit(ws, ['commit', '-m', 'first commit']);
  return ws;
}

afterEach(async () => {
  await cleanupWorkspaces();
});

describe('git tools', () => {
  it('git-status reports branch and changed entries', async () => {
    const ws = await makeRepo();
    await writeFile(path.join(ws, 'a.txt'), 'one-changed\n');
    await writeFile(path.join(ws, 'untracked.txt'), 'new\n');
    const result = await callTool(gitTools().get('git-status'), {}, makeContext(ws));
    expect(result.ok).toBe(true);
    expect(result.output).toContain('## main');
    expect(result.output).toContain(' M a.txt');
    expect(result.output).toContain('?? untracked.txt');
    expect(result.output).toMatch(/\d+ changed entr/);
  });

  it('git-diff shows working tree changes and staged changes separately', async () => {
    const ws = await makeRepo();
    await writeFile(path.join(ws, 'a.txt'), 'one-changed\n');
    const unstaged = await callTool(gitTools().get('git-diff'), {}, makeContext(ws));
    expect(unstaged.ok).toBe(true);
    expect(unstaged.output).toContain('-one');
    expect(unstaged.output).toContain('+one-changed');
    const cleanStaged = await callTool(gitTools().get('git-diff'), { staged: true }, makeContext(ws));
    expect(cleanStaged.ok).toBe(true);
    expect(cleanStaged.output).toContain('no changes');
    runGit(ws, ['add', 'a.txt']);
    const staged = await callTool(gitTools().get('git-diff'), { staged: true }, makeContext(ws));
    expect(staged.ok).toBe(true);
    expect(staged.output).toContain('+one-changed');
  });

  it('git-diff can be limited to one path', async () => {
    const ws = await makeRepo();
    await writeFile(path.join(ws, 'a.txt'), 'one-changed\n');
    await writeFile(path.join(ws, 'b.txt'), 'beta-changed\n');
    const result = await callTool(gitTools().get('git-diff'), { path: 'b.txt' }, makeContext(ws));
    expect(result.ok).toBe(true);
    expect(result.output).toContain('+beta-changed');
    expect(result.output).not.toContain('+one-changed');
  });

  it('git-log lists commits', async () => {
    const ws = await makeRepo();
    const result = await callTool(gitTools().get('git-log'), { limit: 5 }, makeContext(ws));
    expect(result.ok).toBe(true);
    expect(result.output).toContain('first commit');
    expect(result.output).toMatch(/\(\d+ commits?\)/);
  });

  it('reports a friendly error outside a git repository', async () => {
    const ws = await makeWorkspace();
    const result = await callTool(gitTools().get('git-status'), {}, makeContext(ws));
    expect(result.ok).toBe(false);
    expect(result.output).toMatch(/not a git repository/);
    expect(result.output).not.toMatch(/fatal:/);
  });

  it('repo-instructions concatenates instruction files in priority order', async () => {
    const ws = await makeWorkspace();
    await writeFile(path.join(ws, 'CLAUDE.md'), '# Repo rules\nbe careful');
    await writeFile(path.join(ws, 'README.md'), '# Readme\noverview');
    const result = await callTool(gitTools().get('repo-instructions'), {}, makeContext(ws));
    expect(result.ok).toBe(true);
    expect(result.output).toContain('# Repo rules');
    expect(result.output).toContain('# Readme');
    expect(result.output.indexOf('# Repo rules')).toBeLessThan(result.output.indexOf('# Readme'));
  });

  it('repo-instructions caps each file and reports when none exist', async () => {
    const ws = await makeWorkspace();
    const empty = await callTool(gitTools().get('repo-instructions'), {}, makeContext(ws));
    expect(empty.ok).toBe(true);
    expect(empty.output).toMatch(/no repository instruction files found/);

    await mkdir(path.join(ws, 'sub'));
    await writeFile(path.join(ws, 'CLAUDE.md'), 'y'.repeat(25_000));
    const capped = await callTool(gitTools().get('repo-instructions'), {}, makeContext(ws));
    expect(capped.ok).toBe(true);
    expect(capped.output).toContain('first 20000 of 25000 chars');
    expect(capped.output.length).toBeLessThanOrEqual(60_000);
  });

  it('registers no mutating git operations', async () => {
    const registry = gitTools();
    const mutating = ['git-add', 'git-commit', 'git-push', 'git-checkout', 'git-reset', 'git-stash', 'git-clean'];
    for (const name of mutating) {
      expect(registry.has(name)).toBe(false);
    }
  });
});
