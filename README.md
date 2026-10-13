# Relay

Relay is a local-first, model-agnostic CLI that carries product intent from discovery through specification, technical design, planning, implementation, verification, and delivery. Models are replaceable workers: Relay owns the workflow state, the artifacts, the permissions, and the transition policy. Every durable workflow change is an append-only event in a local SQLite log; every product artifact is versioned, readable Markdown you can review in any editor. Nothing leaves your machine except the model calls you configure.

## Requirements

- Node.js 22 or later (the built-in `node:sqlite` module needs 22.5.0+; Node 24 recommended). Relay fails with an actionable message on older runtimes.
- npm 10+.
- Git is optional; the Git tools report a clear error when the workspace is not a repository.

## Installation

No package has been published to npm yet. Install from a checkout:

```bash
git clone <this repository> && cd relay
npm ci
npm run build
npm install -g .        # exposes the `relay` command
```

`npm link` works equally well for development. CI runs on macOS, Linux, and Windows against Node 22 and 24.

## Quick start

```bash
cd your-project            # any directory; Relay stores state in .relay/
relay init --goal "Cut onboarding drop-off by walking new users through one saved filter"
relay run                   # interactive session; Relay drives discovery first
```

Relay asks you to approve each contractual artifact (brief, PRD, TRD, plan, verification report) before moving on. `relay status` shows where you are; `relay why` explains the last decision.

### The seven workflow modes

Modes run in order. Each produces one artifact and is gated by the deterministic transition policy, never by model say-so.

| Mode | Produces | Tool access | User approval to finish |
| --- | --- | --- | --- |
| `discovery` | opportunity brief | read | Yes — brief is a contractual checkpoint |
| `specification` | PRD | read | Yes |
| `technical-design` | TRD | read + execute | Yes |
| `planning` | execution plan | read + execute | Yes |
| `implementation` | decision log | read + write + execute (the only mode that may modify workspace files) | No artifact gate; work must stay inside the approved plan |
| `verification` | verification report | read + execute | Yes |
| `delivery` | delivery report | read | No artifact gate, but Relay always pauses before any external delivery action |

Revising an approved artifact deterministically marks every downstream artifact stale and routes work back to the earliest affected stage.

### Slash commands (inside `relay run`)

| Command | Effect |
| --- | --- |
| `/status` | Goal, mode, artifact versions and status, next action |
| `/why` | Explain the most recent routing or transition decision |
| `/inspect <artifact>` | Print an artifact's Markdown |
| `/mode <mode>` | Request a mode change (validated; forward skips are rejected) |
| `/auto <bound>` | Set the autonomy bound (bare `/auto` means `workflow`) |
| `/pause <reason>` | Pause automatic advancement |
| `/resume` | Resume a paused workflow |
| `/approve [artifact]` | Approve the current (or named) artifact |
| `/revise <kind> <reason>` | Revise an artifact; downstream artifacts go stale |

### Non-interactive and CI usage

`relay run` performs one workflow step per invocation in non-interactive mode — it works toward the next checkpoint or pause, prints the outcome, and exits with machine-readable output when asked:

```bash
relay run "clarify: the target is B2B admins" --no-interactive --json
relay status --json
```

Artifact approval is an explicit user action: it is requested through `relay run` (for example with the `/approve` slash command) so even scripted pipelines approve deliberately.

`--auto workflow` lets a run continue through reversible steps; it still stops for approvals, consequential choices, destructive operations, and delivery.

## Provider setup

Relay reads credentials from the environment and never writes them to project state, artifacts, logs, or the event database. Copy [.env.example](.env.example) to `.env` (already git-ignored) or export the variables:

```bash
export ANTHROPIC_API_KEY=sk-ant-...   # https://console.anthropic.com/settings/keys
export OPENAI_API_KEY=sk-...          # https://platform.openai.com/api-keys
```

