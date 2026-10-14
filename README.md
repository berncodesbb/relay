# Relay

Relay is a local-first, model-agnostic CLI that carries product intent from discovery through specification, technical design, planning, implementation, verification, and delivery. Models are replaceable workers: Relay owns the workflow state, the artifacts, the permissions, and the transition policy. Every durable workflow change is an append-only, hash-chained fact in a local JSONL log; every product artifact is versioned, readable Markdown you can review in any editor. Nothing leaves your machine except the model calls you configure.

The implementation is a native Rust Cargo workspace (version `0.3.0-preview`) built around a durable **work graph** of addressable goals, work items, dependencies, decisions, artifacts, evidence, attempts, capability grants, and receipts — evaluated by a deterministic, pure kernel. There is no global workflow-mode state machine; the familiar discovery→delivery phases are grouped views over the graph, and human decisions are first-class blocking work. Models and executors propose; deterministic policy validates and applies. The primary interactive surface is a workspace-scoped chat (`relay` from any project directory); the structured commands below remain the automation surface.

## Requirements

- Rust 1.99 or later (the repository pins `1.99.0` in `rust-toolchain.toml`; rustup picks that channel automatically).
- Git is optional; the Git tools report a clear error when the workspace is not a repository.
- No Node.js, npm, or other JavaScript tooling is needed anywhere in the build, test, or runtime path.

## Installation

No binary has been published yet. Install from a checkout:

```bash
git clone <this repository> && cd relay
cargo install --path crates/relay-cli    # exposes the `relay` command
```

For development, `cargo run -p relay-cli -- <args>` works directly from the checkout.

## Quick start

```bash
cd your-project            # any directory; Relay stores state in .relay-graph/
relay                      # opens the workspace chat (interactive terminal)
```

Ask a question ("explain this repository"), describe an outcome ("find why checkout duplicates an order and fix it"), or answer a decision. Opening and quitting a fresh workspace creates no state; the first submitted prompt starts a durable conversation, and asking a read-only question never invents a goal — a goal is created only when you ask for one and deterministic validation accepts the proposal.

The goal-first path still exists alongside the chat:

```bash
relay init --goal "Cut onboarding drop-off by walking new users through one saved filter"
relay project summary      # goal, phase-grouped progress, recommendation
relay run                  # run ready work (offline by default: the fake provider)
```

- `relay run` executes ready work within its autonomy level and stops at human decisions; it never auto-resolves them. Levels: `manual`, `safe` (default), `until-decision`, `bounded:<n>`.
- `relay work ready|blocked|list` and `relay explain <work-id>` show what is actionable and why.
- `relay decisions list` / `relay decisions resolve <id> --choice <label>` manage the decisions blocking work.
- `relay artifact inspect <id>[@version]` prints structured content plus the rendered Markdown view; `relay evidence <work-or-criterion>` shows attached evidence.
- `relay tui` opens the interactive Ratatui interface (`q` quits).

### Non-interactive and CI usage

One conversational turn, printed and exited — identical on a terminal and under redirection, never raw mode:

```bash
relay --print "summarize the state of this project"
```

`--print` writes the final assistant reply to stdout (nothing else), per-proposal outcomes and turn diagnostics to stderr, and exits 0 when the turn completed (1 for a failed/cancelled turn, 2 for a malformed invocation). The first `--print` in a fresh workspace creates the project (without a goal) plus the conversation; later runs resume the latest conversation.

Every structured command accepts `--json` for machine-readable output:

```bash
relay project summary --json
relay work ready --json
```

`relay doctor check` validates the local store read-only; `relay doctor rebuild-projection` rebuilds the disposable SQLite projection from the fact log. Bare or prompt-form conversational invocation without a terminal is rejected with `--print` guidance and never enters raw mode.

## Provider setup

Relay reads credentials from the environment and never writes them to project state, artifacts, logs, or the fact store. Copy [.env.example](.env.example) to `.env` (already git-ignored) or export the variables:

```bash
export ANTHROPIC_API_KEY=sk-ant-...   # https://console.anthropic.com/settings/keys
export OPENAI_API_KEY=sk-...          # https://platform.openai.com/api-keys
```

Routing is deterministic configuration, never model output. Each routing purpose maps to a named model role, and each role maps to a provider (`anthropic`, `openai`, `fake`) and a model. Roles are user-defined — define your own in `relay.config.yaml`. See [docs/configuration.md](docs/configuration.md) for the full annotated configuration and examples.

The offline fake provider is the default, so a fresh checkout runs end to end with no keys and no network:

```bash
relay init --goal "demo"
relay run                       # fake provider: deterministic, offline
relay run --provider anthropic  # opt in to a live provider
```

## Project files

All Relay state lives inside your workspace:

```text
.relay-graph/
  manifest.json             # store metadata (versions, active segment)
  facts/                    # segmented, hash-chained canonical JSONL facts
  projection.sqlite         # disposable derived index; rebuildable from facts
  artifacts/                # canonical artifact content addressed by facts
  locks/                    # writer lock
  recovery/                 # crash artifacts (e.g. rejected tails)
```

- JSONL facts are the sole authoritative record; replaying them deterministically rebuilds all graph state. The SQLite projection is a cache — delete it and `relay doctor rebuild-projection` recreates it.
- Rendered artifact views are deterministic Markdown derived from structured content stored as facts.
- `.relay-graph/` is git-ignored by default (see [.gitignore](.gitignore)).
- A legacy TypeScript-era `.relay/` directory (if you have one) is never touched; see [Legacy migration](#legacy-migration).

## Workspace conversations

Three concepts are distinct:

- a **workspace** is the canonical local directory and its Relay state;
- a **conversation** is an ordered, replayable exchange within that workspace (the chat resumes the latest one);
- a **goal** is durable work you have asked Relay to accomplish.

Every message and turn is a durable, versioned fact in the same authoritative JSONL log as the work graph — conversation, turn, and message ids replay deterministically, and a restart resumes the transcript exactly. A turn that was started but never finished (crash, kill) renders honestly as interrupted; Relay never invents an assistant reply.

Inside the chat:

- `Enter` sends; `Ctrl-J` inserts a newline; `Ctrl-C` cancels a running turn first (a second press force-quits) and exits when idle; `PageUp`/`PageDown` scroll the transcript.
- Single-word input that looks like a mistyped command (`relay statusx`) is a usage error with a "did you mean" hint, never a prompt; natural multi-word requests ("explain this repository") are prompts.
- Slash commands: `/status`, `/work`, `/decisions`, `/inspect <id>`, `/help`, `/quit` open the existing work-graph views as overlays.
- While a turn runs, the interface stays live: assistant text streams (bounded, redacted) and compact activity lines show what Relay is doing. Submission is disabled while busy — input is never silently dropped.

The model can answer, propose a goal, propose typed work or decision actions, run bounded ready work, or ask for clarification — but model text never mutates state by itself. Every proposal is decoded, validated, and applied individually through the existing deterministic application services; rejected proposals are reported in the transcript with a safe explanation, never coerced. Secrets are redacted before any text becomes durable or visible.

## The work graph

- **Goals** carry product intent; **work items** (requirements, decisions, tasks, attempts) are addressable nodes with dependencies.
- **Readiness is derived**, not stored: a work item is ready when its dependencies are complete and its inputs are valid; `relay explain` shows the exact chain.
- **Decisions** block work until a human resolves them; **evidence** and **receipts** record verification and executed attempts.
- Executors operate only through immutable attempt specs and **capability grants**; read/model attempts may run concurrently while workspace-changing attempts are serialized.
- Revision invalidates downstream work deterministically; nothing goes stale by convention.

See [docs/architecture.md](docs/architecture.md) for the full architecture.

## Security boundaries

These are enforced in code, not convention:

- **Workspace-bounded tools.** Every model-supplied path is resolved through a path guard that rejects absolute paths, `~`, `..`, and NUL bytes, and resolves symlinks so a link inside the workspace cannot escape it.
- **Writes never overwrite.** Writes only create new files; modifications happen through all-or-nothing unified patches with exact context matching.
- **Capability grants.** Executors receive grants scoped to the operations and paths their work item needs; overlapping write grants are rejected.
- **No shell interpolation.** Commands run with argument arrays — never a shell string. Timeouts and cancellation terminate the whole process group.
- **Bounded environment inheritance.** Child processes inherit only an allowlisted environment plus explicitly requested, non-secret-shaped names.
- **Secret redaction.** Tool outputs, transcripts, and provider errors are redacted before they are recorded; keys are never persisted anywhere.
- **Read-only Git surface.** Only read-only Git tools exist; no Relay tool can add, commit, push, checkout, reset, or clean.
- **Deterministic validation.** Every model-produced structure and tool argument is decoded against typed schemas before use; unknown or newer records fail closed with actionable guidance.
- **No automatic delivery.** Relay never commits, pushes, opens PRs, deploys, or publishes on its own; delivery-class actions always require explicit user authorization.
- **Dirty worktrees are normal.** Pre-existing untracked/modified files are preserved: writes refuse overwrites, patches conflict-detect instead of clobbering, and no Git mutation tools exist.

## Commands reference

```text
relay                                         Open or resume the workspace chat (terminal)
relay "<prompt>"                              Open the chat and submit the prompt once
relay --print "<prompt>"                      One conversational turn; print the reply and exit
relay init --goal <text>                      Create a project and declare its goal
relay project summary                         Goal, phase-grouped progress, recommendation
relay work ready|blocked|list                 Ready / blocked / all work, with explanations
relay work inspect <id>                       One work item in full (deps, attempts, evidence)
relay decisions list                          Decisions, with options and status
relay decisions resolve <id> --choice <label> Resolve a decision (optionally --rationale)
relay artifact inspect <id>[@version]         Structured content + rendered Markdown view
relay evidence <work-or-criterion>            Evidence attached to a work item or artifact
relay run [--autonomy <level>] [--provider <name>] [--max-ticks <n>]
                                              Run ready work (never auto-resolves decisions)
relay cancel <attempt-id>                     Cancel an active attempt
relay explain <work-id>                       Why a work item is ready, blocked, or waiting
relay expand                                  Apply the workflow's next proposed round
relay import-prd <file>                       Import an edited PRD view as a revision
relay doctor check|rebuild-projection         Verify / rebuild the local fact store
relay migrate --workspace <legacy-copy>       Migrate a legacy project copy (--dry-run to plan)
relay config show|path|get <key>              Inspect effective configuration
relay tui                                     Compatibility alias for the chat interface

Options:
  --workspace <path>    Operate on this directory (default: current directory)
  --print <prompt>      One scripted conversational turn; prints the reply and exits
  --json                Machine-readable output where supported
  --autonomy <level>    manual | safe | until-decision | bounded:<n>  (default: safe)
  --provider <name>     fake | openai | anthropic  (default: fake, offline)
  --max-ticks <n>       Scheduling waves per `run` (default: 1)
  -h, --help            Show this help
  -V, --version         Show the version
```

Legacy command names (`status`, `why`, `inspect`) map to their work-graph equivalents; `mode`, `auto`, and `pause` exit with migration guidance because no global modes or pause authority exist.

## Legacy migration

`relay migrate` imports an existing TypeScript-era Relay project (`.relay/state.db` plus artifacts) into a new `.relay-graph` fact store:

```bash
relay migrate --workspace /path/to/legacy-copy --dry-run   # plan and report first
relay migrate --workspace /path/to/legacy-copy             # staged, validated cutover
```

The legacy source is opened read-only and never modified; the new store is staged under `.relay-graph-migration-staging`, validated through the graph fold and the fact doctor, then switched in atomically. Any previous store is retained as `.relay-graph-pre-migration-<stamp>`; failures leave the workspace exactly as it was, and nothing is ever deleted automatically. An extraction report lists mapped, inferred, and unmapped legacy data. See [docs/migration-to-rust.md](docs/migration-to-rust.md).

## Development

The Rust workspace is pinned in `rust-toolchain.toml`. Run the same checks CI does before proposing a change:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo build --workspace --all-targets --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps --exclude relay-core --exclude relay-runtime
```

Integration tests live beside each crate in `crates/*/tests/`; the cross-crate suites (fact replay, semantic migration, crash recovery, capability security, scheduling concurrency, compatibility fixtures) are in `crates/relay-tests/`. The frozen fixtures under `tests/fixtures/` are immutable historical evidence exported by the retired TypeScript implementation — inputs to compatibility tests, not regenerable output. `crates/relay-core` and `crates/relay-runtime` are frozen reference material from the stopped compatibility port — read them for requirements and scenarios, but do not extend them. `crates/relay-tests/tests/repo_hygiene.rs` keeps implementation and build surfaces Rust-only while allowing sample JavaScript inside `tests/fixtures/`.

CI (`.github/workflows/ci.yml`) runs the fmt, clippy, test, and build gates, the doc gate over the live crates, and the focused intent-first suites on Ubuntu, macOS, and Windows.

## Local data and recovery

- **Reset a project:** delete `.relay-graph/`. Relay recreates state on the next `init`. Consider archiving it first — it is the complete export (the fact log plus every artifact version); zip it and attach it to a bug report.
- **Projection drift:** `relay doctor rebuild-projection` rebuilds the SQLite index from the JSONL facts alone — safe at any time, and a good smoke test that the fact log reconstructs all state.
- **Credentials:** never stored. API keys live in environment variables (or your OS credential store) only.
- **Corrupted store:** `relay doctor check` re-validates everything read-only and names the segment, line, and reason for any invalid record. Invalid history is never silently removed; the one automatic repair is truncating an incomplete crash tail, which is saved under `recovery/` first.
- **Legacy state and migration backups** are never deleted automatically.

## Limitations

Deliberately out of scope for this release:

- No autonomous multi-agent implementation inside Relay.
- No vector databases or semantic repository indexes.
- No browser or desktop automation.
- No hosted sync, accounts, collaboration, telemetry, or web interface.
- No deployment execution, automatic publishing, automatic PR creation, or unattended release actions.
- Static model routing only — no dynamic benchmarking or cost-optimized routing.

## License

MIT (SPDX: `MIT`). See [LICENSE](LICENSE).
