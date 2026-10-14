# Relay repository instructions

## Product direction

Relay is a local-first, model-agnostic CLI that carries product intent through discovery, specification, technical design, implementation, verification, and delivery. Models are replaceable workers; Relay owns workflow state, artifacts, permissions, and transition policy.

## Implementation status

Rust is the sole implementation. The authoritative architecture plan is `docs/plans/2026-10-13-intent-first-rust-work-graph.md`; `docs/plans/2026-10-13-rust-rewrite-architecture.md` and `docs/plans/2026-10-13-rust-rewrite-implementation.md` are historical references for the stopped compatibility port:

- The domain is a pure work-graph kernel (`crates/relay-domain`, `crates/relay-engine`, `crates/relay-workflows`) built on addressable goals, work items, dependencies, decisions, artifacts, evidence, attempts, grants, and receipts. There is no global seven-mode state machine, hidden `current_mode`, global pause authority, or monolithic controller. Familiar discovery–delivery phases are grouped views, not states.
- Models and executors propose; deterministic policy validates and applies. Executors never advance the graph directly and operate only through immutable attempt specs and capability grants.
- Authority order in the store: accepted domain facts in JSONL → deterministic replay → graph state; SQLite projections are disposable and rebuildable; Markdown artifacts are deterministic grouped views over structured content. Never let a model, provider, UI, database row, or Markdown file directly mutate workflow state.
- Read/model attempts may run concurrently; workspace-changing attempts are serialized and overlapping write grants are rejected.
- The original TypeScript/Node.js implementation (`src/**`, `test/**`, npm packaging and scripts) was removed from the repository on 2026-10-14 by the explicit retirement change `docs/plans/2026-10-14-remove-typescript-rust-only-codebase.md`; Git history is the archive. Do not restore it or reintroduce Node.js, npm, TypeScript, React, Ink, Vitest, or JavaScript tooling as Relay implementation, build, test, or fixture-generation surfaces.
- Legacy compatibility is maintained in Rust: `crates/relay-store` imports existing `.relay/state.db` workspaces through a read-only, staged, rollback-safe semantic migration, verified by Rust-native tests against the immutable historical fixtures under `tests/fixtures/**`. Those fixtures are archived evidence exported by the retired TypeScript implementation — historical inputs, not the acceptance oracle for new kernel behavior, and not regenerable output.
- `crates/relay-core` and `crates/relay-runtime` are the stopped Rust compatibility-port crates, kept as frozen reference material: sources of requirements, scenarios, hardened adapters, and infrastructure. Do not redesign, extend, or delete them before their own documented retirement gate.
- JavaScript/TypeScript files inside sample user workspaces under `tests/fixtures/**` are permitted test inputs (Relay is a language-agnostic coding tool); they are not implementation or build tooling. `crates/relay-tests/tests/repo_hygiene.rs` enforces the boundary.

## Engineering rules

- Use the Cargo workspace, the pinned toolchain (`rust-toolchain.toml`), safe Rust only, and the committed `Cargo.lock`.
- Keep the work-graph kernel (`crates/relay-domain`, `crates/relay-engine`, `crates/relay-workflows`) pure: no IO, clocks, randomness, HTTP, SQL, terminal, provider SDK, or filesystem dependencies. Time, IDs, provider output, and filesystem facts enter only through facts, commands, or attempt receipts.
- Keep all crates independent of provider SDK types. Translate provider responses into Relay-owned request and event types at adapter boundaries. Provider wire types stay private to their adapter crate.
- Model workflow changes as explicit, replayable facts. Persist durable project state locally and keep briefs, PRDs, TRDs, plans, decision logs, and reports as deterministic Markdown views over structured content.
- JSONL facts are the sole authoritative record for the work graph. Persist a durable fact before reporting its operation successful or updating derived projections. Keep fact and store formats explicitly versioned; unknown or newer authoritative records fail closed with upgrade guidance; additive fields must not invalidate legacy records.
- Hold a project writer lock only for short storage operations, never across network calls or long-running tools.
- Let language models propose intents and transitions; deterministic application code validates and applies them.
- Treat user files and dirty Git worktrees as owned by the user. Never overwrite, discard, commit, or push changes unless explicitly requested.
- Require user intervention for consequential product choices, changes to approved artifacts, destructive operations, deployment, publishing, and delivery. Continue routine reversible work automatically.
- Prefer small modules with single ownership. Coordinate before changing shared contracts. Shared-contract ownership:
  - work-graph domain types, facts, graph evaluation, scheduling policy, built-in workflow definitions, scenario/property tests — domain owner (`relay-domain`, `relay-engine`, `relay-workflows`);
  - fact envelope/persistence, log/lock/recovery primitives, SQLite projections, artifact publication utilities, semantic migration — storage/migration owner;
  - executor contracts, capability grants, attempts, reconciliation, provider/tool adaptation, cancellation, budgets — runtime/security owner (`relay-executor`, `relay-providers`, `relay-tools`, `relay-config`);
  - application service, CLI commands, JSON/human views, TUI, exit codes, legacy command mapping — interface owner (`relay-app`, `relay-ui`, `relay-cli`);
  - `crates/relay-core` and `crates/relay-runtime` belong to nobody: reference only, no mutations until an explicit retirement change;
  - `CLAUDE.md`, root `Cargo.toml`, CI, README, architecture/configuration/migration docs, `docs/event-format.md` — lead integrator.
- Validate all model-produced structured data and tool arguments at runtime with typed decoding and actionable errors.
- Use `spawn`-style process execution with argument arrays. Do not interpolate untrusted input into shell command strings.
- Keep secrets in environment variables or OS-backed credential storage. Never persist API keys in project state, JSONL, SQLite, artifacts, fixtures, or logs.
- Preserve workspace containment, symlink checks, no-overwrite writes, all-or-nothing patches, bounded redacted output, allowlisted child environments, read-only Git tools, and delivery holds.
- Add focused tests for state transitions, policy gates, provider normalization, persistence replay, and tool safety. Avoid tests that merely mirror implementation details. Tests are offline and deterministic by default; live provider tests are explicit opt-ins.
- Never delete legacy project state or migration backups automatically.

## Required checks

Run the Rust checks for any change under `crates/**`, `tests/**`, or the workspace manifests:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo build --workspace --all-targets --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps --exclude relay-core --exclude relay-runtime
```

The doc gate covers the live crates only; `relay-core` and `relay-runtime` are frozen reference material whose doc warnings cannot be fixed in place.

Run the focused intent-first suites while they exist:

```bash
cargo test -p relay-domain
cargo test -p relay-engine
cargo test -p relay-workflows
cargo test -p relay-executor
cargo test --test intent_first_scenarios
cargo test --test graph_properties
cargo test --test fact_replay
cargo test --test semantic_migration
cargo test --test crash_recovery
cargo test --test capability_security
cargo test --test scheduling_concurrency
cargo test --test cli_work_graph
cargo test --test tui_work_graph
```

Legacy-migration changes additionally run `cargo test -p relay-tests --test migration`, `--test semantic_migration`, `--test compatibility`, and `cargo test -p relay-config --test legacy_compat`.

Do not commit or push unless the user explicitly asks.
