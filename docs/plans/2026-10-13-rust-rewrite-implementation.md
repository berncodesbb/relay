# Relay Rust rewrite implementation plan

> **Historical (2026-10-14):** this implementation plan was stopped and
> superseded by the intent-first work-graph plan
> (`2026-10-13-intent-first-rust-work-graph.md`). Its npm/TypeScript checks
> no longer apply — the TypeScript application was removed from the
> repository. Retained as design history; not current instructions.

Status: implementation-ready  
Date: 2026-10-13  
Architecture reference: `docs/plans/2026-10-13-rust-rewrite-architecture.md`

## Objective

Replace Relay's TypeScript/Node.js application with a behaviorally compatible Rust application distributed as a single native executable. Adopt a Codex-style storage model in which append-only JSONL is authoritative, SQLite is a rebuildable projection and query layer, and immutable Markdown remains the human-readable artifact record.

The implementation must preserve Relay's existing seven-mode workflow, deterministic transitions, approval and intervention gates, model-role routing, provider neutrality, tool restrictions, artifact semantics, CLI behavior, recovery properties, and local-first posture. Existing TypeScript behavior is the compatibility oracle until the Rust cutover is accepted.

## Repository instructions and current-state constraints

Read and follow `CLAUDE.md` before implementation. Its safety, ownership, provider-boundary, event-sourcing, secrets, dirty-worktree, testing, and no-commit/no-push rules continue to apply throughout the rewrite.

The current instruction to use TypeScript, Node.js 22+, Zod, and npm describes the existing implementation and conflicts with the approved Rust direction. Resolve this deliberately:

1. Keep the TypeScript application intact as the reference implementation during compatibility work.
2. When the Cargo workspace is introduced, update `CLAUDE.md` to describe the dual-runtime migration and Rust rules; do not silently ignore the current instruction.
3. At final cutover, update `CLAUDE.md` again so Rust is the primary implementation and TypeScript is retained only for the documented compatibility window.
4. Continue to run the existing required npm checks while TypeScript remains in the repository. Add Rust checks; do not replace the npm checks until the TypeScript retirement phase.

The working tree is already dirty with model-role routing work across configuration, core events, runtime, CLI, documentation, and tests. Treat every pre-existing modification and untracked file as user-owned. Do not reset, overwrite, reformat, or fold unrelated changes into rewrite work. Begin by capturing `git status --short` and `git diff`; coordinate ownership before touching any overlapping file.

In particular, preserve and finish the work described by `docs/plans/2026-10-13-model-role-routing.md` before freezing compatibility fixtures. Do not use an earlier routing behavior as the Rust baseline.

## Scope

### In scope

- A Cargo workspace containing a pure domain core, canonical storage, artifacts, configuration, providers, tools, runtime, UI, and executable crates.
- A versioned, segmented, hash-chained JSONL event format as the source of truth.
- A disposable SQLite database containing indexed events and materialized projections with a sequence/hash checkpoint.
- Deterministic replay and automatic projection catch-up or rebuild.
- Immutable Markdown artifacts with content digests and orphan detection.
- Read-only migration from the existing `.relay/state.db` format, with rollback-safe directory switching and retained backups.
- OpenAI, Anthropic, and deterministic fake provider adapters behind Relay-owned contracts.
- Existing model purpose-to-role-to-provider routing, fallback, retry, usage, and diagnostics semantics.
- Workspace-bounded filesystem, patch, process, Git-read, search, redaction, and permission tooling.
- Existing CLI commands, flags, slash commands, JSON views, exit behavior, and an interactive Ratatui terminal interface.
- Cross-language compatibility fixtures, offline tests, crash recovery tests, and macOS/Linux/Windows CI.
- Native release packaging and an opt-in preview period before cutover.

### Out of scope

