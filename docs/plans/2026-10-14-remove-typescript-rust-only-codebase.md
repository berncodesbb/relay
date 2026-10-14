# Remove the TypeScript implementation and establish a Rust-only codebase

Status: implemented 2026-10-14 (working tree; not yet committed)  
Date: 2026-10-14  
Primary architecture reference: `docs/plans/2026-10-13-intent-first-rust-work-graph.md`  
Related product plan: `docs/plans/2026-10-14-default-interactive-chat-interface.md`

## Objective

Remove Relay's retired TypeScript/Node.js implementation from the active repository and make the intent-first Rust work-graph implementation the sole supported product, build, test, migration, and documentation path.

The completed repository must build and test without Node.js, npm, TypeScript, React, Ink, Vitest, `tsx`, or JavaScript-based maintenance scripts. Preserve the behavior and evidence still needed by Rust: checked-in legacy fixtures, the Rust-native semantic importer for existing `.relay/state.db` workspaces, historical design records, user-owned sample workspaces, and all current Rust behavior. Do not redesign the Rust product or implement the chat-first interface as part of this retirement change.

This is an explicit retirement gate authorized after the earlier freeze. It supersedes repository instructions that describe `src/**`, `test/**`, or the TypeScript application as a live or frozen in-tree reference. Git history and, if created by the human maintainer outside this implementation, a release/archive tag are the source archive; do not keep a second copy under `legacy/`.

## Current state and repository instructions

Read `CLAUDE.md`, this plan, `docs/plans/2026-10-13-intent-first-rust-work-graph.md`, and the related chat-interface plan before changing files.

- `CLAUDE.md` currently forbids deletion of the TypeScript reference before a retirement gate. This plan is that explicit retirement change, so implementation must update `CLAUDE.md` to describe a Rust-only repository and remove obsolete TypeScript checks and ownership rules.
- The active implementation is the intent-first Rust graph in `crates/relay-domain`, `relay-engine`, `relay-workflows`, `relay-executor`, `relay-app`, `relay-store`, `relay-artifacts`, `relay-providers`, `relay-tools`, `relay-config`, `relay-ui`, and `relay-cli`.
- `src/**` and `test/**` are the retired TypeScript application and tests. They are deleted, not ported or archived in-tree.
- `scripts/export-compatibility-fixtures.mjs` imports the TypeScript application as an oracle. The generated `tests/fixtures/**` already exist and should become immutable historical evidence rather than regenerable output.
- `scripts/export-legacy-project.mjs` is no longer needed because `relay-store` directly imports legacy `.relay/state.db` workspaces. `crates/relay-tests/tests/migration.rs` contains one optional Node/`tsx` exporter parity test that must be replaced or removed without weakening direct Rust migration coverage.
- `scripts/make-cli-executable.mjs` exists only for the npm build and is deleted with the Node package.
- `.github/workflows/ci.yml` currently has Node and Rust jobs. Remove the Node matrix and preserve the Rust gates on Linux, macOS, and Windows.
- `tests/fixtures/**` contains checked-in legacy event/config/CLI fixtures. Preserve fixture bytes unless a targeted metadata-only conversion is necessary; these files are historical inputs, not the acceptance oracle for new graph behavior.
- `test/fixtures/sample-repo/**` is inside the retired TypeScript test tree. Before deleting it, verify whether any Rust test references it. If Rust needs a JavaScript sample workspace, move only the minimal sample into a Rust-owned fixture directory in a history-preserving way. JavaScript inside a sample user project is allowed; JavaScript as Relay implementation or build tooling is not.
- `crates/relay-core` and `crates/relay-runtime` are stopped Rust compatibility-port crates, not TypeScript. Their retirement is outside this plan. Keep them frozen unless a separate approved plan removes them.
- The working tree was already substantially modified when this plan was written. Treat every pre-existing modification, deletion, and untracked file as user-owned. Inspect `git status --short` and relevant diffs first. Do not reset, overwrite, reformat, stage, commit, or push user changes.
- Several files this plan must update, including `README.md`, `.github/workflows/ci.yml`, and the chat-interface plan, already have user changes. Integrate narrowly with their current contents; never restore their `HEAD` versions or replace whole files from the plan's snapshot.

## Scope

### In scope

