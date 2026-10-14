# Named model roles and purpose-aware routing implementation plan

> **Historical (2026-10-14):** role-name routing shipped in the TypeScript
> implementation and is retained in Rust (`crates/relay-config`, documented
> in `docs/configuration.md`). The TypeScript application itself was removed
> from the repository. Retained as design history; not current
> instructions.

## Objective

Introduce a stable, user-defined model-role layer between Relay's workflow purposes and provider-specific model identifiers. Users should be able to define roles such as `reasoning`, `coding`, `review`, and `fast`, map workflow modes and utility purposes to those roles, change a role's underlying provider/model in one place, and see the selected role in routing diagnostics.

Preserve Relay's current deterministic workflow, provider-neutral core, fallback behavior, CLI overrides, event replay, existing configuration files, and fake-provider test paths. This work changes how a route is described and resolved; it must not let a model choose its own role or alter transition, permission, autonomy, artifact, or delivery policy.

## Repository instructions and current-state grounding

Implementation must follow the repository-root `CLAUDE.md`, especially:

- use TypeScript, Node.js 22+, ESM, strict compiler settings, and npm;
- keep provider SDK types at adapter boundaries;
- keep routing and workflow changes deterministic and replayable;
- coordinate changes to shared configuration and event contracts;
- validate configuration and model-produced structured data with Zod;
- preserve user files and dirty worktrees, and do not commit or push;
- run `npm run typecheck`, `npm test`, and `npm run build` before reporting completion.

The current codebase has these relevant behaviors:

- `src/config/schema.ts` defines inline routes containing `provider`, `model`, optional `fallbacks`, and optional `limits`. Routing keys are currently restricted to the seven workflow modes.
- `src/config/defaults.ts` repeats provider/model pairs across modes.
- `src/config/load.ts` merges user and project layers and applies `--provider` by rewriting every inline route.
- `src/runtime/model-router.ts` routes by workflow mode. Calls without a usable mode, including intent classification, fall back to the first configured route.
- `src/runtime/controller.ts` uses purposes such as `intent` and `mode:<mode>`, applies invocation-level provider/model overrides, and appends provider/model routing events.
- `src/core/events.ts` and `src/runtime/controller-views.ts` persist and display provider/model/purpose, but not the semantic role.
- `src/cli/parse-arguments.ts`, `src/cli/commands/run.ts`, and `src/cli/context.ts` expose `--provider` and `--model`, but no role override.
- `docs/configuration.md` and `README.md` document direct mode-to-provider/model routing and the current mixed-provider defaults.

## Proposed configuration and routing contract

Add a top-level `models` catalog whose keys are user-defined role names. Keep provider connection settings under `providers`; provider names remain infrastructure identifiers rather than role names.

The target default shape should be equivalent to:

```yaml
models:
  reasoning:
    provider: anthropic
    model: claude-sonnet-5-5
  architect:
    provider: anthropic
    model: claude-opus-5-5
  coding:
    provider: anthropic
    model: claude-sonnet-5-5
  review:
    provider: openai
    model: gpt-5.2
    fallbacks: [reasoning]
  fast:
    provider: anthropic
    model: claude-haiku-4-5

routing:
  intent: fast
  discovery: reasoning
  specification: reasoning
  technical-design: architect
  planning: reasoning
  implementation: coding
  verification: review
  delivery: fast
```

Use role names that describe execution responsibility, not claims about a vendor or an inherently vague quality such as "creative." Role names are configuration keys, not a hard-coded enum: users may add names such as `product`, `ux-writing`, or `security-review`.

The detailed contract is:

1. A model-role entry owns `provider`, `model`, optional per-route `limits`, and optional role-name `fallbacks`.
2. A routing entry may be a role-name string. `intent` becomes an explicit supported routing purpose in addition to the seven required workflow modes.
3. Legacy inline routing objects remain accepted so existing user/project configuration continues to work unchanged. Internally, normalize both syntaxes into one resolved representation before execution.
4. The seven workflow-mode routes remain required. `intent` is optional in a legacy layer/effective legacy configuration; when absent, retain the historical non-mode fallback behavior. New built-in defaults must define `intent`, so ordinary effective configuration is explicit.
5. Model catalogs merge by role name across defaults, user config, and project config. Later layers replace one complete role definition, while unrelated roles remain available. Routing continues to merge by routing key.
6. Validate the effective configuration after layers are merged: every referenced role and fallback role must exist; role/fallback graphs must not contain self-reference or cycles; the fully expanded chain must retain the existing maximum of one primary plus three fallbacks; all providers, limits, routing keys, and model strings retain strict validation.
7. Role fallbacks inherit the referenced role's provider, model, and limits. Expand deterministically in declared order, deduplicate roles/routes without reordering, and do not recursively include more than the validated maximum chain.
8. Routing remains application-controlled. Resolve an exact purpose route first (`intent`), otherwise the current workflow-mode route, and only then use the legacy first-configured fallback when no explicit non-mode purpose exists.
9. Add `--role <name>` as an invocation-wide semantic override. Validate it against the effective model catalog before creating project state. Apply override precedence as: resolved configured role, then `--role`, then the existing low-level `--provider`, then `--model`. Preserve the existing behavior of `RELAY_FAKE=1` and existing flags. Document that low-level provider/model overrides can intentionally produce combinations not present in the catalog.
10. Include the selected role in runtime route metadata and routing diagnostics. Add `role` as an optional field to existing `model.requested`, `model.responded`, and `model.failed` payload schemas so old event logs remain valid and replayable. Legacy inline routes may report `role: null`/absent. Do not require a database migration solely for an optional event-payload field.

## Scope

### In scope

- Named, user-defined model roles in YAML configuration.
- Purpose-aware routing for `intent` plus existing mode routing.
- Role-based fallbacks and per-role limits.
- Backward-compatible parsing and normalization of existing inline routes.
- Layered model-catalog merge and effective-configuration validation.
- A `--role` run override with documented precedence alongside `--provider` and `--model`.
- Role identity in model execution events, `relay why`, JSON output, and TUI-derived views.
- Updated defaults, configuration docs, README examples, and focused unit/integration/replay tests.

### Out of scope

- Dynamic model selection, benchmarking, automatic cost/latency optimization, or model self-routing.
- Hard-coded capability scoring or a fixed enum of allowed role names.
- New provider adapters or changes to OpenAI/Anthropic SDK integration.
- Changes to workflow modes, prompts, artifact schemas, transition policy, permissions, autonomy, or delivery authorization.
- Persisting credentials or placing API-key values in configuration/events.
- Database schema changes unless implementation discovers a demonstrated replay requirement that cannot be met by optional event fields.

## Affected files and ownership boundaries

The implementation team should confirm exact touch points before editing, but the expected surface is:

| Area | Expected files | Responsibility |
| --- | --- | --- |
| Configuration contracts | `src/config/schema.ts`, `src/config/defaults.ts`, `src/config/load.ts` | Raw/normalized types, role catalog, legacy compatibility, layered merge, effective validation, defaults, config overrides |
| Routing runtime | `src/runtime/model-router.ts`, `src/runtime/agent-loop.ts`, `src/runtime/controller.ts` | Purpose/mode lookup, role/fallback expansion, role metadata propagation, override precedence |
| CLI composition | `src/cli/parse-arguments.ts`, `src/cli/commands/run.ts`, `src/cli/context.ts`, `src/cli/main.ts` if required | `--role` parsing, validation, error messages, context/controller wiring |
| Durable diagnostics | `src/core/events.ts`, `src/runtime/controller-views.ts`, `src/cli/output.ts`, relevant TUI/view-model files if their types require it | Optional replay-safe role field and human/JSON routing output |
| Unit tests | `test/unit/runtime/model-router.test.ts`, `test/unit/cli/parse-arguments.test.ts`, configuration/load tests (add a focused file if none exists), `test/unit/core/contracts.test.ts`, `test/unit/core/reducer.test.ts`, relevant TUI tests | Contract, precedence, compatibility, fallback graph, display, and replay coverage |
| Integration tests | `test/integration/cli.test.ts`, `test/integration/workflow.test.ts`, `test/integration/replay.test.ts`, fixtures/helpers that construct `RelayConfig` | End-to-end role selection, legacy config, fake-provider workflow, event replay |
| Documentation | `docs/configuration.md`, `docs/architecture.md`, `README.md` | Conceptual separation, schema, migration examples, override precedence, diagnostics |

Treat `src/config/schema.ts`, `src/core/events.ts`, and shared test fixtures/config builders as coordinated shared files. Assign a single owner to each or agree on contract changes before parallel edits. Avoid drive-by formatting or unrelated cleanup.

## Phased implementation

### Phase 1: Lock contracts and compatibility cases

1. Capture current behavior with focused tests before changing implementation:
   - the seven legacy inline routes parse and execute;
   - intent without an explicit route resolves using the current fallback behavior;
   - inline fallbacks preserve order and provider filtering;
   - `--provider`, `--model`, and `RELAY_FAKE=1` retain their current effects;
   - old model events replay and render without a role field.
