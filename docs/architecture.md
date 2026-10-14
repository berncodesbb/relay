# Relay architecture

Relay is a local-first, model-agnostic development harness written in Rust. Deterministic code owns workflow state, permissions, and transitions; language models are replaceable workers consulted through a provider-neutral protocol. This document describes the intent-first work-graph implementation that is the sole supported product: the module boundaries, the fact-sourced work-graph core, the execution model, the persistence layers, and the safety rails.

Historical context: Relay was first implemented in TypeScript/Node.js, then rebuilt around its product intent in Rust. The original TypeScript application was removed from the repository on 2026-10-14 (`docs/plans/2026-10-14-remove-typescript-rust-only-codebase.md`); Git history and the frozen fixtures under `tests/fixtures/` are its archive. Two crates from the stopped one-for-one compatibility port (`relay-core`, `relay-runtime`) remain in-tree as frozen reference material.

## Overview

```text
              +--------------------------------------------------+
              |  relay-cli (binary): args, dispatch, render,      |
              |  terminal driver · relay-ui: work-graph TUI,      |
              |  human/JSON view renderers                        |
              +------------------------+-------------------------+
                                       |
                                       v
              +--------------------------------------------------+
              |  relay-app (application service)                  |
              |  ProjectSession · command handlers · typed views  |
              |  runner (executor composition) · StoreFactSink    |
              +----+-------------------------+-------------------+
                   |                         |
                   v                         v
     +----------------------+   +---------------------------+
     | relay-engine         |   | relay-executor            |
     | evaluate · explain · |   | Orchestrator · attempts · |
     | validate · recommend |   | grants · budgets          |
     +----------+-----------+ +-------------+-------------+
                |                           |
                v                           v
     +----------------------+   +---------------------------+
     | relay-domain (pure)  |   | relay-providers (neutral  |
     | graph fold · facts · |   | ModelClient port) ·       |
     | ids · validation     |   | relay-tools · relay-config|
     +----------------------+   +---------------------------+
                |
                v
     +------------------------------------------------------+
     | relay-store: .relay-graph JSONL fact store (authori- |
     | tative), SQLite projection (disposable), legacy      |
     | importers, doctor · relay-workflows: built-in        |
     | delivery workflow · relay-artifacts: publication     |
     +------------------------------------------------------+
```

Data flows one way: the CLI/TUI calls application handlers, handlers validate commands, persist accepted facts through the checked store, and return typed view models. Presentation renders those views and owns no policy.

## Authority order

The fixed order for all durable state:

```text
canonical JSONL facts -> deterministic replay -> work-graph state
                     -> SQLite projections -> efficient queries
                     -> Markdown views -> human review
```

If JSONL and SQLite disagree, JSONL wins; SQLite is disposable and rebuildable. Markdown artifacts are deterministic views over structured content. Nothing except validated fact append may change workflow state — never a model, provider, UI, database row, or Markdown file.

## Dependency rules

- `relay-domain`, `relay-engine`, and `relay-workflows` are pure: no IO, clocks, randomness, HTTP, SQL, terminal, provider SDK, or filesystem dependencies. Time, IDs, provider output, and filesystem facts enter only through facts, commands, or attempt receipts.
- Providers never leak SDK types. `relay-providers` defines the neutral model contract; the OpenAI/Anthropic adapters translate wire responses into Relay-owned request and event types inside their modules.
- Executors never advance the graph directly. They return receipts and proposals that deterministic policy validates (`relay_engine::validate_graph_change`, `accept_receipt`).
- Library crates never write to the terminal; only `relay-cli` produces output.

## The work-graph domain (`relay-domain`)

The innermost crate owns the addressable entities — goals, work items, dependencies, decisions, artifacts, evidence, attempts, capability grants, attempt receipts — plus their validation, the minimal durable fact set, and the `ProjectGraph` fold that replays accepted facts into graph state.

Design rules that hold the kernel honest:

- **Facts are the only history.** Everything operational (readiness, blockers, invalidation, completion) is derived by `relay-engine` from the folded graph; nothing derived is ever stored.
- **Exact versions carry dependency precision.** Work items pin exact artifact versions (or explicit selection rules), publications record the versions they were derived from, and invalidation follows those pins — never coarse phase order.
- **Executors propose; policy disposes.** Graph changes and receipts are data validated elsewhere; nothing in the domain mutates the graph on an executor's behalf.
- **Humans are graph nodes.** Decisions are first-class work; there is no global mode, hidden `current_mode`, or pause state anywhere in the domain.

