# Relay architecture

Relay is a local-first, model-agnostic development harness. Deterministic TypeScript code owns workflow state, permissions, and transitions; language models are replaceable workers consulted through a provider-neutral protocol. This document describes the module boundaries, the event-sourced state core, the workflow engine, and the safety rails, as implemented in `src/`.

## Overview

```text
                 +----------------------------------------------------+
                 |  CLI (src/cli)  &  TUI (src/tui, Ink)               |
                 |  commands from USAGE; slash commands; renderers      |
                 +--------------------+-------------------------------+
                                      |
                                      v
                 +----------------------------------------------------+
                 |  Runtime (src/runtime)                              |
                 |  controller  agent-loop  model-router               |
                 |  budgets     cancellation  retry                    |
                 +--------------------+-------------------------------+
                                      |
                                      v
         +-----------------------------------------------------------+
         |  Core contracts (src/core)                                 |
         |  events  modes  intents  outcomes  project-state            |
         |  reducer  transitions  artifacts (kinds + dependency graph) |
         +---------------------------+-------------------------------+
                                     ^ implements / depends on core only
   +-----------------+---------------+-----------+------------------+
   |                 |               |           |                  |
 providers/      storage/        artifacts/   policies/          tools/
 openai          sqlite event    markdown     permissions        registry +
 anthropic       store + repos   store +      intervention        filesystem, search,
 fake            + migrations    render +     transitions         patch, process, git
                 schemas
```

Data flows one way: the CLI/TUI invokes the runtime, the runtime applies core contracts and orchestrates adapters. Adapters (providers, storage, artifacts, policies, tools) implement interfaces owned by the core/runtime boundary.

## Dependency rules

- One-way: CLI/TUI → runtime → core. `src/core/**` imports nothing from storage, providers, tools, config, or the CLI. It is pure data plus pure functions.
- Providers never leak SDK types. `src/providers/provider.ts` defines the neutral `ModelProvider`, `ModelRequest`, `ProviderStreamEvent`, `TokenUsage`, and finish reasons; the OpenAI/Anthropic adapters translate SDK responses into these Relay-owned types inside their modules. No SDK object crosses an adapter boundary.
- Storage folds events through an injected `StateFold` reducer, so persistence never depends on workflow logic.
- Library code never writes to the terminal; only `src/cli/**`, `src/tui/**`, and `src/cli.tsx` produce output.

## Event sourcing

Every durable workflow change is one event in an append-only SQLite log (`src/storage/event-store.ts`, `src/core/events.ts`).

- **Envelope.** Each row carries `id`, `project_id`, `sequence` (per project, increasing by exactly 1 from 1), `session_id`, `type`, `schema_version`, `timestamp`, `correlation_id`, `causation_id`, and a canonical-JSON `payload`.
- **Payload validation.** Every event type has a Zod schema (`eventPayloadSchemas`). The store validates before writing — an invalid batch aborts whole with nothing written — and re-validates every row on read via `validateStoredEvent`.
- **Append-only.** SQLite triggers `RAISE(ABORT)` on any UPDATE or DELETE of `events`. Batches append inside one `BEGIN IMMEDIATE` transaction with contiguous sequences; a concurrent writer collides on the unique `(project_id, sequence)` index and is told to retry.
- **Pure replay.** `applyEvent`/`replayProject` (`src/core/reducer.ts`) fold a validated log into `ProjectState` with no clocks, randomness, or IO. The same log always yields identical state, which is what makes restart resume and step-by-step non-interactive runs possible.
- **Diagnostics.** Unknown event types, incompatible `schema_version` values (newer Relay), invalid payloads, sequence gaps, duplicate initialization, and broken revision chains throw named errors (`UnknownEventTypeError`, `IncompatibleEventVersionError`, `InvalidEventPayloadError`, `EventFoldError`) that cite the sequence number and suggest a remedy.
- **Migrations.** `src/storage/migrations.ts` applies forward-only migrations, each recorded in `schema_migrations` inside the same transaction as its statements. A database containing migrations unknown to this build is refused with an upgrade hint.

