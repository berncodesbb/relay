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

Each YAML layer is validated in isolation: unknown keys, malformed connection names, and out-of-range values fail per file, and every `routing` table in a file must cover all seven workflow modes (`intent` is optional per layer — the built-in defaults define it). Cross-reference checks — does every referenced role and provider connection exist, are fallback graphs acyclic, do presets match adapters — run once, on the effective merged configuration, before any model call or project-state change. `relay config path` lists the files that apply; `relay config show` prints the effective result.

## Terminology

| Term | What it is | Where it lives |
| --- | --- | --- |
| **Adapter kind** | The wire *protocol* a connection speaks: `anthropic` (the native Messages API — also used for Anthropic Messages-compatible gateways via `baseUrl`) or `fake` (deterministic, offline, in-process). Adapters are the closed set Relay ships; they never grow one entry per vendor. Anthropic Messages is Relay's only live external protocol; the `openai-chat` adapter was removed (see [Migrating off the removed OpenAI Chat support](#migrating-off-the-removed-openai-chat-support)). | `providers.<name>.adapter` |
| **Provider connection** | A named piece of infrastructure you configure: an adapter, the environment variable holding its credential, an optional base URL, and capability settings. Connection names are yours (`anthropic`, `claude-proxy`, `local-gateway`); several connections may share one adapter. Connections are infrastructure, not responsibilities. | `providers:` |
| **Model role** | An execution responsibility that you name and define: a connection, a model, optional limits, and optional fallback roles. Role names are user-defined catalog keys — not an enum — such as `reasoning`, `coding`, `review`, `fast`, or your own (`product`, `ux-writing`, `security-review`). Name a role after what it does, never after a vendor or a vague quality. | `models:` |
| **Routing purpose** | Why a model call happens: one of the seven workflow modes (`discovery`, `specification`, `technical-design`, `planning`, `implementation`, `verification`, `delivery`), or the utility purpose `intent` (conversational turns). Each purpose maps to a role. | `routing:` |

A model call resolves **purpose → role → provider connection → adapter → model**, deterministically, before the attempt starts; models never choose or rewrite their own routing. Relay owns the conversation transcript and the tool loop on top of whatever connection serves the call: tools are native `tool_use`/`tool_result` exchanges executed against the attempt's capability grant, and provider-hosted tools or provider-side conversation state are never authoritative.

## The built-in defaults are offline

The defaults define two connections — `fake` (adapter `fake`) and `anthropic` (adapter `anthropic`, reading `ANTHROPIC_API_KEY`) — and route all five built-in roles to the `fake` connection with the model `fake-standard`. A fresh checkout therefore runs `relay`, `relay run`, and `relay --print` end to end with no keys, no configuration, and no network. This is effective configuration, not hidden CLI branching: override any role or add a live connection in your own layers and the default profile yields predictably, role by role.

`relay config show` renders exactly this effective shape on a fresh checkout:

```json
{
  "version": 2,
  "providers": {
    "fake": { "adapter": "fake" },
    "anthropic": { "adapter": "anthropic", "apiKeyEnv": "ANTHROPIC_API_KEY" }
  },
  "models": {
    "reasoning": { "provider": "fake", "model": "fake-standard" },
    "architect": { "provider": "fake", "model": "fake-standard" },
    "coding": { "provider": "fake", "model": "fake-standard" },
    "review": { "provider": "fake", "model": "fake-standard" },
    "fast": { "provider": "fake", "model": "fake-standard" }
  },
  "routing": {
    "intent": "fast",
    "discovery": "reasoning",
    "specification": "reasoning",
    "technical-design": "architect",
    "planning": "reasoning",
    "implementation": "coding",
    "verification": "review",
    "delivery": "fast"
  }
}
```

## Provider connections

```yaml
providers:
  # Any name (lowercase kebab-case, 1-64 characters); the name is what
  # models.<role>.provider and --provider refer to.
  anthropic:
    adapter: anthropic                # required: anthropic | fake
    apiKeyEnv: ANTHROPIC_API_KEY      # NAME of the env var; never a key value

  gateway:
    adapter: anthropic                # any Anthropic Messages-compatible endpoint
    baseUrl: https://gateway.example/anthropic
    apiKeyEnv: GATEWAY_API_KEY

  local:
    adapter: anthropic                # a second anthropic connection coexists
    baseUrl: http://localhost:8787
    apiKeyEnv: LOCAL_GATEWAY_KEY
```