## Evaluation (`relay-engine`)

`relay-engine` evaluates a `ProjectGraph` deterministically:

- `evaluate()` — the full projection: ready work, blocked work with reasons, awaiting-human decisions, active attempts, invalidated work, terminals, completion.
- `explain()` — why one work item is in its current state, in deterministic prose with typed cause chains (what `relay explain` renders).
- `validate_graph_change()` — accept or reject an executor/model proposal (cycles, duplicates, capability ceilings, scope).
- `accept_receipt()` — validate an attempt receipt and produce the facts recording acceptance, including engine-assigned artifact versions.
- `recommend()` — ordered scheduling advice under an autonomy policy, separating parallelizable read-only work from serialized writers.

## Built-in workflow (`relay-workflows`)

The one opinionated software-delivery workflow compiled into the binary: a catalog of work definitions for discovery through delivery authorization, structured requirement/criterion/decision/task/evidence schemas, deterministic grouped Markdown views (opportunity brief, PRD, TRD, plan, decision log, verification report, delivery report), per-kind completion and invalidation rules, and controlled graph-expansion proposals. Discovery, specification, design, planning, implementation, verification, and delivery remain recognizable as grouped views over work — never as a global state machine. The crate is pure; publication is an application concern, and every expansion still passes `validate_graph_change`.

## Workspace conversations (`relay-domain` facts + `relay-app` service)

Chat is an interface over the same fact authority — there is no separate chat database. Five versioned domain facts record conversations (all bounded, replayed by the same fold):

- `conversation_started` — conversation identity (a workspace may hold several; the chat resumes the latest);
- `user_message_recorded` — the accepted user message; recording it opens the turn;
- `assistant_message_recorded` — the completed assistant reply, with bounded references to attempts, decisions, and work it produced;
- `conversation_turn_failed` / `conversation_turn_cancelled` — truthful terminal outcomes.

Invariants are enforced in the fold: duplicate or cross-conversation identities fail closed, a terminal fact requires a started turn and is unique per turn, message text is bounded, and conversations are legal in a project with **no goal** — asking a read-only question never invents one. A turn whose user message exists without a terminal fact is *interrupted*: it is derived honestly at reopen, never answered retroactively. Bounded transcript reads come from the replayed graph (`transcript_tail`) — no SQLite index.

`relay-app`'s conversation service owns the turn flow:

```text
prompt -> append user-message facts (short store lock) -> release lock
       -> build bounded context (transcript tail + graph summary + open decisions)
       -> provider stream (Relay-owned events; redacted, settled deltas)
       -> decode typed proposals (structured output, runtime-validated)
       -> apply each proposal through existing command/engine services (at most one mutating per turn)
       -> append the terminal fact (assistant reply / failure / cancellation)
       -> immutable ChatView for rendering
```

Failed and cancelled turns are *successful service calls with truthful status* — the only `Err`s are store/IO failures where no terminal fact could be persisted. The writer lock is never held across a model call (proven by test). Model prose never mutates the graph: proposals are typed (`declare-goal`, `expand-work`, `resolve-decision`, `run-ready-work`, `request-clarification`), validated, and applied independently; rejections are reported, never coerced. Secrets are redacted before any text streams or persists; streamed deltas are settled so a secret split across provider fragments can never display partially.

`relay-ui`'s chat module is presentation-only (multiline editor, keymap, bounded transcript, overlays over the existing work views); `relay-cli` owns the terminal lifecycle: TTY detection happens in exactly one place (`invocation::detect_terminal_capability`), the chat driver runs turns on a worker thread over a std channel, `Ctrl-C` cancels before it exits, and every exit path restores raw mode, the alternate screen, and panic hooks. `relay --print` runs the same service for one bounded turn without any terminal mode. Non-TTY conversational invocation is rejected with `--print` guidance before any setup.

## Persistence (`relay-store`)


Two storage surfaces live in `relay-store`:

- **The fact store (`.relay-graph/`, authoritative).** Segmented, hash-chained canonical JSONL under `facts/`, a `manifest.json`, a disposable `projection.sqlite`, `locks/`, and `recovery/`. The generic layer is parameterized over an opaque record — fact type and version acceptance and the projection fold are injected by the domain (`FactValidator`, `FactFold`), so storage never depends on workflow logic. Appends are durable before any operation is reported successful; a project writer lock is held only for short storage operations, never across model calls or tools. Crash tails are recovered conservatively (incomplete tails are archived to `recovery/` before truncation); `doctor check` re-validates everything read-only and `doctor rebuild-projection` rebuilds SQLite from JSONL alone. The fact format is specified in `docs/plans/fact-format-proposal.md`.
- **The legacy event store and importers (compatibility).** The canonical legacy JSONL event format is specified self-contained in `docs/event-format.md`; it is the import format for old `.relay` workspaces. Legacy `.relay/state.db` SQLite projects are imported directly and read-only (see [migration-to-rust.md](migration-to-rust.md)). The legacy event path (`envelope`, `EventLog`, `RelayStore`) remains only as the authority for the frozen compatibility surface until its own documented retirement.

## Execution (`relay-executor`)

Executors perform work and return receipts and proposals:

- The `Executor` trait and five executor classes — human, model, workspace, verifier, system.
- `AttemptSpec`/`RunContext`: immutable per-attempt inputs and the runtime handle (cancellation, model client, tools, budget) an executor receives.
- The `FactSink` port: fact append and graph reads, so the orchestrator has no direct store dependency; the application implements it over `relay-store`.
- The `Orchestrator`: schedules from `relay_engine::recommend`, appends `AttemptStarted` (the durable lease) before running anything, never holds the writer lock across executor work, validates receipts through `accept_receipt` before appending acceptance facts, and reconciles abandoned attempts at open.
- `BudgetGuard`: token/attempt/duration ceilings whose exhaustion fires cancellation that threads into tool execution and the process supervisor.

Read/model attempts may run concurrently; workspace-changing attempts are serialized and overlapping write grants are rejected.

## Providers (`relay-providers`)

The neutral contract (`ModelClient`, streaming events, usage, finish reasons) is the only boundary between runtime and vendors. Adapters translate SDK/wire responses into Relay-owned types inside their modules; provider wire types stay private:

- **OpenAI adapter** — Chat Completions with streaming and usage; model-specific token-limit handling.
- **Anthropic adapter** — Messages API with merged tool results and defaulted token limits.
- **Fake provider** — in-process, offline, scriptable; the deterministic engine behind tests and the default for `relay run`.
- **Error normalization** — SDK/transport failures map to neutral kinds (`rate-limit`, `server`, `network`, `timeout`, `canceled`, `auth`, `invalid-response`, `unknown`) with retryability hints and redacted, trimmed messages. Headers and bodies never survive the adapter.

## Tools and safety (`relay-tools`)

`relay_tools::create_default_tool_registry` registers the built-in tools (filesystem, search, patch, process, Git families). Tools are schema-validated with typed decoding, capability-classified (`read`/`write`/`execute`), and bounded:

- **Path guard.** Model-supplied paths are resolved against the workspace; absolute paths, `~`, `..`, and NUL bytes are rejected and symlinks cannot escape containment.
- **Create-only writes, conflict-detecting patches.** Writes never overwrite; patches apply all-or-nothing with exact context matching.
- **Bounded processes.** Argument-array execution only (never a shell string), process-group termination on timeout/cancel, capped collected output, allowlisted child environment with secret-shaped names refused.
- **Redaction.** Tool output is redacted (secret-named values and known token shapes) and bounded before it reaches the model, the transcript, or the fact log.
- **Read-only Git surface.** Only read-only Git operations exist as tools; no Relay tool can add, commit, push, checkout, reset, or clean.

Capability grants (scoped operations and paths per work item) are enforced before any executor receives a tool-bearing attempt.

## Configuration (`relay-config`)

Layered YAML configuration (defaults < user < project < invocation) with strict, actionable validation: the named model-role catalog, purpose routing (including the legacy inline form), budgets, tool settings, and provider credential-variable names. Configuration contains no credentials — only the *names* of environment variables holding them. See [configuration.md](configuration.md).

## Artifacts (`relay-artifacts`)

Validated structured drafts become durable documents: canonical Markdown rendering (same input, identical bytes), atomic no-overwrite publication of versioned content addressed by digest, and deterministic invalidation derived from the dependency graph. Rendered views (brief, PRD, TRD, plan, decision log, reports) are deterministic Markdown over structured content stored as facts — reviewable in any editor, diffable across versions.

## Application service (`relay-app`) and interface (`relay-cli`, `relay-ui`)