- New workflow modes or artifact kinds.
- Hosted state, accounts, synchronization, collaboration, or server databases.
- Web or desktop graphical interfaces.
- Autonomous multi-agent implementation inside Relay.
- Vector databases or semantic repository indexes.
- Automatic Git mutations, pull requests, deployment, publishing, or delivery.
- Provider-driven transitions or permission decisions.
- Opportunistic redesign of CLI output, configuration, prompts, or policy behavior during the port.
- Automatic deletion of legacy data or migration backups.

## Target architecture

Use the decisions and invariants in `docs/plans/2026-10-13-rust-rewrite-architecture.md` as normative. The authority order is:

```text
canonical JSONL events -> deterministic replay -> current project state
                       -> SQLite projections -> efficient queries
                       -> Markdown artifacts -> human review
```

If JSONL and SQLite disagree, JSONL wins. SQLite must be safely deletable and rebuildable. A model, provider, UI, database row, or Markdown file must never directly mutate workflow state; accepted commands produce validated events, and events produce state.

## Proposed Rust workspace and affected files

Paths marked "new" are expected additions. Existing paths must be changed only by their assigned owner and only when the relevant phase begins.

### Repository and instructions

- `CLAUDE.md` — describe dual-runtime migration, Rust engineering rules, shared-contract ownership, and required Rust checks; later describe final cutover.
- `Cargo.toml` — new workspace manifest and shared dependency versions.
- `Cargo.lock` — new, committed lockfile for the application workspace.
- `rust-toolchain.toml` — new pinned stable toolchain and components.
- `deny.toml` — new dependency advisory/license policy if `cargo-deny` is selected.
- `.gitignore` — add only Rust build products and migration scratch paths.
- `.github/workflows/ci.yml` — add Rust format, lint, test, build, compatibility, and platform jobs while retaining Node jobs during migration.
- `README.md` — preview installation, migration, recovery, and final native usage; update only after behavior exists.
- `docs/architecture.md` — update from the TypeScript architecture to the accepted Rust architecture at cutover, preserving history through links to the plans.
- `docs/configuration.md` — preserve configuration behavior while updating runtime-specific examples at cutover.
- `docs/migration-to-rust.md` — new user-facing migration and rollback guide.
- `docs/event-format.md` — new normative JSONL envelope, canonicalization, hashing, segmentation, durability, and compatibility specification.

### Rust crates

- `crates/relay-core/**` — new pure domain crate: IDs, modes, intents, events, commands, outcomes, artifacts, state, reducer, transitions, and policies.
- `crates/relay-store/**` — new canonical JSONL log, manifest, writer lock, tail recovery, SQLite projections, checkpoints, index rebuild, integrity checks, and legacy import.
- `crates/relay-artifacts/**` — new schemas, deterministic renderers, immutable publication, digests, and orphan inspection.
- `crates/relay-config/**` — new layered YAML parsing, validation, normalization, model roles, routing, and diagnostics.
- `crates/relay-providers/**` — new provider-neutral contracts and private OpenAI, Anthropic, and fake implementations.
- `crates/relay-tools/**` — new registry, schemas, path guard, filesystem, search, patch, bounded process, Git reads, and redaction.
- `crates/relay-runtime/**` — new controller, agent loop, prompts, context building, routing, retry, budgets, cancellation, and view services.
- `crates/relay-ui/**` — new stable view models, human/JSON renderers, and Ratatui/Crossterm interface.
- `crates/relay-cli/**` — new binary, Clap parsing, command dispatch, signal handling, lifecycle, and top-level error/exit mapping.

### Compatibility and tests