- Delete the TypeScript application and TypeScript test suite.
- Delete Node/npm/TypeScript/Vitest configuration and Node-only scripts.
- Remove Node jobs, npm caches, npm packaging, and Node commands from CI and developer instructions.
- Make checked-in compatibility fixtures self-contained and immutable.
- Remove the optional Node exporter parity path from Rust migration tests while preserving Rust-native legacy migration coverage.
- Rewrite current README, repository instructions, architecture/configuration/migration documentation, and active implementation plans so Rust is the only supported implementation.
- Replace documentation references to deleted TypeScript source contracts with stable Rust contracts, frozen fixture provenance, or self-contained format definitions.
- Add a small automated repository-hygiene check if it can distinguish Relay implementation files from JavaScript/TypeScript files intentionally used as sample workspace inputs.
- Verify the full Rust workspace and focused intent-first suites on the resulting tree.

### Out of scope

- Implementing the default chat interface described by `2026-10-14-default-interactive-chat-interface.md`.
- Redesigning the work graph, application services, terminal UI, provider routing, tools, or persistence formats.
- Removing `crates/relay-core` or `crates/relay-runtime`; they require a separate Rust compatibility-port retirement decision.
- Changing existing `.relay-graph` formats or requiring migration for current Rust projects.
- Dropping support for importing legacy TypeScript `.relay/state.db` workspaces.
- Deleting or rewriting historical planning documents solely because their decisions are obsolete. Mark them historical where necessary and keep them available as design history.
- Creating a Git tag, branch, commit, release, or push. Recommend a tag to the maintainer in the final report, but do not create it without explicit authorization.
- Removing ignored local artifacts such as `node_modules/` or `dist/`. Report them if present; do not delete user-local data unless explicitly requested.

## Affected files

### Delete: retired implementation and build surface

- `src/**`
- `test/**`, subject to the Rust-reference check for `test/fixtures/sample-repo/**`
- `package.json`
- `package-lock.json`
- `tsconfig.json`
- `tsconfig.build.json`
- `vitest.config.ts`
- `scripts/export-compatibility-fixtures.mjs`
- `scripts/export-legacy-project.mjs`
- `scripts/make-cli-executable.mjs`
- `scripts/lib/canonical-json.mjs`
- `scripts/lib/canonical-json.d.mts`

### Preserve and potentially update narrowly: compatibility evidence

- `tests/fixtures/**` — retain fixture payloads and expected values; update only provenance metadata or manifests needed to remove a deleted generator dependency.
- `crates/relay-tests/tests/migration.rs` — remove the Node/`tsx` exporter parity test and keep or strengthen direct Rust migration assertions.
- `crates/relay-tests/tests/compatibility.rs`
- `crates/relay-tests/tests/semantic_migration.rs`
- `crates/relay-config/tests/legacy_compat.rs`
- Any Rust tests that consume `tests/fixtures/**`.

### Update: repository policy, CI, and active documentation

- `CLAUDE.md` — record the fundamental Rust-only engineering change, remove TypeScript rules/checks, and replace deleted-source references.
- `.github/workflows/ci.yml` — remove Node jobs; retain Rust matrix, docs gate, and focused suites.
- `README.md` — present Rust installation, usage, architecture, development, testing, migration, and limitations only.
- `docs/architecture.md`
- `docs/configuration.md`
- `docs/event-format.md`
- `docs/migration-to-rust.md` — replace the stale stopped-port procedure with a concise legacy `.relay` to `.relay-graph` Rust-native migration guide, or clearly mark it historical and point to the authoritative semantic migration documentation.
- `docs/plans/2026-10-14-default-interactive-chat-interface.md` — remove the TypeScript freeze and npm verification requirements; retain its Rust feature scope unchanged.
- `docs/plans/2026-10-13-intent-first-rust-work-graph.md` and other active documents that state the TypeScript tree remains in-repository.
- Historical plans under `docs/plans/**` only where a short status banner is needed to prevent them from being mistaken for current instructions. Do not rewrite their original decisions.
- `.gitignore` if it contains Node-only entries that no longer serve sample-workspace or developer needs; retaining harmless ignores such as `node_modules/` is acceptable.

### Inspect, normally unchanged

- Root `Cargo.toml`, `Cargo.lock`, and `rust-toolchain.toml` — confirm they have no Node coupling; do not churn them without a real dependency change.
- `crates/relay-store/src/migrate/**` — verify the legacy importer is self-contained. Change only if tests reveal an actual dependency or coverage gap.
- `crates/relay-core/**` and `crates/relay-runtime/**` — remain frozen Rust references.

## Phased implementation

### Phase 0: preservation audit and ownership freeze

