# Configurable provider connections and role-based runtime routing

## Status

Implemented 2026-10-15 by the four-role team this document specifies
(configuration owner, provider/runtime-security owner, interface owner, lead
integrator). All repository verification gates pass; behavior notes and the
final shapes are documented in `docs/configuration.md` and
`docs/architecture.md`. The decisions below are the implementation record.

## Objective

Make Relay's existing YAML model roles and routing effective in the live Rust
runtime, and replace the hard-coded provider-name switch with named provider
connections backed by reusable protocol adapters.

The first extensibility target is GLM through its OpenAI-compatible Chat
Completions endpoint. The design must also allow multiple connections using the
same adapter (for example `openai`, `glm`, `groq`, `openrouter`, and `local`)
without calling those services OpenAI internally or adding one enum variant per
vendor.

The runtime resolution path should be:

```text
work/conversation purpose -> configured role -> provider connection -> adapter
```

Relay-owned request, event, grant, receipt, and fact types remain
provider-neutral. Provider wire types remain private to `relay-providers`.

## Current state and motivating gaps

- `relay-config` already loads layered YAML, defines the five built-in roles
  (`reasoning`, `architect`, `coding`, `review`, and `fast`), normalizes role
  fallback chains, and implements invocation override precedence.
- `ProviderName` in `relay-config/src/schema.rs` is a closed
  `openai | anthropic | fake` enum. `Providers` contains exactly one OpenAI and
  one Anthropic connection, so users cannot name two OpenAI-compatible
  endpoints.
- `relay-app/src/providers.rs` independently hard-codes the same three provider
  names and default models. It constructs `OpenAIProviderOptions` and
  `AnthropicProviderOptions` with `base_url: None`, so configured base URLs and
  configured credential variable names do not reach live adapters.
- `relay-cli/src/dispatch.rs` loads configuration only for `relay config`; run,
  print, and interactive paths use `RuntimeSelection::Production { provider }`
  and the provider-wide factory.
- `RuntimeWiring` contains one `Arc<dyn ModelProvider>` and one `ModelRoute`.
  Every attempt therefore receives the same route, even though
  `ModelClient::complete` already accepts the route stored in its immutable
  capability grant.
- `relay-providers::ProviderName` is also a closed enum. It describes adapter
  identity, but currently leaks vendor-shaped naming into places that need a
  configurable connection identity.
- The work-graph kernel has no global workflow mode, by design. The built-in
  workflow catalog retains `Phase` only as grouped-view metadata. Runtime
  routing must use catalog/work metadata without making phase into mutable
  kernel state or ordering policy.
- The current CLI behavior is intentionally offline by default: bare
  `relay run` uses `fake`. This must remain true after configuration is wired
  into execution.

## Product and configuration decisions

### 1. Separate adapters from provider connections

Use a small closed adapter kind for code that Relay ships, and an open set of
user-defined connection names:

```yaml
providers:
  glm:
    adapter: openai-chat
    baseUrl: https://api.z.ai/api/paas/v4
    apiKeyEnv: GLM_API_KEY

  openai:
    adapter: openai-chat
    baseUrl: https://api.openai.com/v1
    apiKeyEnv: OPENAI_API_KEY

  anthropic:
    adapter: anthropic
    apiKeyEnv: ANTHROPIC_API_KEY
```

Initial adapter kinds:

- `openai-chat`: OpenAI-compatible `/chat/completions`, including GLM;
- `anthropic`: native Anthropic Messages API;
- `fake`: deterministic in-process provider for tests and offline use.

Connection keys use the same bounded lowercase kebab-case rules as role names.
Model definitions refer to connection keys as strings, not adapter enums.

### 2. Preserve version-1 configuration compatibility

Do not bump the schema version for this additive change. Continue accepting
the current provider entries:

```yaml
providers:
  openai: { apiKeyEnv: OPENAI_API_KEY }
  anthropic: { apiKeyEnv: ANTHROPIC_API_KEY }
```

When `adapter` is omitted on the legacy keys, infer `openai-chat` for
`openai` and `anthropic` for `anthropic`. Existing inline routes and model-role
definitions using `provider: openai | anthropic | fake` must normalize to the
same connection names and retain their existing model, limit, fallback, and
event behavior. A `fake` connection may be implicit for compatibility and
offline defaults, but configuration inspection should render its effective
form consistently.

