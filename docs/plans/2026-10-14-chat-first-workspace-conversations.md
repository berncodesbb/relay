# Chat-first workspace conversations implementation plan

Status: implemented 2026-10-14 (working tree; not yet committed). Decision
notes: first-prompt-initialization is goal-less by design (an informational
turn creates the project record + conversation only); the misspelling guard
applies to single-word command-like tokens so natural prompts such as
"explain this repository" remain prompts; `--help`/`--version` now truly win
from any position (a value flag can no longer consume them); the
work-dashboard driver was replaced by the chat driver (`relay tui` is a pure
alias; work views remain reachable as chat overlays).  
Date: 2026-10-14  
Primary architecture reference: `docs/plans/2026-10-13-intent-first-rust-work-graph.md`  
Related plan: `docs/plans/2026-10-14-default-interactive-chat-interface.md`

## Objective

Make the Rust Relay CLI feel like a workspace-scoped conversational development
tool when launched from a terminal:

```text
cd /path/to/project
relay
```

The bare command must treat the selected directory as the workspace, open or
resume a durable conversation, and present a chat-first Ratatui interface in
which the user can ask questions, describe outcomes, answer decisions, and
observe bounded work activity. Existing work-graph screens remain available as
secondary views.

Workspaces, conversations, and goals are separate concepts:

- a workspace is the canonical local directory and its Relay state;
- a conversation is an ordered, replayable exchange within that workspace;
- a goal is durable work that the user has asked Relay to accomplish.

Opening Relay or asking a read-only question must not invent a goal. A first
prompt such as "explain this repository" can receive a bounded read-only answer;
a prompt such as "fix the failing tests" may result in a proposed goal and work,
but only after deterministic application validation. The model may classify or
propose intent; it never gains authority to mutate the graph.

This plan refines `2026-10-14-default-interactive-chat-interface.md`. It retains
that plan's Rust-only chat UI, durable transcript, `--print` mode, terminal
safety, and typed application boundary, but supersedes its rule that every first
prompt is automatically the project goal.

## Current codebase baseline

- Rust is the sole implementation. The retired TypeScript/Node.js/Ink tree is
  deleted and must not be restored.
- `crates/relay-cli/src/args.rs` currently requires an explicit command. It has
  work-centric commands plus `relay tui`; a bare command is a usage error.
- `crates/relay-cli/src/dispatch.rs` maps parsed commands onto `relay-app`
  handlers. `crates/relay-cli/src/tui.rs` owns raw mode, alternate-screen setup,
  event polling, terminal restoration, and typed TUI request dispatch.
- The active UI is `crates/relay-ui/src/work/**`. It is a work-graph dashboard
  over immutable application views, not a chat editor. The compatibility
  `crates/relay-ui/src/tui/**` module and `crates/relay-core` /
  `crates/relay-runtime` are frozen reference material and must not be extended.
- `relay-app::ProjectSession` is the checked application/store boundary.
  Application handlers operate over the folded work graph and append validated
  facts through short store operations.
- `relay-domain::DomainFact` and the accepted `.relay-graph` JSONL stream are
  authoritative. SQLite and rendered views are rebuildable projections.
- `relay-providers` already defines Relay-owned model requests, normalized
  streaming events, and cancellation tokens. Provider SDK or wire types must
  remain private to adapters.
- The current work graph models one declared goal. The implementation must not
  coerce informational conversation into that goal merely to open a session.
- The working tree is substantially dirty, including the TypeScript retirement
  and ongoing Rust changes. Every pre-existing modification and deletion is
  user-owned.

## Product contract

### Invocation behavior

| Invocation | Interactive TTY | Redirected or non-TTY |
| --- | --- | --- |
| `relay` | Open or resume chat in the current workspace | Usage error; never enter raw mode |
| `relay "<prompt>"` | Open chat and submit the prompt once | Reject with guidance to use `--print` |
| `relay --workspace <path>` | Open or resume chat in the selected workspace | Usage error; never enter raw mode |
| `relay --print "<prompt>"` | Run one bounded turn, print the response, exit | Same stable scripted behavior |
| `relay tui` | Compatibility alias for interactive chat | Actionable TTY error before raw mode |
| Existing subcommands | Preserve current human/JSON behavior | Preserve current behavior |