Routing is configuration, not model output. Each routing purpose — the seven workflow modes plus `intent` (input classification) — maps to a named model role, and each role maps to a provider and model. The built-in defaults define five roles (`reasoning`, `architect`, `coding`, `review`, `fast`): Anthropic for everything except verification on OpenAI, so a default setup needs an Anthropic key (plus an OpenAI key unless you reroute verification). Roles are user-defined — add your own (for example `security-review`) in `relay.config.yaml` and point any purpose at it. See [docs/configuration.md](docs/configuration.md) for the full annotated configuration, the legacy inline routing form, and examples routing everything to one vendor.

The offline fake provider (`--provider fake`) never touches the network. It is a valid target for demos and deterministic tests:

```bash
relay init --goal "demo"
relay run --provider fake
```

## Project files

All Relay state lives inside your workspace:

```text
.relay/
  state.db                 # SQLite: append-only events + derived project index
  artifacts/
    prd/v3-onboarding-prd.md
    trd/v1-...md           # one immutable file per version, YAML frontmatter
```

- `state.db` holds the event log (`events`), a derived project listing (`projects`), and `schema_migrations`. UPDATE and DELETE on `events` are aborted by database triggers.
- Each artifact version is an immutable Markdown file with frontmatter (`id`, `kind`, `version`, `title`, `status`, timestamps). Artifacts are the reviewable record of the work.
- `.relay/` is git-ignored by default (see [.gitignore](.gitignore)).

## Security boundaries

These are enforced in code, not convention:

- **Workspace-bounded tools.** Every model-supplied path is resolved through a path guard that rejects absolute paths, `~`, `..`, and NUL bytes, and resolves symlinks on the workspace root and every existing ancestor so a link inside the workspace cannot escape it.
- **Writes never overwrite.** `write-file` only creates new files (atomic create; existing paths are refused). Modifications happen exclusively through `apply-patch`: a unified diff whose context lines must match exactly, applied all-or-nothing across files. On any conflict nothing is written, and a re-stat guard aborts if a target changed while the patch was being prepared. File deletion and rename patches are refused.
- **No shell interpolation.** Commands run via `spawn(file, argsArray)` — never a shell string. Shell metacharacters in the command name are rejected. Timeouts and cancellation terminate the whole detached process group (SIGTERM, escalating to SIGKILL).
- **Bounded environment inheritance.** Child processes inherit only an allowlist (`PATH`, `HOME`, `LANG`, `LC_*`, `TERM`, `TMPDIR`, `CI`, `TZ`, `SHELL`) plus variables explicitly requested by name. Secret-shaped names are refused outright.
- **Secret redaction.** Tool outputs, transcripts, and provider error messages are redacted before they are recorded: literal values of secret-named environment variables and known token shapes (`sk-…`, `sk-ant-…`, AWS keys, GitHub/Slack tokens, bearer tokens, private keys) are replaced with markers. Provider errors are trimmed and header-free. Keys are never persisted anywhere.
- **Read-only Git surface.** Only `git-status`, `git-diff`, `git-log`, and `repo-instructions` exist as tools. No `add`/`commit`/`push`/`checkout`/`reset`/`stash`/`clean` is registered, so user worktree state cannot be rewritten by a tool.
- **Deterministic permission gate.** Every tool call is checked, in order: paused workflow → mode-permitted capability → write rules → autonomy bound → command denylist (`git push`, `git commit`, `git reset --hard`, `git clean`, `git stash`, `npm publish`, `rm -rf`, `mkfs`, `dd of=`, `curl|sh`, `chmod -R 777`, …). Denials are recorded as `tool.denied` events.
- **Append-only event store.** History cannot be edited after the fact; reads re-validate every stored event and surface corrupt or foreign rows as actionable errors.
- **No automatic delivery.** Relay never commits, pushes, opens PRs, deploys, or publishes on its own. The delivery mode assembles the trace and pauses; every delivery-class action requires explicit user authorization.
- **Dirty worktrees are normal.** Pre-existing untracked/modified files are preserved: writes refuse overwrites, patches conflict-detect instead of clobbering, and no Git mutation tools exist.

## Commands reference

