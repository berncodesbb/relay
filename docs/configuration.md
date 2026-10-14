# Relay configuration

Relay is configured in YAML. Configuration contains no credentials — only the *names* of environment variables that hold them. Resolution order (later layers win):

```text
built-in defaults
  <  ~/.config/relay/config.yaml        (user layer)
  <  relay.config.yaml                  (project layer, workspace root)
  <  invocation overrides               (--provider on the CLI; --role/--provider/--model
                                          for embedders via relay-config's invocation API)
```

Exact file names: `~/.config/relay/config.yaml` and `relay.config.yaml` at the workspace root (`--workspace` changes which directory is the root).

Each YAML layer is validated in isolation: unknown keys, providers, and out-of-range values fail per file, and every `routing` table in a file must cover all seven workflow modes (`intent` is optional per layer — the built-in defaults define it). Cross-reference checks — does every referenced role exist, are fallback graphs acyclic — run once, on the effective merged configuration, before any model call. `relay config path` lists the files that apply; `relay config show` prints the effective result.

## Terminology

| Term | What it is | Where it lives |
| --- | --- | --- |
| **Provider** | Infrastructure and credentials: which vendor adapter runs the call (`openai`, `anthropic`, `fake`) and which environment variable holds its key. Providers are connections, not responsibilities. | `providers:` |
| **Model role** | An execution responsibility that you name and define: a provider, a model, optional limits, and optional fallback roles. Role names are user-defined catalog keys — not an enum — such as `reasoning`, `coding`, `review`, `fast`, or your own (`product`, `ux-writing`, `security-review`). Name a role after what it does, never after a vendor or a vague quality. | `models:` |
| **Routing purpose** | Why a model call happens: one of the seven workflow modes, or the utility purpose `intent` (classifying user input). Each purpose maps to a role. | `routing:` |

A model call resolves **purpose → role → provider**. Re-pointing a responsibility (say, verification) at a different model is a one-line role edit; swapping vendors is a one-line provider edit inside the role.

## Full annotated example

The values below are the built-in defaults — a mixed Anthropic/OpenAI setup (Anthropic for everything except verification) expressed with five roles.

```yaml
# Schema version. Currently 1; defaults when omitted.
version: 1

providers:
  openai:
    # NAME of the environment variable holding the API key — never the key itself.
    apiKeyEnv: OPENAI_API_KEY
    # Optional: proxy or self-hosted gateway endpoint.
    # baseUrl: https://example.invalid/v1
  anthropic: { apiKeyEnv: ANTHROPIC_API_KEY }

# Model roles. Keys are your names (lowercase kebab-case, 1-64 characters);
# values are concrete routes.
models:
  reasoning:  { provider: anthropic, model: claude-sonnet-5-5 }
  architect:  { provider: anthropic, model: claude-opus-5-5 }
  coding:     { provider: anthropic, model: claude-sonnet-5-5 }
  review:
    provider: openai               # openai | anthropic | fake
    model: gpt-5.2
    fallbacks: [reasoning]         # optional role names, tried in order (max 3)
  fast:
    provider: anthropic
    model: claude-haiku-4-5
    # limits:                      # optional per-role limits
    #   maxOutputTokens: 8192      # 256 .. 1,000,000
    #   temperature: 0.2           # 0 .. 2

# Required: the seven workflow modes. `intent` is optional and defaults to fast.
# Each value is a role name; the legacy inline object form also still works.
routing:
  intent: fast
  discovery: reasoning
  specification: reasoning
  technical-design: architect
  planning: reasoning
  implementation: coding
  verification: review
  delivery: fast

# Required section; every field has a default, so `budgets: {}` is valid.
budgets:
  maxTurns: 24              # agent-loop turns per run        (1 .. 500)
  maxModelCalls: 48         # model calls per run              (1 .. 2,000)
  maxToolCalls: 96          # tool executions per run          (1 .. 2,000)
  maxOutputTokens: 400000   # output tokens per run            (1,024 .. 10,000,000)
  wallClockMs: 1800000      # 30-minute wall-clock deadline    (10,000 .. 86,400,000)
  maxRepeatedActions: 3     # identical tool calls allowed     (2 .. 20)

autonomy: next-checkpoint   # optional; one of the bounds below
tools: { commandTimeoutMs: 120000, maxSearchResults: 200, maxFileBytes: 2000000 }
```

