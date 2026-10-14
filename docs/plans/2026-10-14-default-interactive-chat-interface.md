# Default interactive chat interface implementation plan

Status: implementation-ready (prerequisites updated 2026-10-14 for the
Rust-only repository created by `2026-10-14-remove-typescript-rust-only-codebase.md`)  
Date: 2026-10-14  
Primary architecture reference: `docs/plans/2026-10-13-intent-first-rust-work-graph.md`

## Objective

Make Relay feel like a workspace-scoped coding conversation when launched from a terminal:

```text
cd /path/to/project
relay
```

The bare command must resolve the current directory as the workspace, open or resume a durable conversational session, and present a Claude Code-like prompt where the user can describe an outcome, answer decisions, ask questions, and observe Relay's work. A new workspace must not require a separate `relay init`; the first meaningful prompt initializes the project through the existing validated application path. An initialized workspace must reopen with its durable transcript and current work-graph context.

This is an interface and application-service change over the intent-first Rust work graph. It must not reintroduce the frozen TypeScript controller, a global workflow mode, UI-owned policy, or model-owned authority. User and model messages become replayable local records; any graph mutation still occurs only through validated domain facts and existing application/engine policy.

## Repository instructions and current-state constraints

Read and follow `CLAUDE.md` and `docs/plans/2026-10-13-intent-first-rust-work-graph.md` before implementation.

- The Rust work-graph application is the target and the sole implementation. `crates/relay-core`, `crates/relay-runtime`, and the compatibility TUI under `crates/relay-ui/src/tui/**` are frozen reference material and must not be extended for this feature. The retired TypeScript tree no longer exists in the repository and must not be reintroduced.
- The current work-centric Ratatui surface lives in `crates/relay-ui/src/work/**`; the terminal driver is `crates/relay-cli/src/tui.rs`; CLI grammar and dispatch live in `crates/relay-cli/src/args.rs` and `crates/relay-cli/src/dispatch.rs`.
- `ProjectSession` and command handlers in `crates/relay-app/**` are the application boundary. The UI may issue typed requests but may not decide readiness, permissions, scheduling, transitions, or decision validity.
- Accepted domain facts in `.relay-graph` JSONL remain authoritative. SQLite and rendered transcript/view state are rebuildable projections. Do not introduce an independent mutable chat database or a UI-only history file.
- Persist accepted facts before showing a user or assistant message as durable. Never hold the project writer lock across model calls or tool execution.
- Models may classify or propose actions; deterministic application code validates them. Free-form chat must not bypass capability grants, workspace containment, decision gates, or delivery holds.
- Preserve dirty worktrees and all existing user changes. At implementation start, inspect `git status --short` and relevant diffs. Do not reset, overwrite, reformat, or absorb unrelated modifications.
- The working tree was already substantially modified when this plan was written. Treat every pre-existing modification and deletion as user-owned and coordinate before touching overlapping files.
- Do not update `CLAUDE.md` for this feature unless implementation introduces a fundamental, durable engineering rule not already covered there. The current instructions already assign interface ownership and define fact authority, model boundaries, concurrency, and required checks.
- Do not commit or push unless explicitly requested.

## Product contract

### Interactive invocation

| Invocation | Interactive TTY | Redirected/non-TTY |
| --- | --- | --- |
| `relay` | Open or resume chat in the current workspace | Preserve the current usage error; never enter raw mode |
| `relay "<prompt>"` | Open chat and submit the prompt once the terminal is ready | Reject with guidance to use `--print`; do not guess automation intent |
| `relay --workspace <path>` | Open or resume chat in the selected workspace | Preserve the current usage error |
| `relay --print "<prompt>"` | Run one bounded conversational turn, print the response, and exit | Same behavior; stable for scripts |
| `relay tui` | Remain a supported compatibility alias for interactive chat | Fail before terminal setup with an actionable TTY diagnostic |
| Existing subcommands | Keep their current human/JSON behavior | Keep their current behavior |

Known command names and flags always win over prompt fallback. `--help` and `--version` continue to win from any position. Unknown option-like tokens remain usage errors. Add parser tests that make command/prompt ambiguity explicit so misspelled commands do not silently perform mutations; at minimum, strings beginning with a known command prefix or containing command-only flags must return usage guidance rather than becoming prompts.

### First-run behavior

