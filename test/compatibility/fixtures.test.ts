/**
 * Compatibility fixture corpus tests (Rust rewrite, phase 0).
 *
 * Guards the versioned corpus under `tests/fixtures/`:
 *
 * - the exporter is deterministic: two fresh runs into temp directories are
 *   byte-identical and identical to the committed corpus;
 * - every case's `events.jsonl` is canonically serialized and correctly
 *   hash-chained per docs/event-format.md §2–§3;
 * - every valid case still replays in the TypeScript reference to exactly the
 *   committed `expected-state.json` (and status/why views), so the corpus
 *   cannot drift from the implementation it pins;
 * - every invalid case still fails for the recorded reason;
 * - the config corpus still normalizes identically and the CLI corpus matches
 *   the current help/version/exit-code behavior.
 */

import { execFileSync } from 'node:child_process';
import { existsSync, mkdtempSync, readFileSync, readdirSync, rmSync, statSync } from 'node:fs';
import { createRequire } from 'node:module';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parse as parseYaml } from 'yaml';
import { afterAll, describe, expect, it } from 'vitest';
import {
  canonicalize,
  verifyCanonicalLog,
} from '../../scripts/lib/canonical-json.mjs';
import { DEFAULT_CONFIG } from '../../src/config/defaults.js';
import { normalizeRouting } from '../../src/config/load.js';
import { parseRelayConfig } from '../../src/config/schema.js';
import {
  RELAY_EVENT_TYPES,
  validateStoredEvent,
  type AnyRelayEvent,
  type StoredEventEnvelope,
} from '../../src/core/events.js';
import { replayProject } from '../../src/core/reducer.js';
import { describeCliError } from '../../src/cli/errors.js';
import { USAGE } from '../../src/cli/parse-arguments.js';
import { buildStatusView, buildWhyView } from '../../src/runtime/controller-views.js';

const REPO_ROOT = fileURLToPath(new URL('../..', import.meta.url));
const FIXTURES_ROOT = join(REPO_ROOT, 'tests', 'fixtures');
const EXPORTER = join(REPO_ROOT, 'scripts', 'export-compatibility-fixtures.mjs');
const TSX_LOADER = createRequire(join(REPO_ROOT, 'package.json')).resolve('tsx');

const tempDirs: string[] = [];
afterAll(() => {
  for (const dir of tempDirs) rmSync(dir, { recursive: true, force: true });
});

/** Run the exporter into a fresh temp directory and return its path. */
function generateCorpus(label: string): string {
  const out = mkdtempSync(join(tmpdir(), `relay-fixtures-${label}-`));
  tempDirs.push(out);
  execFileSync(process.execPath, [EXPORTER, '--out', out], {
    cwd: REPO_ROOT,
    timeout: 240_000,
    encoding: 'utf8',
  });
  return out;
}

/** Recursively compare two directory trees byte-for-byte; returns mismatches. */
function diffTrees(a: string, b: string, prefix = ''): string[] {
  const mismatches: string[] = [];
  const entriesA = existsSync(a) ? readdirSync(a).sort() : [];
  const entriesB = existsSync(b) ? readdirSync(b).sort() : [];
  for (const entry of entriesA) {
    if (!entriesB.includes(entry)) mismatches.push(`${prefix}${entry}: only in first tree`);
  }
  for (const entry of entriesB) {
    if (!entriesA.includes(entry)) mismatches.push(`${prefix}${entry}: only in second tree`);
  }
  for (const entry of entriesA.filter((name) => entriesB.includes(name))) {
    const pathA = join(a, entry);
    const pathB = join(b, entry);
    const where = `${prefix}${entry}`;
    const dirA = statSync(pathA).isDirectory();
    const dirB = statSync(pathB).isDirectory();
    if (dirA && dirB) {
      mismatches.push(...diffTrees(pathA, pathB, `${where}/`));
    } else if (dirA !== dirB) {
      mismatches.push(`${where}: file/directory kind mismatch`);
    } else if (readFileSync(pathA, 'utf8') !== readFileSync(pathB, 'utf8')) {
      mismatches.push(`${where}: content differs`);
    }
  }
  return mismatches;
}

type CaseMeta = {
  readonly name: string;
  readonly valid: boolean;
  readonly envelopeVersion: number;
  readonly eventTypes: readonly string[];
  readonly sessionKey: string | null;
  readonly expectErrorName?: string | undefined;
  readonly expectError?: string | undefined;
  readonly specOnlyFailure?: boolean | undefined;
};