- `scripts/export-compatibility-fixtures.mjs` — new TypeScript fixture exporter using existing public/application boundaries where possible.
- `scripts/export-legacy-project.mjs` — new TypeScript JSONL exporter for the final compatibility release, or an equivalent existing-CLI subcommand if cleaner.
- `test/compatibility/**` — new TypeScript-side fixture generation and assertions.
- `tests/fixtures/**` — new sanitized, versioned, cross-language fixtures and expected normalized outputs.
- `tests/compatibility/**` — new Rust replay, rendering, migration, and CLI compatibility tests.
- `tests/fault-injection/**` — new storage and artifact crash-point tests.
- `tests/security/**` — new cross-platform path, symlink, process, patch, redaction, and permission regressions.
- Existing `src/**` and `test/**` — remain the reference implementation; change only for fixture export, migration support, or completion of the already in-progress routing plan.

## Shared contracts and ownership

Assign exactly one coordinating owner to each shared contract. Other team members propose changes through that owner rather than editing the same files concurrently.

| Contract | Required owner | Includes |
| --- | --- | --- |
| Domain and compatibility | Core/compatibility owner | Event envelope, event payloads, reducer state, commands, fixture schemas, normalized comparison |
| Persistence | Storage/migration owner | JSONL format implementation, hashes, manifest, locking, SQLite schema, checkpoints, importer |
| Runtime boundary | Runtime/security owner | Provider contract, tool contract, controller, permissions, cancellation, retries, budgets |
| User surface | CLI/UI owner | Arguments, commands, JSON/human views, TUI, exit codes, documentation surfaces |
| Integration | Lead integrator | Workspace manifests, CI, shared dependency decisions, integration review, phase gates |

Files such as `CLAUDE.md`, root `Cargo.toml`, `.github/workflows/ci.yml`, `README.md`, and architecture/configuration documentation belong to the lead integrator. Core event definitions and compatibility fixture formats belong only to the core/compatibility owner. SQLite migrations and the legacy importer belong only to the storage/migration owner.

## Team execution model

Use an implementation team with non-overlapping primary ownership. A suitable initial team is:

1. **Lead integrator** — establishes the workspace, guards scope, owns shared root files and CI, reviews contract changes, integrates completed branches/worktrees, runs phase gates, and resolves cross-crate API decisions.
2. **Core and compatibility engineer** — finishes/captures the TypeScript routing baseline, specifies event canonicalization, builds golden fixtures, and owns `relay-core` plus cross-language comparisons.
3. **Storage and migration engineer** — owns `relay-store`, JSONL durability, SQLite projections, crash recovery, artifact/store coordination, and legacy import.
4. **Runtime and security engineer** — owns configuration, provider contracts/adapters, tools, permissions, process supervision, agent loop, retry, budget, and cancellation. Split providers/tools into sequential sub-phases if one agent owns both.
5. **CLI and TUI engineer** — owns `relay-ui`, `relay-cli`, view compatibility, terminal restoration, user-facing migration commands, and UI snapshots.

If concurrency is limited, keep the lead active and run roles 2–5 in waves. Do not create two agents that edit the same shared contracts. Parallelize only work whose input contract is already frozen. Each agent must report files changed, decisions made, checks run, and unresolved integration needs. The lead reviews every contribution and runs repository-wide checks after integration.

## Phased implementation

### Phase 0: inventory, preserve user work, and freeze the TypeScript baseline

1. Read `CLAUDE.md`, both Rust rewrite plans, the model-role routing plan, current architecture/configuration docs, package scripts, and CI.
2. Capture the dirty working tree. Identify which current changes implement model-role routing and which are unrelated; preserve all of them.
3. Complete and verify `docs/plans/2026-10-13-model-role-routing.md` before generating baselines. Do not rewrite those files from stale `HEAD` content.
4. Run the existing focused routing tests and the full npm checks.
5. Record CLI help/version, exit codes, configuration normalization, status/why/inspect JSON, representative TUI snapshots, artifact bytes, and end-to-end fake-provider results.
6. Define normalized cross-language comparison rules for timestamps, paths, IDs, maps, and nondeterministic test inputs.
7. Write `docs/event-format.md`, specifying canonical JSON, supported numbers and strings, timestamps, line framing, SHA-256 hash input, previous-hash behavior, segment names, rotation, manifest format, durability modes, and failure diagnostics.
8. Add sanitized fixtures for every event type, workflow mode, gate, failure path, legacy configuration form, and role-based configuration form.
9. Update `CLAUDE.md` for the dual-runtime transition without weakening any existing safety rule.

