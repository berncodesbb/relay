# Intent-first Rust work-graph implementation plan

Status: proposed replacement plan — now the authoritative implementation;
the TypeScript application it was replacing was removed from the repository
on 2026-10-14 (`2026-10-14-remove-typescript-rust-only-codebase.md`)  
Date: 2026-10-13  
Supersedes as implementation direction: `docs/plans/2026-10-13-rust-rewrite-implementation.md`  
Retains useful background from: `docs/plans/2026-10-13-rust-rewrite-architecture.md`

## Objective

Rebuild Relay in Rust around its product intent rather than porting the TypeScript implementation one-for-one.

Relay's product intent is:

> Turn an ambiguous product goal into verified software through a controlled, inspectable, resumable process, while keeping models replaceable and consequential decisions under human control.

The new application will be a durable work-graph engine with one opinionated built-in software-delivery workflow. Discovery, specification, technical design, planning, implementation, verification, and delivery remain recognizable product phases, but they are no longer a global seven-state machine. They become work definitions and grouped views over addressable goals, requirements, decisions, tasks, artifacts, evidence, attempts, and human decisions.

The rewrite must preserve Relay's user-facing product guarantees and safety boundaries, but it must not preserve accidental TypeScript architecture merely for compatibility. The current TypeScript app and the partially implemented Rust port are sources of requirements, scenarios, tests, adapters, and hardened infrastructure—not structural specifications for the new domain model.

## Why the previous approach is being stopped

The active Rust rewrite follows the current TypeScript decomposition too closely: event payloads, reducer state, controller behavior, provider streams, CLI shapes, and artifacts are being reproduced for byte-level compatibility. That approach transfers the current monolithic controller and global-mode assumptions into Rust and captures little architectural value beyond native packaging.

This plan changes the success criterion:

- **Old criterion:** Rust reproduces TypeScript state and output byte-for-byte.
- **New criterion:** Rust naturally satisfies Relay's product scenarios and invariants through a small deterministic work-graph kernel.

Compatibility remains important at product boundaries—safe migration, readable existing artifacts, familiar workflow concepts, configuration intent, and user protections—but internal state, event types, module structure, SQLite projections, and exact CLI JSON are not automatically preserved.

## Applicable repository instructions and current working state

Read `CLAUDE.md` completely before acting. Its user-work preservation, provider isolation, deterministic policy, local-first storage, secrets, safe process execution, test quality, shared ownership, and no-commit/no-push rules remain applicable.

`CLAUDE.md` currently describes the now-stopped compatibility rewrite as authoritative. Updating those instructions is an explicit first-phase task owned by the lead integrator. Do not silently disregard them and do not continue implementing the old plan while this plan is being adopted.

The working tree contains substantial user-owned, unfinished Rust work in:

- `crates/relay-core/**`;
- `crates/relay-store/**`;
- `crates/relay-artifacts/**`;
- `crates/relay-providers/**`;
- `crates/relay-tools/**`;
- `crates/relay-ui/**`;
- `crates/relay-cli/**`;
- `crates/relay-tests/**`;
- `Cargo.lock`, crate manifests, `docs/event-format.md`, and `docs/plans/rewrite-execution-log.md`.

Treat all of it as user work. Do not reset, delete, overwrite, or mass-reformat it. Before editing, inventory each component and classify it as:

1. reusable without carrying the old domain architecture;
2. reusable after isolating its interfaces;
3. useful only as a test/reference implementation;
4. incompatible with the intent-first design and therefore to be retired later through an explicit, reviewed change.

Likely reusable areas include canonical local storage primitives, locking and crash recovery, provider wire adapters, redaction, path guarding, process supervision, artifact rendering utilities, terminal rendering infrastructure, and configuration parsing. The old event model, seven-mode reducer, transition compatibility machinery, and controller-shaped interfaces require redesign rather than incremental extension.

Do not remove obsolete code in the same change that establishes the new kernel. Keep it buildable or clearly isolated until its replacement is verified and the user approves retirement.

## Scope

### In scope

