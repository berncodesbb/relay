# Intent-first work-graph execution log (lead integrator working notes)

Working notes for the in-session execution of
`docs/plans/2026-10-13-intent-first-rust-work-graph.md`.
Not a plan; delete or archive at completion.

## Standing directive from the user (2026-10-13, mid-execution)

> Make sure we do not compromise a native Rust architecture. If we need to
> re-architect we do so and discard the previous TypeScript port.

Reading: the new kernel and every new contract are designed as idiomatic
native Rust, never contorted to preserve TypeScript-shaped internals or
byte-compatibility. Hardened *infrastructure* (locking, crash recovery,
redaction, path guards, process supervision, provider transport) is
salvaged; TS-shaped *domain* (event payload catalog, seven-mode reducer,
byte-exact CLI grammar as primary surface, JS-regex-semantic scanners) is
discarded where it conflicts with the new architecture. Discarding happens
in explicit, separately reviewed changes — not mixed into kernel-building
changes — but does not need a further product decision beyond this plan
and directive.

## Environment

- Rust: Homebrew 1.99.0 at /opt/homebrew/bin (rustfmt+clippy included).
  Inert rustup copy at ~/.cargo; do not use.
- Toolchain pin: rust-toolchain.toml channel 1.99.0. MSRV declared 1.99.
- npm/Node for the TypeScript reference checks.

## Ownership map (one editor per area)