1. Resolve the selected workspace to an absolute, normalized path before opening state. Display it prominently in the header.
2. If `.relay-graph` contains an initialized project, load its transcript and current graph-derived summary.
3. If no initialized project exists, enter a welcome state without creating files.
4. On the first non-empty user prompt, record the prompt and initialize the project by calling the existing validated `relay_app::commands::init` path (or a thin application service that delegates to it). The first prompt is the goal; do not duplicate it as two independently visible user messages.
5. If initialization fails, retain the prompt in the input editor, show the error, and do not display a false durable transcript entry.
6. Empty input and opening/quitting a fresh workspace must not create `.relay-graph`.

### Conversation and action behavior

- Present one chronological transcript containing user messages, assistant text, compact activity entries, decision requests, errors, cancellations, and completion summaries.
- For an initialized project, submit free-form input to an application-level conversational turn service. That service builds bounded context from durable conversation records plus graph-derived views and returns a typed turn result.
- A turn may answer without changing the graph, propose one or more typed application actions, or request clarification. Proposed mutations must be applied individually through existing command/engine validation; rejected proposals must be reported rather than coerced.
- Existing work execution continues through `relay_app::runner` and capability-granted executors. The chat layer may request a bounded scheduling run but cannot execute arbitrary tools directly.
- Human decisions remain explicit. Optioned decisions render as selectable choices; free-form decisions accept text. A conversational answer may resolve a decision only when the turn service identifies the exact open decision and application validation accepts the choice.
- Tool and attempt activity is compact by default: show state, target/name, and a bounded summary. The transcript must never expose unredacted secrets or unbounded output.
- While a turn is running, keep rendering progress. Accept a queued follow-up only if the implementation can preserve strict ordering; otherwise disable submission with an explicit busy state. Do not silently discard input.
- `Ctrl-C` cancels an active turn or attempt first; a subsequent `Ctrl-C` may force exit. When idle, `Ctrl-C` exits. Every path restores cursor, raw mode, alternate screen, and panic hooks.

### Input and navigation behavior

- Support multiline editing, cursor movement, selection-safe paste/bracketed paste, backspace/delete, home/end, and local history traversal.
- `Enter` submits a single-line prompt; define and display an unambiguous multiline binding such as `Alt-Enter` or `Ctrl-J`.
- Preserve draft text when opening summary, work, decision, artifact, and help overlays.
- Keep work-centric views reachable from chat through slash commands or an overlay/key binding: `/status`, `/work`, `/decisions`, `/inspect <id>`, `/help`, and `/quit` are the minimum set.
- Existing command-line subcommands remain the automation surface; slash commands are interactive conveniences mapped to typed requests, not a second policy implementation.
- Bound in-memory and rendered history. Long transcripts must scroll without allowing one message, tool result, or Markdown artifact to exhaust terminal memory.

## Scope

### In scope

- Bare-command interactive detection and prompt-form invocation.
- Explicit `--print` one-turn mode for redirected/scripted use.
- Automatic first-prompt initialization without eager filesystem writes.
- Durable, replayable conversation records in the authoritative fact stream and a deterministic conversation projection.
- An application-level conversational turn service with Relay-owned request/result types and provider-neutral model access.
- A chat-first Ratatui layout integrated with existing work-graph summary, work, decision, detail, and artifact views.
- Multiline input, prompt history, scrolling, cancellation, bounded activity rendering, slash-command routing, resize handling, and reliable terminal restoration.
- Offline deterministic provider fixtures and focused unit, integration, and snapshot tests.
- Help text and README updates after behavior is implemented.

### Out of scope

- Changes to work-graph readiness, dependency, validation, scheduling, or capability policy.
- Extending or reviving the TypeScript/Ink implementation.
- A new global workflow mode, monolithic controller, or model-controlled state transition.
- Autonomous multi-agent execution inside Relay.
- Git mutation, commits, pushes, pull requests, deployment, publishing, or unattended delivery.
- Browser/desktop automation, hosted accounts, sync, collaboration, telemetry, semantic indexes, or vector databases.
- Full-screen diff editing, IDE features, mouse-first interaction, or a plugin system.
- Persisting hidden chain-of-thought, raw provider payloads, credentials, or unredacted tool output.

## Proposed design

### Authority and data flow

