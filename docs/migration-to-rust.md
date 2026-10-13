# Migrating a Relay project to the Rust implementation

> **Status note (2026-10-14):** this guide describes the migration path of
> the stopped compatibility port (legacy events → canonical event store
> per `docs/event-format.md`), which remains in the repository as frozen
> reference. The preview binary instead performs the semantic work-graph
> import — `relay migrate --workspace <legacy-copy>` (`--dry-run` to plan;
> read-only source, staged under `.relay-graph-migration-staging`,
> rollback-safe) — documented in README.md and
> `docs/plans/2026-10-13-intent-first-rust-work-graph.md`. The guide is
> rewritten at the Rust cutover, not before.

Status: preview documentation for the Rust rewrite migration
Date: 2026-10-13

This guide explains how an existing Relay project (created with the
TypeScript/Node.js implementation) moves to the native Rust implementation,
how to verify the migration, and how to roll back. The Rust implementation
stores the same workflow events in a canonical, hash-chained JSONL log; the
format is specified in `docs/event-format.md`.

During the preview period both implementations can read their own stores.
The TypeScript implementation continues to work unchanged; migration is
explicit and opt-in, never automatic.

## Before you migrate

- Update the TypeScript implementation and finish any in-flight run
  (`relay status` should show the workflow paused or idle, no run active).
- Back up your workspace or rely on the automatic timestamped backup the
  migration creates (see below). Both exist for safety; neither is ever
  deleted automatically.
- The Rust `relay` binary must be installed and on your `PATH` (during the
  preview it does not replace the npm-distributed `relay` command unless
  you choose to).

## What the migration does

The Rust `relay migrate` command (also available read-only as
`relay doctor export`) converts a legacy `.relay/state.db` project:

1. It verifies no other Relay process is writing the project.
2. It runs SQLite integrity checks and validates every stored event with
   the same schemas the TypeScript reducer uses.
3. It replays the legacy event log and confirms the state is consistent.
4. It writes a new store into a staging directory
   `.relay-rust-migration-<projectId>/`: a versioned manifest, segmented
   hash-chained JSONL event log, and the SQLite projection index.
5. It copies every artifact Markdown file byte-for-byte and records its
   SHA-256 content digest.
6. It replays the new JSONL log with the Rust reducer and compares the
   final state against the legacy replay; any difference aborts the
   migration.
7. On success it renames the old `.relay/` to `.relay-backup-<UTC>/` and
   moves the staged store into place as the new `.relay/`.

The source database is only ever opened read-only. A failed migration at
any step leaves the original `.relay/` untouched and in charge.

## Running the migration

```bash
relay migrate --workspace /path/to/project
```

The command prints exactly what it did: how many events were migrated,
where the backup was placed, and any recoverable findings (for example,
orphaned artifact files). Nothing is deleted.

## Verifying the migration

```bash
relay doctor check --workspace /path/to/project   # validates manifest, event chain, artifacts, index
relay status --workspace /path/to/project         # same goal, mode, artifacts, next action as before
relay why --workspace /path/to/project            # same last decision as before
relay inspect prd --workspace /path/to/project    # identical Markdown bytes
```

`relay doctor rebuild-index` deletes and rebuilds only the derived SQLite
index from the JSONL log — safe at any time, and a good smoke test that
the event log alone reconstructs all state.

## Rolling back

1. Move the new store aside and restore the backup:
   ```bash
   mv .relay .relay-rust-migrated-aside
   mv .relay-backup-<UTC> .relay
   ```
2. The TypeScript implementation now sees the original project exactly as
   it was (its database and artifacts were never modified).
3. Keep or delete the aside/backup directories at your discretion; Relay
   never removes them for you.

## Alternatives to direct migration

If your environment cannot run the Rust binary against a workspace, the
final TypeScript release ships an exporter:

```bash
node dist/cli.js export-legacy-project --workspace /path/to/project --out ./exported
```

It writes the same canonical JSONL (read-only against the source), which
the Rust implementation can then open directly.

## Frequently asked questions

- **Are my artifacts changed?** No. Artifact Markdown files are copied
  byte-for-byte; their digests are recorded so later checks can tell
  modified from pristine.
- **Is my event history preserved?** Yes — every event since
  `project.initialized` is carried over in order, re-hash-chained into the
  new format.
- **What about credentials?** Nothing to migrate: Relay reads provider
  credentials from environment variables and never stores them in project
  state.
- **Can both implementations run at the same time?** During the preview,
  yes, on separate workspaces. On a migrated workspace only the Rust
  implementation can open the store; point the TypeScript implementation
  at the backup if you need it.