Unknown connection references must fail during effective-configuration
validation with a dotted path and available connection names. Unknown adapter
kinds, duplicate YAML keys, malformed connection names, invalid URLs, literal
secrets in `apiKeyEnv`, fallback cycles, and overlong fallback chains must fail
before project state is created or changed.

### 3. Keep secrets out of configuration and state

`apiKeyEnv` continues to contain only an environment-variable name. Resolve
the value at adapter construction/call time. Never serialize secret values into
facts, SQLite, Markdown artifacts, diagnostics, fixtures, or errors.

The runtime may report the configured connection name, adapter kind, endpoint
host, and missing environment-variable name. It must not report credential
values or endpoint user information.

### 4. Treat OpenAI compatibility as a transport, not the domain contract

Do not reuse OpenAI request/response structs outside the `openai` adapter.
Retain the Relay-owned provider contract and translate at the adapter boundary.
Keep the native Anthropic adapter.

The first implementation should support the subset Relay currently sends:
streaming text, normalized usage, local function tools, cancellation, token
limits, temperature, and structured JSON output. Add an explicit capability
model so compatibility gaps fail early rather than halfway through an attempt.

Initial capability fields should cover at least:

- developer/system instruction representation;
- tool calls and streamed tool arguments;
- parallel tool calls, when Relay begins requesting them;
- structured-output mode (`json-schema`, `json-object`, or unsupported);
- streamed usage availability;
- output-token parameter convention;
- image/file input support, recorded as unsupported until Relay requests it.

Provide conservative adapter defaults and optional presets for known services.
The first preset is `glm`; explicit YAML capability overrides win over a
preset. Do not silently discard a requested Relay feature. Either translate it
using a documented safe fallback (for example developer to system instruction)
or return an actionable unsupported-capability error before issuing the HTTP
request.

### 5. Make roles and routing effective without reintroducing modes

For built-in work items, derive a routing purpose from the work definition in
`WorkflowCatalog`:

| Catalog phase | Configuration routing key |
| --- | --- |
| Discovery | `discovery` |
| Specification | `specification` |
| Design | `technical-design` |
| Planning | `planning` |
| Implementation | `implementation` |
| Verification | `verification` |
| Delivery | `delivery` |

This mapping is composition metadata used while constructing an attempt grant;
it is not stored as global state, does not impose phase ordering, and does not
change engine scheduling. For custom work kinds that are absent from the
built-in catalog, fail with an actionable missing-route diagnostic unless the
work declaration or a later explicit extension supplies a routing purpose. Do
not silently route unknown work through the first configured phase.

Conversational intent/proposal turns use the explicit `intent` route. Preserve
the legacy first-route fallback only where the existing normalized routing API
already documents it for a non-mode purpose; do not extend that fallback to
work-graph execution.

Resolve the complete route chain before an attempt starts. Record the selected
connection, model, and role in the immutable `ModelRoute` grant. Fallback
attempts must be deterministic and bounded: try the primary and then normalized
fallback routes in order only for failures classified as retryable/provider
availability failures. Do not fall back on cancellation, invalid model output,
policy rejection, budget exhaustion, or unsupported required capabilities.
Every attempted route must consume the existing attempt/run budgets and be
visible in bounded, redacted diagnostics.

### 6. Preserve the offline default and CLI compatibility

- Bare `relay run`, interactive chat, and `--print` must remain deterministic
  and offline in a fresh checkout with no user/project provider configuration
  or live-provider override.
- Preserve `--provider fake|openai|anthropic` as compatibility spellings.
  Interpret the value as a connection name in the effective configuration;
  permit any configured connection name in the new implementation.
- Preserve the current precedence concept: configured route, then optional
  role override, then provider-connection override, then model override.
- Do not create project state when configuration, credentials, route
  resolution, or required capabilities are invalid.
- Decide the offline-default mechanism explicitly in code and tests. Prefer an
  effective built-in fake routing profile over hidden branching in the CLI.
  Retain the existing five role names and routing-purpose mapping so user and
  project layers can replace individual role definitions predictably.

## Scope

### In scope

- Named provider connections in layered YAML.
- `openai-chat`, native `anthropic`, and `fake` adapter selection.
- Configured base URL and credential-variable wiring.
- GLM preset/capability profile and offline conformance fixtures.
- Runtime resolution of roles, purposes, provider connections, models,
  limits, and fallback chains.