The event types (24 of them):

| Group | Types |
| --- | --- |
| Project lifecycle | `project.initialized`, `session.started` |
| Intents | `intent.proposed`, `intent.applied` |
| Mode transitions | `mode.entered`, `mode.transition.rejected` |
| Autonomy | `autonomy.changed` |
| Artifacts | `artifact.created`, `artifact.revised`, `artifact.approval.requested`, `artifact.approved`, `artifact.invalidated`, `artifact.inspected` |
| Decisions | `transition.evaluated` |
| Model calls | `model.requested`, `model.responded`, `model.failed` |
| Tools | `tool.executed`, `tool.denied` |
| Verification | `verification.completed` |
| Pause/resume | `workflow.paused`, `workflow.resumed` |
| Limits/interruption | `budget.exhausted`, `run.interrupted` |

### State projection

`replayProject` folds the log into a `ProjectState` (`src/core/project-state.ts`): the goal and workspace root, `currentMode`, `autonomy`, `paused` (with question and options), the artifact registry (`artifacts`, `artifactsByKind`, statuses, versions, revision history), transition and rejected-transition history, the last evaluated outcome, call statistics, the stale-artifact set, and the verification summary. The projection is the only thing the CLI renders and the only thing policies reason over; it is rebuilt from events, never mutated in place.

## Workflow engine

### Run lifecycle

A run is a loop over controller steps, each of which is replayable from events:

1. Load: open the database, read the project's events, fold them into `ProjectState`.
2. Classify: user input becomes either a parsed slash command or a model-proposed intent, validated against `intentProposalSchema` with confidence floors.
3. Execute: for the current mode, the controller runs the agent loop with the mode's permitted capabilities, the permission gate, budgets, and a cancellation token; model calls follow the routed provider chain.
4. Decide: `evaluateTransition` (composed with the intervention policy) emits exactly one `ControllerOutcome`; `shouldAutoAdvance` bounds whether the loop continues without asking.
5. Record: every step — requests, responses, tool executions and denials, artifact changes, outcomes — is appended to the event log before the process moves on.

Because state is a pure fold of that log, a restart resumes exactly where the run stopped, and a non-interactive invocation can perform a single step and exit.

### Mode contracts

Seven modes are declared in `src/core/modes.ts` with a `ModeContract` each: the artifact produced, required input kinds, permitted tool capabilities, whether the mode may modify user files, completion evidence, and predecessor/successor. Contracts drive both the transition policy and the permission policy.

### Outcomes and transitions

After every controller step the deterministic policy (`src/core/transitions.ts`) emits exactly one `ControllerOutcome` (`src/core/outcomes.ts`):

- `CONTINUE` — keep working in the current mode.
- `ADVANCE` — enter the next mode.
- `NEEDS_USER` — stop and ask (with a question and options).
- `REVISE` — jump back to `stage`.

`evaluateTransition` handles triggers: `mode-complete`, `artifact-approved`, `verification-failed`, `user-request`, `resume`. Mode entry is gated (`canEnterMode`): no entry while paused, required input artifacts must exist and not be stale, forward movement is one mode at a time (no skips), and `delivery` additionally requires a passing verification. `src/policies/transitions.ts` wraps this so completing delivery always surfaces a `NEEDS_USER` outcome citing the delivery intervention rule.

Models propose; deterministic code decides. Model-proposed intents are validated against `intentProposalSchema` (`src/core/intents.ts`) before the controller applies anything. Ambiguous or low-confidence classifications surface a focused question instead of acting: confidence below 0.6 is ambiguous for any intent, below 0.8 for consequential intents (`revise-artifact`, `change-mode`, `set-autonomy`). Explicit slash commands bypass classification.

### Autonomy bounds

