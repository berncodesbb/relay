# Relay Rust rewrite: target architecture and migration plan

Status: accepted direction; implementation not started  
Date: 2026-10-13  
Scope: complete replacement of the TypeScript application with a Rust application while preserving Relay's product behavior and local-first guarantees

## Decision summary

Relay will be rewritten as a Rust workspace and distributed as a single native executable. The rewrite will use a Codex-style hybrid persistence model:

- append-only JSONL is the canonical workflow record;
- SQLite contains rebuildable indexes and materialized projections;
- immutable Markdown remains the human-readable artifact record;
- deterministic Rust code owns workflow state, permissions, transitions, and recovery;
- model providers remain replaceable adapters and never own durable workflow state.

The rewrite is a behavioral migration, not a product redesign. The seven workflow modes, approval gates, artifact lifecycle, provider-neutral model contract, permission policy, autonomy bounds, and delivery hold remain intact unless changed through a separate product decision.

This design deliberately avoids a dual-authority system. If JSONL and SQLite disagree, JSONL wins and SQLite is repaired or rebuilt.

## Why Rust

The intended gains are operational rather than computational:

- a single executable with no Node.js installation requirement;
- predictable startup and memory use;
- stronger types around event contracts, state transitions, paths, permissions, and provider data;
- tighter process lifecycle, cancellation, and platform-specific sandbox integration;
- an easier path to signed binaries and native installers;
- explicit concurrency and durability behavior in the local state layer.

Rust is not expected to materially improve model latency. Provider calls remain network-bound, and workflow correctness remains more important than throughput.

## Non-goals

The rewrite does not introduce:

- hosted synchronization, accounts, or a server database;
- automatic commits, pushes, deployments, or publishing;
- autonomous multi-agent execution;
- a web interface;
- semantic repository indexing or a vector database;
- a new workflow, new artifact taxonomy, or new approval model;
- compatibility with arbitrary concurrent writers on network filesystems.

## Architectural principles

1. **One canonical record.** Valid JSONL records are the source of truth. SQLite is never required to recover a project.
2. **Deterministic replay.** The same accepted event sequence produces the same project state on every supported platform.
3. **Write the fact before the projection.** A durable event is appended before derived state is updated.
4. **Fail closed at boundaries.** Invalid events, unsafe paths, unknown tool calls, incompatible schemas, and ambiguous consequential actions do not execute.
5. **Provider isolation.** Vendor request and response types do not cross provider-adapter boundaries.
6. **Human-readable outputs.** Contractual artifacts remain immutable Markdown files suitable for ordinary review and version control.
7. **Recoverability over cleverness.** A user can inspect, copy, validate, and rebuild local Relay state without a running service.
8. **Compatibility is tested, not assumed.** The Rust implementation must pass fixtures produced from the TypeScript implementation before cutover.

## System overview

```text
                         relay CLI / terminal UI
                                    |
                                    v
                         application controller
                     commands, runs, cancellation
                                    |
                +-------------------+-------------------+
                |                   |                   |
                v                   v                   v
          workflow core        agent runtime       query service
        events + reducer     prompts + tools +      status/history/
       modes + transitions    provider routing         inspect
                |                   |                   |
                +-------------------+-------------------+
                                    |
                +-------------------+-------------------+
                |                   |                   |
                v                   v                   v
       canonical event log    SQLite projections   Markdown artifacts
        append-only JSONL       rebuildable cache     immutable versions
                                    |
                          provider and tool adapters
```

Dependencies point inward. The workflow core contains pure domain types and functions and performs no filesystem, database, terminal, process, clock, randomness, or network access.

## Rust workspace

The repository will become a Cargo workspace. Crate boundaries are architectural enforcement, not a crate per source file.