- Run, `--print`, and interactive-chat integration.
- Backward-compatible CLI provider override behavior.
- Actionable validation and redacted errors.
- Configuration, adapter, runtime, CLI, replay, and security tests.
- Documentation and examples.

### Out of scope

- OpenAI Responses API support; `openai-chat` targets Chat Completions.
- A native GLM adapter or GLM SDK dependency.
- Automatic network probing or capability discovery during normal startup.
- Live-provider tests in default CI.
- Dynamic plugin loading, user-supplied executable adapters, or provider SDK
  types outside `relay-providers`.
- Pricing tables or provider-specific cost accounting.
- Changes to work-graph scheduling, policy, phase ordering, artifact authority,
  persistence formats, or delivery authorization.
- Changes to the frozen `relay-core` and `relay-runtime` crates.
- Committing or pushing changes.

## Affected files and ownership

Exact test-file additions may vary, but keep ownership boundaries explicit.

### Runtime/security owner

- `crates/relay-config/src/schema.rs`
  - replace the fixed `Providers` shape with an ordered map of named
    connections;
  - introduce adapter kinds, capability/preset configuration, and provider
    connection-name validation;
  - change role and inline-route provider references from the vendor enum to a
    validated connection identifier/string.
- `crates/relay-config/src/defaults.rs`
  - define the explicit fake offline default while retaining role names and
    routing keys.
- `crates/relay-config/src/load.rs`
  - merge provider connections by name rather than replacing the whole fixed
    section;
  - validate effective provider references and capability/preset combinations;
  - preserve source attribution for provider diagnostics.
- `crates/relay-config/src/normalized.rs`
  - carry connection names and effective route limits/capability requirements
    through normalized chains.
- `crates/relay-config/src/invocation.rs`
  - make provider overrides connection-name based and keep precedence/fallback
    semantics deterministic.
- `crates/relay-config/src/inspect.rs`, `crates/relay-config/src/lib.rs`
  - expose the new effective shape and public types.
- `crates/relay-config/tests/{schema_roles,layered_load,normalize_routing,legacy_compat,validation_sweeps,golden_corpus}.rs`
  - update focused tests and add fixtures for named connections, legacy
    inference, merge behavior, validation, inspection, and overrides.
- `crates/relay-providers/src/contract.rs`
  - distinguish adapter identity/capabilities from configured connection
    identity without changing Relay-owned request/event semantics.
- `crates/relay-providers/src/openai.rs`
  - accept the effective endpoint, credential value, capability profile, and
    safe compatibility translations;
  - retain private wire types and bounded/redacted errors.
- `crates/relay-providers/src/anthropic.rs`,
  `crates/relay-providers/src/fake.rs`, `crates/relay-providers/src/lib.rs`
  - expose consistent adapter metadata/capabilities and constructors.
- `crates/relay-providers/tests/openai_conformance.rs`
  - extend loopback-only fixtures for GLM-shaped streaming, tools, usage,
    structured output, errors, and capability fallbacks; do not call GLM live.
- New focused modules such as `crates/relay-app/src/provider_registry.rs` or
  `crates/relay-executor/src/routed_model.rs`
  - build named connections once;
  - resolve the provider from each granted `ModelRoute`;
  - apply bounded fallback policy without holding storage locks.
- `crates/relay-executor/src/model.rs`
  - replace the single-provider `ProviderModelClient` assumption with a
    connection registry/router while preserving the `ModelClient` port.
- Focused executor tests for connection lookup, mismatch rejection,
  cancellation, retry classification, fallback order, and budget accounting.

### Interface owner

- `crates/relay-app/src/providers.rs`
  - retire the hard-coded factory in favor of configuration-driven registry
    construction; keep only compatibility helpers if useful.
- `crates/relay-app/src/runner.rs`
  - make grant construction resolve a purpose-specific normalized route chain
    from each work item's catalog definition;
  - retain immutable grants and existing test-injected wiring.
- `crates/relay-app/src/conversation.rs`
  - use the `intent` route for conversational turns and preserve durable turn
    facts and proposal validation.
- `crates/relay-cli/src/args.rs`, `crates/relay-cli/src/dispatch.rs`,
  `crates/relay-cli/src/tui.rs`
  - load and validate effective configuration for run/print/chat before state
    mutation;
  - accept configured connection names for `--provider`;
  - preserve injected fake wiring used by tests.