```text
terminal input
  -> relay-cli invocation/TTY policy
  -> relay-ui editor and typed ChatRequest
  -> relay-app conversational turn service
       -> record user-message fact
       -> build bounded context from graph + transcript projection
       -> provider-neutral model request (no writer lock held)
       -> validate proposed typed actions
       -> apply accepted actions through existing app/engine services
       -> record assistant-message / turn-result facts
  -> refresh immutable ChatSnapshot
  -> render transcript, activity, decisions, and prompt
```

Conversation records should be explicit versioned facts (names are illustrative and may be refined by the domain owner):

- `ConversationStarted { conversation, started_at_ms }`
- `UserMessageRecorded { conversation, message, text, at_ms }`
- `AssistantMessageRecorded { conversation, message, text, at_ms, related_attempts, related_decisions }`
- `ConversationTurnFailed { conversation, turn, safe_message, at_ms }`
- `ConversationTurnCancelled { conversation, turn, reason, at_ms }`

Do not store transient character-by-character stream deltas as authoritative facts. Stream them to the live UI, then persist the final bounded assistant message or a terminal failure/cancellation record. If the process dies before the terminal fact, startup should derive and display an interrupted turn rather than inventing an assistant answer.

The domain types and fold stay pure. The application service owns orchestration, context selection, model calls, and translation of proposed actions into existing handlers. The provider layer continues to expose Relay-owned contracts. The UI consumes immutable snapshots/events and emits typed requests only.

### UI layout

Use a chat-first screen while retaining work-graph overlays:

```text
┌ Relay · /absolute/workspace/path ───────── ready / awaiting you / working ┐
│ transcript (scrollable)                                                    │
│ You                                                                       │
│   Add passkey authentication.                                             │
│                                                                           │
│ Relay                                                                     │
│   I inspected the current authentication boundary and prepared work.      │
│   ● completed: inspect repository                                         │
│   ◇ decision: choose session lifetime                                     │
│                                                                           │
├───────────────────────────────────────────────────────────────────────────┤
│ › editable prompt                                                         │
└ /help · Enter send · multiline binding · Ctrl-C cancel/exit ─────────────┘
```

Narrow terminals degrade to header, transcript, input, and one-line status. Overlays must be dismissible without losing the input draft or transcript scroll position. Colors are enhancements only; status, speaker, selection, and failure states must remain legible without color.

## Affected files and ownership

The exact module split may be refined, but ownership must remain non-overlapping. Shared contract changes go through their designated owner.

### Domain and canonical facts — domain owner

- `crates/relay-domain/src/facts.rs` — add versioned conversation facts to `DomainFact`.
- `crates/relay-domain/src/graph.rs` — fold conversation records into a bounded/indexable conversation projection or delegate to a new pure conversation projection module.
- `crates/relay-domain/src/ids.rs` — add validated conversation, turn, and message IDs if the existing ID facilities cannot express them.
- `crates/relay-domain/src/lib.rs` — export only the new stable domain contracts.
- `crates/relay-domain/tests/**` and focused property/scenario tests — replay, ordering, duplicate IDs, interrupted turns, and deterministic projection.

Avoid adding chat semantics to `crates/relay-core/**`; it is frozen compatibility reference material.

### Store projection and recovery — storage owner

- `crates/relay-store/src/fact/graph.rs` and related typed fact codecs — encode/decode new facts without weakening unknown-version failures.
- `crates/relay-store/src/fact/projection.rs` and/or `crates/relay-store/src/projection.rs` — add rebuildable message/turn indexes only if required for bounded transcript reads.
- `crates/relay-store/src/store.rs` — expose bounded chronological retrieval through an existing projection seam; do not make SQLite authoritative.
- `crates/relay-store/tests/**` and `crates/relay-tests/tests/fact_replay.rs` / `crash_recovery.rs` — rebuild, interrupted-turn, append ordering, and compatibility coverage.

### Application and runtime boundary — application/runtime owner

- `crates/relay-app/src/conversation.rs` (new) — conversational turn service, typed requests/results, first-prompt initialization, context construction, action validation/application, and final fact recording.
- `crates/relay-app/src/session.rs` — narrowly expose conversation snapshot/access methods; preserve checked append and refresh semantics.
- `crates/relay-app/src/views.rs` or `crates/relay-app/src/conversation_views.rs` (new) — stable message, activity, prompt, decision, and turn-result views.
- `crates/relay-app/src/lib.rs` — export the new service and views.
- `crates/relay-executor/**` — only changes needed for a cancellable conversational model turn and bounded event feed; reuse existing model/provider contracts where possible.
- `crates/relay-providers/src/contract.rs`, `collect.rs`, `fake.rs`, `openai.rs`, and `anthropic.rs` — add normalized streaming events only where the current contract cannot support live assistant text; keep wire types private.
- `crates/relay-app/tests/**` — fresh/resumed turns, action proposal validation, decision resolution, model failure, cancellation, and no-lock-across-model-call tests.