```text
Cargo.toml
crates/
  relay-core/          events, state, reducer, modes, outcomes, transitions
  relay-store/         JSONL log, locks, validation, SQLite projections, migration
  relay-artifacts/     artifact schemas, rendering, immutable Markdown storage
  relay-providers/     neutral model contract and OpenAI/Anthropic/fake adapters
  relay-tools/         registry, path guard, filesystem, patch, process, Git reads
  relay-runtime/       controller, agent loop, routing, budgets, retry, cancellation
  relay-config/        layered configuration, normalization, diagnostics
  relay-ui/            CLI output and terminal UI view models/components
  relay-cli/           executable, argument parsing, signal and lifecycle wiring
tests/
  fixtures/            versioned cross-language event and artifact fixtures
  compatibility/       TypeScript-to-Rust replay and output assertions
```

Dependency direction:

```text
relay-cli -> relay-ui -> relay-runtime -> relay-core
                    \-> relay-config     ^
relay-runtime -> relay-providers         |
relay-runtime -> relay-tools             |
relay-runtime -> relay-artifacts         |
relay-runtime -> relay-store ------------+
```

`relay-core` must remain usable with default features disabled and must not depend on Tokio, SQL, HTTP, terminal, provider SDK, or filesystem crates.

## Canonical persistence

### Project layout

```text
.relay/
  manifest.json
  events/
    0000000000000001-0000000000010000.jsonl
    0000000000010001-open.jsonl
  index.sqlite
  artifacts/
    prd/v3-onboarding-prd.md
    trd/v1-onboarding-trd.md
  locks/
    writer.lock
  recovery/
    rejected-tail-<timestamp>.jsonl
```

The segmented log avoids rewriting an indefinitely growing file and makes backup, validation, and future compaction manageable. Closed segments are immutable. Only the final `-open.jsonl` segment may grow. Segment rotation occurs at a configured byte or event threshold; the default should be conservative and fixed in version 1.

`manifest.json` contains only store metadata: store format version, project ID, creation time, active segment, and the minimum Relay version required to read the log. It is not workflow state.

### Event envelope

The Rust event envelope preserves the current logical fields:

```json
{
  "envelopeVersion": 1,
  "id": "...",
  "projectId": "...",
  "sequence": 42,
  "sessionId": "...",
  "type": "artifact.approved",
  "schemaVersion": 1,
  "timestamp": "2026-10-13T08:00:00.000Z",
  "correlationId": "...",
  "causationId": "...",
  "payload": {},
  "previousHash": "...",
  "hash": "..."
}
```

Each physical line is one UTF-8 JSON object terminated by `\n`. Embedded line breaks are JSON escaped. Object serialization used for hashing is canonical and independently specified; readers must not hash raw input bytes after parsing.

`hash` covers the canonical event envelope excluding `hash` itself. `previousHash` creates a per-project chain and is `null` for sequence 1. The chain detects truncation, reordering, and accidental editing; it is an integrity mechanism, not authentication against a malicious local user.

### Append protocol

Only one process may append to a project log at a time.

1. Acquire the project writer lock with a bounded timeout.
2. Read and validate the manifest and active segment tail.
3. Verify the expected next sequence and previous hash.
4. Validate the entire proposed event batch using domain schemas.
5. Serialize the batch into one buffer using canonical JSON, one event per line.
6. Append the buffer to the active segment.
7. Flush and sync the event file according to the configured durability mode; contractual state uses durable mode by default.
8. Commit corresponding SQLite projection changes in one transaction.
9. Update the projection checkpoint to the last indexed event hash and sequence.
10. Release the lock.

If the process stops after step 7 and before step 9, the next open detects that SQLite is behind and replays the missing events. SQLite must never advance beyond a successfully validated JSONL event.

No workflow operation may depend on SQLite being updated in the same crash window as JSONL. The user-visible success boundary is the durable JSONL append.

### Tail recovery

A crash can leave an incomplete final line only in the active segment. On open:

- all closed segments must be complete and valid or opening fails;
- valid complete lines in the active segment are retained;
- an incomplete final line is copied to `recovery/`, then the active file is truncated to the last valid newline while holding the writer lock;
- a complete but invalid line is not silently removed; opening fails with its segment and line number;
- sequence gaps, duplicate IDs, project mismatches, incompatible versions, and broken hashes fail with actionable diagnostics.

Automatic recovery is limited to an incomplete byte tail because its cause and safe boundary are unambiguous.

### SQLite projections

`index.sqlite` is disposable. Initial projections should include:

- event lookup by ID, sequence, type, session, correlation, and time;
- current project summary and active workflow mode;
- sessions and run summaries;
- artifact identity, kind, version, status, and path;
- model-call usage and failure summaries;
- tool execution and denial summaries;
- searchable diagnostic/log metadata needed by `status`, `why`, `inspect`, and resume selection;
- one checkpoint row containing the last indexed sequence and hash.

The database does not need triggers to make events append-only because it is not the event authority. Projection tables may be replaced transactionally during a rebuild.

On startup, Relay compares the SQLite checkpoint with the JSONL tail:

- matching sequence and hash: open normally;
- SQLite behind: replay the missing suffix;
- missing database, incompatible projection schema, failed integrity check, or divergent checkpoint: rename the database into `recovery/` and rebuild;
- SQLite ahead of JSONL: treat it as divergent and rebuild from JSONL.

Rebuilding must be available explicitly through `relay doctor rebuild-index` and must not modify JSONL or artifacts.

### Artifact consistency

Artifact creation crosses two durable resources: a Markdown file and the event log. Use a prepare-and-publish protocol:

1. Validate and deterministically render the artifact.
2. Write and sync a uniquely named temporary file inside the artifact directory.
3. Publish it with a no-overwrite atomic operation.
4. Append the event referencing its relative path and content digest.
5. If the event append fails, leave the unreferenced file recoverable and report it; `relay doctor` may identify and optionally quarantine orphan files.

An event must never reference an artifact file that was not successfully published. Artifact files remain immutable. Their SHA-256 digest is recorded in the event so replay can distinguish missing, modified, and valid files.

## Domain model and replay

Rust enums represent every known event type and payload. Deserialization occurs in two stages: a minimally typed envelope identifies the event type and schema version, then the matching versioned payload is decoded and validated.

Unknown event types or newer schema versions are preserved in diagnostic context but are not ignored during authoritative replay. Relay fails with an upgrade instruction rather than deriving incomplete state.

`apply_event(state, event) -> Result<State, FoldError>` is pure. In practice the implementation may mutate an owned state for efficiency, but its observable behavior must remain equivalent to a pure fold. Time, IDs, provider results, and filesystem facts enter the core only through commands or events.

Commands express requested actions; events express accepted facts. The controller follows:

```text
input -> command -> policy/validation -> event batch -> append -> replay/project -> output
```

This separation prevents UI, provider, or storage code from directly changing project state.

## Runtime and concurrency

Tokio hosts provider streaming, cancellation, timers, and subprocess supervision. The domain core itself remains synchronous.

Each project has one logical controller operation at a time. Multiple read-only processes may query a project, but writers serialize through the project lock. A long provider request does not hold the storage writer lock; the controller records the request, releases the lock, performs the call, then reacquires the lock to record its result. Before accepting the result it verifies the expected project revision. If another writer advanced relevant state, deterministic conflict handling decides whether to rebase, retry, or reject the stale result.

Cancellation propagates through a tree rooted at the run. Subprocesses receive graceful termination followed by forced termination after a bounded interval. On Unix, process groups are used; Windows uses Job Objects. Platform-specific behavior lives behind one process-supervisor interface and has integration tests per supported operating system.

## Providers

The provider contract continues to expose Relay-owned messages, tool calls, stream events, usage, finish reasons, structured-output requests, and normalized failures.

Use direct HTTP adapters built on `reqwest` rather than making the core depend on vendor SDK types. Provider-specific wire structs remain private to each adapter. This reduces dependency weight and makes protocol fixtures straightforward, but it requires explicit handling of streaming formats and API evolution.

