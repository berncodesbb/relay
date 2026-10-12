# Relay configuration

Relay is configured in YAML. Configuration contains no credentials — only the *names* of environment variables that hold them. Resolution order (later layers win):

```text
built-in defaults
  <  ~/.config/relay/config.yaml        (user layer)
  <  relay.config.yaml                  (project layer, workspace root)
  <  CLI overrides                      (--provider / --model / --auto)
```

Exact file names: `~/.config/relay/config.yaml` and `relay.config.yaml` at the workspace root (`--workspace` changes which directory is the root).

Each YAML layer is validated in isolation and must be internally complete for routing: every routing table in a file must cover all seven modes. Sections merge key-by-key across layers, with later layers overriding earlier ones. Use `relay config path` to see which files apply and `relay config show` to print the effective result.

## Full annotated example

```yaml
# Schema version. Currently 1; defaults when omitted.
version: 1

providers:
  openai:
    # NAME of the environment variable holding the API key — never the key itself.
    apiKeyEnv: OPENAI_API_KEY
    # Optional: proxy or self-hosted gateway endpoint.
    # baseUrl: https://example.invalid/v1
  anthropic:
    apiKeyEnv: ANTHROPIC_API_KEY
    # baseUrl: https://example.invalid

# Required: exactly the seven workflow modes, no more, no less.
routing:
  discovery:
    provider: anthropic            # openai | anthropic | fake
    model: claude-sonnet-5-5
    fallbacks:                     # optional, tried in order after retries
      - provider: openai           # (max 3 entries)
        model: gpt-5.2
    limits:                        # optional per-route limits
      maxOutputTokens: 8192        # 256 .. 1,000,000
      temperature: 0.2             # 0 .. 2
  specification:            { provider: anthropic, model: claude-sonnet-5-5 }
  technical-design:         { provider: anthropic, model: claude-opus-5-5 }
  planning:                 { provider: anthropic, model: claude-sonnet-5-5 }
  implementation:           { provider: anthropic, model: claude-sonnet-5-5 }
  verification:             { provider: openai, model: gpt-5.2 }
  delivery:                 { provider: anthropic, model: claude-haiku-4-5 }

# Required section; every field has a default, so `budgets: {}` is valid.
budgets:
  maxTurns: 24              # agent-loop turns per run        (1 .. 500)
  maxModelCalls: 48         # model calls per run              (1 .. 2,000)
  maxToolCalls: 96          # tool executions per run          (1 .. 2,000)
  maxOutputTokens: 400000   # output tokens per run            (1,024 .. 10,000,000)
  wallClockMs: 1800000      # 30-minute wall-clock deadline    (10,000 .. 86,400,000)
  maxRepeatedActions: 3     # identical tool calls allowed     (2 .. 20)

# Optional; defaults to next-checkpoint.
autonomy: next-checkpoint

tools:
  commandTimeoutMs: 120000  # run-command ceiling              (1,000 .. 600,000)
  maxSearchResults: 200     # search-text match cap            (1 .. 10,000)
  maxFileBytes: 2000000     # read-file byte cap               (1,024 .. 20,000,000)
```

Notes on specific fields:

- `providers.*.apiKeyEnv` is a variable **name**, for example `OPENAI_API_KEY`. Putting a key value here is a configuration error by design; Relay reads the value from the environment at call time and never persists it.
- `routing` keys must be exactly the seven workflow modes (`discovery`, `specification`, `technical-design`, `planning`, `implementation`, `verification`, `delivery`); `provider` must be one of `openai`, `anthropic`, `fake`.
- Apart from the illustrative `fallbacks`/`limits` keys on `discovery`, the values above are the built-in defaults — which are already a mixed Anthropic/OpenAI setup (Anthropic for six modes, OpenAI for verification).

### Autonomy bounds

| Value | Meaning |
| --- | --- |
| `off` | Pause at every checkpoint; non-read tools are denied entirely |
| `next-checkpoint` | Default. Run until the next artifact approval or intervention |
| `until:<mode>` | Run until the workflow enters `<mode>` (one of the seven modes) |
| `workflow` | Run end to end, still pausing for consequential choices, approvals, destructive operations, and delivery |

## Examples

All modes on OpenAI (`~/.config/relay/config.yaml` or `relay.config.yaml`):