Phase gate:

- Existing npm checks pass.
- The model-role routing plan is complete and represented in fixtures.
- Fixture generation is deterministic on repeated runs.
- Event format and normalized comparison specifications have no unresolved authority or durability decisions.

### Phase 1: create the Rust workspace and pure domain core

1. Add the pinned Rust toolchain, Cargo workspace, shared lints, formatting policy, and minimal CI jobs.
2. Implement strongly typed IDs, modes, autonomy bounds, artifact kinds/statuses, intents, outcomes, commands, events, payload schema versions, and project state in `relay-core`.
3. Implement two-stage event decoding: minimal envelope/version inspection followed by typed payload validation.
4. Port the reducer, transition rules, invalidation graph, approval rules, intervention policy, and permission decisions without IO dependencies.
5. Use explicit constructors/validation rather than exposing unchecked domain structs.
6. Add unit, golden, and property tests for replay determinism, contiguous sequences, invalid transitions, stale artifacts, and old/new event compatibility.
7. Match normalized TypeScript replay output for all Phase 0 fixtures.

Phase gate:

- `relay-core` has no Tokio, SQL, HTTP, terminal, provider SDK, or filesystem dependency.
- Valid fixtures replay identically; invalid fixtures fail for the expected reason and location.
- Rust format, lint, unit, and documentation tests pass.

### Phase 2: implement canonical JSONL storage and SQLite projections

1. Implement `.relay/manifest.json`, segmented closed logs, one active log, and deterministic segment discovery.
2. Implement canonical serialization and the per-project SHA-256 hash chain exactly as specified.
3. Implement a bounded, cross-platform single-writer lock with readable lock diagnostics.
4. Implement batch validation, one-buffer append, flush/sync, segment rotation, and sequence/hash conflict checks.
5. Implement conservative active-tail recovery. Quarantine only an incomplete final byte tail; never silently remove a complete invalid record.
6. Define forward-only SQLite projection migrations and tables for event lookup, project/session summaries, artifacts, model/tool summaries, diagnostic views, and a sequence/hash checkpoint.
7. Implement startup reconciliation: open, catch up a suffix, or quarantine/rebuild a missing, corrupt, incompatible, ahead, or divergent index.
8. Add `relay doctor check` and `relay doctor rebuild-index` services behind library APIs before exposing CLI commands.
9. Add crash injection after every append/reconcile step and concurrency tests for readers, writers, lock timeouts, and stale expected revisions.

Phase gate:

- Deleting SQLite and rebuilding produces equivalent database-backed views.
- No crash point loses an event already reported as successful.
- Closed-segment corruption, active-tail truncation, gaps, duplicates, incompatible versions, and broken hashes produce the specified diagnostics.
- SQLite never becomes authoritative.

### Phase 3: port artifacts and legacy migration

1. Port artifact schemas and deterministic Markdown renderers from the current Zod/render implementation.
2. Preserve existing filenames, frontmatter, section ordering, escaping, versioning, and no-overwrite behavior.
3. Add SHA-256 content digests to new artifact events without making old events unreadable.
4. Implement temporary write, sync, no-overwrite publication, then event append. Detect orphan files without deleting them automatically.
5. Implement direct, read-only legacy import from `.relay/state.db`: integrity check, row validation, contiguous replay, artifact copy/digest, staged destination, Rust replay comparison, SQLite build, atomic directory switch, and retained timestamped backup.
6. Provide a TypeScript exporter for environments where direct Rust import cannot support a legacy edge case.
7. Test empty, typical, large, corrupt, interrupted, already-migrated, missing-artifact, modified-artifact, and rollback scenarios using copies only.