Initial adapters:

- OpenAI;
- Anthropic;
- deterministic offline fake provider.

Provider conformance tests replay recorded, sanitized HTTP fixtures without network access. Live tests are opt-in and never part of the default suite.

Routing remains deterministic configuration:

```text
purpose -> role -> ordered provider/model route chain
```

The current model-role routing behavior under development must be completed and captured as compatibility fixtures before its Rust port begins.

## Tools and security boundary

The Rust tool registry retains capability labels (`read`, `write`, `execute`) and schema-validated arguments. Permission evaluation remains deterministic and occurs before any adapter runs.

Required invariants:

- every model-supplied path is workspace-relative and symlink-safe;
- file creation never overwrites;
- patches are parsed completely and applied all-or-nothing with context and restat checks;
- commands use executable-plus-argument arrays, never shell interpolation;
- output is bounded and redacted before persistence or model exposure;
- child environments are allowlisted and secret-shaped names are refused;
- Git tools remain read-only;
- destructive and delivery-class actions remain unavailable or require explicit user authorization outside the model tool surface.

Where Rust libraries cannot provide identical cross-platform filesystem semantics, Relay must document the narrower guarantee and fail rather than silently weaken it.

## CLI and terminal UI

Use `clap` for command parsing. Human and JSON output are separate renderers over stable view models; domain and runtime crates never print directly.

Use `ratatui` with `crossterm` for the interactive UI. The TUI owns terminal setup and restoration but not workflow state. All screens render immutable snapshots from a runtime feed and dispatch typed actions back to the controller.

The first Rust release must preserve existing commands, flags, slash commands, exit codes, and JSON shapes unless a documented compatibility exception is approved. Snapshot tests cover narrow terminals, cancellation, streaming, overlays, and non-interactive output.

## Configuration

Configuration remains layered:

```text
built-in defaults < user configuration < project configuration < invocation overrides
```

YAML remains supported for compatibility. Parsed configuration and normalized runtime configuration are distinct Rust types. Validation resolves all role references, provider registrations, fallback cycles, limits, and routing keys before a run mutates project state.

Secrets remain environment-sourced and never enter configuration, events, SQLite, artifacts, error bodies, or diagnostic bundles.

## Migration from TypeScript Relay

The rewrite must import existing `.relay/state.db` projects without destroying or modifying them.

### Import strategy

The final TypeScript release gains an export command that reads the existing append-only SQLite events in sequence, validates them with the current schemas, and writes the new canonical JSONL format plus a migration manifest. The Rust executable also supports direct read-only import from the legacy database for users who did not run the exporter.

Import procedure:

1. Acquire a migration lock and verify no active Relay writer.
2. Run SQLite integrity checks and validate every legacy event.
3. Verify contiguous per-project sequence and existing reducer replay.
4. Copy existing artifacts without changing their bytes and calculate their digests.
5. Write the new store into `.relay-rust-migration-<id>/`.
6. Replay JSONL in Rust and compare the normalized final state with the TypeScript export manifest.
7. Build SQLite projections from JSONL and run integrity checks.
8. Atomically rename the old `.relay/` to a timestamped backup and publish the new directory as `.relay/`.
9. Never delete the backup automatically.

Migration is all-or-nothing from the user's perspective. A failed migration leaves the original `.relay/` active.

### Compatibility fixtures

Before porting modules, generate and commit sanitized fixtures covering:

- empty and initialized projects;
- every event type and schema version;
- every workflow stage and approval boundary;
- artifact revision and transitive invalidation;
- retry, provider fallback, budget exhaustion, cancellation, and resume;
- denied tools and redacted output;
- verification failure and delivery hold;
- legacy inline routes and role-based routes;
- corrupt rows, sequence gaps, incompatible events, and missing artifacts.