```yaml
routing:
  discovery:           { provider: openai, model: gpt-5.2 }
  specification:       { provider: openai, model: gpt-5.2 }
  technical-design:    { provider: openai, model: gpt-5.2, limits: { maxOutputTokens: 16384 } }
  planning:            { provider: openai, model: gpt-5.2 }
  implementation:      { provider: openai, model: gpt-5.2 }
  verification:        { provider: openai, model: gpt-5.2, fallbacks: [{ provider: anthropic, model: claude-sonnet-5-5 }] }
  delivery:            { provider: openai, model: gpt-5-mini }
budgets: {}
```

All modes on Anthropic, with a looser autonomy bound for long unattended runs:

```yaml
routing:
  discovery:           { provider: anthropic, model: claude-sonnet-5-5 }
  specification:       { provider: anthropic, model: claude-sonnet-5-5 }
  technical-design:    { provider: anthropic, model: claude-opus-5-5 }
  planning:            { provider: anthropic, model: claude-sonnet-5-5 }
  implementation:      { provider: anthropic, model: claude-sonnet-5-5 }
  verification:        { provider: anthropic, model: claude-sonnet-5-5 }
  delivery:            { provider: anthropic, model: claude-haiku-4-5 }
budgets: {}
autonomy: workflow
```

Mixed routing is what the defaults shown above already do — Anthropic for six modes (a heavier `claude-opus` model for technical design, a lighter `claude-haiku` for delivery) and OpenAI for verification; copy that block and adjust per mode.

Offline demo/test setup — route everything to the fake provider, which never contacts the network:

```yaml
routing:
  discovery:           { provider: fake, model: fake-standard }
  specification:       { provider: fake, model: fake-standard }
  technical-design:    { provider: fake, model: fake-standard }
  planning:            { provider: fake, model: fake-standard }
  implementation:      { provider: fake, model: fake-standard }
  verification:        { provider: fake, model: fake-standard }
  delivery:            { provider: fake, model: fake-mini }
budgets: {}
```

## CLI overrides

Flags override every file layer for a single invocation:

- `--provider <name>` forces one provider for all modes this invocation (models stay as routed).
- `--model <name>` overrides the routed model.
- `--auto <bound>` sets the autonomy bound for the run.

Example: `relay run "refine the PRD" --provider openai --model gpt-5.2 --auto until:implementation`.

## Validation behavior

Configuration is validated strictly with actionable errors (`ConfigValidationError` listing each issue as `path: message`):

- Unknown fields anywhere are rejected — misspelled keys fail instead of being ignored.
- Unknown providers (`provider: openaai`) are rejected by the enum.
- `routing` keys that are not one of the seven modes are rejected by name.
- A routing table missing any of the seven modes is rejected with the missing mode list.
- Out-of-range numbers (for example `maxTurns: 0` or `temperature: 5`) are rejected with the allowed range.
- Invalid YAML fails with the parser error and the file path.

## Environment variables

| Variable | Required | Purpose |
| --- | --- | --- |
| `OPENAI_API_KEY` | For any route using `openai` | OpenAI credential (read at call time; never persisted) |
| `ANTHROPIC_API_KEY` | For any route using `anthropic` | Anthropic credential (default routing needs this) |
| `OPENAI_BASE_URL` | No | Optional OpenAI-compatible gateway endpoint (see `.env.example`; also settable via `providers.openai.baseUrl`) |
| `ANTHROPIC_BASE_URL` | No | Optional Anthropic gateway endpoint (see `.env.example`; also settable via `providers.anthropic.baseUrl`) |
| `RELAY_FAKE` | No | Set to `1` to force the fake provider for the invocation (same idea as `--provider fake`) |
| `NO_COLOR` | No | Disable ANSI colors, same as `--no-color` |

The `apiKeyEnv` settings only change *which variable name* Relay reads for each provider; they never hold values. A provider is registered only when its credential variable is non-empty, so routing never fails late inside an SDK call — a route pointing at an unregistered provider fails immediately with an error telling you to set the key or reroute.

## Inspecting the effective configuration

```bash
relay config show          # print the fully resolved configuration
relay config path          # list the layers that applied and their file paths
relay config get routing   # print one top-level key (e.g. routing, budgets, autonomy)
```

`show` prints the merged result after defaults, both file layers, and CLI overrides — exactly what a run would use.
