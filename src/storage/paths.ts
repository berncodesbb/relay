/**
 * Well-known locations of Relay state inside a workspace.
 *
 * All durable Relay state for a workspace lives under `<root>/.relay`: the
 * SQLite event log and derived tables in `state.db`, and generated artifacts
 * under `artifacts/`. This module only computes paths; callers own creation.
 */

import { join } from 'node:path';

/** Directory (relative to the workspace root) that holds all Relay state. */
export const RELAY_DIR = '.relay';

/** Absolute paths of Relay state for a given workspace root. */
export function relayPaths(workspaceRoot: string): {
  readonly root: string;
  readonly database: string;
  readonly artifactsDir: string;
} {
  const root = join(workspaceRoot, RELAY_DIR);
  return {
    root,
    database: join(root, 'state.db'),
    artifactsDir: join(root, 'artifacts'),
  };
}