1. Read all applicable instructions and plans.
2. Capture `git status --short` and relevant diffs. Record which affected shared files already contain user edits.
3. Inventory all `.ts`, `.tsx`, `.mts`, `.mjs`, `package*.json`, npm, Node, TypeScript, and deleted-source references, including ignored files only for reporting.
4. Find every Rust or documentation reference to `src/**`, `test/**`, the Node exporter, fixture generator, npm, and TypeScript runtime behavior.
5. Confirm whether Rust tests use `test/fixtures/sample-repo/**`. If so, assign one owner to relocate the minimum fixture before deleting `test/**`.
6. Run the existing Rust baseline, or record pre-existing failures with exact commands and output. Do not run npm as a prerequisite for retirement unless specifically needed to diagnose a baseline; the final state must not require it.
7. Assign one lead/integrator as the sole owner of `CLAUDE.md`, `README.md`, CI, root manifests, and cross-cutting documentation. Other team members report required shared-file changes to that owner.

Phase gate:

- Every planned deletion has been classified as implementation, test, generated fixture tool, migration tool, or sample input.
- No Rust production or test path depends on executing the TypeScript implementation.
- Pre-existing user changes and failures are documented and protected.

### Phase 1: make legacy compatibility Rust-self-contained

1. Remove the conditional Node/`tsx` exporter parity test from `crates/relay-tests/tests/migration.rs`.
2. Preserve the behavioral guarantees that test was meant to cover using Rust-native fixtures or direct assertions: legacy SQLite is opened read-only, sequences and payloads are validated, artifacts are carried over, semantic extraction is deterministic, staging/rollback is safe, and failure leaves `.relay/` intact.
3. Confirm the Rust importer accepts representative legacy databases without invoking Node or importing TypeScript code.
4. Treat `tests/fixtures/**` as immutable archived evidence. Update manifest/meta provenance only if consumers or documentation currently claim the deleted generator remains executable.
5. Replace any specification statement whose sole authority is a deleted TypeScript symbol with an explicit enumerated contract, a Rust contract, or a checked-in fixture reference.
6. Ensure unknown/newer legacy data still fails closed with actionable guidance.

Phase gate:

- `cargo test --test migration` and `cargo test --test semantic_migration` pass with Node unavailable.
- No Rust test spawns `node`, `npm`, `npx`, `tsx`, or a deleted script.
- Legacy `.relay/state.db` migration remains supported and non-destructive.

### Phase 2: remove the TypeScript and Node tree

1. Delete `src/**`, the TypeScript-owned `test/**`, package manifests/lockfile, TypeScript/Vitest configuration, and Node-only scripts listed above.
2. If a sample JavaScript repository is required by Rust tests, relocate only that fixture to a Rust-owned fixture path and update consumers. Do not retain TypeScript test infrastructure around it.
3. Do not remove ignored local `node_modules/`, `dist/`, coverage, or cache directories; they are local user data. Note their presence in the completion report.
4. Search for executable or build-time Node dependencies after deletion. Distinguish intentional JavaScript sample-workspace content from Relay tooling.
5. Ensure no Cargo build script, test, or fixture setup assumes npm-installed dependencies.

Phase gate:

- No Relay implementation `.ts`/`.tsx` files, Node package manifests, or Node maintenance scripts remain.
- Rust tests do not require Node on `PATH`.
- Intentional JavaScript sample projects, if any, are clearly isolated as test inputs.

### Phase 3: establish Rust-only policy and CI

1. Update `CLAUDE.md` because this retirement is a fundamental engineering change:
   - declare the intent-first Rust implementation authoritative;
   - remove TypeScript work rules, TypeScript checks, and the no-delete retirement warning;
   - preserve work-graph authority, security, ownership, dirty-worktree, migration-backup, and no-commit/no-push rules;
   - keep `relay-core`/`relay-runtime` frozen until separately retired;
   - state that legacy compatibility is maintained by Rust importers and immutable fixtures.
2. Remove the Node job and npm cache from `.github/workflows/ci.yml`.
3. Retain the cross-platform Rust matrix and the full fmt, clippy, test, build, rustdoc, and focused intent-first gates.
4. Add a lightweight repository-hygiene check only if it is precise. It may reject root/package tooling and implementation TypeScript paths, but it must allow fixture projects written in languages Relay is expected to edit.
5. Confirm CI shell syntax remains portable across the existing Linux, macOS, and Windows runners; avoid Unix-only hygiene commands in a cross-platform job unless isolated appropriately.

Phase gate:

- CI has no Node setup or npm commands.
- Repository instructions contain no required TypeScript workflow.
- CI continues to cover all supported platforms and live Rust crates.

### Phase 4: documentation cutover

