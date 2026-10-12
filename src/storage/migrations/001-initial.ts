/**
 * Migration 001: append-only event log.
 *
 * Creates the `events` table holding every durable workflow change, the
 * indexes used for per-project replay, and triggers that enforce append-only
 * semantics at the database level: any UPDATE or DELETE on `events` aborts.
 */

import type { Migration } from '../migrations.js';

export const migration001Initial: Migration = {
  id: 1,
  name: 'initial-append-only-events',
  up: [
    `CREATE TABLE events (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL,
  sequence INTEGER NOT NULL,
  session_id TEXT,
  type TEXT NOT NULL,
  schema_version INTEGER NOT NULL,
  timestamp TEXT NOT NULL,
  correlation_id TEXT NOT NULL,
  causation_id TEXT,
  payload TEXT NOT NULL
);`,
    'CREATE UNIQUE INDEX events_project_sequence ON events (project_id, sequence);',
    'CREATE INDEX events_project_type ON events (project_id, type);',
    'CREATE INDEX events_project_timestamp ON events (project_id, timestamp);',
    `CREATE TRIGGER events_append_only_no_update
BEFORE UPDATE ON events
BEGIN
  SELECT RAISE(ABORT, 'events table is append-only');
END;`,
    `CREATE TRIGGER events_append_only_no_delete
BEFORE DELETE ON events
BEGIN
  SELECT RAISE(ABORT, 'events table is append-only');
END;`,
  ],
};