- A small, pure Rust work-graph kernel.
- Addressable goals, work items, dependencies, decisions, typed artifacts, evidence, attempts, capability grants, receipts, and invalidation.
- One built-in, opinionated software-delivery workflow expressed using the kernel.
- Familiar discovery, specification, design, planning, implementation, verification, and delivery views without a global current-mode state machine.
- Dynamic but policy-controlled graph expansion.
- Human decisions represented as first-class blocking work.
- Explicit executor contracts for human, model-only, workspace-changing, verification, and deterministic system work.
- Conservative scheduling: parallel read-only/model-only work, serialized workspace writes initially.
- Structured artifact content with deterministic Markdown views.
- Durable domain facts in JSONL, rebuildable SQLite projections, immutable artifact/evidence storage, and bounded operational logs.
- Migration that extracts durable product meaning from existing Relay projects rather than reproducing every legacy internal event.
- A new CLI/TUI projection centered on ready work, blockers, decisions, evidence, and recommendations.
- Scenario-driven and property-based verification of product invariants.

### Out of scope for the first intent-first release

- A user-authored workflow DSL.
- Third-party workflow or executor plugins.
- Distributed execution.
- Automated merging of concurrent workspace writes.
- Hosted accounts, synchronization, or collaboration.
- A web interface.
- Autonomous delivery, Git mutation, deployment, or publishing.
- Semantic indexes or vector databases.
- Byte-for-byte reproduction of legacy reducer state, internal event names, SQLite schema, or every CLI JSON field.
- Deletion of the legacy TypeScript implementation or unfinished compatibility port before replacement and migration are proven.

## Product scenarios that define the architecture

Implement against scenarios, not old module parity. These scenarios are the initial executable specification:

1. An ambiguous goal creates discovery work and a focused human clarification; the resulting brief can be approved.
2. An approved brief expands into addressable requirements and acceptance criteria, rendered as a coherent PRD view.
3. A changed requirement invalidates only work, artifacts, and evidence that actually depend on that requirement version.
4. Technical research and repository inspection can become ready together and run concurrently because both are read-only.
5. Two workspace-changing tasks with overlapping write scopes cannot run concurrently.
6. A model proposes new work and dependencies; deterministic policy rejects cycles, duplicates, excessive capabilities, and unauthorized scope expansion.
7. An implementation attempt receives a bounded capability grant and cannot operate outside it.
8. Verification attaches evidence to acceptance criteria; a failure creates targeted corrective work and later makes verification ready again.
9. A crash after a durable fact or receipt can be resumed without pretending incomplete external work succeeded.
10. Relay explains why any work item is ready, blocked, invalid, awaiting a person, or rejected.
11. Providers can be changed without changing graph or policy semantics.
12. Relay prepares a delivery package but cannot deliver without an explicit human decision.

If implementation of these scenarios requires a hidden `current_mode`, global pause flag, backward jump, or monolithic controller special case, stop and revisit the kernel design.

## Target domain model

### Core entities

- **Project** — owns one goal graph and its policy/configuration context.
- **Goal** — desired outcome, constraints, success definition, and provenance.
- **WorkItem** — bounded objective with kind, scope, inputs, expected outputs, completion rule, executor class, and dependency references.
- **Dependency** — typed relation such as requires, informs, validates, supersedes, or conflicts-with.
- **Artifact** — versioned structured content with a deterministic human-readable rendering.
- **Decision** — unresolved or resolved human choice containing options, recommendation, evidence, consequences, authority, and supersession rules.
- **Evidence** — typed observation attached to a claim, criterion, work item, or artifact version.
- **Attempt** — one scheduled execution of a work item against an expected project revision.
- **CapabilityGrant** — exact read, write, process, provider, budget, and delivery authority for an attempt.
- **AttemptReceipt** — bounded durable account of outcome, outputs, evidence, changes, usage, and failures.
- **Policy** — deterministic rule deciding readiness, acceptance, expansion, invalidation, intervention, and scheduling.

### Derived lifecycle

Keep durable facts small and derive operational state:

```text
proposed -> ready -> running -> completed
              |        |          |
              |        |          +-> invalidated
              |        +-> failed / canceled / abandoned
              +-> blocked / awaiting-human
```

Do not append `ready` or `blocked` facts merely because a projection changed. Derive them from dependencies, accepted receipts, decisions, grants, and policy. Persist facts such as declarations, accepted completions, resolved decisions, published outputs, cancellations, supersessions, and invalidations.

