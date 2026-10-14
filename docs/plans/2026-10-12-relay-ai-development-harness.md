# Relay AI development harness implementation plan

> **Historical (2026-10-14):** this is the original TypeScript/Node.js
> implementation plan. That implementation was removed from the repository
> after the Rust work-graph became the sole product; see
> `2026-10-14-remove-typescript-rust-only-codebase.md`. Retained as design
> history; not current instructions.

## Objective

Build Relay as a local-first, model-agnostic TypeScript CLI that preserves product intent from discovery through delivery. Relay should infer the user's intent, select an appropriate workflow mode and model, create durable reviewable artifacts, advance automatically when evidence and policy allow, and pause when human judgment or authorization is required.

The first release should prove one complete workflow:

```text
Discovery -> PRD -> TRD -> implementation plan -> implementation -> verification -> delivery report
```

The workflow must support revision loops when later evidence invalidates an earlier artifact. Delivery produces a reviewable report and stops before committing, pushing, opening a pull request, deploying, or publishing unless the user explicitly authorizes those actions.

## Current codebase

At the time this plan was written, the repository contained only a one-line `README.md` and an uncommitted MIT `LICENSE`. Because no `CLAUDE.md` existed, the repository was prepared with:

- `CLAUDE.md` containing product and engineering constraints.
- `package.json` for a Node.js 22+, TypeScript, ESM CLI using npm.
- Strict `tsconfig.json` and `tsconfig.build.json` configurations.
- `.gitignore` entries for build output, dependencies, local Relay state, logs, and secrets.

No harness functionality or source files have been implemented. The implementation team must preserve the existing `LICENSE` and any user changes visible in the worktree.

## Scope

### Included in the first release

- An interactive Ink-based CLI plus non-interactive commands suitable for scripts and CI.
- Explicit modes: `discovery`, `specification`, `technical-design`, `planning`, `implementation`, `verification`, and `delivery`.
- A deterministic workflow controller with `CONTINUE`, `ADVANCE`, `NEEDS_USER`, and `REVISE(stage)` outcomes.
- Natural-language intent classification with explicit slash-command overrides.
- Fixed, configurable model routing by mode, initially supporting OpenAI and Anthropic through separate adapters.
- A provider-neutral streaming event protocol and agent loop with cancellation, retries, budgets, and tool-call limits.
- Versioned Markdown artifacts for the opportunity brief, decision log, PRD, TRD, execution plan, verification report, and delivery report.
- An append-only SQLite event store from which project state can be replayed.
- Local tools for file reading, repository search, patch application, subprocess execution, Git inspection, and test execution.
- Permission and transition policies that protect user files, secrets, approved artifacts, and consequential actions.
- Status, explanation, inspection, revision, mode override, and bounded-autonomy commands.
- Independent verification that evaluates the implementation against approved acceptance criteria.
- Unit, integration, replay, provider-contract, tool-safety, and CLI smoke tests.

### Excluded from the first release

- Autonomous multi-agent implementation inside Relay.
- Vector databases or semantic repository indexes.
- Browser or desktop automation.
- Hosted synchronization, accounts, collaboration, telemetry, or a web interface.
- Deployment execution, automatic publishing, automatic pull-request creation, or unattended release actions.
- Dynamic model benchmarking and cost-optimized routing; routing remains explicit and configurable.
- A Rust runtime, sandbox, or native launcher. Stable performance- or security-critical components may be extracted later without changing Relay's core protocols.

## Proposed architecture

Use one npm package initially, with module boundaries that can later become workspaces without changing public contracts:

```text
src/
  cli.tsx
  cli/
    commands/
    parse-arguments.ts
    renderers/
  core/
    events.ts
    intents.ts
    modes.ts
    project-state.ts
    reducer.ts
    transitions.ts
  runtime/
    agent-loop.ts
    context-builder.ts
    model-router.ts
    budgets.ts
    cancellation.ts
  providers/
    provider.ts
    openai.ts
    anthropic.ts
  artifacts/
    artifact.ts
    schemas.ts
    markdown-store.ts
    invalidation.ts
  storage/
    database.ts
    migrations/
    event-store.ts
    project-repository.ts
  policies/
    permissions.ts
    intervention.ts
    transitions.ts
  tools/
    registry.ts
    schemas.ts
    filesystem.ts
    search.ts
    patch.ts
    process.ts
    git.ts
  tui/
    app.tsx
    components/
    hooks/
test/
  fixtures/
  integration/
  providers/
  unit/
```

Keep dependency direction one-way: CLI/TUI -> runtime -> core contracts. Providers, storage, artifacts, policies, and tools implement interfaces owned by the core/runtime boundary. Provider SDK objects must not cross their adapter modules.

## Affected files

The paths below are anticipated. The implementation may refine filenames while preserving the stated boundaries.