`shouldAutoAdvance` bounds unattended advancement by the current autonomy setting and never advances while paused. `off` never auto-advances (and non-read tools are denied); `next-checkpoint` (default) auto-advances only on `ADVANCE`; `workflow` auto-advances on `ADVANCE` or `CONTINUE`; `until:<mode>` advances until the workflow enters the target mode.

### Approval gates

The artifact kinds `opportunity-brief`, `prd`, `trd`, `execution-plan`, and `verification-report` are contractual (`src/core/artifacts.ts`). Completing a mode that produces one requires the user to approve the current version (`/approve`); only the user can approve. `src/policies/intervention.ts` additionally requires explicit user action for artifact approval/revision, mode changes, autonomy changes, destructive operations, scope expansion beyond the approved plan, deployment, delivery, and consequential product choices.

### Deterministic invalidation

Revising an artifact (`artifact.revised`) marks every existing artifact whose kind is transitively downstream in the core dependency graph as `stale`, and work routes back to `earliestAffectedStage` — the lowest mode whose produced artifact exists and is downstream of the revision. `src/artifacts/invalidation.ts` computes the same plan from the graph alone, so the reducer and policies replay identically.

### Verification independence

Verification scores every approved acceptance criterion pass/fail/unevaluated with evidence from the diff and test runs, in a mode that cannot modify user files (read + execute only) and runs against the approved plan rather than the implementation transcript.

### Delivery hold

Delivery assembles the trace from requirement to change to evidence, then stops: external delivery actions require explicit user authorization, and no Relay tool can commit, push, publish, or deploy.

## Artifacts

`src/artifacts/` turns validated model drafts into durable documents.

- **Kinds.** `opportunity-brief`, `decision-log`, `prd`, `trd`, `execution-plan`, `verification-report`, `delivery-report` — each with a Zod draft schema (`schemas.ts`): bounded lengths, capped arrays, single-line titles, identifiers restricted to `A-Z0-9-` so they cannot inject Markdown structure, and workspace-relative file references.
- **Versioned Markdown.** `ArtifactMarkdownStore` writes each version once as `<kind>/v<n>-<slug>.md` with YAML frontmatter (`id`, `kind`, `version`, `title`, `status`, `createdAt`, `updatedAt`). Writes are atomic (temp file + rename), existing version files are never overwritten, and per-artifact version uniqueness is enforced even across retitles.
- **Deterministic rendering.** `renderArtifactMarkdown` maps a validated draft to a fixed section layout per kind — same input, identical bytes — so version diffs are meaningful. Table cells are escaped; no HTML is emitted.
- **Status lifecycle.** draft → approved (user action) → stale (an upstream revision invalidated it); revising resets an artifact to draft at version + 1.

## Model routing

Routing is deterministic configuration — never model output, and never a model choosing its own role. Every call resolves **purpose → role → provider**:

1. **Purpose.** The call's routing purpose is either the current workflow mode (`discovery` … `delivery`) or a utility purpose such as `intent` (input classification).
2. **Role.** The routing entry for that purpose names a model role from the user-defined `models` catalog — built-in names like `reasoning` or `coding`, or user names like `security-review`. A role owns a provider, a model, optional limits, and optional role-name fallbacks. Legacy inline routing objects carry no role and keep working unchanged.
3. **Provider.** The role resolves to a registered provider adapter.

Normalization happens once, at the configuration boundary: `src/config` reduces role references and legacy inline objects into `NormalizedRouting` (`src/config/normalized.ts`) — a chain per defined role, a chain per routing key, plus a legacy fallback — before the router runs, so the raw string/object union never reaches the runtime or controllers. `ModelRouter` (`src/runtime/model-router.ts`) resolves the chain: an explicit purpose entry (`intent`) wins over the workflow mode's route, and only a non-mode purpose with no explicit entry falls back to the first configured route (the historical behavior). Role fallback chains expand deterministically — declared order, deduplicated without reordering, at most one primary plus three fallbacks — keeping only providers actually registered; an unregistered primary provider fails immediately with an actionable error. `routeChain()` returns the chain; the agent loop retries within a route before failing over to the next.