### Work graph invariants

- Stable typed IDs identify every addressable node and version.
- Dependency edges are acyclic where ordering semantics require it.
- A work item references exact input versions or an explicit version-selection rule.
- Completion is accepted only from a receipt matching the work item and expected project revision.
- An executor may propose graph changes but never install them directly.
- Scope-expanding proposals require deterministic validation and, when consequential, a human decision.
- Invalidation follows dependency relationships and exact input versions, not coarse phase order.
- Human decisions are graph nodes; global `paused` state is a projection, not an authority.
- Project completion is derived from required terminal outcomes and unresolved blockers.

## Built-in software-delivery workflow

The initial workflow is compiled into Rust rather than expressed in a public DSL. It provides work definitions and grouped views for:

- discover opportunity;
- clarify consequential unknowns;
- define requirements and acceptance criteria;
- design technical approach and capture decisions;
- decompose an execution plan;
- inspect/research the repository;
- implement bounded tasks;
- integrate changes;
- verify criteria with evidence;
- create corrective work;
- prepare delivery evidence;
- request delivery authorization.

These definitions declare required inputs, produced outputs, default executor class, required evidence, capability ceiling, approval rule, invalidation behavior, and multiplicity. Several instances may exist simultaneously where appropriate; for example, many implementation tasks and verification checks can coexist.

The workflow remains opinionated. Do not build a general plugin system before real usage demonstrates which definitions need external configuration.

## Executor model

Executors perform work and return proposals/receipts. They never advance the graph directly.

Initial executor classes:

- **Human executor** — approval, clarification, consequential choice, scope authorization, delivery authorization.
- **Model executor** — reasoning and structured artifact proposals without workspace mutation.
- **Workspace executor** — model-guided repository work under an explicit capability grant.
- **Verifier executor** — read/execute-only evidence collection and criterion evaluation.
- **System executor** — deterministic rendering, projection, invalidation, and other pure/controlled transformations.

The central boundary should resemble:

```rust
pub struct AttemptSpec {
    pub work: WorkItemId,
    pub objective: Objective,
    pub inputs: Vec<ArtifactVersionRef>,
    pub grant: CapabilityGrant,
    pub budget: Budget,
    pub model_route: Option<ModelRoute>,
    pub expected_revision: ProjectRevision,
}

pub struct AttemptReceipt {
    pub attempt: AttemptId,
    pub outcome: AttemptOutcome,
    pub outputs: Vec<OutputProposal>,
    pub evidence: Vec<EvidenceProposal>,
    pub graph_changes: Vec<GraphChangeProposal>,
    pub change_set: Option<ChangeSetRef>,
    pub usage: Usage,
}
```

These are illustrative contracts, not mandatory field-for-field specifications. Design them from the scenarios and keep them immutable at executor boundaries.

## Persistence model

Retain the useful Codex-style hybrid principle without copying the old event catalog:

- **Canonical JSONL facts:** accepted domain facts, attempt lifecycle facts, decision resolutions, artifact publications, accepted receipts, cancellations, supersessions, and invalidations.
- **SQLite projections:** graph adjacency, readiness/blocker queries, work and attempt indexes, artifact/evidence lookup, usage summaries, search, and UI projections.
- **Artifact/evidence store:** structured artifact bodies, deterministic Markdown renderings, patch/change-set references, and retained verification evidence.
- **Bounded operational logs:** stream deltas, detailed process output, transport diagnostics, and other data not required to reconstruct domain truth.

Reuse the existing Rust store's segmentation, atomic append, writer locking, conservative tail recovery, and rebuildable projection work only after separating it from legacy event payloads. Reassess per-event hash chaining: retain checksums and crash recovery, but require a distinct product/security justification before making a hash chain part of the new permanent domain format.

Write facts before projections. Never hold the writer lock across model calls or long-running tools. Treat attempts that were started but have no accepted terminal receipt after a crash as recoverable/abandoned work requiring deterministic reconciliation.

## Artifact model

Artifacts have three separable representations:

1. structured typed content, which carries authoritative meaning;
2. deterministic Markdown grouped views, which people review and edit through controlled import;
3. optional referenced blobs such as change sets, logs, or evidence attachments.