Do not route conversational mutations through the stopped `crates/relay-runtime` controller.

### Interactive UI — interface owner

- `crates/relay-ui/src/work/mod.rs` — evolve the work interface into a chat-first immutable state machine, or split it into `work` overlays plus a new `chat` module with one public interface facade.
- `crates/relay-ui/src/work/keymap.rs` — typed editing, navigation, overlay, submit, cancel, and slash-command actions.
- `crates/relay-ui/src/work/screens.rs` — chat layout, responsive rendering, transcript scrolling, status/activity rows, decisions, and overlays.
- `crates/relay-ui/src/work/editor.rs` (new) — Unicode-safe multiline editor/history/paste state, unless a small well-maintained Ratatui editor dependency is selected and reviewed.
- `crates/relay-ui/src/work/transcript.rs` (new) — bounded transcript fold and rendering helpers.
- `crates/relay-ui/tests/tui_work_graph.rs` — retain current work-screen coverage and add chat snapshots, editor behavior, narrow widths, resize, scrolling, overlays, and cancellation states.
- `crates/relay-ui/Cargo.toml` — add only narrowly justified editor/text dependencies.

### CLI and terminal lifecycle — interface owner, coordinated with UI owner

- `crates/relay-cli/src/args.rs` — default chat command, prompt fallback, `--print`, help text, and ambiguity rules.
- `crates/relay-cli/src/dispatch.rs` — preserve existing command dispatch while routing default chat and print turns through application services.
- `crates/relay-cli/src/tui.rs` — lazy fresh-session handling, event feed, async/background turn execution, cancellation, resize/paste events, terminal lifecycle, and typed request translation.
- `crates/relay-cli/src/main.rs` / `src/lib.rs` — pass terminal capability through a testable invocation seam rather than reading TTY state inside the pure parser.
- `crates/relay-cli/tests/cli_work_graph.rs` — bare/prompt/print dispatch, existing-command regressions, non-TTY behavior, and exit codes.
- `crates/relay-cli/tests/**` (new focused terminal-driver tests if needed) — fake event source/sink, cancellation, and restoration without requiring a real TTY.
- `crates/relay-cli/Cargo.toml` — async/channel dependencies only if required by the selected driver design.

### Documentation and integration — lead integrator

- `README.md` — make bare `relay` the quick start, document `--print`, workspace behavior, first-run initialization, resume, keys, slash commands, and privacy boundaries.
- `docs/architecture.md` — record the conversation facts/application/UI data flow after implementation exists.
- `docs/plans/2026-10-14-default-interactive-chat-interface.md` — this plan; update status/decision notes only when implementation materially departs from it.
- `CLAUDE.md` — no change expected; update only for a genuinely new fundamental engineering rule.
- Root `Cargo.toml` / `Cargo.lock` — dependency coordination owned by the lead integrator.

## Phased implementation

### Phase 0: baseline, contracts, and test seams

1. Read the applicable instructions and plans; inspect the current worktree and preserve all pre-existing changes.
2. Run or capture the focused CLI/TUI baseline before modifying shared files.
3. Write a small invocation decision table in tests: known commands, bare TTY, bare non-TTY, prompt TTY, prompt non-TTY, `--print`, `--workspace`, help/version precedence, JSON commands, and malformed flags.
4. Define Relay-owned conversation facts, view models, typed UI requests, streaming activity events, terminal-capability input, and cancellation semantics before parallel implementation.
5. Decide whether transcript lookup can replay a bounded suffix cheaply enough or needs a SQLite projection. If a projection is added, document that JSONL remains authoritative and prove rebuild equivalence.
6. Freeze file ownership. The CLI/UI owner owns parser/driver/UI files; the domain owner owns fact and fold contracts; the application/runtime owner consumes those contracts; the lead alone updates root manifests and documentation.

Phase gate:

- Existing CLI/TUI tests pass before feature edits, or pre-existing failures are recorded with evidence.
- Contract types and ownership have one agreed owner each.
- No implementation depends on the frozen TypeScript or stopped Rust compatibility controller.

### Phase 1: durable conversation model

1. Add validated IDs and versioned conversation facts with explicit timestamps and bounded text validation.
2. Extend the pure fold/projection so message order is deterministic, duplicate IDs fail closed, and a started-but-unfinished turn is identifiable after replay.
3. Extend canonical fact serialization/deserialization and any SQLite projection required for bounded transcript retrieval.
4. Add golden/property tests for valid replay, unknown/newer records, duplicate or out-of-order turns, Unicode text, maximum sizes, interrupted turns, and projection rebuild.
5. Verify old projects with no conversation facts still open unchanged and receive a conversation only after the first submitted prompt.

Phase gate:

- JSONL replay alone reconstructs the same transcript/turn state as a rebuilt projection.
- Existing fact fixtures and migrations remain readable.
- No UI, provider, clock, SQL, terminal, or filesystem dependency enters the pure domain/kernel crates.

### Phase 2: conversational application service

1. Add a typed `ConversationRequest`/`ConversationResult` application API covering fresh initialization, normal prompts, slash-command intents, clarification, decision answers, bounded run requests, response-only turns, failure, and cancellation.
2. For a fresh workspace, delay `open_or_create`/`init` until a non-empty prompt is accepted. Delegate initialization and first workflow expansion to the existing command path.
3. Record the user message, release the writer lock, construct a bounded prompt from the transcript suffix and graph-derived views, and call the provider through Relay-owned contracts.
4. Require model output to separate user-visible answer text from typed proposed actions. Decode and validate it; never parse shell commands or apply free-form mutations.
5. Apply accepted actions through existing app/engine handlers. Record rejections in the turn result and explain them to the user.
6. Persist the final assistant message or terminal failure/cancellation fact, then refresh the snapshot. Define crash behavior for every boundary between these steps.
7. Reuse runner cancellation and activity feeds for bounded work execution. Do not hold the store writer lock during the model call or executor work.
8. Add deterministic fake-model scripts for answering, proposing valid/invalid actions, resolving a decision, failing, cancelling, and resuming an interrupted turn.

Phase gate:

- A complete fresh and resumed turn works without a terminal.
- Invalid model output and rejected actions cannot mutate the graph.
- Cancellation and provider failure leave replayable, honest state.
- A concurrency test proves the project writer lock is not held across model/tool execution.

### Phase 3: CLI invocation and non-interactive turn

1. Refactor parsing so terminal capability is an injected dispatch concern, not a global read hidden in the pure parser.
2. Add the default chat command for bare invocation and a prompt-form command for `relay "..."`, retaining known-command precedence and current subcommand behavior.
3. Add `--print "..."` as the explicit one-turn automation surface. It must never enter raw mode, must emit bounded plain text by default, and must have a documented structured-output decision before accepting `--json`.
4. Keep `relay tui` as a compatibility alias. Keep `relay run` as the work scheduler and the existing legacy `relay run <message>` mapping unless a separate deprecation is approved.
5. Add tests for all invocation matrix cases, exit codes, workspace selection, first-run creation timing, no-TTY behavior, help text, and every existing command parser case.

Phase gate:

- Bare `relay` enters chat only with an interactive input/output terminal.
- Merely opening and quitting a fresh workspace creates no state.
- All current explicit CLI and JSON integration tests pass unchanged except deliberate help-text snapshots.
- `--print` is deterministic under the fake provider and safe under redirection.

### Phase 4: chat-first Ratatui state and rendering

1. Introduce immutable chat snapshot state: workspace, project state, transcript window, activity, open decision, busy/cancel state, overlay, input draft, history cursor, and scroll position.
2. Implement a Unicode-safe multiline editor and explicit submit/newline bindings. Handle bracketed paste as text, never as key commands.
3. Render the workspace header, scrollable transcript, compact activity, decision prompts, input editor, and context-sensitive footer.
4. Preserve current work summary, ready, blocked, decisions, detail, and artifact views as overlays/screens driven by existing typed views.
5. Implement slash-command parsing as a pure UI/application adapter with actionable usage errors. Do not duplicate domain validation.
6. Add responsive layouts for wide, normal, narrow, and degenerate terminal sizes. Bound message/tool/artifact rendering and retain no-color legibility.
7. Add TestBackend snapshots and state-machine tests for first run, resumed transcript, streaming/busy state, multiline editing, history, paste, scrolling, open decisions, overlays, errors, cancellation, and narrow terminals.