| Path | Intended change |
| --- | --- |
| `README.md` | Replace the placeholder with installation, configuration, security, command, and workflow documentation. |
| `package.json` | Add or adjust scripts and dependencies only as required by the implementation; keep the MIT SPDX identifier and CLI binary entry. |
| `package-lock.json` | Generate and retain the npm lockfile. |
| `.env.example` | Document supported provider environment variables without real credentials. |
| `src/cli.tsx` and `src/cli/**` | Add executable entry point, argument parsing, commands, plain output, and exit behavior. |
| `src/core/**` | Define stable events, modes, intents, state, reducer, and workflow outcomes. |
| `src/runtime/**` | Add orchestration, routing, context construction, limits, retries, streaming, and cancellation. |
| `src/providers/**` | Add provider-neutral interface and OpenAI/Anthropic adapters. |
| `src/artifacts/**` | Add artifact schemas, Markdown rendering, versions, approvals, and invalidation. |
| `src/storage/**` | Add SQLite initialization, migrations, append-only events, snapshots if needed, and replay. |
| `src/policies/**` | Add deterministic transition, approval, and intervention decisions. |
| `src/tools/**` | Add schema-validated local tools and bounded process/Git operations. |
| `src/tui/**` | Add Ink conversation, status, artifact inspection, diff, and intervention views. |
| `test/**` | Add fixtures and focused unit/integration tests. |
| `docs/architecture.md` | Explain contracts, dependency direction, state replay, artifacts, and security boundaries. |
| `docs/configuration.md` | Document routing, budgets, autonomy, providers, project state, and precedence. |

Do not modify `LICENSE` except to correct an explicit user-requested copyright detail.

## Phased implementation

### Phase 1: Establish contracts and executable shell

1. Install dependencies and generate `package-lock.json`; confirm all chosen versions support Node.js 22.
2. Add the executable `src/cli.tsx` with a Node shebang and ensure `npm run build` preserves a runnable entry point at `dist/cli.js`.
3. Define Relay-owned types for model requests, streaming model events, tool calls/results, usage, modes, intents, artifacts, and workflow outcomes.
4. Implement argument parsing and the initial commands: `init`, `run`, `status`, `inspect`, `mode`, `auto`, `pause`, `why`, and `config`.
5. Provide helpful behavior for non-TTY execution, `--help`, `--version`, invalid arguments, cancellation, and provider errors.
6. Write contract tests before provider and persistence work proceeds so parallel contributors share stable interfaces.

Deliverable: a runnable CLI shell and reviewed core contracts, with no provider-specific types outside `src/providers/`.

### Phase 2: Event-sourced project state and artifacts

1. Create a `.relay/` project directory containing the SQLite database and generated artifacts; keep it ignored by Git by default.
2. Implement a migration runner and an append-only events table. Include event ID, project/session ID, type, schema version, timestamp, correlation/causation IDs, and validated payload.
3. Implement a pure reducer that reconstructs project state from events. Reject unknown or incompatible event versions with actionable diagnostics.
4. Add artifact definitions and versioned Markdown storage for opportunity briefs, decision logs, PRDs, TRDs, execution plans, verification reports, and delivery reports.
5. Record artifact creation, revision, approval, invalidation, and inspection in the event log. Never silently replace an approved artifact.
6. Add deterministic downstream invalidation: changes to an approved upstream artifact mark dependent artifacts and results stale until regenerated and reviewed.
7. Test full replay, interruption recovery, migrations, corrupt-event behavior, and invalidation chains.

Deliverable: restartable local project state and human-readable, versioned artifacts.

### Phase 3: Workflow modes, intent detection, and intervention policy

1. Implement the seven modes and their contracts: permitted tools, required inputs, outputs, completion evidence, and modification permissions.
2. Implement validated state transitions and outcomes: `CONTINUE`, `ADVANCE`, `NEEDS_USER`, and `REVISE(stage)`.
3. Add a structured intent-classification request whose result is validated before the controller applies it. Low-confidence or ambiguous consequential intents must pause or ask a focused question.
4. Add explicit command overrides so users can select a mode, revise an artifact, limit autonomy, or request an explanation when intent detection is wrong.
5. Implement autonomy bounds such as `off`, `next-checkpoint`, `until:<mode>`, and `workflow`, with the bound persisted as project state.
6. Implement policy-driven pauses for product choices with material consequences, approval or revision of contractual artifacts, destructive operations, scope expansion, deployment/publishing, and delivery.
7. Implement `/why` data explaining the active mode, recent transition, evidence, routing decision, invalidated artifacts, and reason for any pause.
8. Exercise normal progress, revision loops, ambiguous input, rejected transitions, stale artifacts, and manual mode overrides in tests.

Deliverable: deterministic orchestration around model-proposed intent, with reviewable checkpoints and bounded auto-advancement.