function caseNames(): string[] {
  return readdirSync(join(FIXTURES_ROOT, 'cases'), { withFileTypes: true })
    .filter((dirent) => dirent.isDirectory())
    .map((dirent) => dirent.name)
    .sort();
}

function readMeta(caseName: string): CaseMeta {
  return JSON.parse(readFileSync(join(FIXTURES_ROOT, 'cases', caseName, 'meta.json'), 'utf8')) as CaseMeta;
}

function readLines(caseName: string): string[] {
  const text = readFileSync(join(FIXTURES_ROOT, 'cases', caseName, 'events.jsonl'), 'utf8');
  return text === '' ? [] : text.split('\n').slice(0, -1);
}

/** The TypeScript validation + replay pipeline over a case's raw envelopes. */
function replayInTypeScript(caseName: string): { state?: ReturnType<typeof replayProject>; error?: Error } {
  const envelopes = readLines(caseName).map((line) => JSON.parse(line) as StoredEventEnvelope);
  try {
    const typed = envelopes.map((envelope) => validateStoredEvent(envelope));
    return { state: replayProject(typed) };
  } catch (error) {
    return { error: error instanceof Error ? error : new Error(String(error)) };
  }
}

describe('fixture exporter determinism', () => {
  it('regenerating the corpus is byte-identical and matches the committed corpus', () => {
    const first = generateCorpus('determinism-1');
    const second = generateCorpus('determinism-2');
    expect(diffTrees(first, second), 'two generated corpora must be byte-identical').toEqual([]);
    expect(
      diffTrees(FIXTURES_ROOT, second),
      'committed corpus is stale; run node scripts/export-compatibility-fixtures.mjs to refresh',
    ).toEqual([]);
  }, 600_000);
});

describe('fixture case corpus', () => {
  const names = caseNames();

  it('has a manifest listing every case directory', () => {
    const manifest = JSON.parse(readFileSync(join(FIXTURES_ROOT, 'manifest.json'), 'utf8')) as {
      readonly caseNames: readonly string[];
      readonly eventTypesCovered: readonly string[];
    };
    expect([...manifest.caseNames].sort()).toEqual(names);
    expect(manifest.eventTypesCovered).toEqual([...RELAY_EVENT_TYPES]);
    expect(names.length).toBeGreaterThanOrEqual(20);
  });

  it('covers all 24 event types across the valid hand-built cases', () => {
    const covered = new Set<string>();
    for (const name of names) {
      const meta = readMeta(name);
      if (!meta.valid || name === 'e2e-demo-run') continue;
      for (const type of meta.eventTypes) covered.add(type);
    }
    expect([...covered].sort()).toEqual([...RELAY_EVENT_TYPES].sort());
  });

  for (const name of names) {
    describe(`case ${name}`, () => {
      const meta = readMeta(name);

      it('events.jsonl is canonically serialized and correctly hash-chained', () => {
        const text = readFileSync(join(FIXTURES_ROOT, 'cases', name, 'events.jsonl'), 'utf8');
        const violations = verifyCanonicalLog(text, { expectedEnvelopeVersion: meta.envelopeVersion });
        expect(violations).toEqual([]);
      });

      if (meta.valid) {
        it('replays to the committed expected state and views', () => {
          const { state, error } = replayInTypeScript(name);
          expect(error).toBeUndefined();
          if (state === undefined) throw new Error(`case ${name} replayed without state or error`);
          const events = readLines(name).map((line) => JSON.parse(line) as StoredEventEnvelope);
          const typed = events.map((envelope) => validateStoredEvent(envelope)) as AnyRelayEvent[];
          const sessionId = meta.sessionKey;
          const expectedState = readFileSync(join(FIXTURES_ROOT, 'cases', name, 'expected-state.json'), 'utf8');
          const expectedStatus = readFileSync(join(FIXTURES_ROOT, 'cases', name, 'expected-status.json'), 'utf8');
          const expectedWhy = readFileSync(join(FIXTURES_ROOT, 'cases', name, 'expected-why.json'), 'utf8');
          expect(`${canonicalize(state)}\n`).toBe(expectedState);
          expect(`${canonicalize(buildStatusView(state, typed, sessionId))}\n`).toBe(expectedStatus);
          expect(`${canonicalize(buildWhyView(state, typed))}\n`).toBe(expectedWhy);
        });
      } else {
        it('fails for the recorded reason', () => {
          if (meta.specOnlyFailure === true) {
            // Spec-level failure (envelopeVersion): the TypeScript reference
            // accepts the log; only the Rust reader must reject it.
            const { error } = replayInTypeScript(name);
            expect(error).toBeUndefined();
            expect(meta.expectError).toBeDefined();
            return;
          }
          const { error } = replayInTypeScript(name);
          expect(error, 'invalid case must fail in TypeScript').toBeDefined();
          expect(error!.name).toBe(meta.expectErrorName);
          expect(error!.message).toContain(meta.expectError);
        });
      }
    });
  }
});