For each valid fixture, TypeScript emits normalized replay state, status JSON, why JSON, artifact bytes, and expected next action. Rust must match them before the corresponding phase is complete.

## Rewrite phases

### Phase 0: freeze the behavioral contract

- Finish and verify the in-progress model-role routing change.
- Tag a TypeScript compatibility baseline.
- Specify canonical JSON, timestamp, ID, hashing, and normalized comparison rules.
- Add the fixture exporter and representative golden fixtures.
- Record current CLI exit codes and JSON output contracts.

Exit criterion: the TypeScript system can produce a self-contained compatibility corpus and a valid legacy project export.

### Phase 1: Rust core

- Create the Cargo workspace and CI matrix.
- Port IDs, modes, artifacts, events, intents, outcomes, reducer, transitions, and policies.
- Add property tests for replay determinism and transition invariants.
- Match all core compatibility fixtures.

Exit criterion: Rust replays every valid fixture to the expected normalized state and rejects every invalid fixture for the expected reason.

### Phase 2: canonical store and migration

- Implement segmented JSONL, canonical serialization, hashes, locking, append, recovery, and validation.
- Implement SQLite projections, checkpoints, incremental catch-up, and full rebuild.
- Implement artifact storage and integrity checks.
- Implement read-only legacy SQLite import.

Exit criterion: crash-injection tests at every append step recover without losing a reported-success event, and imported projects match TypeScript replay.

### Phase 3: tools and configuration

- Port layered configuration and normalized routing.
- Port path guard, redaction, file tools, patch engine, process supervisor, and Git reads.
- Run adversarial path, symlink, race, secret, output-bound, and process-tree tests on macOS, Linux, and Windows.

Exit criterion: security regression fixtures pass on all supported platforms.

### Phase 4: providers and agent runtime

- Implement the provider-neutral stream contract.
- Add OpenAI, Anthropic, and fake adapters.
- Port retry, fallback, budgets, repeated-action protection, cancellation, context building, prompts, and agent loop.
- Port the controller in small workflow slices.

Exit criterion: the offline fake provider completes the seven-stage workflow with output and state equivalent to the TypeScript baseline.

### Phase 5: CLI and TUI

- Implement commands and machine-readable output.
- Implement the Ratatui interface and slash commands.
- Match exit codes, status/why/inspect output, cancellation, and terminal restoration behavior.

Exit criterion: CLI integration and TUI snapshot suites pass across the CI platform matrix.

### Phase 6: parallel preview

- Ship the Rust executable as an opt-in preview.
- Keep TypeScript Relay as the default and read-only reference implementation.
- Add a comparison command that runs replay and view generation in both implementations without executing tools or calling providers.
- Gather migration, startup, memory, database rebuild, and compatibility evidence.

Exit criterion: no unexplained state divergence across the agreed preview window and all release-blocking defects resolved.

### Phase 7: cutover and retirement

- Make Rust the default executable.
- Retain the TypeScript importer/exporter for at least one compatibility release.
- Publish recovery and rollback instructions.
- Remove TypeScript runtime code only after the rollback window; keep golden fixtures and format documentation permanently.

Exit criterion: supported legacy projects migrate successfully, release artifacts are signed, and rollback has been exercised from published packages.

## Testing and verification

The Rust suite includes:

- unit tests for domain contracts and pure policies;
- golden replay and rendering tests;
- property tests for event sequences, reducers, and configuration normalization;
- crash-injection tests for append and artifact publication;
- corruption and recovery tests for JSONL and SQLite;
- concurrency tests for readers, writers, stale provider results, and lock timeouts;
- adversarial filesystem and patch tests;
- provider protocol fixture tests;
- end-to-end fake-provider workflows;
- CLI output and exit-code compatibility tests;
- TUI snapshots and terminal restoration tests;
- migration tests using copies of real, sanitized legacy databases.