Requirements, acceptance criteria, technical decisions, plan tasks, and evidence must have stable addressable IDs and versions even when displayed together as PRD, TRD, plan, or verification-report documents.

A Markdown edit does not silently change graph meaning. Importing edited Markdown creates a validated new structured version and an explicit change proposal. Preserve familiar readable documents, but do not use whole-document staleness when precise dependency invalidation is available.

## Scheduling and concurrency

- Compute ready work deterministically from the graph.
- Autonomy is scheduling policy (`manual`, `safe`, `until-decision`, or a bounded equivalent), not mode advancement.
- Parallelize read-only and model-only attempts when dependencies allow.
- Serialize all workspace-changing attempts in the first release.
- Represent declared read/write scopes now so later isolated worktree execution can be added without redesign.
- Refuse overlapping active write grants.
- Do not implement automatic merge orchestration in the first release.
- Prioritize work using explicit policy and explain the recommendation; do not hide scheduling choices in queue order.

## User experience

The primary UI should answer:

- What outcome are we pursuing?
- What work is ready now?
- What is active?
- What is blocked and why?
- What decisions need me?
- What changed or became invalid?
- What evidence supports completion?
- What does Relay recommend next?

Replace mode-centric interaction gradually with work-centric commands/views such as project summary, ready work, blockers, decisions, work inspection, artifact inspection, evidence, run/schedule, cancel, and explain. Familiar phase names may remain as grouped filters and onboarding language.

Legacy CLI commands should receive intentional migration behavior: map them to an equivalent work-graph operation when semantically sound, otherwise return a clear migration message. Do not maintain misleading commands merely for byte compatibility.

## Affected files and ownership

### Lead integrator only

- `CLAUDE.md` — replace compatibility-port instructions with the intent-first migration direction and phase-specific checks.
- `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml` — workspace membership and shared dependency decisions.
- `.github/workflows/ci.yml` — phase gates and platform matrix.
- `README.md`, `docs/architecture.md`, `docs/configuration.md` — update only as implemented behavior stabilizes.
- `docs/event-format.md` — revise or supersede only after the new fact model is accepted.
- `docs/plans/rewrite-execution-log.md` — close the stopped effort and start a distinct execution record without erasing history.

### Domain/kernel owner

- New `crates/relay-domain/**` — IDs, goals, work items, dependencies, artifacts, decisions, evidence, attempts, grants, receipts, facts, validation.
- New `crates/relay-engine/**` — graph evaluation, readiness, blockers, invalidation, acceptance, expansion, scheduling recommendations.
- New `crates/relay-workflows/**` — built-in software-delivery definitions and scenario builders.
- `crates/relay-core/**` — reference/salvage only until an explicit retirement decision; do not mutate it concurrently with the new domain contracts.
- New intent-first scenario and property tests.

### Storage/migration owner

- `crates/relay-store/**` — isolate generic log/lock/recovery primitives, then add new fact/projection adapters without overwriting unfinished work.
- New SQLite graph projection migrations and rebuild tests.
- New semantic migration from legacy projects.
- `crates/relay-artifacts/**` — coordinate generic publication/digest utilities; domain artifact schemas remain owned by the domain owner.

### Runtime/security owner

- New `crates/relay-executor/**` — executor contracts, attempt orchestration, capability leases, reconciliation.
- `crates/relay-providers/**` — salvage provider transport/normalization behind new executor-facing contracts.
- `crates/relay-tools/**` — preserve hardened path/process/redaction/tool primitives; adapt authorization to `CapabilityGrant`.
- `crates/relay-runtime/**` — do not complete the old controller port; retain as reference, then retire or repurpose explicitly.
- `crates/relay-config/**` — preserve provider/model configuration; add scheduling/workflow configuration only after domain contracts settle.

### Interface owner

- `crates/relay-ui/**` — work-centric projections and Ratatui views.
- `crates/relay-cli/**` — work-centric command surface and explicit legacy command mapping.
- UI snapshot and CLI integration tests.

### TypeScript reference

- `src/**` and `test/**` remain read-only product evidence except for a separately coordinated semantic export tool or critical fix.
- Existing compatibility fixtures remain historical evidence; they are not the acceptance oracle for the new internal architecture.

## Team structure and coordination

Spawn a team with non-overlapping ownership:

1. **Lead integrator/architect** — owns shared root files, architecture decisions, phase gates, integration, and reviews every cross-crate contract.
2. **Domain and workflow engineer** — exclusively owns `relay-domain`, `relay-engine`, `relay-workflows`, fact schemas, scenario tests, and graph invariants.
3. **Storage and migration engineer** — exclusively owns generic store salvage, new fact persistence, projections, artifact publication integration, and semantic legacy migration.
4. **Runtime and security engineer** — exclusively owns executor orchestration, capabilities, providers, tools, attempts, cancellation, and scheduling execution.
5. **CLI and UI engineer** — exclusively owns work-centric views, commands, TUI, JSON/human presentation, and interface migration.

With limited concurrency, keep the lead active and run the other roles in waves. The domain owner freezes each required contract before dependent agents implement against it. Other agents propose changes to shared contracts through the owning agent instead of editing them directly. The lead must inspect all diffs, integration assumptions, and tests before accepting work.

## Phased implementation

### Phase 0: stop, inventory, and establish the new authority

1. Stop agents and tasks executing the previous compatibility plan.
2. Read this plan, `CLAUDE.md`, both earlier Rust plans, the execution log, and current diffs.
3. Inventory unfinished Rust work by crate and classify it using the four salvage categories above.
4. Record reusable primitives, obsolete coupling, unfinished risks, and test coverage without deleting anything.
5. Update `CLAUDE.md` so this plan is authoritative and the previous plans are historical references.
6. Create a new execution log for this effort; close but retain the old log.
7. Freeze the TypeScript application and old Rust port as reference surfaces.
8. Agree on naming for the new domain/engine/workflow crates and prevent circular dependencies.

Exit criteria:

- No old-plan implementation remains active.
- User changes are intact and classified.
- Shared ownership is recorded.
- Repository instructions identify the intent-first plan as authoritative.
- The existing TypeScript and Rust checks are run and baseline failures are documented, not casually fixed.

### Phase 1: executable domain prototype

1. Implement only the pure domain and engine needed for the first four architecture scenarios: discovery plus approval, precise requirement invalidation, parallel read readiness, and verification-driven corrective work.
2. Define addressable IDs/version references, work items, dependencies, decisions, evidence, attempts, grants, receipts, and the minimal durable fact set.
3. Implement deterministic graph evaluation returning ready work, blockers with reasons, awaiting-human decisions, invalidations, and project-completion status.
4. Implement cycle detection, exact-version dependency tracking, receipt revision matching, and proposal validation.
5. Model human decisions as graph work; do not introduce global mode or pause state.
6. Add scenario tests and property tests for order-independent projections, cycle rejection, targeted invalidation, and deterministic explanations.

Exit criteria:

- The four scenarios read naturally in tests without mode/back-jump/controller special cases.
- The kernel has no IO, Tokio, SQL, HTTP, terminal, provider, or filesystem dependency.
- The graph API and durable facts are reviewed before dependent implementation begins.

### Phase 2: built-in software-delivery workflow

1. Define built-in work kinds for discovery through delivery using the accepted kernel.
2. Define structured, addressable requirement, criterion, decision, task, and evidence schemas.
3. Create deterministic grouped Markdown views for opportunity brief, PRD, TRD, plan, decision log, verification report, and delivery report.
4. Define completion/evidence rules, capability ceilings, approval rules, multiplicity, and invalidation semantics per work kind.
5. Implement controlled graph expansion proposals and validation for scope, duplication, cycles, and authority.
6. Implement all twelve product scenarios in memory using deterministic fake executors.

Exit criteria:

- All product scenarios pass without a global workflow mode.
- Changing one requirement invalidates only dependent nodes and views.
- The familiar seven phases remain understandable as grouped projections.
- No public workflow DSL or plugin system has been introduced.

### Phase 3: persistence and artifact integration

1. Extract reusable segmented-log, atomic-append, lock, crash-recovery, and projection-rebuild primitives from `relay-store` behind generic interfaces.
2. Specify the new fact envelope and retention boundaries. Decide separately whether hash chaining is justified.
3. Persist accepted facts and receipts; rebuild graph projections in SQLite.
4. Store typed artifact content and deterministic Markdown renderings with version/digest references.
5. Keep operational stream/process data bounded and outside authoritative domain facts unless required as retained evidence.
6. Add crash, tail-recovery, projection-deletion/rebuild, stale-attempt, and orphan-artifact tests.