Additional parsing rules:

- `--help` and `--version` continue to win from any position.
- Known commands always win over prompt fallback.
- Unknown option-like input is a usage error, never prompt text.
- Misspelled command-like input must not silently become a mutating prompt.
- `--json` remains for structured commands; conversational scripting uses
  `--print` with a stable plain response contract unless an explicit future
  machine-readable turn schema is introduced.
- Resolve the workspace to an absolute normalized path before state access and
  display it prominently in the chat header.

### Fresh workspace behavior

1. Opening and quitting a fresh workspace creates no `.relay-graph` files.
2. The initial screen explains the workspace boundary and invites a prompt.
3. Empty input does nothing.
4. The first meaningful prompt starts a conversation turn, not automatically a
   goal.
5. An informational/read-only turn may answer without declaring a goal or work.
6. An actionable turn may propose a typed goal/work operation. Existing
   application and domain validation must accept it before it becomes durable.
7. If the system needs a consequential product choice, it records or surfaces a
   decision rather than guessing.
8. If initialization or durable message recording fails, preserve the editor
   draft, report the error, and do not show a false durable transcript entry.

### Conversation behavior

- Present one chronological transcript containing user messages, assistant
  messages, compact activity, decision requests, cancellations, failures, and
  completion summaries.
- Assign explicit conversation, turn, and message identities. Do not encode the
  assumption that a workspace has one endless transcript, even if the first UI
  release exposes only the most recent conversation.
- Resume the latest active conversation by default. Keep the data and application
  contracts capable of listing and resuming other conversations later without a
  fact-format rewrite.
- Persist final durable messages and terminal turn outcomes, not character-level
  stream deltas or hidden chain-of-thought.
- Stream bounded assistant text and activity to the live UI, then append the
  final assistant message or terminal failure/cancellation fact.
- On restart, derive an interrupted state for a started turn without a terminal
  fact. Never invent an assistant response.
- Keep transcript context and rendering bounded by explicit message, byte, tool
  output, and token limits.
- Redact secrets before any tool summary, activity entry, transcript fact, log,
  fixture, or rendered view becomes durable.

### Goal and action behavior

The conversation service must return a typed result rather than letting raw
model text control state. At minimum, a turn can:

- answer without changing the work graph;
- propose a goal when no goal exists;
- propose typed work, decision, or bounded-run requests against an existing
  graph;
- answer one exact open decision;
- request clarification;
- fail or be cancelled.

Each proposed mutation is validated and applied independently through existing
application/engine services. A rejected proposal is reported in the transcript
with a safe explanation; it is never coerced into a valid mutation. Existing
capability grants, workspace containment, scheduling policy, approval rules,
and delivery holds remain authoritative.

### Chat interface behavior

The primary layout is chat-first:

```text
+ Relay · /absolute/workspace/path -------- ready / working / awaiting you +
| transcript                                                               |
| You                                                                      |
|   Find why checkout duplicates an order and fix it.                      |
|                                                                          |
| Relay                                                                    |
|   I am tracing the checkout and retry paths.                             |
|   * completed: inspect checkout handlers                                 |
|   * decision: choose the idempotency boundary                            |
+--------------------------------------------------------------------------+
| > editable prompt                                                        |
+ /help · Enter send · Ctrl-J newline · Ctrl-C cancel/exit ----------------+
```

- Support Unicode-safe multiline editing, cursor movement, backspace/delete,
  home/end, selection-safe bracketed paste, draft preservation, local history,
  and transcript scrolling.
- `Enter` submits a single-line prompt. Use and document one unambiguous
  multiline binding such as `Ctrl-J`; tests pin the selected binding.
- While a turn runs, continue rendering progress. Disable submission with an
  explicit busy state unless queued input is implemented with strict ordering.
  Never silently discard input.
- `Ctrl-C` cancels the active turn or attempt first; a subsequent `Ctrl-C` may
  force exit. When idle, `Ctrl-C` exits.
- Preserve editor text and transcript position while opening overlays.
- Keep existing summary, ready, blocked, decisions, work detail, and artifact
  views reachable through overlays or slash commands.
- Minimum slash commands: `/status`, `/work`, `/decisions`, `/inspect <id>`,
  `/help`, and `/quit`. They translate to typed requests; they do not implement
  domain policy.