### Phase 4: Model adapters, routing, and resilient agent loop

1. Define a provider interface that returns an async stream of Relay-owned events and accepts Relay-owned messages, tools, structured-output schemas, and cancellation signals.
2. Implement OpenAI and Anthropic adapters using their official SDKs. Normalize text deltas, tool calls, completion reasons, errors, and usage.
3. Keep provider conversation identifiers optional optimizations; Relay's event log and context builder remain authoritative.
4. Implement fixed routing by mode in YAML configuration, including fallback order and per-route limits. Validate configuration and reject unknown models or providers clearly.
5. Build task-specific context packets from approved artifacts, current state, relevant repository facts, and recent events. Do not pass the complete historical conversation by default.
6. Implement the agent loop with maximum turns, model calls, tool calls, output tokens where supported, wall-clock duration, and repeated-action detection.
7. Add retry behavior only for transient failures, with bounded exponential backoff and idempotency awareness. Never retry a state-changing tool blindly.
8. Record routing, provider request metadata, normalized responses, usage, errors, and cancellation without persisting secrets or sensitive headers.
9. Test adapters with recorded fixtures or mocked transports. Keep routine tests offline and deterministic.

Deliverable: interchangeable OpenAI and Anthropic execution engines behind a stable Relay protocol.

### Phase 5: Safe repository tools

1. Create a typed tool registry with Zod schemas, consistent results, capability metadata, side-effect classification, timeouts, output limits, and cancellation.
2. Implement read-only tools for files, directory inventory, fast text search, Git status/diff/log, and repository instruction discovery.
3. Implement patch application that detects conflicts, preserves unrelated user changes, and reports exact affected files.
4. Implement subprocess execution with argument arrays, explicit working directories, bounded environment inheritance, stdout/stderr streaming, timeouts, cancellation, and process-tree termination.
5. Apply workspace boundaries after resolving symlinks. Reject traversal and writes outside the authorized root.
6. Add a permission decision before each tool execution based on mode, side-effect class, current autonomy bound, artifact approval state, and requested target.
7. Treat dirty worktrees as normal. Snapshot status before changes and never reset, clean, commit, stash, or check out over user changes automatically.
8. Redact known secret values and sensitive environment variables from recorded commands, outputs, and diagnostics.
9. Add adversarial tests for traversal, symlinks, shell metacharacters, oversized output, cancellation, timeouts, concurrent changes, and patch conflicts.

Deliverable: a bounded tool layer suitable for implementation work in a user-owned repository.

### Phase 6: End-to-end product workflow

1. Implement mode prompts and artifact templates for discovery through delivery, keeping prompts versioned and testable.
2. In discovery, capture the user problem, target users, evidence, desired outcomes, assumptions, constraints, and unresolved questions.
3. In specification, produce testable requirements, explicit exclusions, and acceptance criteria; pause before treating the PRD as an approved contract.
4. In technical design, inspect the target repository and map requirements to architecture, interfaces, migrations, risks, rollout, and rollback.
5. In planning, produce an executable task graph with dependencies, ownership boundaries, affected files, checks, and integration points.
6. In implementation, execute only approved and in-scope tasks, continuously preserving user changes and recording evidence.
7. In verification, use an independent review context that receives approved criteria plus the resulting diff and test evidence. It must be able to return to specification, design, planning, or implementation.
8. In delivery, create a report linking requirements to changes and verification evidence. Pause before any external or durable delivery action.
9. Add an end-to-end fixture repository and deterministic fake provider to exercise successful completion, user intervention, revision loops, failure recovery, and resumed sessions.

Deliverable: one complete, resumable workflow from initial idea to a delivery-ready report.

### Phase 7: Interactive terminal experience

1. Build the Ink interface around an event-driven view model rather than embedding orchestration in React components.
2. Provide conversation, current mode, autonomy bound, streaming activity, tool activity, changed files, verification progress, and next action without overwhelming the main view.
3. Implement focused views for `/status`, `/inspect <artifact>`, `/why`, configuration, diffs, and user intervention.
4. At a pause, show the decision, recommendation, supporting evidence, affected artifacts, and a small set of concrete commands.
5. Ensure terminal restoration on normal exit, errors, signals, and component failures. Support resize, narrow terminals, non-color environments, and accessible textual indicators.
6. Keep a plain non-interactive renderer for redirected output and automation.
7. Add Ink rendering tests for stable states and manual smoke tests for streaming, cancellation, resize, and terminal restoration.

Deliverable: a trustworthy CLI interface that makes state, decisions, and intervention points easy to inspect.

### Phase 8: Documentation, packaging, and release readiness