Default tests perform no network access. CI runs stable Rust on macOS, Linux, and Windows. The minimum supported Rust version is pinned and tested. Dependency advisories, licenses, formatting, lints, tests, and release builds are required gates.

## Operational commands

The Rust CLI adds a small recovery surface:

```text
relay doctor check             validate manifest, event chain, artifacts, and index
relay doctor rebuild-index     rebuild SQLite exclusively from JSONL
relay doctor export            create a portable diagnostic/export bundle
relay migrate                  import a legacy TypeScript project explicitly
```

Recovery commands are read-only by default. Any command that quarantines a tail, replaces an index, or switches project directories reports exactly what changed and where the recoverable backup was placed.

## Observability and privacy

Structured diagnostics use `tracing`, but durable diagnostic events are bounded and redacted. Logs must not include prompts, model responses, artifact bodies, environment values, HTTP headers, or tool output unless a user explicitly requests a local diagnostic export that clearly identifies those contents.

SQLite growth is bounded through retention policies for non-authoritative operational logs. Canonical workflow events are never silently pruned. Any future compaction format must preserve a verifiable snapshot plus the audit records required by Relay's product contract and will require a separate format version and migration design.

## Release and packaging

Release artifacts target, at minimum:

- macOS arm64 and x86_64;
- Linux x86_64 and arm64;
- Windows x86_64.

The executable embeds its build version, event format range, projection schema version, and minimum readable store version. Packages include shell completions, license notices, checksums, and signatures. Update behavior remains explicit; Relay does not silently replace itself.

## Risks and mitigations

| Risk | Mitigation |
| --- | --- |
| Behavioral drift during port | Golden cross-language fixtures and normalized state comparison |
| JSONL/SQLite divergence | JSONL authority, hash checkpoint, automatic projection rebuild |
| Partial writes | Single buffered append, sync, active-tail recovery, crash injection |
| Multi-process conflicts | Per-project writer lock and expected-revision checks |
| Provider API churn | Private wire types and sanitized protocol fixtures |
| TUI regression | Stable view models, snapshots, and a non-interactive path independent of TUI |
| Platform filesystem differences | Platform CI, explicit adapters, fail-closed behavior |
| Rewrite stalls while product evolves | Vertical phases, TypeScript baseline freeze, preview before cutover |
| Migration damages user state | Read-only source, staged destination, atomic directory switch, retained backup |
| Rust dependency complexity | Small crate surface, pinned MSRV, audit and license gates |

## Final acceptance criteria

The rewrite is complete only when:

1. A supported legacy Relay project migrates without changing its logical replay state or artifact bytes.
2. JSONL alone can reconstruct all authoritative workflow state.
3. Deleting `index.sqlite` and reopening produces an equivalent database and user-visible state.
4. An interrupted append cannot lose an event previously reported as successful.
5. Invalid, incompatible, reordered, truncated, or edited event history is detected with actionable diagnostics.
6. The seven-mode fake-provider workflow matches the TypeScript behavioral baseline.
7. Approval, intervention, permission, autonomy, verification, and delivery gates remain deterministic and equivalent.
8. Provider fallback, retry, budgets, cancellation, and resume remain equivalent.
9. Tool safety tests pass on macOS, Linux, and Windows.
10. Existing commands and supported JSON outputs remain compatible or have an explicitly approved migration note.
11. Rust is the default distributed executable and requires no Node.js runtime.
12. Migration rollback has been tested using published release artifacts.

## Decisions intentionally deferred

These choices should be resolved during Phase 0 or by focused prototypes without changing the architecture above:

- exact segment rotation threshold;
- lock implementation per platform;
- `rusqlite` versus `sqlx` for the projection store (prefer `rusqlite` unless async SQL provides a demonstrated benefit);
- exact Ratatui component structure;
- installer and auto-update channels;
- optional OS sandbox mechanisms beyond Relay's existing permission model.

None of these deferred decisions changes the authority hierarchy: JSONL first, SQLite derived, Markdown human-readable.