- `crates/relay-cli/tests/cli_work_graph.rs` and relevant TUI/app tests
  - cover offline default, configuration failures before mutation, named
    connections, overrides, routing by work phase, and human/JSON diagnostics.
- `crates/relay-tests/tests/{capability_security,scheduling_concurrency,fact_replay}.rs`
  - add cross-crate coverage only where the behavior crosses those contracts.

### Lead integrator

- `README.md`, `.env.example`, `docs/configuration.md`, and
  `docs/architecture.md`
  - document adapter versus connection versus role versus purpose;
  - include GLM and multiple-connection examples;
  - accurately document the offline default and CLI override semantics;
  - remove statements that imply the config is live before it is wired.
- `tests/fixtures/**` and `docs/event-format.md` only if an additive fixture or
  event-contract change is genuinely required. Prefer retaining the existing
  `ModelRoute`/attempt fact shape so no event-format change is needed.
- `CLAUDE.md` does not require an update: it already mandates provider-neutral
  boundaries, secret handling, deterministic policy, offline tests, and shared
  ownership. Update it only if implementation establishes a genuinely new
  repository-wide invariant, not merely to describe this feature.

Do not modify `crates/relay-core` or `crates/relay-runtime`.

## Phased implementation

### Phase 0: Lock behavior and migration fixtures

1. Add characterization tests for current no-config behavior: bare run/print
   use fake, require no credential, and do not access the network.
2. Capture representative version-1 YAML cases:
   - omitted `providers`;
   - legacy `openai` and `anthropic` settings;
   - role references and legacy inline routes;
   - provider/model/role invocation overrides;
   - configured base URLs and non-default credential variable names.
3. Specify which failures occur before `ProjectSession::open_or_create` and
   assert that no `.relay-graph` state is created on those failures.
4. Keep historical fixtures immutable unless the existing compatibility suite
   requires additive copies. Do not regenerate retired TypeScript fixtures.

### Phase 1: Generalize the configuration contract

1. Introduce `AdapterKind`, `ProviderConnection`, capability settings, preset
   names, and a validated connection-name type or boundary validator.
2. Parse `providers` as an order-preserving record. Infer adapters for the
   legacy `openai` and `anthropic` keys when omitted.
3. Change model roles, inline routes, normalized routes, and provider overrides
   to reference connection names.
4. Merge provider connections by name across defaults, user configuration, and
   project configuration. A later connection definition replaces the complete
   connection unless individual-field merging is explicitly documented and
   tested; prefer complete replacement to match role semantics.
5. Cross-validate all role/route provider references after merging and report
   the originating layer.
6. Render an unambiguous effective configuration through `relay config show`
   and dotted lookups.
7. Preserve schema version 1 and all legacy inline routing behavior.

Exit criterion: all `relay-config` tests pass, including round-trip inspection,
legacy syntax, named connections, multiple `openai-chat` connections, invalid
references, and layer precedence.

### Phase 2: Build adapter registry and capability enforcement

1. Replace provider-name construction with a registry keyed by configured
   connection name. Construct adapters from effective configuration without
   logging secrets.
2. Pass configured `baseUrl` and the value from `apiKeyEnv` to adapter options.
   Validate the variable name in config; resolve the variable value only at the
   provider boundary.
3. Keep adapter identity (`openai-chat`, `anthropic`, `fake`) separate from the
   connection name recorded in routes.
4. Implement effective capability profiles: adapter defaults, then optional
   preset, then explicit override. Add the GLM preset.
5. Add preflight validation for Relay features a request requires. Implement
   only documented safe translations; otherwise fail explicitly.
6. Expand offline OpenAI conformance tests with GLM-shaped sanitized fixtures.
   Include chunked SSE/tool arguments, missing usage, error normalization,
   cancellation, structured-output variants, and base URL joining.

Exit criterion: two different named `openai-chat` connections can coexist and
the correct endpoint/key/model are selected in deterministic loopback tests;
unsupported capabilities fail before an outbound request.

### Phase 3: Route model calls through the connection registry

1. Refactor the executor-side model client so `complete(route, ...)` looks up
   `route.provider` in an immutable connection registry rather than ignoring it
   and always calling one wrapped provider.
2. Reject missing connections and adapter/route mismatches with bounded,
   actionable errors.
3. Define retry/fallback classification using normalized `ProviderFailure`
   kinds. Preserve cancellation and budget semantics.
