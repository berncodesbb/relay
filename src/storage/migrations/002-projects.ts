/**
 * Migration 002: project listing index.
 *
 * `projects` is a derived index maintained by the project repository so
 * project listing does not scan the event log. The event log remains the
 * source of truth. No triggers: the repository owns consistency of these rows
 * and writes them in the same transaction as the corresponding events.
 */

import type { Migration } from '../migrations.js';

export const migration002Projects: Migration = {
  id: 2,
  name: 'projects-listing-index',
  up: [
    `CREATE TABLE projects (
  project_id TEXT PRIMARY KEY,
  created_at TEXT NOT NULL,
  goal TEXT NOT NULL
);`,
  ],
};
