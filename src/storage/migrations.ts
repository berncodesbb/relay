/**
 * Forward-only schema migrations for the Relay state database.
 *
 * Each migration runs exactly once and is recorded in `schema_migrations`
 * within the same transaction as its statements, so a partially applied
 * migration can never be recorded as applied. Re-running the runner is a
 * no-op. A database that already contains migrations unknown to this build
 * (written by a newer Relay) is refused with an actionable error rather than
 * being silently modified.
 */

import { nowIso } from '../core/ids.js';
import type { RelayDatabase } from './database.js';
import { migration001Initial } from './migrations/001-initial.js';
import { migration002Projects } from './migrations/002-projects.js';

/** A single named, forward-only migration. Statements run in order inside one transaction. */
export type Migration = {
  readonly id: number;
  readonly name: string;
  readonly up: readonly string[];
};

/** A migration recorded as applied in `schema_migrations`. */
export type AppliedMigration = {
  readonly id: number;
  readonly name: string;
  readonly appliedAt: string;
};

/** All migrations known to this Relay build, in application order. */
export const MIGRATIONS: readonly Migration[] = [migration001Initial, migration002Projects];

const ENSURE_SCHEMA_MIGRATIONS_SQL = `CREATE TABLE IF NOT EXISTS schema_migrations (
  id INTEGER PRIMARY KEY,
  name TEXT NOT NULL,
  applied_at TEXT NOT NULL
);`;

/**
 * Apply all pending migrations. Safe to run on every startup; applying nothing
 * when the database is current.
 */
export function runMigrations(db: RelayDatabase): void {
  const raw = db.raw;
  raw.exec(ENSURE_SCHEMA_MIGRATIONS_SQL);

  const applied = appliedMigrations(db);
  const appliedIds = new Set(applied.map((migration) => migration.id));

  let maxKnown = 0;
  for (const migration of MIGRATIONS) {
    maxKnown = Math.max(maxKnown, migration.id);
  }

  let newestUnknown: AppliedMigration | undefined;
  for (const migration of applied) {
    if (migration.id > maxKnown && (newestUnknown === undefined || migration.id > newestUnknown.id)) {
      newestUnknown = migration;
    }
  }
  if (newestUnknown !== undefined) {
    throw new Error(
      `This Relay project database contains migration ${newestUnknown.id} ("${newestUnknown.name}"), ` +
        `but this Relay build only knows migrations up to ${maxKnown}. The project was written by a newer ` +
        'version of Relay. Upgrade Relay (or open this project with the newer install) before using this database again.',
    );
  }

  for (const migration of MIGRATIONS) {
    if (appliedIds.has(migration.id)) continue;
    raw.exec('BEGIN IMMEDIATE;');
    try {
      for (const statement of migration.up) {
        raw.exec(statement);
      }
      raw
        .prepare('INSERT INTO schema_migrations (id, name, applied_at) VALUES (?, ?, ?);')
        .run(migration.id, migration.name, nowIso());
      raw.exec('COMMIT;');
    } catch (error) {
      try {
        raw.exec('ROLLBACK;');
      } catch {
        // The original failure may have already closed the transaction.
      }
      throw new Error(
        `Relay migration ${migration.id} ("${migration.name}") failed: ${errorMessage(error)}`,
        { cause: error },
      );
    }
  }
}

/** Migrations already recorded in this database, in id order. */
export function appliedMigrations(db: RelayDatabase): readonly AppliedMigration[] {
  const raw = db.raw;
  const table = raw
    .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'schema_migrations';")
    .get();
  if (table === undefined) return [];
  const rows = raw
    .prepare('SELECT id, name, applied_at AS appliedAt FROM schema_migrations ORDER BY id ASC;')
    .all();
  return rows.map((row) => {
    const typed = row as { id: number; name: string; appliedAt: string };
    return { id: Number(typed.id), name: typed.name, appliedAt: typed.appliedAt };
  });
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
