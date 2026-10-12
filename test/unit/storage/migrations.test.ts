import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { openDatabase, type RelayDatabase } from '../../../src/storage/database.js';
import { appliedMigrations, runMigrations } from '../../../src/storage/migrations.js';

let dir: string;
let open: RelayDatabase | undefined;

beforeEach(() => {
  dir = mkdtempSync(join(tmpdir(), 'relay-storage-migrations-'));
});

afterEach(() => {
  open?.close();
  open = undefined;
  rmSync(dir, { recursive: true, force: true });
});

function openMigrated(): RelayDatabase {
  const handle = openDatabase(join(dir, 'state.db'));
  runMigrations(handle);
  open = handle;
  return handle;
}

describe('runMigrations', () => {
  it('applies all known migrations and records them in order', () => {
    const db = openMigrated();
    const applied = appliedMigrations(db);
    expect(applied.map((migration) => [migration.id, migration.name])).toEqual([
      [1, 'initial-append-only-events'],
      [2, 'projects-listing-index'],
    ]);
    for (const migration of applied) {
      expect(migration.appliedAt).toMatch(/^\d{4}-\d{2}-\d{2}T/);
    }
  });

  it('is idempotent: running twice applies nothing new and does not throw', () => {
    const db = openMigrated();
    expect(() => runMigrations(db)).not.toThrow();
    expect(appliedMigrations(db)).toHaveLength(2);
  });

  it('reports no applied migrations on a fresh database', () => {
    const db = openDatabase(join(dir, 'fresh.db'));
    open = db;
    expect(appliedMigrations(db)).toEqual([]);
  });

  it('creates the events and projects tables plus append-only triggers', () => {
    const db = openMigrated();
    const tables = (
      db.raw.prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name;").all() as {
        name: string;
      }[]
    ).map((row) => row.name);
    expect(tables).toContain('events');
    expect(tables).toContain('projects');
    expect(tables).toContain('schema_migrations');

    const triggers = (
      db.raw.prepare("SELECT name FROM sqlite_master WHERE type = 'trigger' ORDER BY name;").all() as {
        name: string;
      }[]
    ).map((row) => row.name);
    expect(triggers).toEqual(['events_append_only_no_delete', 'events_append_only_no_update']);
  });

  it('refuses a database containing a migration from a newer Relay', () => {
    const db = openMigrated();
    db.raw
      .prepare('INSERT INTO schema_migrations (id, name, applied_at) VALUES (?, ?, ?);')
      .run(99, 'future-schema', '2026-10-12T00:00:00.000Z');
    expect(() => runMigrations(db)).toThrow(/newer version of Relay/);
  });
});