1. Rewrite the README around the Rust binary and current work-graph behavior. Remove npm installation, Node requirements, TypeScript quick starts, TypeScript project layout, TypeScript test inventory, and claims that Node is the shipping reference.
2. Preserve only behavior that currently exists. Do not present the planned default chat interface as shipped before its implementation lands.
3. Update architecture and configuration documentation to point at live Rust crates and current config behavior.
4. Make `docs/event-format.md` self-contained for formats still supported. Clearly distinguish the active `.relay-graph` fact format from the legacy `.relay/state.db` import format.
5. Replace the outdated migration guide with the actual Rust-native semantic migration flow, including dry-run, staging, rollback, backups, and the guarantee that legacy state is never automatically deleted.
6. Update the chat-interface plan so its prerequisites and verification commands assume a Rust-only repository. Do not alter its product requirements.
7. Add short historical status notices to obsolete plans only where needed. Preserve the original documents for architectural archaeology.
8. Run a repository-wide prose and path search for deleted files, npm commands, TypeScript checks, and claims of dual implementations. Remaining references must be intentionally historical or legacy-format documentation and labeled as such.

Phase gate:

- A new contributor can install, build, test, and understand Relay using Rust documentation alone.
- Active documentation does not refer readers to deleted source files or scripts.
- Historical references are clearly labeled and cannot be mistaken for current instructions.

### Phase 5: integration and regression review

1. Run focused migration, compatibility, CLI, TUI, store, security, and work-graph suites.
2. Run the complete workspace fmt, clippy, test, build, and rustdoc gates.
3. Exercise `relay --help`, `relay --version`, fake-provider work execution, project summary, doctor checks, and a Rust-native legacy migration using temporary workspaces.
4. Search the final tracked tree for Node/npm/TypeScript build dependencies and deleted paths.
5. Review all changes against the Phase 0 status snapshot. Confirm no unrelated user edit was overwritten or reformatted.
6. Report any ignored local Node artifacts without deleting them.
7. Recommend that the maintainer create an annotated `v0.1.0-typescript-final` tag at the appropriate pre-removal commit, but do not create, commit, or push anything.

Phase gate:

- All acceptance criteria pass.
- Required checks pass, or pre-existing failures are reported separately with evidence.
- The diff contains only retirement, compatibility preservation, policy, CI, and documentation changes.

## Engineering constraints

- Preserve all existing user changes. Never use reset, checkout, clean, bulk replacement, or whole-file restoration to resolve overlap.
- Safe Rust only, pinned toolchain, committed `Cargo.lock`, and existing workspace lint policy.
- Do not redesign or opportunistically refactor active Rust behavior during deletion.
- Keep `relay-domain`, `relay-engine`, and `relay-workflows` pure.
- JSONL facts remain authoritative; SQLite projections remain rebuildable.
- Preserve provider isolation, deterministic validation, capability grants, workspace containment, symlink defenses, atomic/no-overwrite writes, redaction, bounded process execution, read-only Git tools, decision gates, and delivery holds.
- Preserve dirty worktrees and never delete legacy state or migration backups automatically.
- Legacy import must be implemented and tested entirely in Rust. Migration source workspaces remain read-only until a validated staged cutover.
- Checked-in legacy fixtures are historical evidence, not a generator contract and not the acceptance oracle for new work-graph behavior.
- Do not introduce a replacement scripting runtime merely to avoid Node. Prefer Rust tests, build logic, or static fixtures.
- Do not forbid JavaScript/TypeScript in user-project fixtures; Relay is a language-agnostic coding tool. Hygiene rules must target Relay implementation/build surfaces precisely.
- Keep historical documents intact except for short status/provenance notices and broken-link corrections.
- Do not modify `crates/relay-core` or `crates/relay-runtime` as part of TypeScript retirement.
- Do not delete ignored local files or directories.
- Do not create tags, commits, branches, releases, or pushes without explicit authorization.

## Acceptance criteria