- `adapter` is required, except on the legacy keys `anthropic` and `fake`, where it is inferred. (The legacy `openai` key no longer infers anything — a bare `providers.openai` entry fails with migration guidance; see the migration section below.)
- `apiKeyEnv` must be an environment-variable *name* (`[A-Za-z_][A-Za-z0-9_]*`) — anything that looks like a credential value (`sk-…`, contains `=` or whitespace) is a configuration error by design. When omitted it defaults per adapter (`ANTHROPIC_API_KEY` for `anthropic`, none for `fake`). The value is read from the environment only when the adapter is constructed for a run, and never persisted, logged, or echoed.
- `baseUrl` is optional (validated as a URL); `fake` connections ignore it.
- Layers merge connections **by name with complete replacement**: a later layer that defines `gateway` replaces that whole connection; connections it does not name survive — the same semantics as the role catalog. (A side effect inherited from version 1: a layer whose `providers` section omits the legacy `anthropic` key resets that connection to its default, while custom names survive.)

### Capabilities and profiles

Compatible endpoints differ in what they accept, so each connection carries an effective **capability profile**, resolved deterministically as `adapter defaults → explicit overrides`:

```yaml
providers:
  gateway:
    adapter: anthropic
    baseUrl: https://gateway.example/anthropic
    apiKeyEnv: GATEWAY_API_KEY
    capabilities:                     # explicit overrides win over adapter defaults
      structuredOutput: json-object
```

Capability fields and spellings: `instructions` (`system-instruction`), `toolCalls`, `streamedToolArguments`, `parallelToolCalls`, `streamedUsage`, `imageInput` (`supported` | `unsupported`), `structuredOutput` (`json-schema` | `json-object` | `unsupported`), `outputTokenParam` (`max-tokens`). No presets ship today; the preset mechanism remains for future named profiles, and the `glm` preset was removed with the `openai-chat` adapter.

A requested Relay feature is either translated by a documented rule recorded in the profile (native `json-schema` → `json-object` with the schema embedded in the system instruction) or rejected with an actionable error **before any HTTP request is issued** — never silently dropped. Image/file input is recorded as unsupported everywhere until Relay requests it.

## Full annotated example

A mixed live setup expressed with named connections and five roles (all fields shown are the complete set):

```yaml
# Schema version. Currently 2; version 1 files that avoid removed OpenAI
# spellings also load (see the migration section below).
version: 2

providers:
  anthropic: { adapter: anthropic, apiKeyEnv: ANTHROPIC_API_KEY }
  gateway: { adapter: anthropic, baseUrl: https://gateway.example/anthropic,
             apiKeyEnv: GATEWAY_API_KEY }

# Model roles. Keys are your names (lowercase kebab-case, 1-64 characters);
# values reference provider connections by name.
models:
  reasoning:  { provider: anthropic, model: claude-sonnet-5-5 }
  architect:  { provider: anthropic, model: claude-opus-5-5 }
  coding:     { provider: gateway, model: claude-sonnet-5-5 }
  review:
    provider: gateway               # any configured connection name
    model: claude-sonnet-5-5
    fallbacks: [reasoning]          # optional role names, tried in order (max 3)
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

### How a call is routed

Resolution is deterministic configuration; models never choose their own roles, connections, or fallbacks.

1. **Purpose.** Every model call carries a purpose. Built-in work items derive theirs from the workflow catalog's grouped-view phase (Discovery→`discovery` … Delivery→`delivery`); conversational turns use `intent`. The mapping is composition metadata used while constructing an attempt's immutable grant — it is not stored workflow state and does not affect scheduling or ordering. An executable work kind absent from the catalog fails with an actionable missing-route error rather than silently routing through another purpose.
2. **Lookup order.** An explicit routing entry for the purpose (`intent`) wins; otherwise the workflow purpose's route is used; otherwise — legacy behavior, only for a non-mode purpose with no explicit entry — the first configured route in configuration order. A missing workflow-mode route is a configuration error, never a silent reroute through another mode.
3. **Role → connection → adapter.** The route's role expands to a connection, a model, limits, and a fallback chain (ordered, deduplicated, at most one primary plus three fallbacks). The selected connection, model, and role are recorded in the attempt's immutable grant and diagnostics.
4. **Fallback.** The complete chain is resolved before the attempt starts. A connection whose credential variable is unset is *unavailable*: an unavailable **primary** fails immediately with an actionable error naming the variable; unavailable **fallback** routes drop out of the chain. At call time, Relay advances to the next route only for provider **availability** failures (rate-limit, server, network, timeout) — never for cancellation, invalid model output, rejected credentials, unsupported capabilities, policy rejection, or budget exhaustion. Every attempted route consumes the run's budgets and is visible in bounded, redacted diagnostics.

Model events may carry the attempted role as an optional payload field; events recorded before roles existed, or from legacy inline routes, carry none. `relay config show` prints the effective connections, role catalog, and routing table.

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
version: 2
budgets: {}
models:
  security-review:
    provider: anthropic
    model: claude-sonnet-5-5
    limits: { temperature: 0 }
routing: { intent: fast, discovery: reasoning, specification: reasoning,
           technical-design: architect, planning: reasoning, implementation: coding,
           verification: security-review, delivery: fast }
```