Model events carry the attempted role as an optional payload field (`role` on `model.requested`, `model.responded`, and `model.failed`; the event schema version stays 1), so pre-role event logs replay unchanged and legacy inline routes simply record no role. `relay why` and its JSON/TUI projections surface the role next to provider, model, and purpose when it is present.

## Providers

`src/providers/provider.ts` is the only contract between runtime and vendors: `ModelProvider.stream(request)` yields Relay-owned events (`text-delta`, `tool-call`, `usage`, `structured-output`, `completed`, `error`); `collectTurn` folds a stream into one `ModelTurnResult`.

- **OpenAI adapter.** Chat Completions with streaming and `stream_options.include_usage`; usage maps from `prompt_tokens`/`completion_tokens`; streamed tool-call fragments are accumulated by index and their JSON argument text parsed (raw string fallback); the system prompt becomes a leading developer message; `gpt-5*`/`o*` models get `max_completion_tokens` instead of `max_tokens`; structured output uses `response_format: json_schema` with a fenced-JSON fallback parse. SDK retries are disabled so Relay's retry policy owns backoff.
- **Anthropic adapter.** Messages API; `max_tokens` defaulted (8192); consecutive tool results merged into one user message to satisfy alternating roles; structured output requested via instruction and parsed from text with the same fenced-JSON fallback.
- **Error normalization.** `toProviderFailure` maps SDK/fetch errors to `ProviderFailure` with a neutral kind (`rate-limit`, `server`, `network`, `timeout`, `canceled`, `auth`, `invalid-response`, `unknown`), a retryability hint, and a message trimmed to 300 chars with credential-shaped substrings redacted. Headers and bodies never survive the adapter.
- **Fake provider.** In-process, offline, scriptable per purpose/model/call-count with ordered failures; records a call log for assertions.
- **Retry/fallback.** `withRetries` retries only retryable failures (rate limits, 5xx, network, timeouts) with exponential backoff plus jitter, at most 3 attempts per route; cancellation and auth fail fast. The agent loop then tries the route chain's fallbacks in order.
- **What gets recorded.** `model.requested` / `model.responded` (provider, model, purpose, optional role, usage, tool-call count, duration) / `model.failed` (kind, redacted message, attempts). Keys, headers, and request bodies are never recorded.

## Tools and safety

`src/tools/registry.ts` defines `ToolDefinition`: a kebab-case name, a `read`/`write`/`execute` capability, a Zod `argsSchema` (exposed to providers as JSON Schema with `additionalProperties: false`), a timeout, and an output cap. The registry is the only tool surface the agent loop uses.

The default registry (`createDefaultToolRegistry`) registers ten tools: `read-file`, `list-dir`, `search-text`, `git-status`, `git-diff`, `git-log`, `repo-instructions` (read); `write-file`, `apply-patch` (write); `run-command` (execute).

- **Path guard.** `resolveWorkspacePath` rejects absolute paths, drive letters, `~`, `..`, and NUL bytes, then realpaths the workspace root and the deepest existing ancestor of the target and re-checks containment, so symlinks cannot escape.
- **Create-only writes, conflict-detecting patches.** `write-file` refuses to overwrite (atomic temp + hardlink, `EEXIST` refused). `apply-patch` parses the whole unified diff, applies it in memory, requires exact context matches, refuses deletions/renames, re-stats targets before writing, and writes all files or none.
- **Bounded processes.** `spawnBounded` always uses argument arrays, detaches the child into its own process group, kills the group on timeout/abort (SIGTERM, escalating to SIGKILL after 2s), bounds collected stdout/stderr (head+tail caps), and builds the child environment from an explicit allowlist. `run-command` rejects shell metacharacters in the command name, clamps caller timeouts to the configured maximum, and refuses secret-shaped env names.
- **Redaction.** All tool output passes through `redactSecrets` (secret-named env values plus known token shapes) and `boundOutput` before it reaches the model, the transcript, or the event log.
- **Permission gate order.** `evaluateToolPermission` (`src/policies/permissions.ts`) checks, in order: paused workflow → mode-permitted capability → write rules (`canModifyUserFiles`) → autonomy (`off` denies non-read tools) → command denylist for execute tools (`DENIED_COMMAND_PATTERNS`: git push/commit/reset --hard/clean/stash/checkout-over-paths, npm publish, `rm -rf`, mkfs, `dd of=`, `curl|sh`, `chmod -R 777`). Denials are recorded as `tool.denied` events.