- Narrow terminals degrade to a compact header, transcript, input, and status
  line. Color is optional enhancement, never the only signal.

## Scope

### In scope

- Bare-command and prompt-form TTY invocation.
- Explicit `--print` one-turn mode for scripts and redirected output.
- Lazy workspace/conversation creation with no eager state writes.
- Durable versioned conversation/turn/message facts and deterministic replay.
- Multiple-conversation-capable identities and projections, with latest
  conversation resume as the initial UI behavior.
- An application-level conversational turn service with bounded graph,
  repository, and transcript context.
- Typed, validated action proposals separated from assistant prose.
- Provider-neutral streaming, cancellation, final response collection, and
  deterministic fake-provider fixtures.
- Chat-first Ratatui UI integrated with existing work-graph views.
- Input editing, history, paste handling, scrolling, cancellation, resize, and
  terminal restoration.
- Focused domain, store, application, provider, UI, CLI, recovery, and security
  tests.
- README and architecture/event-format documentation after behavior exists.

### Out of scope

- Restoring or extending TypeScript, Node.js, React, Ink, npm, or Vitest.
- Extending `crates/relay-core`, `crates/relay-runtime`, or the compatibility
  `crates/relay-ui/src/tui/**` implementation.
- Replacing work-graph readiness, dependency, scheduling, invalidation, grant,
  artifact, or decision policy.
- A global workflow mode, global pause state, monolithic controller, or mutable
  chat database.
- Autonomous multi-agent execution inside Relay.
- Git mutation, commits, pushes, PR creation, deployment, publishing, or
  unattended delivery.
- Browser/desktop automation, hosted sync, accounts, collaboration, telemetry,
  semantic indexes, vector databases, or a plugin system.
- Full-screen diff editing, IDE features, or mouse-first interaction.
- Persisting chain-of-thought, raw provider payloads, credentials, unredacted
  tool output, or unbounded repository content.
- Shipping conversation list/resume commands in the first release; the storage
  and application contracts must merely avoid preventing them.

## Proposed architecture

### Authority and turn flow

```text
terminal input
  -> relay-cli invocation and TTY policy
  -> relay-ui editor and typed ChatRequest
  -> relay-app conversation service
       -> open/create a conversation without inventing a goal
       -> append the accepted user-message / turn-start facts
       -> build bounded context from transcript + graph + safe repository views
       -> release the writer lock
       -> call the provider and stream ephemeral UI events
       -> decode assistant prose plus typed proposed actions
       -> validate/apply each proposed action through application/engine services
       -> append assistant-message and terminal turn facts
  -> refresh immutable ChatSnapshot and work overlays
  -> render transcript, activity, decisions, and prompt
```

No project writer lock may be held across a model request, tool execution, or
terminal wait. Persist a durable fact before reporting its operation successful.

### Durable facts and projections

Add explicit versioned domain records. Exact names may be refined by the domain
owner, but the fact model must distinguish:

- conversation creation/selection identity;
- user message accepted;
- turn started;
- assistant message completed;
- turn failed;
- turn cancelled.

Every message includes stable IDs, conversation/turn association, bounded text,
and caller-supplied time. Assistant records may contain references to accepted
attempts, decisions, work, or artifacts, but not provider wire payloads.

The pure graph/project fold should preserve enough ordered conversation metadata
for deterministic validation and recovery. If bounded transcript retrieval
needs an index, add it as a rebuildable SQLite projection derived from JSONL.
JSONL remains authoritative.

Conversation facts must coexist with a project that has no goal. Update
invariants narrowly: work declaration still requires the appropriate goal and
workflow prerequisites, but conversation creation and informational turns do
not. Do not weaken unrelated graph validation.

### Conversational model contract

Define Relay-owned request/result structures above provider adapters. The
result separates visible assistant text from typed proposals, for example:

```text
ConversationTurnResult
  assistant_text
  proposals[]
    AnswerOnly
    DeclareGoal
    DeclareOrExpandWork
    ResolveDecision
    RunReadyWork
    RequestClarification
  usage
  finish_reason
```

This is illustrative, not a required enum spelling. The selected schema must be
runtime-validated, versioned where persisted, and capable of partial proposal
rejection. Context construction must use explicit budgets and include only safe,
relevant, bounded repository and graph information. Model text alone never
constitutes an accepted graph mutation.

