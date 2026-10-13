# Relay repository instructions

## Product direction

Relay is a local-first, model-agnostic CLI that carries product intent through discovery, specification, technical design, implementation, verification, and delivery. Models are replaceable workers; Relay owns workflow state, artifacts, permissions, and transition policy.

## Runtime migration status

Relay is mid-migration from a TypeScript/Node.js implementation to a Rust implementation (see `docs/plans/2026-10-13-rust-rewrite-architecture.md` and `docs/plans/2026-10-13-rust-rewrite-implementation.md`):

- The TypeScript application under `src/**` is the reference implementation and behavioral compatibility oracle. Keep it intact and working until the documented retirement phase. Do not redesign it.
- The Rust application under `crates/**` is the replacement. It must match TypeScript behavior as frozen in `tests/fixtures/**` (see `docs/event-format.md` for the normative storage contract).
- Authority order in the Rust store: canonical JSONL events → deterministic replay → current state; SQLite projections are disposable and rebuildable; Markdown artifacts are the human-readable record. Never let a model, provider, UI, database row, or Markdown file directly mutate workflow state.
- Change existing `src/**` and `test/**` files only for fixture export, legacy migration support, or completion of the already-approved model-role routing plan. Any other TypeScript change needs explicit coordination.

## Engineering rules

- For TypeScript work: use Node.js 22 or later, ESM, strict compiler settings, and npm. For Rust work: use the Cargo workspace, pinned toolchain (`rust-toolchain.toml`), safe Rust only, and committed `Cargo.lock`.
- Keep the workflow core (`crates/relay-core`) pure: no IO, clocks, randomness, HTTP, SQL, terminal, or provider SDK dependencies. Time, IDs, provider output, and filesystem facts enter only through commands or events.
- Keep the workflow core independent of provider SDK types. Translate provider responses into Relay-owned request and event types at adapter boundaries. Provider wire types stay private to their adapter crate.
- Model workflow changes as explicit, replayable events. Persist durable project state locally and keep PRDs, TRDs, plans, and reports as readable Markdown artifacts.
- JSONL is the sole authoritative workflow record. Persist a durable event before reporting its operation successful or updating derived projections. Keep event and store formats explicitly versioned; unknown or newer authoritative records fail closed with upgrade guidance; additive fields must not invalidate legacy records.
- Hold a project writer lock only for short storage operations, never across network calls or long-running tools.
- Let language models propose intents and transitions; deterministic application code validates and applies them.
- Treat user files and dirty Git worktrees as owned by the user. Never overwrite, discard, commit, or push changes unless explicitly requested.
- Require user intervention for consequential product choices, changes to approved artifacts, destructive operations, deployment, publishing, and delivery. Continue routine reversible work automatically.
- Prefer small modules with single ownership. Coordinate before changing shared contracts. Shared-contract ownership during the migration:
  - domain events, reducer, payload schemas, compatibility fixtures — core/compatibility owner;
  - JSONL format, hashes, manifest, locking, SQLite schema, legacy importer — storage/migration owner;
  - provider contract, tool contract, controller, permissions, cancellation, retries, budgets — runtime/security owner;
  - CLI arguments, commands, JSON/human views, TUI, exit codes — CLI/UI owner;
  - `CLAUDE.md`, root `Cargo.toml`, CI, README, architecture/configuration docs — lead integrator.
- Validate all model-produced structured data and tool arguments at runtime (Zod on the TypeScript side; typed decoding with actionable errors on the Rust side).
- Use `spawn`-style process execution with argument arrays. Do not interpolate untrusted input into shell command strings.
- Keep secrets in environment variables or OS-backed credential storage. Never persist API keys in project state, JSONL, SQLite, artifacts, fixtures, or logs.
- Preserve workspace containment, symlink checks, no-overwrite writes, all-or-nothing patches, bounded redacted output, allowlisted child environments, read-only Git tools, and delivery holds.
- Add focused tests for state transitions, policy gates, provider normalization, persistence replay, and tool safety. Avoid tests that merely mirror implementation details. Tests are offline and deterministic by default; live provider tests are explicit opt-ins.
- Never delete legacy project state or migration backups automatically.

## Required checks

Run the TypeScript checks while the reference implementation remains in the repository:

```bash
npm run typecheck
npm test
npm run build
```

Run the Rust checks for any change under `crates/**`, `tests/**`, or the workspace manifests:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo build --workspace --all-targets --all-features
```

Do not commit or push unless the user explicitly asks.