Exit criteria:

- JSONL facts alone reconstruct the project graph.
- SQLite can be deleted and rebuilt with equivalent work/blocker/decision views.
- A reported-success fact survives crash injection.
- Incomplete attempts reconcile without false completion.

### Phase 4: semantic migration

1. Build a read-only importer that replays legacy state and extracts goals, approvals, artifact versions, requirements, criteria, decisions, tasks, evidence, and delivery status into new domain proposals.
2. Preserve existing Markdown bytes as imported views/attachments while validating or deriving typed content.
3. Produce a migration report listing exact mappings, inferred relationships, unmapped legacy events, and decisions requiring user review.
4. Stage migration into a new directory, validate the new graph, and retain the original project and backup.
5. Do not fabricate precise dependencies when legacy data only supports coarse relationships; mark them as imported/coarse and surface the limitation.

Exit criteria:

- Migration never modifies source state.
- Product-meaningful data is preserved or explicitly reported as unmapped.
- The migrated graph is explainable and rebuildable.
- Rollback is tested.

### Phase 5: capabilities and executors

1. Implement immutable attempt specifications and receipts.
2. Implement capability grants covering workspace paths, operations, commands, environment, model route, budgets, and delivery authority.
3. Adapt existing provider, path guard, process supervisor, redaction, patch, Git-read, and tool work behind the new executor boundary.
4. Implement human, model, workspace, verifier, and system executors.
5. Implement expected-revision acceptance, abandoned-attempt reconciliation, cancellation, retries, budget enforcement, and bounded evidence capture.
6. Serialize workspace writes; allow safe parallel read/model attempts.
7. Reject overlapping write grants and unauthorized graph expansion.

Exit criteria:

- Executors cannot mutate graph state directly.
- Every accepted outcome has a matching receipt and grant.
- Security tests cover traversal, symlinks, patch races, secrets, process trees, environment leakage, cancellation, and write overlap.
- Provider changes do not affect engine semantics.

### Phase 6: work-centric application and interface

1. Implement an application service that submits intents, queries projections, schedules ready work, accepts human decisions, and explains policy results.
2. Do not introduce a monolithic controller; compose small command handlers over engine/store/executor ports.
3. Implement summary, ready, blocked, decisions, work inspect, artifact inspect, evidence, run/schedule, cancel, and explain views.
4. Implement Ratatui screens over immutable work-centric view models.
5. Map legacy commands where semantics are honest; provide clear guidance where they are not.
6. Preserve non-interactive and JSON operation, but define new stable schemas around the work graph rather than copying legacy internals.

Exit criteria:

- Users can understand what is ready, blocked, awaiting them, invalid, and recommended.
- All twelve scenarios work through the application boundary with fake providers/tools.
- CLI and TUI contain no domain decision logic.
- Terminal restoration and non-interactive operation pass integration tests.

### Phase 7: preview, evaluate, and retire obsolete paths

1. Ship the intent-first Rust app as an explicit preview alongside existing Relay.
2. Exercise real project copies, semantic migration, crash recovery, provider switching, dirty worktrees, verification correction, and delivery hold.
3. Evaluate whether the graph model improves explanations, targeted invalidation, concurrency, and extensibility in practice.
4. Resolve architectural defects in the kernel rather than adding controller/mode exceptions.
5. After user approval, retire incompatible parts of the stopped port and eventually the TypeScript runtime in separate, reviewable changes.
6. Update root documentation and `CLAUDE.md` to reflect only behavior that has reached the relevant release gate.

Exit criteria:

- No unexplained loss of product meaning during migration.
- Product scenarios and safety invariants hold in preview usage.
- Obsolete code retirement is separately reviewed and recoverable.
- Rust becomes the default only after explicit approval.

## Engineering constraints