(Routing is folded onto fewer lines here; a layer's routing table must still cover all seven modes. `technical-design: security-review` would work the same way.)

### Role fallbacks and limits (project layer)

```yaml
version: 2
budgets: {}
models:
  coding:
    provider: gateway
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
version: 2
budgets: {}
models:
  reasoning: { provider: anthropic, model: claude-sonnet-5-5 }
routing:
  intent: fast
  discovery: reasoning                                               # role reference
  specification: reasoning
  technical-design: { provider: anthropic, model: claude-opus-5-5 }  # legacy inline
  planning: reasoning
  implementation: { provider: gateway, model: claude-sonnet-5-5 }
  verification: { provider: gateway, model: claude-sonnet-5-5 }
  delivery: { provider: anthropic, model: claude-haiku-4-5 }
```

### Migrating off the removed OpenAI Chat support

Relay shipped an `openai-chat` adapter (Chat Completions compatibility, including the `glm` preset) until 2026-10-15, when Anthropic Messages became the sole live external protocol. The removal is intentional and deliberate: endpoint capabilities and model identifiers are not equivalent between protocols, so Relay never translates or guesses a replacement.

Concretely, version-1 files **that do not name removed spellings load unchanged** — the legacy `anthropic` and `fake` keys still infer their adapters, inline routes still work, and old event logs replay unchanged. Configuration that does name removed spellings fails at validation time — before any network call or project-state change — with a targeted migration error:

- `adapter: openai-chat` (any connection) →
  `providers.<name>.adapter: openai-chat was removed; configure an Anthropic Messages-compatible endpoint with adapter: anthropic and select a model supported by that endpoint`
- a bare legacy `providers.openai` entry (no `adapter`) →
  `providers.openai: the implicit openai connection was removed with the openai-chat adapter; configure an Anthropic Messages-compatible connection (adapter: anthropic) and update routes that name openai`
- `preset: glm` →
  `providers.<name>: the glm preset was removed with the openai-chat adapter; configure an Anthropic Messages-compatible connection instead`
- removed capability spellings (`instructions: developer`, `outputTokenParam: max-completion-tokens` or `model-default`) → an error of the same shape naming the `anthropic` replacement (`system-instruction`, `max-tokens`).

Routes that referenced the removed connection by *name* (`provider: openai`) fail effective validation with the available connections listed — define a replacement connection and update the reference:

```yaml
# BEFORE — fails: the openai connection no longer exists.
version: 1
budgets: {}
providers:
  openai: { apiKeyEnv: OPENAI_API_KEY }
  anthropic: { apiKeyEnv: ANTHROPIC_API_KEY }
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
# AFTER — identical shape, one live protocol. The explicit `intent` route is new.
version: 2
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
```

### One connection for everything

All modes on one Anthropic connection, with a second connection as a verification fallback (fallbacks name roles, so the backup is a role too):

```yaml
version: 2
budgets: {}
providers:
  anthropic: { adapter: anthropic, apiKeyEnv: ANTHROPIC_API_KEY }
  gateway:   { adapter: anthropic, baseUrl: https://gateway.example/anthropic,
               apiKeyEnv: GATEWAY_API_KEY }
models:
  reasoning:  { provider: anthropic, model: claude-sonnet-5-5 }
  architect:
    provider: anthropic
    model: claude-opus-5-5
    limits: { maxOutputTokens: 16384 }
  coding:     { provider: anthropic, model: claude-sonnet-5-5 }
  review:
    provider: anthropic
    model: claude-sonnet-5-5
    fallbacks: [second-opinion]
  second-opinion: { provider: gateway, model: claude-sonnet-5-5 }
  fast:       { provider: anthropic, model: claude-haiku-4-5 }
routing: { intent: fast, discovery: reasoning, specification: reasoning,
           technical-design: architect, planning: reasoning, implementation: coding,
           verification: review, delivery: fast }
```

### Multiple connections on one adapter

Two Anthropic Messages-compatible endpoints coexist as separate names; roles and `--provider` pick between them:

```yaml
version: 2
budgets: {}
providers:
  anthropic: { adapter: anthropic, apiKeyEnv: ANTHROPIC_API_KEY }
  gateway:   { adapter: anthropic, baseUrl: https://gateway.example/anthropic,
               apiKeyEnv: GATEWAY_API_KEY }
  local:     { adapter: anthropic, baseUrl: http://localhost:8787,
               apiKeyEnv: LOCAL_GATEWAY_KEY }
models:
  fast: { provider: gateway, model: claude-haiku-4-5 }
```

### Offline demo/test setup

This is already the built-in default; write it explicitly only to pin it:

```yaml
version: 2
budgets: {}
providers:
  fake: { adapter: fake }
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
- `routing` merges by routing key.
- `providers` merges by connection name, with complete replacement (see above).
- Cross-reference validation runs on the effective configuration after all layers merge, so a project layer may route a purpose to a role defined in the user layer and reference a connection either layer defined.

## Invocation overrides

Overrides replace every file layer for a single invocation. The precedence
model lives in `relay-config`'s invocation API — when combined, later wins:

```text
configured route  <  --role <name>  <  --provider <name>  <  --model <name>
```

- `--role <name>` routes every model call this invocation to one configured role (with that role's fallback chain), regardless of purpose. An unknown role fails with a usage error listing the available roles, before any project state is created.
- `--provider <name>` forces every route onto one configured provider connection; model ids and limits stay as routed (so point it at a connection your routes are configured for — on the offline defaults, `--provider anthropic` would send the `fake-standard` model id to Anthropic). An unknown connection fails with a usage error listing the configured connections.
- `--model <name>` overrides the routed model.

`--provider` and `--model` are low-level overrides and may intentionally
produce connection/model combinations that do not appear in the `models`
catalog.

The shipped CLI exposes `--provider <name>` with any configured connection
name (the defaults provide `fake` and `anthropic`; the default is
`fake`, fully offline). Configuration errors and unknown connections exit
with code 2 and create no project state; provider transport failures during
a run remain runtime failures (exit 1). The `--role`/`--model` flags are
available to embedders through the library API
(`relay_config::apply_invocation_overrides`). Run-scheduling autonomy is
chosen per run with `relay run --autonomy` (see above), not through a
configuration override.

## Validation behavior

Each file layer is validated strictly with actionable errors (`ConfigValidationError` listing each issue as `path: message`):

- Unknown fields are ignored exactly as before (Zod default-object behavior); values are validated strictly.
- Connection names must be lowercase kebab-case, 1–64 characters.
- `adapter` must be one of `anthropic | fake` (required except on the legacy keys `anthropic` and `fake`); the removed `openai-chat` spelling fails with the migration error above rather than a generic unknown-adapter message.
- `apiKeyEnv` must be an environment-variable name; a credential-looking value is rejected.
- `baseUrl` must be a valid URL.
- Capability values must use their documented spellings; removed spellings (`developer`, `max-completion-tokens`, `model-default`) and unknown preset names fail with actionable errors naming the replacement or the available options.
- `routing` keys other than the seven modes (and `intent`) are rejected by name; a routing table missing any of the seven modes is rejected with the missing-mode list.
- Out-of-range numbers (for example `maxTurns: 0` or `temperature: 5`) are rejected with the allowed range.
- Invalid YAML (including duplicate mapping keys) fails with the parser error and the file path.

After the layers merge, the effective configuration is cross-reference validated — every issue names a dotted path and the layer that introduced it, and all of them fail before any model call or project-state change. Representative messages:

- `routing.verification.provider: references unknown provider connection "openaai"; available connections: fake, anthropic. Define a providers.openaai entry.`
- `models.review.fallbacks[0]: unknown role "reasonig"` — a fallback naming an undefined role.
- `models.review.fallbacks[0]: role "review" cannot fall back to itself` — self-reference.
- `models.review.fallbacks: cycle detected (review -> reasoning -> review)` — a fallback cycle.
- `providers.gateway: unknown capability preset "grok" (available presets: )` — no presets ship today.
- An expanded chain longer than one primary plus three fallbacks is rejected.

## Environment variables

| Variable | Required | Purpose |
| --- | --- | --- |
| `ANTHROPIC_API_KEY` | For any connection defaulting to it | Anthropic credential (read at adapter construction; never persisted) |
| *your `apiKeyEnv` values* | For the connections that name them | e.g. `GATEWAY_API_KEY` for a `gateway` connection |
| `NO_COLOR` | No | Disable ANSI colors, same as `--no-color` |

Endpoints are configured per connection with `baseUrl` in YAML; there are no base-URL environment variables. A connection whose credential variable is unset is *unavailable*: routing that uses it as a primary fails immediately with an error telling you which variable to set; fallback routes that use it are skipped.

## Inspecting the effective configuration

```bash
relay config show                        # print the fully resolved configuration
relay config path                        # list the layers that applied and their file paths
relay config get providers               # the effective connection table
relay config get providers.gateway       # one connection (including its effective adapter)
relay config get providers.gateway.baseUrl   # one connection field
relay config get routing                 # the purpose → role table
relay config get models.review           # one role definition from the effective catalog
```

`show` prints the merged result of defaults plus both file layers (CLI flags are per-invocation and are not included); the implicit `fake` connection is rendered explicitly, with its adapter.