The agent loop (`src/runtime/agent-loop.ts`) validates every model tool call against the tool's Zod schema before execution, reports validation issues back to the model, answers hallucinated tool names with the list of real tools, aborts after the repeated-action limit (identical tool + arguments executed too many times), and streams text deltas to the caller live.

## Budgets and cancellation

`BudgetTracker` (`src/runtime/budgets.ts`) tracks six dimensions against configured limits: turns, model calls, tool calls, output tokens, wall-clock time (plus the repeated-action limit enforced by the loop). The loop checks budgets before every turn and after every tool execution; exhaustion ends the run with a `budget-exhausted` status, records one `budget.exhausted` event per budget, and pauses the workflow in state so the next run resumes rather than restarts.

`CancellationToken` (`src/runtime/cancellation.ts`) wraps `AbortController` with a human-readable reason. Library code never binds process signals directly; the CLI decides when a run is canceled. The token propagates into model requests and tool contexts; subprocesses are killed as a group. An interrupted run records `run.interrupted` and leaves resumable state — restarting Relay reconstructs identical state from the event log and continues.

## Testing strategy

All suites run offline; no test touches the network.

| Suite | What it verifies |
| --- | --- |
| `test/unit/core` | Event schema contracts, reducer replay (including corrupt logs), transition policy gates |
| `test/unit/policies` | Permission gate ordering and denylist, intervention rules, policy transitions |
| `test/unit/tools` | Path-guard escapes, patch conflicts and atomicity, redaction, bounded spawn |
| `test/unit/providers` | Adapter normalization and error mapping via mocked transports, `collectTurn`, fake provider |
| `test/unit/runtime` | Agent-loop control flow, routing/fallbacks, budget exhaustion, cancellation, retries |
| `test/unit/storage` | Migrations, append-only enforcement, event-store validation, project repository |
| `test/unit/artifacts` | Draft validation, deterministic rendering, Markdown store versioning, invalidation |
| `test/unit/cli`, `test/unit/tui` | Argument parsing and terminal components |
| `test/integration/replay` | Restart reconstructs identical state |
| `test/integration/workflow` | Full seven-mode workflow over the deterministic fake provider |

The fake provider is the key to determinism: tests script responses (and ordered failures) per purpose and assert routing, retries, and cancellation without keys or connectivity.

## Extension points

- **New provider adapter.** Implement `ModelProvider` from `src/providers/provider.ts`, normalize errors through `toProviderFailure`, and add the name to `PROVIDER_NAMES` in `src/config` so model roles can reference it. Nothing outside the adapter changes.
- **New tool.** Define a `ToolDefinition` (Zod args, capability, timeout, output cap) and register it in `createDefaultToolRegistry`. Mode capability lists and the permission policy gate it automatically.
- **New mode or artifact kind.** Extend `WORKFLOW_MODES`/`MODE_CONTRACTS` and `ARTIFACT_KINDS`/`ARTIFACT_DEPENDENCIES` together (plus a draft schema, renderer section, and mode-entry gates). The dependency graph and invalidation derive from these tables, never from hardcoded lists.
- **Configuration layers.** Add or adjust layers in `src/config/load.ts`; see [configuration.md](configuration.md) for the current precedence.