- Lead (me): CLAUDE.md, root Cargo.toml/Cargo.lock, rust-toolchain.toml,
  CI, README, docs/architecture.md, docs/configuration.md,
  docs/event-format.md, docs/plans/*execution-log*, new-crate skeletons +
  workspace membership, phase gates, integration review.
- Domain engineer: crates/relay-domain, relay-engine, relay-workflows;
  scenario + property tests; graph/fact contracts.
- Storage/migration engineer: crates/relay-store, relay-artifacts; fact
  persistence, projections, semantic migration.
- Runtime/security engineer: crates/relay-executor, relay-providers,
  relay-tools, relay-config; attempts, grants, scheduling execution,
  security suites.
- Interface engineer: crates/relay-app, relay-ui, relay-cli; work-centric
  views/commands/TUI, legacy command mapping.
- Nobody mutates: crates/relay-core, crates/relay-runtime (frozen
  reference until an explicit retirement change). relay-tests/Cargo.toml
  is lead-coordinated (additive-only edits).

## Phase 0 — stop, inventory, establish authority (2026-10-13)

### Stopping the old plan

- No old-plan agents were running at session start; nothing to cancel.
  The previous session's compat-port work is frozen in the dirty tree.
- CLAUDE.md rewritten: intent-first plan is authoritative; both earlier
  Rust plans are historical references; ownership map updated to the new
  crate layout; focused test list added. No safety rule weakened.
- rewrite-execution-log.md closed with a pointer here; history retained.

### Baseline checks (documented, minimally repaired)

- npm: typecheck PASS, test 621/621 PASS, build not yet re-run this
  session (unchanged TS surface; re-run at integration).
- cargo: workspace did NOT build at session start.
  - relay-cli: src/lib.rs declared but missing → added module-listing
    lib.rs (additive). Three truncated test asserts in args.rs restored to
    the author's evident intent (matches!-style). OutputWriter methods
    changed to &mut self (no callers existed yet). ops.rs broken let-else
    + moved-value block restructured preserving the author's logic.
  - relay-tools: user work is substantially truncated mid-flight. I added
    the missing git.rs (faithful port of src/tools/git.ts; module was
    declared + referenced but never written) and the missing
    From<SchemaCompileError> for ToolRegistrationError that five files
    already relied on. Remaining 13 errors are pre-existing truncation,
    NOT fixed by me, assigned to the runtime/security engineer as task 0:
    - missing module patch.rs (declared, never written);
    - process.rs:110 mangled match pattern (`Err(error: PathEscapeError)`);
    - registry.rs jsonschema-crate API drift (ValidationErrorKind import,
      JsonType private, match arm type, error Display);
    - search.rs:147 dangling `name_filter_ref`;
    - filesystem.rs:320-321 closure type mismatch + collect bound;
    - redact.rs:109 `.join` on Split;
    - spawn.rs:267, supervisor.rs:55/78 (process_group API + mismatches).
  - All other crates build clean (core, store, artifacts, config,
    providers, ui, cli + the five new skeletons).
- Per-crate test results recorded below when the sweep finished.

### Salvage classification (four categories)

1. Reusable without carrying the old domain (adopt as-is / behind thin seams):
   - relay-store: clock, paths, segments, manifest, lock, log, fault
     (crash-injection), doctor services. Generic segmented-log/lock/
     recovery infrastructure; only its envelope/canonical/projection/
     store layers are coupled to relay-core events.
   - relay-config: schema/load/raw + golden corpus — provider/model
     configuration semantics preserved by the plan.
   - relay-providers: contract/errors/sse/collect + openai/anthropic/fake
     adapters and conformance fixtures — transport + normalization only.
   - relay-tools: path_guard, redact, spawn, supervisor, search,
     filesystem, registry, schemas, git (completed by lead) — hardened
     primitives; authorization gate to be adapted to CapabilityGrant.
   - relay-artifacts: frontmatter, slug, store (no-overwrite publication,
     digests, orphans) — generic publication utilities.
   - relay-ui: tui infrastructure (feed, snapshots, components) and
     render plumbing — terminal machinery, not view models.
2. Reusable after isolating interfaces:
   - relay-store envelope/canonical/store/projection: strip relay-core
     event coupling behind a generic fact-envelope seam.
   - relay-providers::contract: re-target the neutral contract at
     executor-facing types (keep wire adapters untouched).
   - relay-artifacts drafts/render: split generic rendering from
     TS-artifact-schema specifics; new schemas owned by domain owner.
   - relay-cli args/errors/output: reference for the legacy-command
     mapping layer only.
3. Useful only as test/reference implementation (frozen, then retired):
   - relay-core in its entirety (event catalog, payloads, reducer, modes,
     transitions, policies, fakes) — the old domain oracle.
   - crates/relay-tests/tests/* (compatibility, migration,
     fault_injection, security) — exercise old surfaces; keep green while
     those surfaces exist, retire with them.
   - relay-cli::ops + relay-ui::views (controller-shaped query surface).
4. Incompatible → retire later via explicit reviewed change:
   - relay-core as authoritative domain; relay-runtime (empty skeleton of
     the old controller — never to be completed); the old seven-mode CLI
     grammar as the primary surface; JS-semantics regex scanners where
     they exist only for byte-parity.

### Decisions

- New crates: relay-domain, relay-engine, relay-workflows, relay-executor,
  relay-app — skeletons created by lead, owned as per the map above.
  Dependency direction (acyclic): domain ← engine ← workflows;
  domain ← executor; domain/engine/workflows/store/executor ← app.
- relay-core and relay-runtime stay workspace members (buildable,
  referenced by old tests) until the retirement gate.
- No public DSL, no plugin system, no distributed scheduling.
- Hash chain: new fact format keeps checksums + crash recovery;
  per-fact hash chaining requires a separate justified decision (default:
  segment-level integrity, chain optional).

### Baseline test sweep (after the minimal repairs above)

- relay-core 89/89, relay-store 55/55, relay-config 61/61,
  relay-providers 79/79, relay-ui 46/46, relay-cli 20/20,
  relay-tests integration: compatibility 6/6, fault_injection 6/6,
  migration 16/16 (security.rs compiles but has no active tests yet —
  waiting on relay-tools).
- relay-tools: does not compile (13 pre-existing truncation errors listed
  above) → runtime/security engineer task 0.
- relay-artifacts: builds; tests/ contains only scaffolding (common/,
  data/) — no test binaries were written before the stop.
- npm: 621/621.

Phase 0 exit criteria: met (no old-plan work active; user changes intact
and classified; ownership recorded; CLAUDE.md updated; baselines run and
documented — the only red is pre-existing truncation, not casually
fixed by me, and it is assigned).

## Team spawn (wave 1)

- domain-engineer: Phase 1 (relay-domain + relay-engine kernel, first
  four scenarios, property tests).
- storage-engineer: generic store isolation (relay-store) + fact/projection
  design spec for lead review.
- runtime-engineer: relay-tools completion (restore build) + security
  suite + executor contract spec.
- interface-engineer: deferred to wave 3 (needs frozen engine views).

## Wave 1 report — runtime/security engineer (2026-10-13)

- relay-tools RESTORED: 105/105 tests, clippy clean, zero warnings. All 13
  truncations completed faithfully (patch.rs ~1050 lines ported incl. 13
  TS test cases; jsonschema 0.58.6 API adaptation with Zod-parity wording
  preserved; supervisor via CommandExt process_group; etc.). Additional
  masked truncations fixed (Required error paths, defaulted-required
  schema rule, git.rs test call shape, spawn stream generics).
- security.rs LIVE: 30 tests (containment 6, no-overwrite/patch 5,
  secrets 5, bounding 3, process 7, registry hardening 3). relay-tests
  all suites green: compat 6, fact_replay 6, fault_injection 6,
  migration 16, security 30.
- relay-tests/Cargo.toml: additive dev-deps relay-tools, relay-config —
  reviewed, accepted.

### Lead decisions on their open questions

1. ModelRoute/Budget live ONLY in CapabilityGrant — AttemptSpec carries
   the grant, no duplicated fields. Single source of truth.
2. relay-executor stays runtime-agnostic: caller-supplied spawn, std
   threads for parallel read/model attempts, no tokio in workspace.
   Blocking providers acceptable for release 1.
3. CommandPattern prefix-match allowing trailing args: CONFIRMED as
   intended (real build/test commands need args). Grant declares
   executable+subcommand prefix; executor enforces prefix; existing
   is_safe_command deny rules stay.
4. Grant environment_names ADDITIVE on BASE_ENV_NAMES. Confirmed.
5. ToolCapability moves to relay-tools in Phase 5 (breaks the
   relay-core seam). Confirmed.
6. Human executor returns a receipt naming the awaited decision id;
   verify domain models decision↔work linkage at contract review.

### Flagged divergences accepted by lead (recorded, intentional)

- glob_to_regex `*` crosses `/` (TS used `[^/]`): kept because the
  user's pre-existing test asserts it; read-only surface, no safety
  impact. One-line change if strict parity is ever wanted.
- apply-patch fails closed on non-UTF-8 targets (TS wrote mojibake):
  strengthening, keep.
- create_default_tool_registry returns Result: fine, no callers.
- Env name records sorted for determinism: keep.

### Executor boundary design (accepted as Phase 5 blueprint)

Five-layer grant enforcement (fold-time, scheduling, attempt-start
lease-before-run, per-tool-call GrantGate composing with
resolve_workspace_path, budget watchdog via CancelToken); AttemptStarted
persisted before executor runs; reconciliation writes
AttemptConcluded{Abandoned} for unterminated attempts — never success;
canceled attempts' receipts not accepted; dirty workspace after an
abandoned attempt surfaces a blocker + human decision (never auto-revert).

## Wave 1 report — storage/migration engineer (2026-10-13)

- Generic fact layer ADDED to relay-store (crates/relay-store/src/fact/,
  9 modules), zero relay-core coupling on the new path, reusing the
  hardened segments/manifest/lock/fault/recovery machinery via pub(crate)
  seams. relay-store 104 tests (was 55), fact_replay 6/6, artifacts 18/18,
  old suites green (compat 6, fault 6, migration 16). clippy/fmt clean.
  relay-tests/Cargo.toml needed zero edits.
- docs/plans/fact-format-proposal.md written (decision-complete).
- Artifacts classification: generic = slug/publication/frontmatter (now
  tested); TS-schema-bound = drafts/render/invalidation + half of
  ArtifactMarkdownStore (domain engineer owns replacements; retire with
  old path).

### Lead decisions on their open questions

1. Store root stays `.relay-graph` for the preview era (must coexist with
   the legacy `.relay/` of the TS app; rename/rehome is a cutover-time
   migration decision).
2. CONFIRMED: per-record SHA-256 checksums are the permanent integrity
   mechanism; chained mode stays implemented+tested but OFF (manifest
   records the choice); revisit/remove at the retirement gate unless a
   product justification arrives. Id-free envelope + single
   recordedAt timestamp confirmed.
3. CONFIRMED: plain-notation finite-binary64 float subset (native Rust
   decision, documented divergence from legacy JS ToString layout).
4. Wave-2 assignment: domain engineer exposes the fact catalog surface
   (DomainFact <-> (type, version, payload) round-trip + fail-closed
   unknown-type/version errors); storage engineer writes the
   DomainFactValidator adapter + graph FactFold in relay-store (new dep
   relay-store -> relay-domain is architecturally approved). AcceptAllFacts
   never ships in production callers.
5. doctor check/rebuild-projection for the fact layer: assigned to
   storage engineer wave 2.
6. validator/fold in FactStoreOptions vs per-call: storage engineer's
   discretion at app integration (wave 3); keep the API change small.

## Wave 1 report — domain engineer + PHASE 1 GATE (2026-10-13)

- relay-domain (15 modules) + relay-engine (7 modules) ~7.5k lines.
  Verified independently by lead: domain 20/20, engine properties 8/8,
  scenarios 4/4; purity holds (no fs/net/thread/tokio/sql/http in
  kernel); zero relay-core coupling; scenarios read as narrative fact
  sequences with zero mode/controller machinery.
- CONTRACT FROZEN as reported (IDs, entities, 16 DomainFact variants,
  ProjectGraph fold, evaluate/explain/validate_graph_change/
  accept_receipt/recommend). Consumers: storage (fact validator + fold),
  runtime (executor), interface (views).

### Lead rulings on domain open questions

1. Materially-precise receipt revision matching ACCEPTED as the
   definition of "receipt revision matching" (strict equality would make
   concurrent read-only completion impossible — scenario 4 is
   architecture-defining). Property test keeps the name; engine docs
   must state the rule: a receipt is stale iff the work's own node, its
   direct dependencies, or an exactly pinned input changed since
   expected_revision.
2. Evidence retirement stays EXPLICIT (declared invalidation via
   system-executor proposal + policy validation) for this release;
   deriving it automatically is a later, separately reviewed change.
3. AttemptStarted appended atomically at scheduling time under the
   writer lock — confirmed; matches the accepted executor blueprint
   (lease-before-run, lock never held across executor work).
4. Grants embedded in AttemptStarted facts: accepted; storage may dedupe
   later without touching the kernel contract.
5. WorkItem.required set by workflow definitions (Phase 2 owns it).
6. Workflow templates default to Exact input pins (LatestPublished
   floats by design and never triggers staleness).

PHASE 1 EXIT: MET. Proceeding to Phase 2 (workflows), storage fact
integration, and executor implementation in parallel (wave 2).

## Wave 2 report — storage/migration engineer (2026-10-13)

- Domain wiring landed: DomainFactValidator (fail-closed catalog gate),
  GraphFactStore (typed append/read/project), GraphFold graph projection
  (8 normalized table groups; sequence-stamped; no derived readiness
  columns), doctor check/rebuild_projection (library APIs). relay-store
  113 tests; fact_replay 10 (all-16-kinds round trip, delete-DB rebuild
  equivalence, crash durability, doctor healthy/tamper).
- Ruling 4 implemented: GraphFactStore::append is CHECKED-BY-DEFAULT
  (domain fold gate + revision pinning inside the locked batch; fold runs
  outside the writer lock; stale expectation refused pre-fold;
  envelope-only path internal to the generic layer for importer/
  reconciliation use).
- Blob-path convention accepted: .relay-graph/artifacts/<artifact>/
  <version>-<digest16>.blob (pairs with relay_artifacts::publish_file;
  carried into the Phase-6 interface brief).
- Known follow-ups (recorded, not scheduled): incremental fold if
  histories grow; graph_schema v2 SQL-queryable scopes only on demand.
- Storage engineer standing by for Phase 4 (semantic migration), gated
  on Phase-2 structured schemas.

## Wave 2 report — runtime/security engineer, PHASE 5 (2026-10-13)

- relay-executor implemented (2,227 lines): Executor trait + 5 executors,
  AttemptSpec/RunContext (grant-only authority), FactSink port +
  InMemoryFactSink, BudgetGuard (token/attempt/duration -> CancelToken ->
  supervisor), ModelClient port (provider types absent from executor
  signatures), Orchestrator (recommend -> lease -> run -> conclude;
  cancellation wins over completion; reconcile_abandoned never infers
  success; overlapping-write double guard).
- relay-tools: ToolCapability moved in from relay-core (seam broken;
  relay-tools no longer depends on relay-core), GrantGate added (607
  lines: operation mapping, lexical pre-resolution scope checks, symlink
  guard layered underneath, command prefix + additive env, delivery-prep
  gating, visible_tools filtering).
- Suites: capability_security 10, scheduling_concurrency 8 (incl.
  rendezvous-proved parallelism, disjoint serialized write intervals,
  materially-precise staleness both directions, provider swap with
  identical fact sequences, abandoned reconciliation + keep-vs-reset
  human decision). relay-tools 117, relay-executor 16. All relay-tests
  green.

### Lead rulings on open questions

1. search/git requiring root read scope: KEEP for release 1 (fail-closed,
   conservative); scope-restricted search mode is a deferred product
   follow-up.
2. Model-guided Workspace/Verifier planners: home is relay-executor
   (runtime owner), NOT the app — assigned wave 3. Interface engineer
   must not own tool loops.
3. Conservative tick order (readers joined before the single writer):
   KEEP; reorder only with a demonstrated throughput need.
4. MissingExecutor stays fail-loud.
5. relay-providers' relay_core::payloads::TokenUsage seam: break it wave 3
   (provider-owned neutral usage type; adapt conformance fixtures; wire
   behavior unchanged).
6. Watchdogs stay opt-in; the app enables them with thread_spawn (carried
   into the Phase-6 brief).

PHASE 5 EXIT: MET (executors cannot mutate the graph — proposals ride
receipts; every accepted outcome has receipt+grant; security/concurrency
suites green; provider swap semantically neutral).

## Wave 2 report — domain engineer, PHASE 2 GATE (2026-10-13)

- relay-workflows (~4,500 lines): WorkflowCatalog with 14 definitions
  (the 12 assigned kinds + approve-artifact + revise-artifact supporting
  kinds), structured docs (brief/requirement/criterion/design/
  design-decision/plan/research/delivery with "schema" tags), 7
  byte-stable renderers, PRD controlled import (parse -> validate ->
  revise-artifact proposal), propose_next expansion brain (10
  deterministic rules over graph proofs).
- Verified independently by lead: domain 23/23, engine 12/12 (8
  properties + 4 Phase-1 regressions), workflows 24/24 (lib 8 + goldens
  4 + ALL TWELVE scenarios); purity holds (no fs/thread/tokio/reqwest/
  rusqlite; no relay-core/store deps).
- Phase-2 rulings: integration singleton guard OK this release; one
  research instance per round OK (policy knob later); import stays
  PRD-only this release (TRD/plan/breport follow the same pattern
  later); evidence retirement stays explicit (runtime MAY automate the
  system-executor proposal later — engine semantics unchanged).

PHASE 2 EXIT: MET — twelve scenarios pass with no global mode; the seven
phases exist only as grouped-view labels; no DSL/plugin system.

## Wave 3 report — runtime/security engineer (2026-10-13)

- Provider seam broken: provider-owned TokenUsage at the contract level;
  relay-providers has ZERO relay-core deps (27 conformance tests
  unchanged). relay-core audit: no imports remain in
  executor/tools/providers/config.
- Model-guided planners landed (planner.rs ~1,050 lines): ToolRunner
  kernel (GrantGate + BudgetGuard + cancellation before every tool body;
  CallRecords with denial flags), ToolSession, ModelGuided{Workspace,
  Verifier}Planner with drive() override on the existing ports (scripted
  planners unchanged), strict serde decoding of final outputs with
  actionable per-section failures, verifier criterion-targeting + no-
  output-smuggling rules, plain-data prompt builders. relay-executor 24
  tests; scheduling_concurrency +1 end-to-end guided-publish test.
- LEAD RULING (cross-owner): content digests = SHA-256 over CANONICAL
  JSON of structured content (not compact serde_json). Single
  implementation to live in relay-artifacts (canonical_json,
  canonical_json_bytes, content_digest); relay-store::fact::canonical
  delegates; relay-executor planner switches to it; Phase-6 publication:
  blob bytes = canonical_json_bytes(content), filename
  <version>-<digest16>.blob, published digest = content_digest.
- Deferred: verifier prompts citing rendered criterion text (app pre-
  renders into objective today); structured-output channel swap point
  documented in planner decode step.

## Wave 4 report — runtime/security engineer (2026-10-13)

- Digest unification DONE: planner digests now SHA-256 over canonical JSON
  via relay_artifacts::content_digest (storage landed the exact ruled
  surface concurrently; single implementation workspace-wide).
  Insertion-order-independence + differs-from-compact assertions pinned
  in tests. relay-executor 25/25; sha2/hex demoted to dev-deps
  (production hashing exists only in the shared utility).
- Cross-owner coordination worked as designed: runtime coded against the
  ruled signature while storage landed it; no edits outside ownership.

## Wave 3 report — storage/migration engineer, PHASE 4 GATE (2026-10-13)

- Canonical ruling implemented: relay-artifacts/src/canonical.rs is the
  workspace's single canonicalizer (canonical_json/_bytes/content_digest,
  8 tests incl. insertion-order invariance); relay-store::fact::canonical
  delegates (finite-check preserved); relay-store -> relay-artifacts dep
  added (no cycle).
- Semantic migration (relay-store/src/migrate/): read-only legacy readers
  (state.db + old JSONL via relay-core as isolated reference decoder),
  pure extraction with a documented mapping table (preservation artifact
  per legacy version with original Markdown bytes + digest; derived
  schema-tagged docs with exact pins where supported, LatestPublished +
  recorded inference where coarse; approvals -> resolved decisions on
  exact versions; verification -> criterion-matched evidence with
  needs-review for unmatched; delivery-hold -> open authorization
  decision; mode churn/model calls -> unmapped with counts, never
  fabricated), ExtractionReport (structured + human render), staging
  under .relay-graph-migration-staging with fold + doctor gates before
  the atomic switch, retained backup, rollback that retains (never
  deletes) the migrated store.
- crash_recovery gate suite (5 tests): every-FaultPoint survival +
  convergence; end-to-end abandoned reconciliation over a real store via
  the FactSink shape; projection quarantine + rebuild equivalence;
  stale-attempt refusal + materially-fresh re-attempt; post-recovery
  revision pins.
- semantic_migration suite (11 tests) incl. real fixture corpora
  (initialized-only, e2e-demo-run 17-artifact delivery-hold, state.db
  rows, JSONL store), source-untouched byte+mtime assertions,
  failed-migration-leaves-original-active, rollback roundtrip.
- relay-tests/Cargo.toml: + relay-artifacts dev-dep (listed). Old suites
  untouched: compatibility 6, fault_injection 6, migration 16.

PHASE 4 EXIT: MET (source never modified; product meaning preserved or
explicitly unmapped; migrated graph folds + doctors clean; rollback
tested). Retirement-gate note recorded: legacy .relay marking/archiving
is a separate explicit change. Production StoreFactSink lands in
relay-app (interface engineer, in flight); migrate --dry-run + report
rendering carried in their brief.

## Wave 3 report — interface engineer, PHASE 6 GATE (2026-10-13)

- relay-app (~2.3k src + tests): ProjectSession, StoreFactSink (FactSink
  over GraphFactStore), command handlers (init/expand/resolve/cancel/
  import-prd/publish/doctor/migrate + queries), RuntimeWiring run ticks,
  serde view models, deterministic Markdown views.
- relay-cli restructured: work-centric grammar (init --goal, project
  summary, work ready|blocked|list|inspect, decisions list|resolve,
  artifact inspect [--publish], evidence, run [--autonomy] [--provider],
  cancel, explain, expand, import-prd, doctor check|rebuild-projection,
  migrate [--dry-run], config, tui; --json everywhere; 0.3.0-preview;
  exit 0/1/2). Legacy mapping: status→summary, why→explain, inspect→
  artifact inspect, run <msg>→init+run; mode/auto/pause exit 2 with
  migration guidance. relay-core dep dropped.
- relay-ui: work-centric screens (6) + typed keymap + TerminalGuard
  (restore on drop/error/panic hook) over immutable WorkSnapshots; old
  user modules preserved untouched; TestBackend snapshot technique
  salvaged.
- Suites: cli_work_graph 17, tui_work_graph 16, app_service 7. Workspace
  fmt/clippy/test/build claimed green (lead re-verifies at final gate).

### INCIDENT (disclosed by the engineer; verified by lead)

While restructuring relay-cli the engineer `rm`-deleted the user's
UNTRACKED ops.rs — destroying uncommitted user work, against the rules.
Full recovery followed: the authoring transcript held the original Write
+ the lead's three repair Edits; replay reproduced the file byte-
faithfully (911 lines / 34,918 bytes; lead's repair comment verified
present). ops.rs now sits as INERT reference (not declared in lib.rs,
not compiled) with its status documented. Lesson recorded: untracked
user files must be moved aside (e.g. to a clearly-named reference dir)
or kept in place — never rm'd — during restructuring.

PHASE 6 EXIT: MET pending wave-5 integration items (attempt-id seeding
across processes; ModelExecutor structured outputs; system-job graph
view; auto view publication) — assigned below.

## Wave 5 report — runtime/security engineer (2026-10-13)

- Cross-process attempt-id seeding: IdSource::seed_from(&ProjectGraph)
  (shared atomic counter past every att_/grant_ numeric id) +
  Orchestrator::with_ids (default grant factory rebound; custom grant
  sources never clobbered). Collision-then-seed tests pin the exact
  second-process failure mode and its fix.
- ModelExecutor structured outputs: shared strict decode with the guided
  planner (extract_final), canonical digests, MODEL_REMEDIATION_TURNS=2,
  tool-call replies rejected on model-class work, per-section actionable
  failures, full turn accounting in evidence. Discovery/design/research
  work now publishes artifacts end to end.
- Graph access for system jobs: GraphView (Arc<dyn Fn() -> ProjectGraph>)
  on RunContext; default snapshots the sink once at attempt start (no
  lock held into executor work; snapshot isolation by construction);
  SystemOutcome.outputs lets a system job propose assembled docs
  (DeliveryDoc) with canonical digests; delivery stays unspecial-cased —
  the human authorization decision gates it.
- relay-executor 31/31; scheduling/capability suites updated for the
  strict model protocol (semantics preserved). All relay-tests suites +
  clippy green.

## FINAL GATE — lead verification (2026-10-13)

- cargo fmt --all -- --check: PASS · cargo clippy --workspace
  --all-targets --all-features -D warnings: PASS (0 warnings) ·
  cargo build --workspace --all-targets --all-features: PASS ·
  cargo test --workspace --all-features: 789 passed / 0 failed.
- Focused suites: intent_first_scenarios 16, graph_properties 8,
  fact_replay 10, semantic_migration 11, crash_recovery 5,
  capability_security 10, scheduling_concurrency 9, cli_work_graph 17,
  tui_work_graph 16 — all green (plus -p selectors for
  domain/engine/workflows/executor inside the workspace run).
- TypeScript reference: typecheck PASS, 621/621 tests, build PASS.
- Smoke (disposable workspaces): --help / --version (0.3.0-preview) /
  init / project summary / work ready / decisions list / explain
  (awaiting-human semantics) / doctor check (healthy) / migrate
  --dry-run (correct no-legacy message).
- Hygiene: git diff --check clean; secret scan over diffs + new code
  finds only redaction TEST fixtures (documented example keys); user's
  pre-existing dirty files all present (relay-core diff is their
  compat-port work, untouched by this effort); relay-ui old modules and
  relay-cli ops.rs preserved (inert reference).

ALL PHASE GATES: 0 ✓ 1 ✓ 2 ✓ 3 ✓ 4 ✓ 5 ✓ 6 ✓; 7 partial by design
(preview documented; packaging, real-project evaluation, and retirement
are user-gated). Nothing committed or pushed.