- Preserve all existing user changes; never reset, discard, overwrite, or broadly reformat them.
- Do not commit or push unless explicitly requested.
- Preserve product behavior and safety guarantees, not accidental internal architecture.
- Do not use legacy byte-for-byte compatibility as the new kernel's design criterion.
- Do not add hidden modes, global pause state, backward stage jumps, or a monolithic workflow controller.
- Keep the domain and engine pure and deterministic.
- Models and executors propose; deterministic policy accepts or rejects.
- Persist accepted facts before projections and user-visible success.
- Keep SQLite disposable and rebuildable.
- Keep artifacts human-readable while structured content remains authoritative.
- Validate every model-produced structure, graph proposal, executor receipt, and capability request.
- Never hold storage locks across provider calls or long-running tools.
- Use argument-array process execution and retain all current workspace, redaction, environment, patch, Git, and delivery safeguards.
- Serialize workspace writes initially; do not implement automatic merging.
- Keep tests offline by default and use deterministic fake executors/providers.
- Avoid speculative abstractions: no public workflow DSL, distributed scheduler, or plugin system in the first release.
- Keep modules small and shared contracts singly owned.
- Treat old plans, fixtures, and crates as evidence; do not delete them until replacements pass and retirement is approved.

## Acceptance criteria

1. Relay's authoritative domain is a work graph, not a global seven-mode state machine.
2. The engine deterministically derives ready work, blockers, human decisions, invalidations, and completion.
3. Requirements, criteria, decisions, tasks, artifacts, and evidence have stable addressable identities and versions.
4. Requirement changes invalidate only actual dependents.
5. Human intervention is represented as first-class decision work with evidence and consequences.
6. Executors operate only through immutable attempt specs and capability grants and return receipts/proposals.
7. Models cannot directly install graph changes, accept completion, expand scope, or authorize delivery.
8. Read/model work may run concurrently; workspace writes are serialized and overlapping write grants are rejected.
9. The built-in software-delivery workflow supports all twelve defining scenarios without mode/controller special cases.
10. PRD, TRD, plan, verification, and delivery documents remain readable deterministic grouped views.
11. Canonical facts reconstruct graph state; SQLite projections are disposable.
12. Crash recovery does not falsely complete attempts or lose acknowledged facts.
13. Semantic migration preserves legacy product meaning, reports inference/unmapped data, retains source state, and supports rollback.
14. Provider substitution does not affect engine or policy semantics.
15. Existing security boundaries continue to pass cross-platform tests.
16. CLI/TUI explain readiness, blockers, decisions, invalidation, evidence, and recommendations without owning policy.
17. The old port and TypeScript runtime are not removed until replacement, migration, preview, and explicit retirement gates pass.
18. No implementation work is committed or pushed without user authorization.

## Verification commands

Run baseline checks before changes and after integrations while the old implementations remain present:

```bash
git status --short
git diff --check
npm run typecheck
npm test
npm run build
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo build --workspace --all-targets --all-features
```

Add and run focused checks as the new crates appear:

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

The scenario suite must name and cover all twelve product scenarios in this plan. Property tests must cover at least dependency-cycle rejection, deterministic evaluation, targeted invalidation, exact-version references, receipt revision matching, duplicate proposal rejection, and non-overlapping scheduling.

Run smoke tests only against disposable temporary workspaces and copies of legacy projects:

```bash
cargo run -p relay-cli -- --help
cargo run -p relay-cli -- --version
cargo run -p relay-cli -- project summary --workspace <temporary-workspace>
cargo run -p relay-cli -- work ready --workspace <temporary-workspace>
cargo run -p relay-cli -- decisions list --workspace <temporary-workspace>
cargo run -p relay-cli -- explain <work-id> --workspace <temporary-workspace>
cargo run -p relay-cli -- migrate --workspace <legacy-project-copy>
```

Before reporting completion:

```bash
git status --short
git diff --check
git diff --stat
```

Review all diffs for preserved user work, secrets, accidental legacy deletions, old-controller leakage into the new engine, unowned shared-file changes, generated build output, and unrelated formatting.

## Completion report

Report:

1. completed phases and product scenarios;
2. salvage decisions for each pre-existing Rust crate;
3. files/crates changed by ownership area;
4. kernel and workflow decisions, including any rejected abstractions;
5. migration, persistence, concurrency, durability, and security evidence;
6. every verification command and its result;
7. unresolved issues, deferred decisions, and any legacy behavior intentionally not preserved;
8. confirmation that user changes remain intact and nothing was committed or pushed.