Phase gate:

- Existing artifact bytes are preserved during migration.
- Import never mutates the source database or source artifacts.
- A failed migration leaves the original `.relay/` active.
- Successful migration matches normalized TypeScript state and can rebuild SQLite from JSONL.

### Phase 4: port configuration, providers, and tools

1. Port layered configuration with the same precedence and YAML compatibility: defaults, user, project, invocation.
2. Port the completed purpose-to-role-to-provider normalization, fallback cycles/errors, limits, and invocation override precedence.
3. Define Relay-owned provider messages, structured output requests, tool calls, stream events, usage, finish reasons, and normalized errors.
4. Implement private OpenAI and Anthropic wire adapters with `reqwest`, streamed response parsing, bounded/redacted errors, explicit retry ownership, and sanitized offline protocol fixtures.
5. Implement the deterministic fake provider first and use it for integration work.
6. Port the tool registry and runtime argument validation using JSON Schema-compatible Rust types.
7. Port path guarding, create-only writes, strict all-or-nothing patches, search, bounded output, redaction, read-only Git tools, allowlisted child environments, and command denial rules.
8. Implement Unix process groups and Windows Job Objects behind one process-supervisor contract.
9. Add adversarial tests for traversal, symlinks, races, patch conflicts, output truncation, secret patterns, environment leakage, command metacharacters, timeouts, cancellation, and process descendants.

Phase gate:

- Configuration fixtures normalize identically to TypeScript.
- Default tests perform no network access.
- Provider SDK/wire types do not cross adapter boundaries.
- Security regression tests pass on macOS, Linux, and Windows.

### Phase 5: port runtime orchestration

1. Port model routing, retries, fallback, budgets, repeated-action limits, cancellation, prompts, and context construction.
2. Port the agent loop using the fake provider first; validate every structured result and tool call before use.
3. Port the controller one vertical workflow slice at a time, recording facts before deriving output.
4. Do not hold the store writer lock during model requests or long tool execution. Record the request, release the lock, perform external work, then validate the expected project revision before recording results.
5. Define deterministic stale-result handling and cover it with competing-writer tests.
6. Match model request/respond/fail events, usage, tool executions/denials, pause/resume, interruption, and budget exhaustion fixtures.
7. Run the complete seven-mode offline fake-provider workflow and compare state, artifacts, and views with TypeScript.

Phase gate:

- End-to-end fake-provider behavior is equivalent to the frozen baseline.
- Restart and resume work at each workflow stage and failure boundary.
- Cancellation terminates full process trees and leaves replayable state.

### Phase 6: implement CLI, JSON views, and terminal UI

1. Implement the current CLI surface with Clap, including argument spellings, aliases, defaults, usage errors, and exit codes.
2. Implement stable view models, then separate JSON and human renderers. Library crates must not print directly.
3. Expose `doctor check`, `doctor rebuild-index`, `doctor export`, and `migrate` with read-only defaults and explicit reporting of any recoverable change.
4. Implement the Ratatui/Crossterm TUI over immutable snapshots and typed controller actions.
5. Preserve slash commands, streaming transcript behavior, status/why/inspect panels, pause overlay, keyboard cancellation, forced exit behavior, narrow-terminal handling, and terminal restoration.
6. Compare help, version, status, why, inspect, run, configuration, error, and JSON outputs with TypeScript fixtures.
7. Add terminal snapshots and pseudo-terminal tests where stable; keep core behavior independently testable without a TTY.

Phase gate:

- Supported command and JSON compatibility tests pass.
- TUI snapshots and terminal restoration tests pass across the platform matrix.
- Non-interactive workflows do not depend on TUI code.

### Phase 7: preview, compare, package, and cut over