Notes on specific fields:

- `providers.*.apiKeyEnv` is a variable **name**, for example `OPENAI_API_KEY`. Putting a key value here is a configuration error by design; Relay reads the value from the environment at call time and never persists it.
- Role names are lowercase kebab-case, 1–64 characters (`reasoning`, `ux-writing`, `security-review`). Nothing is reserved and no default set is hard-coded.
- `models.<role>.fallbacks` lists role names (max 3). A fallback inherits the referenced role's provider, model, and limits; the chain expands in declared order, deduplicates without reordering, and never exceeds one primary plus three fallbacks.
- `routing` keys are exactly the seven workflow modes plus the optional `intent`; a value is either a role name (string) or a legacy inline object.

### How a call is routed

Resolution is deterministic configuration; models never choose their own roles.

1. **Purpose.** Every model call carries a purpose: the current workflow mode, or `intent` when classifying user input.
2. **Lookup order.** An explicit routing entry for the purpose (`intent`) wins; otherwise the current workflow mode's route is used; otherwise — legacy behavior, only for a non-mode purpose with no explicit entry — the first configured route in configuration order. A missing workflow-mode route is a configuration error, never a silent reroute through another mode.
3. **Role → provider.** The route's role expands to a provider, a model, limits, and a fallback chain. Fallback routes whose provider is not registered (credential variable unset) drop out of the chain; an unregistered primary provider fails immediately with an actionable error.

Model events may carry the attempted role as an optional payload field; events recorded before roles existed, or from legacy inline routes, carry none. `relay config show` prints the effective role catalog and routing table, including each role's provider, model, limits, and fallbacks.

### Autonomy

The `autonomy` field accepts the legacy vocabulary the schema retains
(`off`, `next-checkpoint`, `workflow`, `until:<mode>`). The work-graph
runner does not read it as its default: scheduling autonomy is selected
per run on the command line, where the levels are `manual` (choose work
explicitly), `safe` (the default), `until-decision` (run until a human
decision is the blocking work), and `bounded:<n>` (at most `n` scheduling
waves). See `relay run --help`.

## Examples

### Adding a role of your own (user layer)

Define a role, reroute one purpose, and leave the rest to the defaults. Later layers merge into the role catalog by name, so the five built-in roles stay available; only `verification` changes.

```yaml
version: 1
budgets: {}
models:
  security-review:
    provider: openai
    model: gpt-5.2
    limits: { temperature: 0 }
routing: { intent: fast, discovery: reasoning, specification: reasoning,
           technical-design: architect, planning: reasoning, implementation: coding,
           verification: security-review, delivery: fast }
```