```text
relay init [--goal <text>]                 Create local Relay state (.relay/)
relay run [message] [options]              Run the workflow (interactive by default)
relay status [--json]                      Show goal, mode, progress, artifacts, next action
relay inspect <artifact>                   Print an artifact's Markdown
relay mode <mode>                          Override the active workflow mode
relay auto [off|next-checkpoint|until:<mode>|workflow]
                                           Change the autonomy bound
relay pause [<reason>]                     Pause automatic advancement
relay why                                  Explain the last routing/transition decision
relay config [show|path|get <key>]         Inspect effective configuration

Options:
  --workspace <path>     Operate on this directory (default: current directory)
  --no-interactive       Force plain, non-interactive output (scripts and CI)
  --role <name>          Route every model call to this configured role
  --provider <name>      Force a provider for this invocation (openai|anthropic|fake)
  --model <name>         Override the routed model for this invocation
  --auto <bound>         Set the autonomy bound for a run
  --json                 Machine-readable output where supported
  --no-color             Disable ANSI colors
  -h, --help             Show this help
  -V, --version          Show version
```

When combined, invocation overrides apply in order — configured route, then `--role`, then `--provider`, then `--model` (later wins); `--provider`/`--model` are low-level and may produce combinations absent from your role catalog. An unknown `--role` fails before any project state is created.

Autonomy bounds: `off` pauses at every checkpoint, `next-checkpoint` (the default) runs until the next artifact approval or intervention, `until:<mode>` runs until the workflow enters that mode, `workflow` runs end to end while still pausing for consequential choices, approvals, destructive operations, and delivery.

## Development

```bash
npm ci               # install dependencies
npm run typecheck    # tsc --noEmit
npm test             # vitest run (no network access required)
npm run build        # emit dist/
npm run check        # typecheck + test + build
```

Test layout under `test/`:

| Suite | Covers |
| --- | --- |
| `unit/core` | Event contracts, reducer replay, transition policy |
| `unit/policies` | Permission, intervention, and transition policies |
| `unit/tools` | Tool safety: path guard, patch conflicts, redaction, bounded spawn |
| `unit/providers` | Adapter normalization, error mapping, fake provider, stream collection |
| `unit/runtime` | Agent loop, router, budgets, cancellation, retries |
| `unit/storage` | Database bootstrap, migrations, event store, project repository |
| `unit/artifacts` | Draft schemas, rendering, Markdown store, invalidation |
| `unit/cli`, `unit/tui` | Argument parsing and terminal components |
| `integration/replay` | Restart reconstruction from the event log |
| `integration/workflow` | End-to-end seven-mode workflow on the fake provider |

Run the end-to-end workflow (deterministic, no API keys, no network):

```bash
npm test -- --run test/integration/workflow.test.ts
```

CI (`.github/workflows/ci.yml`) runs typecheck, tests, build, a CLI smoke check, and `npm pack --dry-run` on Ubuntu/macOS/Windows against Node 22 and 24.

## Local data and recovery

- **Reset a project:** delete `.relay/`. Relay recreates state on the next `init`. Artifacts live in the same directory, so resetting removes them too.
- **Credentials:** never stored. API keys live in environment variables (or your OS credential store) only; `.relay/` contains no secrets.
- **Diagnostics:** the `.relay/` directory is the complete export — the event log plus every artifact version. Zip it and attach it to a bug report.
- **Corrupted database:** archive `.relay/state.db` (and its `-wal`/`-shm` sidecars, if present) and re-run. Artifact files survive as plain Markdown and remain readable with `relay inspect` or any editor. Because project state is rebuilt by replaying events, moving the database aside starts a fresh event log for the workspace. The `events` table is append-only and every row is re-validated on read — unknown event types, incompatible schema versions, and invalid payloads surface as errors that name the sequence number and suggest a remedy rather than silently corrupting state.

## Limitations

Deliberately out of scope for this release:

- No autonomous multi-agent implementation inside Relay.
- No vector databases or semantic repository indexes.
- No browser or desktop automation.
- No hosted sync, accounts, collaboration, telemetry, or web interface.
- No deployment execution, automatic publishing, automatic PR creation, or unattended release actions.
- Static model routing only — no dynamic benchmarking or cost-optimized routing.
- Node.js runtime only (no native launcher or sandbox).

## License

MIT (SPDX: `MIT`). See [LICENSE](LICENSE).