1. Package the Rust executable as an opt-in preview without replacing the TypeScript default.
2. Add a read-only comparison command or harness that runs replay and view generation in both implementations without provider calls or tool execution.
3. Exercise migration and rollback using published preview artifacts and sanitized project copies.
4. Measure startup, memory, index rebuild, large-history behavior, and concurrent-reader/writer behavior; investigate regressions rather than weakening correctness.
5. Resolve all unexplained state/view divergences and release-blocking migration defects.
6. Make Rust the default executable only after the agreed preview gate.
7. Update `README.md`, `docs/architecture.md`, `docs/configuration.md`, migration documentation, CI, packaging metadata, and `CLAUDE.md` for final Rust ownership.
8. Retain the TypeScript importer/exporter and rollback instructions for at least one compatibility release. Remove TypeScript runtime code only in a separately reviewed retirement change after the rollback window.

Phase gate:

- Native artifacts are built and verified for supported macOS, Linux, and Windows targets.
- Migration and rollback have been tested from published artifacts.
- No unexplained compatibility divergence remains.
- Rust is the default executable without requiring Node.js.

## Engineering constraints

- Preserve all pre-existing user changes. Never reset, discard, overwrite, commit, push, or broadly reformat them.
- Do not mix product redesign with the language/storage migration. Any deliberate behavior change requires a separate decision and compatibility note.
- Keep `relay-core` pure and dependency-light. IO and vendor types remain at adapter boundaries.
- JSONL is the sole authoritative workflow record. SQLite projections must be disposable and reproducible.
- Persist a durable event before reporting its operation successful or updating derived projections.
- Keep event and store formats explicitly versioned. Unknown/newer authoritative records fail closed with upgrade guidance.
- Preserve old event readability. Additive event fields must not invalidate legacy fixtures.
- Use canonical serialization for hashes; never depend on map iteration order or raw input formatting.
- Hold a project writer lock only for short storage operations, never across network calls or long-running tools.
- Treat clocks, randomness, IDs, provider output, and filesystem observations as injected boundary data so replay remains deterministic.
- Use argument-array process execution only. Never interpolate model or user input into a shell string.
- Preserve workspace containment, symlink checks, no-overwrite writes, all-or-nothing patches, output bounds, secret redaction, environment allowlists, Git read-only behavior, and delivery holds.
- Keep credentials in environment variables or OS-backed stores; never persist them in JSONL, SQLite, artifacts, fixtures, logs, errors, or diagnostic bundles.
- Tests are offline and deterministic by default. Live provider tests must be explicit opt-ins.
- Keep modules small with clear ownership. Shared event, provider, database, configuration, and CLI contracts require coordination and a single editor at a time.
- Use safe Rust. Any proposed `unsafe` block requires lead review, a written invariant, and focused tests; prefer dependencies or designs that avoid it.
- Pin and test a minimum supported Rust version. Commit `Cargo.lock` for the application workspace.
- Do not delete legacy project state or migration backups automatically.

## Acceptance criteria

1. A supported legacy `.relay/state.db` project migrates without modifying its source, changing its logical replay state, or changing existing artifact bytes.
2. JSONL alone reconstructs all authoritative workflow state.
3. Removing `index.sqlite` and reopening reconstructs equivalent indexed and user-visible state.
4. JSONL records are segmented, versioned, canonically serialized, sequence-checked, and hash-chained.
5. A crash or cancellation cannot lose an event that Relay reported as durably successful.
6. Only an incomplete final active-segment tail is automatically quarantined; complete invalid history fails closed.
7. The Rust reducer matches all valid TypeScript compatibility fixtures and rejects invalid fixtures for the expected reasons.
8. The seven workflow modes, transitions, approval gates, artifact invalidation, intervention rules, autonomy bounds, verification requirements, and delivery hold remain equivalent.
9. Model roles, routing purposes, provider/model selection, fallbacks, retries, budgets, usage accounting, cancellation, and resume remain equivalent.
10. OpenAI and Anthropic protocol types remain private to adapters; the fake provider supports all default offline tests.
11. Tool permissions and security tests pass on macOS, Linux, and Windows, including traversal, symlink, patch race, secret, environment, timeout, and process-tree cases.
12. Existing supported CLI commands, flags, slash commands, exit codes, and JSON output remain compatible or have an explicitly approved migration note.
13. Ratatui restores the terminal on normal exit, cancellation, errors, and forced exit.
14. Rust release artifacts require no Node.js runtime and include version, checksums, licenses, and signatures according to the release process.
15. Migration rollback is exercised with published artifacts, and backups are retained.
16. Documentation and `CLAUDE.md` accurately describe the active implementation and required checks at each migration stage.
17. No implementation agent commits or pushes changes unless separately requested by the user.