### UI state

Create a new chat-first module under `crates/relay-ui/src/chat/**`, or evolve
`work` behind a single chat-first facade while keeping work overlays modular.
Do not build on the frozen compatibility `tui` module.

The UI state machine owns presentation state only:

- immutable `ChatSnapshot` and work-overlay views;
- editor buffer, cursor, selection/paste state, and local history;
- transcript scroll and overlay focus;
- busy/cancelling status and ephemeral stream/activity entries;
- typed requests sent to the CLI/application driver.

It does not classify intent, resolve decisions, decide readiness, grant tools,
or directly mutate the store.

## Affected files and ownership

Exact file splits may be refined, but team ownership must remain non-overlapping.
Shared contracts are changed by their designated owner and consumed by other
owners only after coordination.

### Domain owner — conversation facts and pure validation

- `crates/relay-domain/src/facts.rs` — add versioned conversation/turn/message
  facts.
- `crates/relay-domain/src/ids.rs` — add validated conversation, turn, and
  message IDs if existing ID facilities are insufficient.
- `crates/relay-domain/src/graph.rs` and/or a new pure conversation projection
  module — fold and validate conversation ordering, duplicates, terminal turn
  state, and goal-independent conversations.
- `crates/relay-domain/src/lib.rs` — export stable contracts only.
- `crates/relay-domain/tests/**`, `crates/relay-engine/tests/**`, and focused
  property/scenario tests — replay, order independence where applicable,
  duplicate IDs, interrupted turns, and preservation of goal/work invariants.

The domain owner must not add IO, clocks, provider types, SQL, or terminal code.

### Storage owner — codecs, indexes, replay, and recovery

- `crates/relay-store/src/fact/graph.rs` and related fact codecs — encode/decode
  the new domain facts while preserving fail-closed version handling.
- `crates/relay-store/src/fact/projection.rs` and/or
  `crates/relay-store/src/projection.rs` — add rebuildable bounded conversation
  indexes only if necessary.
- `crates/relay-store/src/store.rs` — expose bounded chronological retrieval
  through an existing store seam without making SQLite authoritative.
- `crates/relay-tests/tests/fact_replay.rs`, `crash_recovery.rs`, and focused
  store tests — rebuild equivalence, interrupted turns, append ordering,
  corruption behavior, and legacy fact compatibility.

The storage owner must not introduce an independent mutable transcript file or
database authority.

### Application/runtime owner — conversational orchestration

- `crates/relay-app/src/conversation.rs` (new) — typed request/result contracts,
  conversation selection/creation, bounded context building, first-turn behavior,
  proposal validation/application, final fact recording, and cancellation.
- `crates/relay-app/src/conversation_views.rs` (new) or
  `crates/relay-app/src/views.rs` — stable transcript, activity, prompt,
  decision, and turn views.
- `crates/relay-app/src/session.rs` — narrow conversation append/read/snapshot
  methods while preserving checked append and short writer locks.
- `crates/relay-app/src/lib.rs` — export the new application service and views.
- `crates/relay-app/src/providers.rs` and `crates/relay-config/**` — route
  conversational purposes through existing provider/model configuration rather
  than hard-coding policy in the UI.
- `crates/relay-executor/**` — only the cancellation, bounded event, and
  capability-controlled execution changes required by conversational turns.
- `crates/relay-providers/src/contract.rs`, `collect.rs`, `fake.rs`, `openai.rs`,
  and `anthropic.rs` — normalized streaming/final response behavior only where
  the present contract is insufficient; keep adapter wire types private.
- `crates/relay-app/tests/**` and provider/executor tests — fresh informational
  turn, actionable turn, existing-goal turn, exact decision response, proposal
  rejection, provider failure, cancellation, bounded context, secret redaction,
  and no-lock-across-model-call coverage.

The application/runtime owner coordinates the proposal schema with the domain
owner before implementation and must not modify frozen compatibility crates.

### UI owner — chat state, editor, transcript, and overlays

- `crates/relay-ui/src/chat/mod.rs` (new) — chat snapshot, presentation-only
  state machine, and typed UI requests.
