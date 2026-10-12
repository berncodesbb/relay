/**
 * SQLite database bootstrap for Relay project state.
 *
 * Relay keeps its append-only event log and derived tables in a single SQLite
 * database inside the workspace's `.relay/` directory, using Node's built-in
 * `node:sqlite` module (no native npm dependency). Opening applies the
 * durability pragmas Relay relies on: WAL journaling so readers never block
 * the writer, enforced foreign keys, and a bounded busy wait for lock
 * contention with concurrent processes.
 */

import { mkdirSync } from 'node:fs';
import { createRequire } from 'node:module';
import { dirname } from 'node:path';
import type { DatabaseSync } from 'node:sqlite';

/** Handle to an open Relay database. */
export type RelayDatabase = {
  /** Raw driver handle; used by migrations, the event store, and repositories. */
  readonly raw: DatabaseSync;
  /** Close the database connection. */
  close(): void;
};

const nodeRequire = createRequire(import.meta.url);

function loadDatabaseSyncConstructor(): typeof DatabaseSync {
  let mod: { DatabaseSync?: unknown };
  try {
    mod = nodeRequire('node:sqlite') as { DatabaseSync?: unknown };
  } catch (cause) {
    throw new Error(
      'Relay could not load the built-in "node:sqlite" module. Relay requires Node.js 22.5.0 or later ' +
        `(Node.js 24+ recommended); the current runtime is ${process.version}. Upgrade Node.js and retry.`,
      { cause },
    );
  }
  if (typeof mod.DatabaseSync !== 'function') {
    throw new Error(
      'The installed Node.js version does not expose "node:sqlite".DatabaseSync. Relay requires Node.js 22.5.0 ' +
        `or later; the current runtime is ${process.version}.`,
    );
  }
  return mod.DatabaseSync as typeof DatabaseSync;
}

/**
 * Open (creating if necessary) the Relay database at `dbPath`, creating parent
 * directories and applying Relay's connection pragmas.
 */
export function openDatabase(dbPath: string): RelayDatabase {
  mkdirSync(dirname(dbPath), { recursive: true });
  const DatabaseSyncConstructor = loadDatabaseSyncConstructor();
  const raw = new DatabaseSyncConstructor(dbPath);
  raw.exec('PRAGMA journal_mode = WAL;');
  raw.exec('PRAGMA foreign_keys = ON;');
  raw.exec('PRAGMA busy_timeout = 5000;');
  raw.exec('PRAGMA synchronous = NORMAL;');
  return {
    raw,
    close(): void {
      raw.close();
    },
  };
}
