/**
 * Process-level CLI integration tests.
 *
 * Each case spawns the real executable (`src/cli.tsx` under tsx) exactly the
 * way a user or CI script would, in throwaway workspace directories with an
 * isolated HOME (so a developer's user-level config never leaks in) and
 * `RELAY_FAKE=1` so nothing ever touches the network. Workflow-dependent
 * cases are skipped until the runtime controller module exists.
 */

import { execFile } from 'node:child_process';
import { existsSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { DatabaseSync } from 'node:sqlite';
import { promisify } from 'node:util';
import { afterAll, describe, expect, it } from 'vitest';

const execFileAsync = promisify(execFile);
const REPO_ROOT = fileURLToPath(new URL('../..', import.meta.url));
const CLI_ENTRY = join(REPO_ROOT, 'src/cli.tsx');
const TSX_LOADER = createRequire(join(REPO_ROOT, 'package.json')).resolve('tsx');
const CONTROLLER_READY = existsSync(join(REPO_ROOT, 'src/runtime/controller.ts'));

const tempDirs: string[] = [];
afterAll(() => {
  for (const dir of tempDirs) rmSync(dir, { recursive: true, force: true });
});

type Workspace = { dir: string; home: string };

function makeWorkspace(prefix: string): Workspace {
  const dir = mkdtempSync(join(tmpdir(), `relay-cli-${prefix}-`));
  const home = mkdtempSync(join(tmpdir(), `relay-cli-${prefix}-home-`));
  tempDirs.push(dir, home);
  return { dir, home };
}

type RelayResult = { code: number; stdout: string; stderr: string };

async function relay(args: readonly string[], ws: Workspace): Promise<RelayResult> {
  try {
    const { stdout, stderr } = await execFileAsync(
      process.execPath,
      ['--import', TSX_LOADER, CLI_ENTRY, ...args],
      {
        cwd: ws.dir,
        env: { ...process.env, HOME: ws.home, RELAY_FAKE: '1' },
        timeout: 55_000,
      },
    );
    return { code: 0, stdout, stderr };
  } catch (error) {
    const failure = error as { code?: number; stdout?: string; stderr?: string };
    return { code: failure.code ?? 1, stdout: failure.stdout ?? '', stderr: failure.stderr ?? '' };
  }
}

function eventCount(ws: Workspace): number {
  const db = new DatabaseSync(join(ws.dir, '.relay', 'state.db'), { readOnly: true });
  try {
    const row = db.prepare('SELECT COUNT(*) AS count FROM events;').get() as { count: number | bigint };
    return Number(row.count);
  } finally {
    db.close();
  }
}

// -- argument surface ----------------------------------------------------------

describe('cli argument surface', () => {
  it(
    '--help prints usage mentioning commands and workflow modes',
    async () => {
      const ws = makeWorkspace('help');
      const result = await relay(['--help'], ws);
      expect(result.code).toBe(0);
      expect(result.stdout).toContain('relay init');
      expect(result.stdout).toContain('workflow');
    },
    60_000,
  );

  it(
    '--version prints a semver version',
    async () => {
      const ws = makeWorkspace('version');
      const result = await relay(['--version'], ws);
      expect(result.code).toBe(0);
      expect(result.stdout.trim()).toMatch(/^\d+\.\d+\.\d+/);
    },
    60_000,
  );

  it(
    'unknown command exits 2 and names the command',
    async () => {
      const ws = makeWorkspace('unknown');
      const result = await relay(['definitely-not-a-command'], ws);
      expect(result.code).toBe(2);
      expect(result.stderr).toContain('definitely-not-a-command');
      expect(result.stderr).toContain('relay --help');
    },
    60_000,
  );
});

// -- init ----------------------------------------------------------------------

describe('cli init', () => {
  it(
    'creates .relay state, leaves user files untouched, and is safe to re-run',
    async () => {
      const ws = makeWorkspace('init');
      writeFileSync(join(ws.dir, 'marker.txt'), 'do-not-touch\n');
      writeFileSync(join(ws.dir, 'README.md'), '# pre-existing readme\n');

      const first = await relay(['init', '--goal', 'Cli test goal'], ws);
      expect(first.code).toBe(0);
      expect(existsSync(join(ws.dir, '.relay', 'state.db'))).toBe(true);
      expect(readFileSync(join(ws.dir, 'marker.txt'), 'utf8')).toBe('do-not-touch\n');
      expect(readFileSync(join(ws.dir, 'README.md'), 'utf8')).toBe('# pre-existing readme\n');
      expect(first.stdout).toContain('Initialized Relay project');

      const eventsAfterInit = eventCount(ws);
      expect(eventsAfterInit).toBeGreaterThan(0);

      const again = await relay(['init', '--goal', 'Another goal'], ws);
      expect(again.code).toBe(0);
      expect(again.stdout).toContain('already exists');
      expect(eventCount(ws)).toBe(eventsAfterInit);
      expect(readFileSync(join(ws.dir, 'marker.txt'), 'utf8')).toBe('do-not-touch\n');

      if (CONTROLLER_READY) {
        const status = await relay(['status'], ws);
        expect(status.code).toBe(0);
        expect(eventCount(ws)).toBe(eventsAfterInit);
      }
    },
    90_000,
  );
});

// -- config (must never create .relay) ------------------------------------------

describe('cli config', () => {
  it(
    'show prints YAML with the models catalog and routing, path lists layers, get resolves dotted keys',
    async () => {
      const ws = makeWorkspace('config');
      const show = await relay(['config', 'show'], ws);
      expect(show.code).toBe(0);
      expect(show.stdout).toContain('routing:');
      expect(show.stdout).toContain('models:');
      expect(show.stdout).toContain('reasoning:');
      expect(show.stdout).toContain('fast:');
      expect(show.stdout).toContain('# effective config');
      expect(existsSync(join(ws.dir, '.relay'))).toBe(false);

      const path = await relay(['config', 'path'], ws);
      expect(path.code).toBe(0);
      expect(path.stdout).toContain('user:');
      expect(path.stdout).toContain('project:');
      expect(existsSync(join(ws.dir, '.relay'))).toBe(false);

      // Routing values are model-role names now; the concrete model lives in
      // the models catalog (routing.discovery -> reasoning -> claude-sonnet-5-5).
      const discovery = await relay(['config', 'get', 'routing.discovery'], ws);
      expect(discovery.code).toBe(0);
      expect(discovery.stdout.trim()).toBe('reasoning');
      expect(discovery.stdout).not.toContain('unknown key');

      const intent = await relay(['config', 'get', 'routing.intent'], ws);
      expect(intent.code).toBe(0);
      expect(intent.stdout.trim()).toBe('fast');

      const roleModel = await relay(['config', 'get', 'models.reasoning.model'], ws);
      expect(roleModel.code).toBe(0);
      expect(roleModel.stdout.trim()).toBe('claude-sonnet-5-5');

      const bogus = await relay(['config', 'get', 'bogus.key'], ws);
      expect(bogus.code).toBe(1);
      expect(bogus.stderr).toContain('unknown key');
      expect(existsSync(join(ws.dir, '.relay'))).toBe(false);
    },
    120_000,
  );
});

// -- status without a project ---------------------------------------------------

describe('cli status without project', () => {
  it(
    'fails with exit 1 and points at relay init',
    async () => {
      const ws = makeWorkspace('empty');
      const result = await relay(['status'], ws);
      expect(result.code).toBe(1);
      expect(result.stderr).toContain('relay init');
    },
    60_000,
  );
});

// -- workflow-dependent commands -------------------------------------------------

describe.skipIf(!CONTROLLER_READY)('cli workflow commands', () => {
  it(
    'status after init reports mode and autonomy',
    async () => {
      const ws = makeWorkspace('status');
      const init = await relay(['init', '--goal', 'Status goal'], ws);
      expect(init.code).toBe(0);

      const result = await relay(['status'], ws);
      expect(result.code).toBe(0);
      expect(result.stdout).toContain('discovery');
      expect(result.stdout).toMatch(/autonomy/i);
    },
    90_000,
  );

  it(
    'inspect of an unknown artifact fails with exit 1',
    async () => {
      const ws = makeWorkspace('inspect');
      await relay(['init', '--goal', 'Inspect goal'], ws);
      const result = await relay(['inspect', 'art_does-not-exist'], ws);
      expect(result.code).toBe(1);
      expect(result.stderr).toContain('artifact not found');
    },
    90_000,
  );

  it(
    'mode rejects invalid names with exit 2 and accepts discovery on a fresh project',
    async () => {
      const ws = makeWorkspace('mode');
      await relay(['init', '--goal', 'Mode goal'], ws);

      const invalid = await relay(['mode', 'not-a-mode'], ws);
      expect(invalid.code).toBe(2);
      expect(invalid.stderr).toContain('not-a-mode');
      expect(invalid.stderr).toContain('discovery');

      const valid = await relay(['mode', 'discovery'], ws);
      expect(valid.code).toBe(0);
    },
    90_000,
  );

  it(
    'runs a full fake-provider workflow non-interactively and pauses for approval',
    async () => {
      const ws = makeWorkspace('run');
      const run = await relay(
        ['run', 'demo goal', '--provider', 'fake', '--no-interactive', '--auto', 'workflow'],
        ws,
      );
      expect(run.code).toBe(0);

      const status = await relay(['status'], ws);
      expect(status.code).toBe(0);
      expect(status.stdout).toMatch(
        /specification|technical-design|planning|implementation|verification|delivery|pause|blocker|!/i,
      );

      const artifactsDir = join(ws.dir, '.relay', 'artifacts');
      const artifactFiles = existsSync(artifactsDir)
        ? readdirSync(artifactsDir, { recursive: true })
            .map((entry) => String(entry))
            .filter((entry) => entry.endsWith('.md'))
        : [];
      expect(artifactFiles.length).toBeGreaterThan(0);

      const relative = artifactFiles[0] ?? '';
      const inspect = await relay(['inspect', join('.relay', 'artifacts', relative)], ws);
      expect(inspect.code).toBe(0);
      expect(inspect.stdout).toContain('#');
    },
    180_000,
  );
});

// -- role override -----------------------------------------------------------------

describe('cli role override', () => {
  it(
    'unknown --role exits 2, names the role and the available roles, and creates no .relay state',
    async () => {
      const ws = makeWorkspace('role-bad');
      const result = await relay(
        ['run', 'demo goal', '--provider', 'fake', '--role', 'nonexistent', '--no-interactive'],
        ws,
      );
      expect(result.code).toBe(2);
      expect(result.stderr).toContain('unknown model role "nonexistent"');
      expect(result.stderr).toContain('reasoning, architect, coding, review, fast');
      expect(result.stderr).toContain('relay config show');
      // A usage failure must never leave project state behind.
      expect(existsSync(join(ws.dir, '.relay'))).toBe(false);
    },
    60_000,
  );

  it(
    'valid --role runs the fake flow and relay why shows the role in routing rows',
    async () => {
      const ws = makeWorkspace('role-fast');
      // Also the combined-override smoke: --provider narrows every route (the
      // role's included) to the fake provider while --role keeps the semantics.
      const run = await relay(
        ['run', 'demo goal', '--provider', 'fake', '--role', 'fast', '--no-interactive'],
        ws,
      );
      expect(run.code).toBe(0);
      expect(existsSync(join(ws.dir, '.relay', 'state.db'))).toBe(true);

      const why = await relay(['why'], ws);
      expect(why.code).toBe(0);
      expect(why.stdout).toContain('Routing:');
      expect(why.stdout).toContain('fast');
    },
    120_000,
  );

  it(
    '--role and --provider combine in either order: the provider filter narrows to fake',
    async () => {
      const ws = makeWorkspace('role-combined');
      const run = await relay(
        ['run', 'demo goal', '--role', 'fast', '--provider', 'fake', '--no-interactive'],
        ws,
      );
      expect(run.code).toBe(0);
    },
    90_000,
  );

  it(
    'RELAY_FAKE=1 without --role still runs the fake flow (existing behavior)',
    async () => {
      const ws = makeWorkspace('fake-only');
      const run = await relay(['run', 'demo goal', '--no-interactive'], ws);
      expect(run.code).toBe(0);
    },
    90_000,
  );
});