- `crates/relay-ui/src/chat/editor.rs` (new) — Unicode-safe multiline editor,
  history, cursor movement, delete behavior, and paste handling, unless a small
  reviewed dependency is selected.
- `crates/relay-ui/src/chat/transcript.rs` (new) — bounded transcript and
  ephemeral activity rendering helpers.
- `crates/relay-ui/src/chat/keymap.rs` and `screens.rs` (new) — typed actions,
  responsive chat layout, scrolling, help, busy/cancel states, and overlays.
- `crates/relay-ui/src/work/**` — only changes required to expose existing work
  screens as reusable overlays. Preserve current work-screen behavior and tests.
- `crates/relay-ui/src/lib.rs` — export the new primary interface facade.
- `crates/relay-ui/tests/tui_work_graph.rs` and new chat-focused tests — editor,
  paste, history, scrolling, snapshots, narrow widths, resize, overlays,
  cancellation, and draft preservation.
- `crates/relay-ui/Cargo.toml` — only narrowly justified text/editor
  dependencies.

The UI owner must not edit `crates/relay-ui/src/tui/**` or add policy decisions.

### CLI owner — grammar, invocation policy, and terminal driver

- `crates/relay-cli/src/args.rs` — default chat command, prompt fallback,
  `--print`, help text, ambiguity rules, and preserved existing grammar.
- `crates/relay-cli/src/dispatch.rs` — route chat/print requests through the
  application service while preserving every existing command and exit code.
- `crates/relay-cli/src/tui.rs` — lazy fresh-session handling, background turn
  work, event feed, cancellation, resize/paste decoding, reliable restoration,
  and typed request translation.
- `crates/relay-cli/src/main.rs` and `src/lib.rs` — pass terminal capability
  through a testable invocation seam; keep pure parsing independent of live TTY
  reads.
- `crates/relay-cli/tests/cli_work_graph.rs` and new focused driver tests — bare,
  prompt, `--print`, command ambiguity, non-TTY behavior, existing-command
  regression, exit codes, and terminal restoration.
- `crates/relay-cli/Cargo.toml` — add async/channel dependencies only if the
  driver design requires them.

### Lead integrator — shared manifests, docs, and final review

- Root `Cargo.toml` and `Cargo.lock` — coordinate dependency changes after
  owners agree on the minimum required set.
- `README.md` — make bare `relay` the quick start and document `--print`,
  workspace/conversation/goal semantics, provider setup, keys, slash commands,
  resume behavior, and privacy limits.
- `docs/architecture.md` — document the final conversation data flow and
  ownership boundary after implementation exists.
- `docs/event-format.md` — document the accepted fact wire format and recovery
  behavior after it is implemented.
- `docs/plans/2026-10-14-chat-first-workspace-conversations.md` — update status
  and decision notes only for material implementation departures.
- `.github/workflows/ci.yml` — change only if new focused tests need explicit CI
  coverage beyond workspace checks.
- `CLAUDE.md` — no change expected. Update only if implementation establishes a
  fundamental durable engineering rule not already captured there.

## Phased implementation

### Phase 0 — preserve the baseline and pin invocation behavior

1. Read `CLAUDE.md`, this plan, the authoritative intent-first plan, and the
   Rust-only retirement plan.
2. Inspect `git status --short` and diffs for every file an owner will touch.
   Record overlapping user changes and coordinate rather than overwriting them.
3. Run focused baseline tests for `relay-domain`, `relay-store`, `relay-app`,
   `relay-providers`, `relay-ui`, and `relay-cli`; distinguish pre-existing
   failures from regressions.
4. Add a table-driven invocation contract covering bare TTY/non-TTY, prompt
   TTY/non-TTY, `--print`, workspace selection, help/version precedence, known
   commands, JSON commands, malformed flags, and misspelled command-like input.
5. Agree on conversation IDs, fact names, proposal schema, transcript limits,
   and TTY capability seam before parallel implementation changes shared types.

Exit criterion: baseline and parser expectations are recorded, and shared
contracts have one designated owner each.

### Phase 1 — domain facts and deterministic replay

1. Add validated conversation, turn, and message identity types.
2. Add durable facts for conversation start, accepted user message/turn start,
   completed assistant message, failure, and cancellation.
3. Define legal ordering and idempotency rules. Reject duplicate identities,
   multiple terminal outcomes, assistant completion without a started turn, and
   cross-conversation references.