describe('config corpus', () => {
  const configRoot = join(FIXTURES_ROOT, 'config');
  const names = readdirSync(configRoot, { withFileTypes: true })
    .filter((dirent) => dirent.isDirectory())
    .map((dirent) => dirent.name)
    .sort();

  it('has the expected config cases', () => {
    expect(names).toEqual([
      'legacy-inline',
      'legacy-inline-limits',
      'mixed-roles-and-inline',
      'roles-default',
      'roles-fallback-chain-limits',
      'roles-full',
    ]);
  });

  for (const name of names) {
    it(`${name} normalizes to the committed output`, () => {
      const dir = join(configRoot, name);
      const inputPath = join(dir, 'input.yaml');
      const config = existsSync(inputPath)
        ? parseRelayConfig(`${name}.yaml`, parseYaml(readFileSync(inputPath, 'utf8')) as object)
        : DEFAULT_CONFIG;
      const normalized = normalizeRouting(config);
      const serialized = {
        roleChains: [...normalized.roleChains.entries()]
          .sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0))
          .map(([key, chain]) => [key, chain.map(serializeRoute)]),
        purposeChains: [...normalized.purposeChains.entries()]
          .sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0))
          .map(([key, chain]) => [key, chain.map(serializeRoute)]),
        legacyFallback:
          normalized.legacyFallback === null ? null : normalized.legacyFallback.map(serializeRoute),
      };
      expect(`${canonicalize(serialized)}\n`).toBe(readFileSync(join(dir, 'expected-normalized.json'), 'utf8'));
    });
  }

  function serializeRoute(route: { role: string | null; provider: string; model: string; limits?: unknown }) {
    return route.limits === undefined
      ? { role: route.role, provider: route.provider, model: route.model }
      : { role: route.role, provider: route.provider, model: route.model, limits: route.limits };
  }
});

describe('cli corpus', () => {
  it('help.txt matches the current USAGE text', () => {
    expect(readFileSync(join(FIXTURES_ROOT, 'cli', 'help.txt'), 'utf8')).toBe(`${USAGE.trimEnd()}\n`);
  });

  it('version.txt matches the package version', () => {
    const pkg = JSON.parse(readFileSync(join(REPO_ROOT, 'package.json'), 'utf8')) as { version: string };
    expect(readFileSync(join(FIXTURES_ROOT, 'cli', 'version.txt'), 'utf8')).toBe(`${pkg.version}\n`);
  });

  it('exit-codes.json matches describeCliError for every classified error', () => {
    const corpus = JSON.parse(readFileSync(join(FIXTURES_ROOT, 'cli', 'exit-codes.json'), 'utf8')) as {
      readonly errorClasses: Readonly<Record<string, number>>;
      readonly exitCodes: { readonly success: number; readonly runtimeFailure: number; readonly usageOrConfigError: number };
      readonly cases: readonly { readonly argv: readonly string[]; readonly exitCode: number }[];
    };
    expect(corpus.exitCodes).toEqual({ success: 0, runtimeFailure: 1, usageOrConfigError: 2 });
    for (const [name, expectedCode] of Object.entries(corpus.errorClasses)) {
      if (name === 'unclassifiedError') continue;
      const error = new Error(`${name} fixture message`);
      error.name = name;
      expect(describeCliError(error).code, name).toBe(expectedCode);
    }
    const unclassified = describeCliError(new Error('anything else'));
    expect(unclassified.code).toBe(corpus.errorClasses.unclassifiedError);
    for (const observed of corpus.cases) {
      expect(observed.exitCode, JSON.stringify(observed.argv)).toBeLessThanOrEqual(2);
    }
  });
});