1. `src/**`, the TypeScript-owned `test/**`, Node-only scripts, `package.json`, `package-lock.json`, TypeScript configuration, and Vitest configuration are absent from the tracked tree.
2. No Relay build, test, CI, migration, fixture-generation, packaging, or documentation workflow requires Node.js, npm, `tsx`, TypeScript, React, Ink, or Vitest.
3. Bare Cargo build and test operations succeed on a machine where Node is unavailable.
4. The Rust semantic migration command still imports representative legacy `.relay/state.db` projects through a read-only, staged, rollback-safe path.
5. Legacy migration failure leaves the source `.relay/` and its artifacts intact; backups are never automatically deleted.
6. Checked-in legacy fixtures remain usable by Rust compatibility tests and have self-contained provenance that does not require a deleted generator.
7. Active documentation references live Rust contracts or self-contained specifications rather than deleted TypeScript source files.
8. README installation, quick start, development, architecture, and testing instructions are Rust-only and describe only shipped behavior.
9. `CLAUDE.md` records Rust as the sole implementation, retains all applicable safety/authority rules, and lists only relevant Rust verification checks.
10. CI contains no Node job and retains cross-platform Rust fmt, clippy, test, build, rustdoc, and focused-suite coverage.
11. Existing Rust CLI commands, JSON/human outputs, work-graph behavior, provider adapters, tools, migration behavior, and TUI behavior are unchanged except where documentation/provenance removal requires deliberate snapshot updates.
12. Intentional JavaScript or TypeScript sample projects remain allowed as isolated user-workspace fixtures and cannot be mistaken for Relay implementation code.
13. `crates/relay-core` and `crates/relay-runtime` remain unchanged and frozen.
14. Existing `.relay-graph` projects open without migration or behavior changes.
15. No pre-existing user change is lost, overwritten, reset, reformatted, committed, or pushed.

## Verification commands

Start with preservation and dependency searches:

```bash
git status --short
git diff --stat
git diff -- README.md .github/workflows/ci.yml CLAUDE.md docs
rg -n "node|npm|npx|tsx|TypeScript|typescript|React|Ink|Vitest|src/|test/" \
  CLAUDE.md README.md .github docs crates tests Cargo.toml
rg -n "Command::new\(\"(node|npm|npx|tsx)\"\)|scripts/.*\.(mjs|mts)|package-lock\.json|package\.json" \
  crates tests .github README.md CLAUDE.md
find . -path './.git' -prune -o -path './target' -prune -o -path './node_modules' -prune \
  -o -type f \( -name '*.ts' -o -name '*.tsx' -o -name '*.mts' -o -name '*.mjs' \) -print
```

Interpret search results rather than requiring zero textual uses of “TypeScript”: intentionally historical documents, legacy migration descriptions, and sample user projects may retain labeled references.

Run focused compatibility and migration checks:

```bash
cargo test -p relay-config --test legacy_compat
cargo test -p relay-tests --test compatibility
cargo test -p relay-tests --test migration
cargo test -p relay-tests --test semantic_migration
cargo test -p relay-tests --test fact_replay
cargo test -p relay-tests --test crash_recovery
```

Run the repository-required Rust gates:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo build --workspace --all-targets --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps --exclude relay-core --exclude relay-runtime
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

Use exact package-qualified targets if Cargo reports ambiguity for root-level `--test` invocations.

Run non-destructive CLI smoke checks:

```bash
cargo run -p relay-cli -- --help
cargo run -p relay-cli -- --version
cargo run -p relay-cli -- --workspace /path/to/temporary-fixture project summary --json
cargo run -p relay-cli -- --workspace /path/to/temporary-legacy-copy migrate --dry-run
cargo run -p relay-cli -- --workspace /path/to/temporary-fixture doctor check
```

Create smoke-test workspaces under a temporary directory, never by deleting or repurposing the repository's own state. Do not run npm checks after the retirement: successful removal means those commands and manifests no longer exist.

## Team structure and integration guidance

Use a small team with explicit, non-overlapping ownership:

1. **Lead/integrator** — owns baseline preservation, shared-file coordination, `CLAUDE.md`, README, CI, active/historical documentation classification, final searches, integration review, and the full verification matrix.
2. **Compatibility/migration engineer** — owns `crates/relay-tests/tests/migration.rs`, Rust-native migration/compatibility coverage, and fixture provenance analysis. Does not edit CI or general documentation.
3. **Retirement/build engineer** — owns deletion inventory, removal of `src/**`, `test/**`, package/config/script files, sample-fixture relocation if required, and precise repository-hygiene checks. Does not edit Rust migration logic or shared docs.
4. **Independent reviewer/test engineer** — reviews the integrated diff for lost behavior, stale references, unsafe migration changes, cross-platform CI problems, and user-change preservation; runs or audits focused checks without taking ownership of implementation files.

The lead must freeze ownership before parallel work and coordinate every shared-file change. The compatibility engineer should complete or specify fixture changes before the retirement engineer deletes generators. The lead integrates in this order: compatibility self-containment, file removal, policy/CI cutover, documentation cutover, independent review, full checks. Team members must not commit or push.