4. Permit conversation facts before goal declaration without permitting work
   that violates existing goal/work prerequisites.
5. Fold the new facts into a deterministic conversation projection with enough
   state to identify the latest conversation and interrupted turns.
6. Add property/scenario tests for replay, duplicates, ordering, interruption,
   and unchanged work-graph validation.

Exit criterion: an informational conversation can replay in a project with no
goal, while all existing domain scenarios still pass.

### Phase 2 — storage codecs, bounded reads, and recovery

1. Encode/decode the new facts using the existing versioned envelope rules.
2. Add bounded chronological transcript access. Prefer replay-derived state;
   add a disposable SQLite index only when measurement or API needs justify it.
3. Ensure accepted facts are durable before projections or success reporting.
4. Rebuild projections from JSONL and derive interrupted turns consistently.
5. Verify older projects and migrated legacy projects open unchanged.
6. Add crash points around user-message, assistant-message, and terminal-turn
   appends; prove recovery never invents or loses an accepted terminal record.

Exit criterion: transcript state rebuilds exactly from authoritative facts and
legacy stores remain readable.

### Phase 3 — conversation application service

1. Implement typed conversation requests, results, views, and limits.
2. Open or select the latest conversation without creating a goal.
3. Record the accepted user message/turn start under a short writer lock, then
   release it before provider or tool work.
4. Build bounded context from recent transcript, graph summary, open decisions,
   relevant artifacts/work, and explicitly safe repository inspection.
5. Ask the provider for visible prose plus typed proposals using Relay-owned
   contracts and runtime validation.
6. Apply proposals independently through existing application/engine services.
   Report accepted and rejected proposals explicitly.
7. Treat goal declaration as one possible validated proposal, not automatic
   first-prompt initialization.
8. Resolve a decision only when the proposal identifies one exact open decision
   and existing validation accepts the response.
9. Persist the final bounded assistant message and terminal turn outcome.
10. Support cancellation and failures without holding locks or fabricating
    success.
11. Add deterministic fake-provider scenarios for informational, actionable,
    clarification, decision, partial-rejection, cancellation, and failure turns.

Exit criterion: the application service handles both read-only and actionable
first turns, and no model text bypasses typed validation.

### Phase 4 — provider streaming and execution events

1. Reuse existing normalized provider events and cancellation token wherever
   sufficient.
2. Add only the missing Relay-owned streaming/final collection contracts.
3. Bound deltas, accumulated text, tool summaries, usage, and errors.
4. Keep provider SDK types private and normalize finish reasons consistently.
5. Feed capability-controlled attempt activity into bounded application events;
   do not let chat invoke arbitrary tools directly.
6. Verify redaction occurs before durable transcript/activity recording.

Exit criterion: live and fake providers expose the same bounded event contract,
with deterministic offline tests and no leaked adapter types or secrets.

### Phase 5 — chat-first Ratatui state and screens

1. Build the presentation-only chat state machine and immutable snapshot.
2. Implement the multiline Unicode-safe editor, history, paste, and cursor
   behavior with focused unit tests.
3. Render the workspace header, scrollable transcript, compact activity,
   decision prompts, editor, and status/footer.
4. Stream ephemeral progress while busy and reconcile it with final durable
   snapshots without duplicating messages.
5. Integrate work summary, ready/blocked work, decisions, detail, and artifact
   screens as overlays or secondary views.
6. Implement minimum slash-command parsing into typed requests.
7. Preserve drafts and scroll position across overlays and resize.
8. Add snapshot tests for empty/fresh/resumed, informational/actionable,
   decision, busy/cancelling, error, long transcript, long line, Unicode, and
   narrow-terminal states.

Exit criterion: `relay-ui` can be fully tested on `TestBackend` without a real
TTY and contains no orchestration or domain policy.

### Phase 6 — CLI invocation, print mode, and terminal lifecycle

1. Add default chat and prompt variants without breaking existing commands.
2. Add `--print` and reject ambiguous or unsafe prompt fallback.
3. Inject terminal capability into dispatch so parser tests stay pure.
4. Make `relay tui` an alias for the same chat driver.
5. Support an unopened/fresh workspace without eager state creation.
6. Run model/application work outside the terminal event loop and deliver
   bounded events through a channel or equivalent driver seam.
