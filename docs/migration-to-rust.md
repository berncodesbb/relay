# Migrating a legacy Relay project to the Rust work graph

Status: current — Rust is the sole implementation
Date: 2026-10-14
Owner: storage/migration owner (implementation), lead integrator (document)

This guide moves an existing Relay project created by the retired
TypeScript/Node.js implementation into the native Rust work-graph store.
The Rust binary performs the import itself — no Node.js, npm, or exporter
scripts are involved, and none exist in the repository anymore.

## What is migrated

A legacy workspace is one of:

- `.relay/state.db` — the TypeScript-era SQLite event store (the usual case), or
- a `.relay/` directory holding a canonical JSONL event store (the format
  specified in [event-format.md](event-format.md)),

in both cases with artifact Markdown under `.relay/artifacts/`.

The migration is **semantic**: it extracts durable product meaning — goal,
requirements, decisions, artifacts and their versions, evidence, approval
history — into a new `.relay-graph` fact store. It does not reproduce every
legacy internal event. Coarse legacy relationships stay coarse and are
surfaced in the report; legacy kinds with no durable meaning are reported
as unmapped. Nothing is fabricated.

## Guarantees

- The legacy source is opened **read-only** and never modified.
- The new store is built in a staging sibling
  (`.relay-graph-migration-staging`) and must fold cleanly through the
  domain graph and pass the fact doctor **before** anything is switched.
- The switch is two atomic renames; any previous `.relay-graph` store is
  retained as `.relay-graph-pre-migration-<stamp>/`.
- Any failure before the switch leaves the workspace exactly as it was.
- Nothing is ever deleted automatically — not the legacy `.relay/`, not the
  backups, not rolled-back stores (`.relay-graph-rolled-back-*/`).

## Running the migration

Always work on a **copy** of the legacy workspace (`--workspace <legacy-copy>`):

```bash
relay migrate --workspace /path/to/legacy-copy --dry-run
relay migrate --workspace /path/to/legacy-copy
```

`--dry-run` extracts and prints the full plan — legacy events read, facts
that would be emitted, inferences made, kinds left unmapped, items needing
review — without staging or switching anything.

The real run prints what it did: how many legacy events became how many
facts, where the new store lives, where the previous store (if any) was
backed up, and the mapped/inferred/unmapped report.

## Verifying the migration

```bash
relay doctor check --workspace /path/to/legacy-copy   # manifest, fact chain, artifacts, projection
relay project summary --workspace /path/to/legacy-copy
relay artifact inspect <id> --workspace /path/to/legacy-copy
```

`relay doctor rebuild-projection` deletes and rebuilds only the derived
SQLite projection from the JSONL facts — safe at any time, and a good
smoke test that the fact log alone reconstructs all state.

## Rolling back

Rolling back a completed migration is two renames (also expressible via
`relay_store::migrate::rollback` in library code):

```bash
mv .relay-graph .relay-graph-rolled-back-manual
mv .relay-graph-pre-migration-<stamp> .relay-graph
```

The legacy `.relay/` directory is untouched by the migration, so the
original evidence is always still there. Keep or delete the backup and
rolled-back directories at your discretion; Relay never removes them.

## Frequently asked questions

- **Are my artifacts changed?** No. Artifact content is carried over and
  published canonically; the report records what was mapped.
- **Is my event history preserved?** The durable meaning is. Legacy
  internal events without work-graph meaning (for example, per-step mode
  bookkeeping) are summarized in the report rather than copied.
- **What about credentials?** Nothing to migrate: Relay reads provider
  credentials from environment variables and never stores them in project
  state.
- **What if the source is a workspace I still work in?** Migrate a copy,
  verify it, then swap directories deliberately. The migration itself
  never writes inside the legacy `.relay/`.
- **Do old `.relay-graph` projects need this?** No. Existing Rust
  work-graph projects open unchanged; this guide is only for legacy
  `.relay/` workspaces.