(Routing is folded onto fewer lines here; a layer's routing table must still cover all seven modes. `technical-design: security-review` would work the same way.)

### Role fallbacks and limits (project layer)

```yaml
version: 1
budgets: {}
models:
  coding:
    provider: anthropic
    model: claude-sonnet-5-5
    limits: { maxOutputTokens: 16384, temperature: 0.2 }
    fallbacks: [reasoning, fast]     # role names, in try order, max 3
routing: { intent: fast, discovery: reasoning, specification: reasoning,
           technical-design: architect, planning: reasoning, implementation: coding,
           verification: review, delivery: fast }
```

A layer that names `coding` replaces that entire role definition — provider, model, limits, and fallbacks together; fields are not merged within a role. Roles the layer does not mention survive from lower layers.

### Mixing roles with legacy inline routes

Both forms are accepted in the same file. Inline routes behave exactly as before and record no role in model events.

```yaml
version: 1
budgets: {}
models:
  reasoning: { provider: anthropic, model: claude-sonnet-5-5 }
routing:
  intent: fast
  discovery: reasoning                                               # role reference
  specification: reasoning
  technical-design: { provider: anthropic, model: claude-opus-5-5 }  # legacy inline
  planning: reasoning
  implementation: { provider: anthropic, model: claude-sonnet-5-5 }
  verification: { provider: openai, model: gpt-5.2 }
  delivery: { provider: anthropic, model: claude-haiku-4-5 }
```

### Migrating a version-1 configuration

No migration is required: a complete version-1 file containing only inline routes loads and behaves exactly as before, and old event logs replay unchanged (`role` is optional on model events). The same routing expressed with roles:

```yaml
# BEFORE — inline-only, still fully supported.
version: 1
budgets: {}
routing:
  discovery:           { provider: anthropic, model: claude-sonnet-5-5 }
  specification:       { provider: anthropic, model: claude-sonnet-5-5 }
  technical-design:    { provider: anthropic, model: claude-opus-5-5 }
  planning:            { provider: anthropic, model: claude-sonnet-5-5 }
  implementation:      { provider: anthropic, model: claude-sonnet-5-5 }
  verification:        { provider: openai, model: gpt-5.2 }
  delivery:            { provider: anthropic, model: claude-haiku-4-5 }
```

```yaml
# AFTER — identical effective routing, named. The explicit `intent` route is new.
version: 1
budgets: {}
models:
  reasoning:  { provider: anthropic, model: claude-sonnet-5-5 }
  architect:  { provider: anthropic, model: claude-opus-5-5 }
  coding:     { provider: anthropic, model: claude-sonnet-5-5 }
  review:     { provider: openai, model: gpt-5.2 }
  fast:       { provider: anthropic, model: claude-haiku-4-5 }
routing: { intent: fast, discovery: reasoning, specification: reasoning,
           technical-design: architect, planning: reasoning, implementation: coding,
           verification: review, delivery: fast }
```

### One vendor for everything

All modes on OpenAI, with an Anthropic fallback for verification (fallbacks name roles, so the backup is a role too):

```yaml
version: 1
budgets: {}
models:
  reasoning:  { provider: openai, model: gpt-5.2 }
  architect:
    provider: openai
    model: gpt-5.2
    limits: { maxOutputTokens: 16384 }
  coding:     { provider: openai, model: gpt-5.2 }
  review:
    provider: openai
    model: gpt-5.2
    fallbacks: [second-opinion]
  second-opinion: { provider: anthropic, model: claude-sonnet-5-5 }
  fast:       { provider: openai, model: gpt-5-mini }
routing: { intent: fast, discovery: reasoning, specification: reasoning,
           technical-design: architect, planning: reasoning, implementation: coding,
           verification: review, delivery: fast }
```

All modes on Anthropic, with a looser autonomy bound for long unattended runs:

```yaml
version: 1
budgets: {}
models:
  reasoning:  { provider: anthropic, model: claude-sonnet-5-5 }
  architect:  { provider: anthropic, model: claude-opus-5-5 }
  coding:     { provider: anthropic, model: claude-sonnet-5-5 }
  review:     { provider: anthropic, model: claude-sonnet-5-5 }
  fast:       { provider: anthropic, model: claude-haiku-4-5 }
routing: { intent: fast, discovery: reasoning, specification: reasoning,
           technical-design: architect, planning: reasoning, implementation: coding,
           verification: review, delivery: fast }
autonomy: workflow
```

### Offline demo/test setup

Route every role to the fake provider, which never contacts the network:

```yaml
version: 1
budgets: {}
models:
  reasoning:  { provider: fake, model: fake-standard }
  architect:  { provider: fake, model: fake-standard }
  coding:     { provider: fake, model: fake-standard }
  review:     { provider: fake, model: fake-standard }
  fast:       { provider: fake, model: fake-mini }
routing: { intent: fast, discovery: reasoning, specification: reasoning,
           technical-design: architect, planning: reasoning, implementation: coding,
           verification: review, delivery: fast }
```

## Layer merge behavior

- `models` merges by role name: a later layer that defines a role replaces that role's whole definition; roles it does not name survive from earlier layers.
- `routing` merges by routing key, and `providers` merges by provider name.
- Cross-reference validation runs on the effective configuration after all layers merge, so a project layer may route a purpose to a role defined in the user layer.

## Invocation overrides

Overrides replace every file layer for a single invocation. The precedence
model lives in `relay-config`'s invocation API — when combined, later wins:

```text
configured route  <  --role <name>  <  --provider <name>  <  --model <name>
```

- `--role <name>` routes every model call this invocation to one configured role (with that role's fallback chain), regardless of mode or purpose. An unknown role fails with a usage error listing the available roles, before any project state is created.
- `--provider <name>` forces one provider for all routes this invocation (models stay as routed).
- `--model <name>` overrides the routed model.

`--provider` and `--model` are low-level overrides and may intentionally
produce provider/model combinations that do not appear in the `models`
catalog.

The shipped CLI currently exposes `--provider <name>` (`fake` | `openai` |
`anthropic`; default `fake`, fully offline) and honors `RELAY_FAKE=1` with
the same effect. The `--role`/`--model` flags are available to embedders
through the library API (`relay_config::apply_invocation_overrides`).
Run-scheduling autonomy is chosen per run with `relay run --autonomy`
(see above), not through a configuration override.

## Validation behavior

Each file layer is validated strictly with actionable errors (`ConfigValidationError` listing each issue as `path: message`):

- Unknown fields anywhere are rejected — misspelled keys fail instead of being ignored.
- Unknown providers (`provider: openaai`) are rejected by the enum.
- `routing` keys other than the seven modes (and `intent`) are rejected by name.
- A routing table missing any of the seven modes is rejected with the missing mode list.
- Out-of-range numbers (for example `maxTurns: 0` or `temperature: 5`) are rejected with the allowed range.
- Invalid YAML fails with the parser error and the file path.

After the layers merge, the effective configuration is cross-reference validated — every issue names a dotted path, and all of them fail before any model call. Representative messages:

- `routing.verification: unknown role "reviewer"` — a routing entry naming a role no layer defines (the message lists the available roles).
- `models.review.fallbacks[0]: unknown role "reasonig"` — a fallback naming an undefined role.
- `models.review.fallbacks[0]: role "review" cannot fall back to itself` — self-reference.
- `models.review.fallbacks: cycle detected (review -> reasoning -> review)` — a fallback cycle.
- An expanded chain longer than one primary plus three fallbacks is rejected.

## Environment variables

| Variable | Required | Purpose |
| --- | --- | --- |
| `OPENAI_API_KEY` | For any role using `openai` | OpenAI credential (read at call time; never persisted) |
| `ANTHROPIC_API_KEY` | For any role using `anthropic` | Anthropic credential (default routing needs this) |
| `OPENAI_BASE_URL` | No | Optional OpenAI-compatible gateway endpoint (see `.env.example`; also settable via `providers.openai.baseUrl`) |
| `ANTHROPIC_BASE_URL` | No | Optional Anthropic gateway endpoint (see `.env.example`; also settable via `providers.anthropic.baseUrl`) |
| `RELAY_FAKE` | No | Set to `1` to force the fake provider for the invocation (same idea as `--provider fake`) |
| `NO_COLOR` | No | Disable ANSI colors, same as `--no-color` |

The `apiKeyEnv` settings only change *which variable name* Relay reads for each provider; they never hold values. A provider is registered only when its credential variable is non-empty, so routing never fails late inside an SDK call — a route pointing at an unregistered provider fails immediately with an error telling you to set the key or reroute.

## Inspecting the effective configuration

```bash
relay config show                 # print the fully resolved configuration
relay config path                 # list the layers that applied and their file paths
relay config get routing          # print one top-level key (e.g. routing, models, budgets)
relay config get models.review    # one role definition from the effective catalog
relay config get routing.intent   # the route for the intent purpose
```

`show` prints the merged result of defaults plus both file layers (CLI flags are per-invocation and are not included).