Phase gate:

- UI snapshot construction and rendering require no live TTY or network.
- Work-centric functionality covered by existing TUI tests remains accessible.
- Draft text and scroll position survive overlay open/close and resize.
- No unbounded provider/tool output is rendered or stored.

### Phase 5: terminal driver, streaming, cancellation, and recovery

1. Refactor the terminal event loop so keyboard/resize/paste events and background turn/activity events can be multiplexed without blocking rendering.
2. Keep terminal setup in `relay-cli`; enter raw/alternate screen only after workspace/config/session preflight succeeds.
3. Stream normalized assistant/activity events into the UI while persisting only terminal turn facts. Ensure redaction and size caps occur before display and persistence.
4. Implement `Ctrl-C` cancel-then-exit semantics and propagate cancellation through model and executor contracts. Do not report cancelled work as successful.
5. Restore raw mode, alternate screen, cursor, and panic hooks on normal exit, preflight error, provider error, cancellation, forced exit, and panic.
6. On resume, surface an interrupted prior turn honestly and allow a new prompt or explicit retry; do not replay side effects blindly.
7. Add driver tests using injected event sources, sinks, and fake providers. Keep real-terminal manual smoke tests supplemental, not the sole verification.

Phase gate:

- The interface remains responsive while a fake turn streams and while an executor runs.
- All terminal restoration tests pass.
- Restart after injected crashes at each durable boundary produces an honest transcript and valid graph.

### Phase 6: integration, documentation, and regression review

1. Run the fresh-workspace flow: open, quit without state, reopen, submit a goal, resolve a decision, run bounded work, inspect an artifact, exit, and resume the transcript.
2. Run the existing-project flow and verify no migration is required for projects without conversation facts.
3. Exercise `relay`, `relay "prompt"`, `relay --print "prompt"`, `relay tui`, `relay run`, every existing read command, JSON output, and redirected invocation.
4. Review security boundaries: workspace header/path, symlinks, capability grants, secret redaction, bounded output, delivery holds, and dirty-worktree preservation.
5. Update README and architecture documentation to match the implemented behavior. Do not describe planned features as shipped.
6. Run focused and repository-wide checks. Review all diffs for accidental changes to frozen/reference files or user-owned work.

Phase gate:

- All acceptance criteria below pass.
- Required checks pass, or unresolved pre-existing failures are separated from feature regressions with commands and output.
- No commits or pushes are made.

## Engineering constraints

- Safe Rust only; use the pinned toolchain and committed `Cargo.lock`.
- Keep `relay-domain`, `relay-engine`, and `relay-workflows` pure. Conversation facts/projections may live in the domain crate, but context building, provider calls, terminal state, SQL, files, clocks, and channels may not.
- JSONL facts are authoritative. SQLite conversation indexes are disposable and rebuildable.
- Append durable state before acknowledging it as durable. Streamed text is provisional until the terminal assistant/failure/cancellation fact is accepted.
- Do not hold a writer lock across model calls, tool execution, terminal waits, or rendering.
- Every model-produced action is typed, decoded, bounded, and validated by deterministic application/domain code.
- Preserve workspace containment, no-overwrite writes, atomic patches, capability grants, redaction, bounded child processes, read-only Git tools, and delivery holds.
- Keep provider wire types private and provider-neutral contracts owned by Relay.
- Keep the parser pure and terminal capability injectable. Non-interactive commands must never manipulate terminal modes.
- Treat Unicode by grapheme/display width rather than byte count where cursor placement or truncation is visible.
- Put explicit caps on prompt size, message size, transcript context, rendered lines, activity entries, tool summaries, and model response size. Report truncation.
- Tests are offline and deterministic by default. Live provider tests remain explicit opt-ins.
- Avoid broad refactors. Do not extend frozen TypeScript/reference crates to make the new interface easier.
- One owner edits each shared contract. Coordinate root manifest, `Cargo.lock`, README, architecture docs, and `CLAUDE.md` through the lead integrator.

## Acceptance criteria