4. Represent the route chain at the application/runtime seam without changing
   persisted kernel authority unnecessarily. The selected concrete route
   remains the `ModelRoute` in each attempt's grant; route changes between
   fallback attempts must remain explicit in diagnostics and deterministic.
5. Ensure no network call or fallback loop holds the project writer lock.
6. Retain deterministic injected `ModelClient`/fake paths for unit and
   scheduling tests.

Exit criterion: executor tests prove per-route provider selection, ordered and
bounded fallback, no fallback on cancellation/policy/invalid output, and
correct token/attempt budget accounting.

### Phase 4: Connect work purposes, roles, and routes

1. Add an application-layer function that maps a built-in work kind through
   `WorkflowCatalog` to the configuration routing key in the table above.
2. During grant construction, resolve the purpose's normalized chain and apply
   invocation overrides. Put the selected role/connection/model and effective
   budget limits in the immutable grant.
3. Keep phase lookup out of `relay-domain` and `relay-engine`. It must not
   affect readiness, ordering, acceptance, or replay.
4. Produce an actionable failure for an executable custom work kind with no
   routing-purpose mapping.
5. Route conversation turns through `intent`. If conversation proposal
   generation and intent classification later need distinct purposes, add them
   explicitly rather than relying on catalog order.
6. Verify role fallbacks skip unavailable optional connections but an
   unavailable primary fails before state mutation, consistent with documented
   behavior. Reconcile this with runtime retry rules in one shared resolver so
   config and execution cannot disagree.

Exit criterion: integration tests observe different configured connection and
model routes in grants for discovery, design, implementation, verification,
delivery, and conversation work without any global mode state.

### Phase 5: Wire CLI and preserve compatibility

1. Load effective configuration once for run, print, and TUI startup, before
   creating state. Reuse the same loaded/normalized value for provider registry
   construction and route resolution.
2. Generalize `--provider` validation from the hard-coded array to effective
   connection names while keeping `fake`, `openai`, and `anthropic` working.
3. Preserve the offline fake behavior for a fresh checkout and explicitly test
   the selected default profile.
4. Ensure `relay config show|path|get` retains stable output where possible and
   clearly presents inferred adapter kinds in the new effective shape.
5. Preserve human and JSON exit-code/error contracts. Invalid config or
   unknown connection remains a usage/configuration error; provider transport
   failure remains a runtime failure.
6. Test user-layer/project-layer precedence and invocation override precedence
   end to end.

Exit criterion: CLI integration tests cover fake default, GLM-style configured
connection using a loopback server, custom connection overrides, missing keys,
invalid capabilities, and absence of state mutation on preflight failure.

### Phase 6: Documentation, review, and full verification

1. Update README, `.env.example`, configuration, and architecture documentation
   to match implemented behavior exactly.
2. Include copyable examples for GLM, a native Anthropic connection, multiple
   OpenAI-compatible connections, role fallbacks, and local/offline fake use.
3. Document that `openai-chat` is Chat Completions compatibility, not a promise
   of full OpenAI feature parity or Responses API support.
4. Review diffs for secret leakage, accidental kernel coupling, changes to
   authoritative fact formats, dirty-worktree damage, and mutations to frozen
   crates.
5. Run focused suites first, then all repository-required checks.

## Engineering constraints

- Follow `CLAUDE.md`; Rust is the sole implementation. Do not introduce Node,
  TypeScript, JavaScript tooling, or provider SDK dependencies.
- Keep `relay-domain`, `relay-engine`, and `relay-workflows` pure. Provider
  selection and catalog-to-purpose mapping belong at runtime/application
  composition boundaries.
- Do not mutate or redesign frozen `relay-core` or `relay-runtime`.
- Keep provider wire structs private to adapter modules. Public contracts use
  Relay-owned types only.
- Do not persist credentials. Environment-variable values must be redacted from
  all errors, facts, logs, fixtures, and snapshots.
- Use direct HTTP through the existing `reqwest` pattern. Do not add vendor SDK
  types or allow arbitrary executable adapters.
- Configuration and route resolution must be deterministic and validated before
  model calls. Models never select their own provider, role, or fallback.
- Facts and immutable grants remain authoritative for what an attempt may use.
  Do not let provider code mutate graph state.
- Do not hold a store writer lock across network calls, retries, streaming, or
  tools.