`relay-app` composes engine, store, and executor ports into small command handlers — init, expand, resolve, cancel, import, publish, doctor, queries, run — each validating a command, persisting accepted facts through the checked store, and returning a typed view model. `ProjectSession` owns open/create, cached graph + evaluation, and refresh after every append. `runner` composes the orchestrator for `run` (which executors, which model client, how jobs spawn). The conversation service (see [Workspace conversations](#workspace-conversations-relay-domain-facts--relay-app-service)) is the same shape: typed requests in, validated facts out, immutable views back.

`relay-cli` owns argument parsing, dispatch, human/JSON output, exit codes, the legacy-command mapping, and the terminal driver. `relay-ui` owns the work-graph Ratatui interface (immutable snapshots, typed keymap, screens, terminal-restoration guard) and the human renderers. Presentation consumes view models and decides nothing about readiness, permissions, scheduling, or decision validity.

## Legacy migration

`relay migrate --workspace <legacy-copy>` (`--dry-run` to plan) imports a TypeScript-era `.relay/state.db` project into a new `.relay-graph` fact store: read-only source, staged under `.relay-graph-migration-staging`, validated through the graph fold and the fact doctor before an atomic switch, previous store retained as a backup, rollback on request, nothing deleted automatically. The full guide is [migration-to-rust.md](migration-to-rust.md).

## Testing strategy

All suites run offline; no test touches the network. Integration tests live beside each crate in `crates/*/tests/`; cross-crate suites live in `crates/relay-tests/`.

| Suite | What it verifies |
| --- | --- |
| `relay-domain`, `relay-engine`, `relay-workflows` (unit + property) | Fact validation, graph fold, evaluation determinism, proposal validation, scheduling properties, conversation ordering/duplicates/interruption, interleaved-conversation order independence |
| `relay-app/conversation` | Turn orchestration on the fake provider: informational/actionable first turns, proposal application and rejection, decision resolution, bounded runs, failure/cancellation facts, redaction, bounded context, no writer lock across the model call |
| `relay-ui/chat` | Multiline editor, keymap bindings, bounded transcript rendering, chat snapshots on TestBackend without a TTY |
| `relay-cli/chat_invocation` | The invocation matrix: bare/prompt/`--print`/TTY policy, misspelling guard, help/version precedence, exit codes |
| `relay-tests/intent_first_scenarios` | End-to-end product scenarios over the deterministic fake provider |
| `relay-tests/graph_properties` | Structural invariants of evaluation and scheduling |
| `relay-tests/fact_replay`, `crash_recovery` | Append durability, replay, tail recovery, reconciliation after injected faults |
| `relay-tests/semantic_migration` | Legacy `.relay` → `.relay-graph` semantic import: staging, rollback, determinism, fail-closed behavior |
| `relay-tests/migration`, `compatibility` | Legacy SQLite/JSONL import contracts against the frozen fixture corpus |
| `relay-tests/capability_security` | Grants, containment, redaction, bounded execution |
| `relay-tests/scheduling_concurrency` | Concurrency of read attempts, serialization of writers |
| `relay-cli/cli_work_graph`, `relay-ui/tui_work_graph` | CLI dispatch/output and terminal-interface snapshots |
| `relay-tests/repo_hygiene` | Implementation and build surfaces stay Rust-only |
| `relay-config/legacy_compat` | Legacy configuration forms keep loading |

The fixture corpus under `tests/fixtures/` is frozen historical evidence exported by the retired TypeScript implementation; it pins byte-level compatibility for the legacy import path and is not the acceptance oracle for new work-graph behavior.

## Frozen reference material

- `crates/relay-core` and `crates/relay-runtime` — the stopped one-for-one compatibility port. Read them for requirements, scenarios, and hardened adapters; do not extend them. They remain until their own documented retirement gate.
- `tests/fixtures/**` — immutable compatibility evidence (see above).
- `docs/plans/**` — design history; superseded plans carry status banners.

## Extension points

- **New provider adapter.** Implement the neutral contract in `relay-providers`, normalize errors through the shared failure mapping, and register the provider name in `relay-config`. Nothing outside the adapter changes.
- **New tool.** Define a typed `ToolDefinition` (args schema, capability, timeout, output cap) and register it in `create_default_tool_registry`; capability grants and the permission gate pick it up.
- **New workflow work definition.** Extend the catalog in `relay-workflows` (work definition, schemas, completion and invalidation rules, view template). Expansion still flows through `validate_graph_change`.
- **Configuration layers.** Add or adjust layers in `relay-config/src/load.rs`; see [configuration.md](configuration.md) for precedence.