2. Define separate raw configuration types (role reference or legacy inline route) and normalized runtime route types. Do not spread a string/object union throughout the runtime.
3. Decide one canonical role-name validation rule and test it. Prefer readable YAML keys with a bounded length; reject empty/reserved/pathological names with actionable paths in error messages, without hard-coding the default role set.
4. Keep configuration schema version compatibility. Do not bump `version` merely to add optional/backward-compatible fields; if a bump proves necessary, document and test a migration path rather than rejecting version-1 files.

### Phase 2: Add model roles and effective configuration validation

1. Extend `src/config/schema.ts` with:
   - a model-role schema containing provider, model, limits, and role-name fallbacks;
   - routing values that accept either a role reference or the existing inline route object;
   - `intent` as an allowed optional routing key;
   - types that distinguish parsed configuration from the normalized runtime contract.
2. Update `src/config/defaults.ts` to define the default roles and map all workflow modes plus `intent` to roles while preserving today's effective providers/models: Anthropic Sonnet for discovery/specification/planning/implementation, Anthropic Opus for technical design, OpenAI for verification, and Anthropic Haiku for delivery. The new `intent` default should use the lightweight `fast` role.
3. Update `src/config/load.ts` to merge role catalogs by key and routing by key, then run cross-reference validation on the effective configuration. Keep diagnostics source-aware where practical.
4. Implement normalization in the configuration/routing boundary:
   - resolve a role reference to a concrete provider/model/limits plus its semantic role name;
   - preserve legacy inline route behavior with no fabricated role name;
   - expand role fallbacks deterministically;
   - detect unknown roles, unknown fallback roles, self-reference, cycles, excessive expanded chains, and invalid routing keys before any model call.
5. Ensure `relay config show` prints a coherent effective role catalog and routing map, and `relay config get models.<role>` / `routing.intent` works through the existing dotted lookup.

### Phase 3: Make runtime routing role- and purpose-aware

1. Extend `ResolvedRoute` in `src/runtime/model-router.ts` with optional/nullable `role` metadata.
2. Change lookup order to prefer an explicit purpose entry such as `intent`, then a configured workflow mode, then the legacy first-configured route only for backward compatibility. A missing workflow-mode route must still fail rather than silently use another mode once effective validation has run.
3. Return a primary route plus expanded, registered fallbacks with limits preserved for every role-based entry. Continue filtering unavailable fallback providers while producing an actionable error when the primary provider is unavailable.
4. Propagate role metadata through `src/runtime/agent-loop.ts` events and `src/runtime/controller.ts` without introducing provider-specific types into the controller.
5. Preserve budget, retry, cancellation, tool permission, structured-output, and failure-resume semantics exactly.

### Phase 4: Add the CLI role override

1. Add `--role <name>` to parsing, usage text, run command options, and CLI context wiring.
2. Validate the role only after effective configuration is loaded so user-defined names are supported. On failure, return a usage error that names the unknown role and lists available roles without creating or mutating Relay project state.
3. Apply invocation overrides in the documented order: configured route, `--role`, `--provider`, `--model`. Ensure the chosen role's fallback chain is used unless a low-level override's existing semantics intentionally narrow or rewrite it.
4. Preserve `RELAY_FAKE=1` and fake-provider demos. Update fake test configuration/helpers to use either normalized roles or legacy routes deliberately, so both paths remain covered.
5. Add CLI tests for spaced and `--role=value` forms, unknown roles, override precedence, help output, and combined role/provider/model behavior.

### Phase 5: Persist and display role-aware diagnostics

1. Add an optional `role` field to the three model event payload schemas in `src/core/events.ts`. Keep old payloads valid and do not alter reducer state semantics or model-call counts.
2. Populate the field from the selected `ResolvedRoute` for request, response, and failure events. Ensure retry/fallback events identify the role actually attempted.
3. Extend the routing rows produced by `src/runtime/controller-views.ts` and rendered by CLI/TUI output to show role when available while retaining provider, model, purpose, timestamp, and success/failure information.
4. Match failed attempts to requests using role as an additional discriminator when present, while continuing to match legacy events without it.
5. Add replay tests containing pre-change events with no role and new events with roles. Verify JSON output remains additive and human output remains concise.

### Phase 6: Documentation and final integration