7. Decode resize, bracketed paste, key modifiers, and cancellation correctly.
8. Restore cursor, raw mode, alternate screen, and panic hooks after normal
   exit, initialization failure, provider failure, cancellation, forced exit,
   and panic.
9. Implement `--print` as one bounded application turn with stable stdout and
   diagnostics on stderr.
10. Add fake event-source/sink tests so lifecycle and cancellation behavior do
    not require a live terminal.

Exit criterion: the invocation table from Phase 0 passes and every exit path
restores the terminal.

### Phase 7 — integration, documentation, and regression review

1. Integrate owner branches/changes in dependency order: domain, storage,
   application/provider, UI, then CLI.
2. Resolve shared-file changes through the designated owner; do not accept
   parallel edits to root manifests, `lib.rs` export surfaces, or shared enums
   without review.
3. Run end-to-end offline scenarios:
   - open/quit fresh workspace without creating state;
   - first read-only question with no goal;
   - first actionable request producing validated goal/work;
   - resume latest conversation after restart;
   - existing project question and bounded work request;
   - explicit decision resolution;
   - rejected proposal;
   - cancellation and interrupted-turn recovery;
   - `--print` on TTY and non-TTY;
   - existing structured commands and legacy mappings.
4. Review security boundaries: containment, grants, redaction, environment
   allowlist, no-overwrite writes, patch atomicity, read-only Git, and delivery
   holds.
5. Update README and architecture/event-format docs to match implemented
   behavior, not planned behavior.
6. Run all required checks and inspect the final diff for accidental edits,
   restored retired files, generated artifacts, or user-change loss.

Exit criterion: all acceptance criteria and checks pass, or remaining failures
are reported with evidence as pre-existing or unresolved.

## Engineering constraints

- Follow `CLAUDE.md`; Rust is the sole implementation and the pinned Cargo
  workspace/toolchain is mandatory.
- Preserve all existing user changes. Never reset, overwrite, absorb, commit,
  or push them.
- Do not modify frozen `crates/relay-core`, `crates/relay-runtime`, or
  `crates/relay-ui/src/tui/**` code.
- Keep `relay-domain`, `relay-engine`, and `relay-workflows` pure.
- JSONL accepted facts remain the sole authority; SQLite and rendered transcript
  views are rebuildable.
- Persist durable facts before success reporting. Fail closed on unknown/newer
  authoritative records.
- Never hold the writer lock across provider calls, tools, terminal waits, or
  other long work.
- Provider SDK types remain private. Validate all model-produced structures and
  tool arguments at runtime.
- Models propose; deterministic application/domain code validates and applies.
- Preserve workspace containment, symlink defense, no-overwrite creation,
  atomic/context-checked patches, bounded redacted output, process argument
  arrays, environment allowlists, read-only Git, and delivery holds.
- Never persist credentials, chain-of-thought, raw provider payloads, or
  unredacted/unbounded tool output.
- Offline deterministic tests are the default; live-provider tests are explicit
  opt-ins.
- Avoid broad dependencies. Any editor, async, or channel dependency must be
  justified against a small in-tree implementation and coordinated through the
  lead integrator.
- Library crates do not print or control terminals. Only `relay-cli` owns
  stdout/stderr and terminal lifecycle.
- Keep modules small and ownership explicit. Parallel work may proceed only on
  non-overlapping files after shared contracts are agreed.
- Do not update `CLAUDE.md` unless implementation creates a new fundamental,
  durable engineering rule. This plan does not require such an update.
- Do not commit or push.

## Acceptance criteria

### Invocation and workspace

- On a TTY, bare `relay` opens chat in the canonical current directory.
- `--workspace` selects and displays the canonical requested workspace.
- Opening/quitting a fresh workspace without input creates no state.
- Bare or prompt-form conversational invocation on non-TTY fails safely with
  `--print` guidance and never enters raw mode.
- `relay --print "<prompt>"` works consistently on TTY and non-TTY.
- Help, version, all existing structured commands, JSON output, exit codes, and
  legacy mappings retain their existing behavior.
- Unknown flags and command-like typos never silently become prompts.

### Conversation, goal, and authority

- A first read-only question can complete durably without declaring a goal.
- A first actionable request creates goal/work only through validated typed
  proposals.