1. From an interactive terminal in any directory, bare `relay` opens a chat interface and displays the canonical workspace path.
2. Opening and quitting an uninitialized directory without submitting text creates no `.relay-graph` state.
3. The first meaningful prompt initializes the project through validated application services, appears once in the transcript, and creates the initial work graph.
4. Reopening Relay restores the durable transcript and current work-graph state without relying on SQLite as authority.
5. A normal turn can answer conversationally, propose validated work-graph actions, show compact activity, and surface a human decision without UI-owned policy.
6. Invalid/malformed model proposals are rejected safely and cannot mutate the graph or workspace.
7. Optioned and free-form decisions can be completed from chat only through the existing decision validation path.
8. Existing summary, ready, blocked, decision, detail, and artifact views remain accessible from the interactive interface.
9. Input supports Unicode-safe multiline editing, cursor movement, paste, local history, scrolling, and draft preservation across overlays/resizes.
10. `Ctrl-C` cancels active work before exiting; terminal state is restored on every tested success, error, cancellation, forced-exit, and panic path.
11. Transcript, provider text, tool summaries, and rendered artifacts are bounded and redacted before persistence/display as applicable.
12. `relay --print "prompt"` performs one bounded turn without entering raw mode and works with redirected input/output.
13. Bare/prompt interactive forms fail safely without a TTY, while all existing explicit commands and JSON surfaces preserve their behavior and exit codes.
14. Old `.relay-graph` projects with no conversation facts open unchanged; no destructive migration is required.
15. No writer lock is held across a provider call or tool execution, proven by a focused concurrency test.
16. The implementation does not modify frozen `crates/relay-core`, `crates/relay-runtime`, or compatibility TUI files except for a separately approved critical fix, and does not reintroduce a TypeScript/Node implementation.
17. Documentation matches the shipped behavior, all required checks pass, and no commit or push is performed.

## Verification commands

Start with preservation and focused feedback:

```bash
git status --short
git diff -- crates/relay-cli crates/relay-ui crates/relay-app crates/relay-domain crates/relay-store
cargo test -p relay-domain
cargo test -p relay-app
cargo test -p relay-ui --test tui_work_graph
cargo test -p relay-cli --test cli_work_graph
cargo test --test fact_replay
cargo test --test crash_recovery
cargo test --test capability_security
```

Add focused tests introduced by the implementation (use their final target names if they differ):

```bash
cargo test -p relay-app conversation
cargo test -p relay-ui chat
cargo test -p relay-cli interactive
cargo test --test conversation_replay
cargo test --test conversation_recovery
```

Run all repository-required Rust checks:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo build --workspace --all-targets --all-features
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

Run the repository's Rust checks (fmt, clippy, the full workspace test/build,
and the doc gate) exactly as CLAUDE.md records them; this repository has no
Node/TypeScript checks — the TypeScript implementation was removed.

Manual smoke checks use a temporary fixture workspace, never the repository itself as disposable state:

```bash
cargo run -p relay-cli -- --help
cargo run -p relay-cli -- --version
cargo run -p relay-cli -- --workspace /path/to/temporary-fixture
cargo run -p relay-cli -- --workspace /path/to/temporary-fixture --print "summarize the current project state"
cargo run -p relay-cli -- --workspace /path/to/temporary-fixture project summary --json
```

For the interactive smoke test, verify opening/quitting without a prompt leaves no `.relay-graph`, then submit a goal, exit, reopen, and confirm the transcript and graph resume. Also pipe or redirect the bare command to verify it returns the documented non-TTY diagnostic without entering raw mode.

## Team integration guidance

Use a small team with explicit, non-overlapping ownership:

1. **Lead/integrator** — owns plan adherence, shared contracts, root manifests/lockfile, docs, integration review, and final checks.
2. **Domain/storage engineer** — owns conversation facts, pure fold, codecs, optional SQLite projection, replay, and crash recovery tests.
3. **Application/runtime engineer** — owns the conversation service, context/action contracts, provider/runtime integration, cancellation, and application tests.
4. **CLI/UI engineer** — owns invocation grammar, terminal capability/driver, Ratatui chat state/editor/rendering, and CLI/TUI tests.

Freeze the cross-role fact/request/event contracts before parallel implementation. The CLI/UI engineer must not edit domain facts; the domain/storage engineer must not edit terminal code; the application/runtime engineer must not edit parser or rendering code. The lead coordinates any necessary shared-file edit, integrates in dependency order, reviews security and authority boundaries, and runs the complete verification matrix.