1. Rewrite the annotated configuration example in `docs/configuration.md` around `providers`, `models`, and `routing`, including:
   - provider-versus-role-versus-purpose terminology;
   - default role mapping;
   - user-defined role examples;
   - role fallbacks and limits;
   - the supported legacy inline form and migration example;
   - layer merge behavior and validation errors;
   - `--role`, `--provider`, and `--model` precedence.
2. Update `docs/architecture.md` to describe deterministic purpose-to-role-to-provider resolution and clarify that models never select their own roles.
3. Update `README.md` quick-start/configuration and CLI option tables. Keep the local-first, model-agnostic positioning and offline fake-provider instructions accurate.
4. Run focused tests first, then all required repository checks. Review the final diff for accidental behavior changes, stale examples/model names, secrets, unrelated formatting, and user-owned changes.

## Engineering constraints

- Preserve all existing user changes and work safely in a dirty worktree. Do not overwrite unrelated files, reset changes, commit, or push.
- Keep the workflow core independent of OpenAI/Anthropic SDK types. Roles resolve to the existing Relay-owned provider abstraction.
- Routing must be deterministic configuration, never model output or a probabilistic model choice.
- Maintain backward compatibility for existing version-1 YAML using inline mode routes and existing event logs lacking role metadata.
- Keep provider credentials in environment variables. Role configuration contains only provider names, model identifiers, limits, and role references.
- Use strict Zod validation with actionable dotted paths. Reject unknown fields, routing keys, references, cycles, and invalid limits before execution.
- Normalize configuration once at a clear boundary. Avoid repeated ad hoc checks and avoid leaking raw route unions into controllers/providers.
- Preserve current layer precedence: built-in defaults < user config < project config < invocation overrides.
- Preserve existing fallback ordering, maximum fallback count, provider registration checks, retries, budgets, and resumable failures.
- Do not silently route a missing workflow mode through another mode. Legacy fallback is only for non-mode purposes when no explicit purpose route exists.
- Adding the optional role field must not make old event records unreadable or require destructive state migration.
- Tests must remain deterministic and offline; use the fake provider and temporary workspaces.
- Keep modules small and assign single ownership for shared contracts, as required by `CLAUDE.md`.

## Acceptance criteria

1. Effective configuration supports a user-defined `models` role catalog and role-name routing for all seven workflow modes.
2. Built-in defaults preserve the current provider/model choice for every workflow mode and explicitly route `intent` to the lightweight `fast` role.
3. An existing complete version-1 configuration containing only inline routes loads and behaves as before without edits.
4. User and project layers can add or replace individual roles without erasing unrelated roles from lower-precedence layers.
5. Unknown role references, unknown fallback roles, cycles, self-reference, excessive chains, unknown routing keys, and invalid fields fail before model execution with actionable messages.
6. `ModelRouter` deterministically resolves purpose, role, provider, model, limits, and ordered fallbacks. Explicit `intent` routing no longer depends on object insertion order.
7. `relay run --role <name>` selects a configured role for the invocation; unknown roles fail cleanly before project-state mutation.
8. Existing `--provider`, `--model`, and `RELAY_FAKE=1` behavior remains available, and combined override precedence is documented and tested.
9. Model request/response/failure events record the attempted role when available, while old role-less events still validate, replay, and render.
10. `relay why` and its JSON/TUI projections show role, provider, model, and purpose for new routing events without regressing legacy output.
11. Provider adapters, workflow transitions, policy gates, artifact generation, approvals, cancellation, retries, budgets, and resumability remain behaviorally unchanged.
12. Documentation accurately explains providers, roles, purposes, defaults, fallbacks, layering, compatibility, and CLI overrides.
13. All focused tests and the full required check suite pass without network access.
14. No implementation commit or push is made.

## Verification commands

Run focused checks during implementation:

```bash
npm test -- --run test/unit/runtime/model-router.test.ts
npm test -- --run test/unit/cli/parse-arguments.test.ts
npm test -- --run test/unit/core/contracts.test.ts test/unit/core/reducer.test.ts
npm test -- --run test/integration/cli.test.ts
npm test -- --run test/integration/replay.test.ts
npm test -- --run test/integration/workflow.test.ts
```

If a dedicated configuration test file is added, run it explicitly as well, for example:

```bash
npm test -- --run test/unit/config/load.test.ts
```

Run all repository-required checks before completion:

```bash
npm run typecheck
npm test
npm run build
```

Finally inspect the working tree without modifying it:

```bash
git status --short
git diff --check
git diff -- docs/configuration.md docs/architecture.md README.md src test
```

Report the implemented behavior, compatibility decisions, tests/checks run with results, and any unresolved issues. Do not commit or push.