- Conversation, turn, and message IDs replay deterministically and support more
  than one conversation per workspace.
- Restart resumes the latest conversation with its bounded durable transcript.
- A started turn without a terminal fact renders as interrupted after recovery.
- Assistant prose cannot mutate graph state.
- Accepted and rejected proposals are distinguishable in the final result.
- Decision answers apply only to one exact open decision and pass existing
  validation.
- Existing work-graph, capability, artifact, approval, and delivery policy is
  unchanged.

### Persistence and safety

- JSONL facts alone rebuild conversation and graph state; SQLite can be removed
  and rebuilt without changing results.
- Older and migrated workspaces open without rewriting or deleting legacy
  state/backups.
- No writer lock spans a model call, tool execution, or terminal wait.
- Message, context, stream, activity, output, and rendering limits are enforced.
- Secrets and sensitive output are redacted before persistence or display.
- Cancellation and provider/tool failure record truthful terminal state without
  fabricated assistant output or duplicate application.
- Workspace and tool security tests continue to pass.

### Interface and terminal lifecycle

- Transcript, editor, activity, decisions, overlays, and status render legibly
  at normal and narrow widths without depending on color.
- Editing supports Unicode, multiline input, movement, delete/backspace,
  home/end, bracketed paste, local history, and draft preservation.
- Long transcripts and messages remain bounded and scrollable.
- Busy input is explicitly disabled or strictly queued; it is never dropped.
- Work summary, work lists, decisions, detail, and artifact views remain
  accessible.
- `Ctrl-C` cancels active work before exit and exits when idle.
- Raw mode, cursor, alternate screen, and panic hooks are restored on every
  normal and abnormal exit path.
- UI and terminal-driver tests run without requiring an actual TTY.

### Repository hygiene

- No TypeScript/Node.js implementation or tooling is restored.
- Frozen reference crates/modules remain unchanged.
- Pre-existing user changes are preserved.
- Documentation matches the final implementation.
- No commits or pushes are made.

## Verification commands

Run focused checks during development:

```bash
cargo test -p relay-domain
cargo test -p relay-engine
cargo test -p relay-workflows
cargo test -p relay-store
cargo test -p relay-app
cargo test -p relay-providers
cargo test -p relay-executor
cargo test -p relay-ui
cargo test -p relay-cli
cargo test --test fact_replay
cargo test --test crash_recovery
cargo test --test capability_security
cargo test --test scheduling_concurrency
cargo test --test cli_work_graph
cargo test --test tui_work_graph
```

Run migration/compatibility checks because fact and project initialization
semantics change:

```bash
cargo test -p relay-tests --test migration
cargo test -p relay-tests --test semantic_migration
cargo test -p relay-tests --test compatibility
cargo test -p relay-config --test legacy_compat
```

Run the complete repository gates before handoff:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo build --workspace --all-targets --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps --exclude relay-core --exclude relay-runtime
```

Also inspect repository hygiene and the final patch without changing state:

```bash
git status --short
git diff --check
git diff --stat
```

## Team execution guidance

Use one lead integrator and four non-overlapping ownership tracks, sized to the
available Claude Code team capacity:

1. **Domain/storage owner:** conversation IDs/facts, pure fold/validation,
   codecs, replay, projection, and recovery tests. If capacity permits, split
   storage into a separate owner only after domain contracts stabilize.
2. **Application/runtime owner:** conversation service, bounded context,
   proposal decoding/application, provider/executor streaming, cancellation,
   and redaction tests.
3. **UI owner:** chat state, editor, transcript, keymap, screens, work overlays,
   and `relay-ui` tests. This owner does not edit the CLI terminal driver.
4. **CLI/integration owner:** parser/dispatch, TTY seam, terminal driver,
   `--print`, invocation/lifecycle tests, shared manifests/docs, integration,
   and final verification.

Before parallel work, the lead must lock the ID/fact/view/proposal/event
contracts and assign ownership of shared `lib.rs`, Cargo manifests, and common
test fixtures. Independent implementation and tests can then proceed in
parallel. The lead integrates in dependency order, reviews every boundary for
authority and safety regressions, resolves shared-file edits, runs the full
verification matrix, and reports unresolved issues without committing or
pushing.