## Verification commands

Run focused checks during each phase, then the complete applicable set before reporting the phase complete. Adjust package/crate selectors only to match the workspace names actually created; do not weaken the checks.

### Preserve and verify the TypeScript reference

```bash
git status --short
git diff --check
npm run typecheck
npm test
npm run build
node dist/cli.js --help
node dist/cli.js --version
npm pack --dry-run
```

Run focused routing and compatibility suites while establishing the baseline:

```bash
npm test -- --run test/unit/config/load.test.ts
npm test -- --run test/unit/runtime/model-router.test.ts
npm test -- --run test/unit/runtime/agent-loop.test.ts
npm test -- --run test/unit/core/contracts.test.ts test/unit/core/reducer.test.ts
npm test -- --run test/integration/cli.test.ts
npm test -- --run test/integration/replay.test.ts
npm test -- --run test/integration/workflow.test.ts
```

### Rust formatting, linting, build, and tests

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo build --workspace --all-targets --all-features
cargo doc --workspace --all-features --no-deps
```

If configured in the repository toolchain:

```bash
cargo deny check
cargo audit
```

### Focused Rust suites

Use exact test targets established by the implementation; they must cover at least:

```bash
cargo test -p relay-core
cargo test -p relay-store
cargo test -p relay-artifacts
cargo test -p relay-config
cargo test -p relay-providers
cargo test -p relay-tools
cargo test -p relay-runtime
cargo test -p relay-ui
cargo test -p relay-cli
cargo test --test compatibility
cargo test --test migration
cargo test --test fault_injection
cargo test --test security
```

### Runtime smoke and recovery checks

Run against disposable temporary workspaces and fixture copies, never a user's live `.relay/` directory:

```bash
cargo run -p relay-cli -- --help
cargo run -p relay-cli -- --version
cargo run -p relay-cli -- doctor check --workspace <fixture-copy>
cargo run -p relay-cli -- doctor rebuild-index --workspace <fixture-copy>
cargo run -p relay-cli -- migrate --workspace <legacy-fixture-copy>
cargo run -p relay-cli -- run --provider fake --no-interactive --json --workspace <fixture-copy>
```

### Final review

```bash
git status --short
git diff --check
git diff --stat
git diff -- CLAUDE.md Cargo.toml Cargo.lock rust-toolchain.toml .github/workflows/ci.yml README.md docs crates tests scripts src test
```

Confirm that:

- pre-existing user changes are still present;
- no secrets, credentials, live transcripts, or unsanitized local paths entered fixtures or logs;
- no unrelated formatting or generated build output is included;
- all required Node and Rust checks applicable to the current migration phase passed;
- failures, platform gaps, compatibility exceptions, and deferred work are reported explicitly;
- nothing was committed or pushed.

## Completion report format

When implementation work concludes, report:

1. Completed phases and user-visible behavior.
2. Files/crates added or materially changed, grouped by ownership area.
3. Compatibility and migration results, including fixture counts and any approved exceptions.
4. Verification commands run and their exact pass/fail outcomes.
5. Security, durability, and cross-platform evidence.
6. Unresolved issues, deferred decisions, and recommended next actions.
7. Confirmation that no commit or push was performed.