- Preserve cancellation responsiveness, bounded retries, budget accounting,
  output redaction, and actionable errors.
- Default tests are offline and deterministic. Any live GLM smoke test must be
  separately ignored/opt-in, require an environment key, redact output, and not
  be part of acceptance or CI.
- Preserve existing user changes and dirty worktrees. Do not overwrite,
  discard, commit, or push them.
- Shared contract files have one owner at a time. Coordinate schema/normalized
  route shapes before parallel work in adapters, executor routing, and CLI.

## Acceptance criteria

1. A fresh checkout with no keys or config still completes the supported
   default run/print paths using the deterministic fake provider without
   network access.
2. Existing version-1 YAML with `providers.openai`, `providers.anthropic`,
   current roles, legacy inline routes, limits, and fallbacks continues to load
   with equivalent semantics.
3. Users can define arbitrary validated provider connection names and multiple
   connections using the same `openai-chat` adapter.
4. A GLM connection can be expressed entirely in YAML using its base URL,
   credential environment-variable name, model name, and optional preset; no
   GLM-specific native adapter is required.
5. Configured `baseUrl` and `apiKeyEnv` are honored by live construction.
   Secrets never appear in serialized configuration, state, logs, fixtures, or
   errors.
6. Model calls resolve deterministically through purpose, role, ordered route
   chain, connection, adapter, and model. The model cannot choose or rewrite
   that routing.
7. Built-in work kinds route according to catalog phase metadata without
   introducing global mode state or changing scheduler behavior.
8. `intent`, discovery, design, implementation, verification, and delivery can
   be assigned to different named connections/models, and their immutable
   grants/diagnostics show the selected connection/model/role.
9. Provider fallbacks are ordered, deduplicated, bounded, capability-checked,
   budgeted, and attempted only for explicitly retryable availability/provider
   failures.
10. Unknown connections/adapters/presets, missing credentials, invalid URLs,
    unsupported required capabilities, and missing custom-work routing fail
    with actionable redacted messages before prohibited state mutation.
11. `--provider` accepts effective connection names while preserving the
    existing `fake`, `openai`, and `anthropic` spellings and exit behavior.
12. All default automated tests use fakes or loopback-only HTTP fixtures; no CI
    test contacts GLM, OpenAI, Anthropic, or another external service.
13. No provider SDK types cross adapter boundaries, no network logic enters the
    kernel crates, no authoritative event format changes unless separately
    justified, and frozen crates remain untouched.
14. Documentation accurately describes the implemented configuration,
    compatibility limitations, offline default, fallbacks, and override
    precedence.

## Verification commands

Run focused checks during development:

```bash
cargo test -p relay-config
cargo test -p relay-providers
cargo test -p relay-executor
cargo test -p relay-app
cargo test -p relay-cli --test cli_work_graph
cargo test -p relay-tests --test capability_security
cargo test -p relay-tests --test scheduling_concurrency
cargo test -p relay-tests --test fact_replay
cargo test -p relay-config --test legacy_compat
```

Run the repository's complete required checks before handoff:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo build --workspace --all-targets --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps --exclude relay-core --exclude relay-runtime
```

Also run the focused intent-first suites required by `CLAUDE.md`:

```bash
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

Because this changes legacy configuration compatibility, also run:

```bash
cargo test -p relay-tests --test migration
cargo test -p relay-tests --test semantic_migration
cargo test -p relay-tests --test compatibility
cargo test -p relay-config --test legacy_compat
```

Optional live validation is not an acceptance gate. If maintainers choose to
perform it, use a separately ignored test or manual command with `GLM_API_KEY`
set in the environment, never print the key or raw credential-bearing request,
and report it separately from deterministic verification.

## Team coordination guidance

Use non-overlapping ownership aligned with `CLAUDE.md`:

- a configuration owner for `relay-config` schema, migration, normalization,
  and its tests;
- a provider/runtime-security owner for adapter registry, capabilities,
  executor model routing, and conformance tests;
- an interface owner for `relay-app`, CLI/TUI integration, and interface tests;
- a lead integrator for shared contracts, documentation, cross-crate review,
  and final verification.

The configuration owner must land or communicate the provider-connection and
normalized-route contract before provider/runtime and interface owners modify
their consumers. Only the lead integrator should resolve overlapping edits in
shared exports, README/configuration docs, workspace manifests, or this plan.
No team member should edit frozen crates, commit, or push.
