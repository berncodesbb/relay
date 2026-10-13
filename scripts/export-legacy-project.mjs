#!/usr/bin/env node
/**
 * Legacy project exporter (Rust rewrite, phase 0 / migration support).
 *
 * Reads an existing TypeScript-era `.relay/state.db` (SQLite, node:sqlite)
 * strictly read-only and writes each project's event log as canonical,
 * hash-chained JSONL per `docs/event-format.md` §10:
 *
 * - rows are validated with the same domain schemas replay uses (identical
 *   error classes and messages, so an invalid legacy row aborts the export),
 * - sequences must be contiguous from 1 and the legacy replay must succeed,
 * - hashes are computed fresh (legacy rows carry none) over the canonical
 *   envelope serialization with envelopeVersion 1,
 * - the source database is never opened for writing.
 *
 * Output layout: `<out>/<projectId>.jsonl` per project plus `<out>/export.json`
 * describing what was exported (project ids, event counts, final sequence and
 * hash). Used later by Rust migration tests.
 *
 * Usage: node scripts/export-legacy-project.mjs --db <path> --out <dir>
 */

import { spawnSync } from 'node:child_process';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

// tsx bootstrap (same pattern as export-compatibility-fixtures.mjs): the
// TypeScript sources are imported dynamically below so plain `node` works.
if (process.env.RELAY_FIXTURES_CHILD !== '1') {
  const require = createRequire(import.meta.url);
  let tsx;
  try {
    tsx = require.resolve('tsx');
  } catch {
    console.error('relay export-legacy: the tsx dev dependency is required (run npm install).');
    process.exit(1);
  }
  const result = spawnSync(
    process.execPath,
    ['--import', tsx, fileURLToPath(import.meta.url), ...process.argv.slice(2)],
    { stdio: 'inherit', env: { ...process.env, RELAY_FIXTURES_CHILD: '1' } },
  );
  process.exit(result.status ?? 1);
}

import { mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { DatabaseSync } from 'node:sqlite';
import { canonicalize, toCanonicalLine, verifyCanonicalLog, withHashChain } from './lib/canonical-json.mjs';

// TypeScript-source imports (validation + replay oracle). These MUST stay
// dynamic: static imports are hoisted above the tsx bootstrap above.
const { validateStoredEvent } = await import('../src/core/events.js');
const { replayProject } = await import('../src/core/reducer.js');

const EVENT_COLUMNS =
  'id, project_id, sequence, session_id, type, schema_version, timestamp, correlation_id, causation_id, payload';

function argValue(name, fallback) {
  const index = process.argv.indexOf(name);
  if (index !== -1 && index + 1 < process.argv.length) return process.argv[index + 1];
  return fallback;
}

function fail(message) {
  console.error(`relay export-legacy: ${message}`);
  process.exit(1);
}

function main() {
  const dbPath = argValue('--db', join(process.cwd(), '.relay', 'state.db'));
  const outDir = argValue('--out', join(process.cwd(), 'relay-export'));

  let db;
  try {
    db = new DatabaseSync(dbPath, { readOnly: true });
  } catch (error) {
    fail(`cannot open legacy database at ${dbPath}: ${error instanceof Error ? error.message : String(error)}`);
  }

  let rows;
  try {
    rows = db.prepare(`SELECT ${EVENT_COLUMNS} FROM events ORDER BY project_id ASC, sequence ASC;`).all();
  } catch (error) {
    fail(`cannot read events table: ${error instanceof Error ? error.message : String(error)}`);
  } finally {
    db.close();
  }

  // Group by project, validating each row exactly the way a replaying reader
  // would; the first invalid row aborts the whole export.
  const byProject = new Map();
  for (const row of rows) {
    const typedRow = row;
    let payload;
    try {
      payload = JSON.parse(typedRow.payload);
    } catch (error) {
      fail(
        `event ${String(typedRow.id)} (sequence ${String(typedRow.sequence)}): payload is not valid JSON ` +
          `(${error instanceof Error ? error.message : String(error)})`,
      );
    }
    let typed;
    try {
      typed = validateStoredEvent({
        id: typedRow.id,
        projectId: typedRow.project_id,
        sequence: Number(typedRow.sequence),
        sessionId: typedRow.session_id,
        type: typedRow.type,
        schemaVersion: Number(typedRow.schema_version),
        timestamp: typedRow.timestamp,
        correlationId: typedRow.correlation_id,
        causationId: typedRow.causation_id,
        payload,
      });
    } catch (error) {
      fail(
        `event ${String(typedRow.id)} (sequence ${String(typedRow.sequence)}) failed validation: ` +
          `${error instanceof Error ? error.message : String(error)}`,
      );
    }
    const list = byProject.get(typed.projectId) ?? [];
    list.push(typed);
    byProject.set(typed.projectId, list);
  }

  rmSync(outDir, { recursive: true, force: true });
  mkdirSync(outDir, { recursive: true });

  const exported = [];
  for (const [projectId, events] of byProject) {
    let state;
    try {
      state = replayProject(events);
    } catch (error) {
      fail(
        `project ${projectId} does not replay cleanly ` +
          `(${error instanceof Error ? error.message : String(error)}); the legacy log is corrupt`,
      );
    }
    const envelopes = withHashChain(
      events.map((event) => ({
        id: event.id,
        projectId: event.projectId,
        sequence: event.sequence,
        sessionId: event.sessionId,
        type: event.type,
        schemaVersion: event.schemaVersion,
        timestamp: event.timestamp,
        correlationId: event.correlationId,
        causationId: event.causationId,
        payload: event.payload,
      })),
      1,
    );
    const text = `${envelopes.map((envelope) => toCanonicalLine(envelope)).join('\n')}\n`;
    const violations = verifyCanonicalLog(text);
    if (violations.length > 0) {
      fail(`project ${projectId} produced a non-canonical log: ${violations.join('; ')}`);
    }
    const file = `${projectId}.jsonl`;
    writeFileSync(join(outDir, file), text);
    const last = envelopes[envelopes.length - 1];
    exported.push({
      projectId,
      file,
      events: envelopes.length,
      lastSequence: last.sequence,
      lastHash: last.hash,
      finalMode: state.currentMode,
    });
  }

  writeFileSync(
    join(outDir, 'export.json'),
    `${canonicalize({
      formatVersion: 1,
      source: 'legacy .relay/state.db',
      generatedBy: 'scripts/export-legacy-project.mjs',
      spec: 'docs/event-format.md §10',
      projects: exported,
    })}\n`,
  );
  console.log(
    `relay export-legacy: exported ${exported.length} project(s), ` +
      `${exported.reduce((total, entry) => total + entry.events, 0)} event(s) to ${outDir}`,
  );
}

main();