1. Expand `README.md` with the product model, installation, quick start, commands, provider setup, project files, security boundaries, and development workflow.
2. Add configuration and architecture documentation, including examples for routing modes to OpenAI and Anthropic.
3. Confirm `npm pack --dry-run` includes only intended distributable files and that a packed installation exposes the `relay` command.
4. Test on supported macOS, Linux, and Windows environments in CI across the supported Node.js versions.
5. Keep npm installation as the initial distribution route. Evaluate Bun or Node single-executable packaging only after the Node CLI is stable and dependency compatibility is demonstrated.
6. Document local data deletion, credential handling, diagnostic export, limitations, and recovery from a corrupted local database.

Deliverable: an installable npm CLI with reproducible checks and accurate user/developer documentation.

## Engineering constraints

- Follow `CLAUDE.md` throughout implementation.
- Preserve all existing behavior and user-owned changes. Inspect `git status` before editing and before reporting completion.
- Use Node.js 22 or later, TypeScript, ESM, npm, Ink, Zod, and SQLite. Avoid adding another framework unless it removes demonstrated complexity.
- Keep the workflow state machine and event reducer deterministic and independent of model providers, terminal rendering, and storage implementation.
- Treat model output as untrusted input. Validate every structured response and tool argument before use.
- Store canonical artifacts as readable Markdown and operational history as append-only SQLite events. Do not make chat transcripts the source of truth.
- Version persisted events, configuration, artifact schemas, and prompts when compatibility matters.
- Keep API keys and authorization headers out of SQLite, artifacts, logs, snapshots, fixtures, and error messages.
- Use explicit policy code for approvals and mode transitions. Prompts may recommend actions but cannot grant themselves authority.
- Commands that can change the filesystem or repository must be bounded to an explicitly resolved workspace and surfaced in the event stream.
- Do not introduce automatic commit, push, PR creation, deployment, or publishing in the first release.
- Do not depend on network access in the standard test suite. Provider tests should use mocked transports or sanitized fixtures.
- Prefer focused tests around state and safety boundaries. Avoid brittle snapshots of prose or provider-specific payloads outside adapter tests.
- Coordinate shared contracts before parallel implementation. Assign one owner to each of `src/core/**`, database schema/migrations, provider interface, and configuration schema at a time.

## Acceptance criteria

1. `relay init` creates local Relay state without modifying unrelated project files and is safe to run repeatedly.
2. `relay run` can take a new product idea through all seven modes using a deterministic fake provider in automated tests.
3. Relay stores and versions the opportunity brief, decision log, PRD, TRD, implementation plan, verification report, and delivery report as readable Markdown.
4. Restarting Relay reconstructs identical project state from persisted events and resumes the interrupted workflow.
5. Changing an approved upstream artifact marks dependent artifacts and verification results stale and routes work back to the earliest affected stage.
6. Natural-language requests produce validated intent proposals, while explicit commands can override the selected mode or autonomy bound.
7. Auto mode advances through reversible, sufficiently evidenced transitions and pauses for consequential choices, contractual artifact approval/revision, destructive operations, and delivery.
8. `/status` reports the goal, mode, progress, active work, blockers, artifact versions, changed files, next action, and autonomy bound.
9. `/why` explains the most recent routing or transition decision using persisted evidence.
10. OpenAI and Anthropic can each serve as a configured mode provider without leaking SDK types into the core or changing workflow behavior.
11. A provider failure, invalid structured response, timeout, cancellation, or exhausted budget leaves resumable state and an actionable message.
12. Filesystem and process tools reject traversal outside the workspace, validate arguments, enforce limits, and preserve pre-existing dirty-worktree changes.
13. Verification checks every approved acceptance criterion against the diff and evidence using a context independent from the implementation conversation.
14. Delivery creates a trace from user problem to requirement, design decision, implementation change, and verification evidence, then pauses without committing or pushing.
15. Interactive and non-interactive CLI modes produce usable output and restore the terminal after success, failure, or interruption.
16. No test fixture, log, database entry, generated artifact, or package output contains provider credentials.
17. The documented required checks pass, and the packed npm artifact exposes a working `relay` command.

## Verification commands

Run from the repository root. Add narrower test commands during implementation where useful, but keep these as the final required checks:

```bash
npm ci
npm run typecheck
npm test
npm run build
node dist/cli.js --help
node dist/cli.js --version
npm pack --dry-run
```

Run the end-to-end workflow with a deterministic fake provider so it requires no API keys or network access:

```bash
npm test -- --run test/integration/workflow.test.ts
```

Run focused safety and replay suites:

```bash
npm test -- --run test/unit/core test/unit/policies test/unit/tools test/integration/replay.test.ts
```

Before reporting completion, inspect repository state and confirm that no commit or push occurred:

```bash
git status --short
git diff --check
git diff --stat
```

Manual checks should cover terminal resize, Ctrl-C during model streaming and subprocess execution, restoration after a thrown error, artifact inspection, a rejected mode transition, revision of an approved PRD, and resuming an interrupted session.
