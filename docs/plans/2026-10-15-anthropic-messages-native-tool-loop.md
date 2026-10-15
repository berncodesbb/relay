# Anthropic Messages native tool-loop implementation plan

Status: implemented 2026-10-15 (see the repository diff for that date; this
plan's phases 0-8 were executed by a six-owner team with the shared-contract
checkpoint sequenced first, and the `openai-chat` removal landed last)  
Date: 2026-10-15  
Depends on: `docs/plans/2026-10-13-intent-first-rust-work-graph.md`

## Objective

Make Anthropic Messages the strategic live model protocol for Relay's intent-first coding harness, and carry its native content-block and tool-use semantics through the executor instead of collapsing provider output to text and asking models to simulate tool calls inside JSON.

Relay must continue to own conversation history, tool authorization and execution, capability grants, budgets, cancellation, evidence, artifacts, and durable work-graph state. The provider remains a replaceable worker. Provider-side conversations, hosted tools, or agent loops must not become authoritative workflow state.

This change should establish:

- a Relay-owned, provider-neutral turn contract that can represent ordered text and tool-use blocks, explicit tool results, usage, completion, failures, and optional opaque provider continuation data;
- Anthropic Messages as the only live external protocol that receives new agent-loop capabilities;
- native tool definitions and `tool_use`/`tool_result` continuation in workspace and verifier sessions;
- strict preservation of Relay's existing capability and safety boundaries;
- complete removal of the `openai-chat` adapter and its configuration, presets, tests, examples, and documentation.

The deterministic in-process `fake` provider remains a required test/runtime facility and is not considered a second external wire protocol.

## Architectural decision

Relay is a coding harness, not a multi-provider SDK. Its core value is deterministic orchestration around replaceable model workers. Anthropic Messages fits that boundary because it expresses a client-owned loop:

```text
Relay context + tool schemas
        -> assistant text/tool_use blocks
        -> Relay validates grant and executes tools
        -> tool_result blocks
        -> next provider turn
```

The internal contract must remain Relay-owned rather than exposing Anthropic SDK or wire types. The contract may use a content-block structure because that is the behavior Relay needs, not because Anthropic owns the domain model.

OpenAI Responses is not added by this plan. Provider-hosted tools, stored conversations, background responses, remote MCP execution, and provider-managed multi-tool loops are also excluded: each would overlap Relay's authorization, evidence, or replay responsibilities.

`openai-chat` is removed in this change. Relay will ship exactly one live external model protocol: Anthropic Messages. Existing configuration that names `openai-chat` or relies on the inferred `providers.openai` connection cannot be translated safely because endpoint capabilities and model identifiers are not equivalent. Such configuration must fail early with a targeted migration diagnostic instructing the user to configure an Anthropic Messages-compatible connection and model. Recognition solely for that diagnostic is not protocol support and must not leave an OpenAI adapter/runtime variant behind.

## Applicable repository instructions and current state

Read `CLAUDE.md` completely before implementation. In particular:

- Rust is the sole implementation; do not introduce Node.js, TypeScript, JavaScript tooling, or provider SDK types.
- `crates/relay-domain`, `crates/relay-engine`, and `crates/relay-workflows` stay pure and provider-independent.
- Models and executors propose; deterministic policy validates and applies.
- Provider wire types stay private to adapter modules.
- Network-backed tests remain opt-in; default tests are offline and deterministic.
- Never hold a project writer lock across a provider call or tool execution.
- Model-produced tool arguments and final structured output require typed runtime validation with actionable failures.
- Preserve workspace containment, symlink checks, no-overwrite writes, atomic patches, bounded/redacted output, allowlisted child environments, read-only Git operations, delivery holds, and cancellation.
- User files and dirty worktrees are user-owned. Do not reset, overwrite, commit, or push them.
- `crates/relay-core` and `crates/relay-runtime` are frozen reference material and must not be modified.

At plan creation, `examples/` is untracked user work. Inventory it before implementation and do not overwrite, delete, stage, or reformat it. More generally, re-run `git status --short` before every phase that edits shared files.

The current implementation has two distinct seams:

1. `crates/relay-providers/src/contract.rs` already represents provider-neutral messages, tool specifications, tool calls, tool results, stream events, usage, structured output, cancellation, and normalized failure.
2. `crates/relay-executor/src/model.rs` narrows that contract to `ModelTurnRequest { system, prompt, ... }` and `ModelTurnResponse { text, usage, ... }`. `crates/relay-executor/src/planner.rs` then embeds tool descriptions in the prompt and parses a synthetic `{"tool_calls": [...]}` JSON object from assistant text.

The implementation must repair the second seam without letting provider types leak into the executor or domain.

## Scope

### In scope

- Refining the Relay-owned provider contract around ordered content blocks and native tool turns.
- Preserving a complete, replayable client-side transcript across tool turns.
- Passing grant-filtered `ToolSpec` values to the model request.
- Returning native tool calls through `ModelClient` without serializing them into assistant prose.
- Sending matching tool results, including explicit error results, on the following turn.
- Mapping Anthropic Messages requests, SSE events, stop reasons, usage, errors, text blocks, tool-use blocks, and tool-result blocks to/from Relay-owned types.
- Strict handling of multiple tool calls in arrival order; execution may remain sequential unless existing grant/scheduling policy explicitly permits otherwise.
- Retaining optional opaque thinking/redacted-thinking continuation blocks when a compatible endpoint requires round-tripping them, without interpreting or persisting hidden reasoning as domain truth.
- Capability profiles that distinguish required core behavior from optional thinking, caching, image/document input, structured output, and server-side tool behavior.
- Offline conformance, executor, security, routing, configuration, and scenario tests.
- Documentation and examples for direct Anthropic Messages-compatible connections.
- Removal of `openai-chat` implementation, capability presets, configuration defaults/inference, conformance tests, examples, and documentation.
- An actionable configuration error for obsolete `openai-chat` declarations and the removed implicit `providers.openai` connection.

### Out of scope

- Adding OpenAI Responses.
- Automatically rewriting an OpenAI connection into an Anthropic connection or guessing replacement endpoints/models.
- Provider-hosted web search, shell, code execution, computer use, file search, or remote MCP tools.
- Provider-side authoritative conversations or reliance on provider response/message IDs for replay.
- Exposing chain-of-thought or storing hidden reasoning in JSONL, SQLite, logs, artifacts, receipts, or UI output.
- Parallel tool execution without a separate scheduling and safety design.
- New delivery, Git-write, deployment, publishing, or destructive tool authority.
- Changes to the work-graph domain model, fact schema, migration fixtures, `relay-core`, or `relay-runtime`.
- Live-provider tests in the default suite.

## Affected files and ownership

Exact file additions may change after contract design, but ownership must be frozen before parallel work starts.

### Runtime/security contract owner

Owns shared contracts and lands them before dependent parallel work:

- `crates/relay-providers/src/contract.rs`
- `crates/relay-providers/src/collect.rs`
- `crates/relay-providers/src/lib.rs`
- `crates/relay-executor/src/model.rs`
- any new provider/executor contract test modules

This owner defines the Relay-owned block, transcript, result, usage, and event types. No other worker edits these files until the contract checkpoint is integrated.

### Anthropic adapter owner

- `crates/relay-providers/src/anthropic.rs`
- `crates/relay-providers/tests/anthropic_conformance.rs`
- sanitized offline SSE/request fixtures under `crates/relay-providers/tests/**`, if split into separate files

The adapter owner must not edit executor contracts independently. Required contract changes go through the runtime/security contract owner.

This owner also removes, after the shared-contract checkpoint:

- `crates/relay-providers/src/openai.rs`
- `crates/relay-providers/tests/openai_conformance.rs`
- OpenAI-only test fixtures/helpers that have no Anthropic or generic consumer

### Executor/tool-loop owner

- `crates/relay-executor/src/planner.rs`
- `crates/relay-executor/src/executors.rs`
- focused executor tests colocated with those modules
- `crates/relay-tools/src/registry.rs` only if a narrowly scoped read-only tool-spec enumeration method is required

The existing registry remains the single execution surface. Do not duplicate argument validation or grant checks in the provider layer.

### Routing/configuration owner

- `crates/relay-executor/src/routed_model.rs`
- `crates/relay-config/src/schema.rs`
- `crates/relay-config/src/defaults.rs`
- `crates/relay-config/src/inspect.rs`, `raw.rs`, or validation modules only as needed
- `crates/relay-config/tests/**`

Own the intentional configuration break. Preserve the existing `anthropic` spelling, remove the `openai-chat` adapter kind and implicit `openai` default, and add a targeted obsolete-protocol diagnostic. Decide whether this requires configuration schema version 2 after inventorying how the version is persisted and validated; do not claim schema-version-1 compatibility if the accepted adapter set and defaults have changed incompatibly.

### Integration/security test owner

- `crates/relay-tests/tests/capability_security.rs`
- `crates/relay-tests/tests/scheduling_concurrency.rs`
- `crates/relay-tests/tests/intent_first_scenarios.rs` or the actual current scenario file
- other cross-crate tests only where behavior crosses crate boundaries

This owner tests public behavior and safety properties, not private implementation structure.

### Lead integrator/documentation owner

- `CLAUDE.md`, only for the fundamental protocol-direction rule described below
- `README.md`
- `docs/configuration.md`
- `examples/README.md`
- `examples/relay.config.yaml`, only after reconciling the pre-existing untracked user work
- root manifests or CI only if strictly required
- this plan's status notes

The lead coordinates any shared-file edit, integrates worker branches/changes, reviews the complete diff, and runs final checks. No worker commits or pushes.

## Target contracts

The exact Rust spelling should be designed during Phase 1, but it must support these semantics.

### Conversation input

- A system/instruction value separate from user-authored content.
- An ordered transcript of user, assistant, and tool-result turns.
- User and assistant content represented as ordered blocks rather than a single string.
- Assistant tool-use blocks retaining provider call ID, tool name, and parsed arguments.
- Tool-result blocks retaining the matching call ID, bounded content, and an explicit error flag.
- Optional opaque continuation blocks with a provider-specific discriminator and bounded bytes. These are replayed only to the same compatible route and are never interpreted as domain facts.
- Grant-filtered tool definitions supplied separately from prose.

### Turn output

- Ordered assistant blocks.
- Convenience accessors may expose concatenated visible text and collected tool calls, but order must not be lost in the authoritative result.
- Completion reason, normalized usage, route-attempt diagnostics, and normalized failure.
- No implicit success when a stream ends without a terminal event. Replace the current permissive fallback with an explicit invalid-response failure unless an existing compatibility test proves a required legacy exception; any exception must be adapter-local and documented.

### Usage

Preserve current input/output token accounting. Additive optional fields may represent cache creation/read or thinking-token data, but budget enforcement must continue to use conservative totals and must not undercount. Unknown provider usage fields are ignored only after the required totals are obtained.

### Capability rules

Required core Anthropic Messages profile:

- system instruction;
- text input/output;
- streaming text;
- client tool definitions;
- `tool_use` and `tool_result`;
- streamed tool arguments;
- finish reason;
- input/output usage;
- cancellation and normalized failures.

Optional and separately advertised:

- thinking/redacted-thinking continuation;
- prompt-cache controls and cache usage;
- image or document input;
- schema-constrained final output;
- parallel tool-use requests.

Server-side tools remain unsupported even if an endpoint advertises them, because they bypass Relay's tool registry and grants.

## Phased implementation

### Phase 0: baseline, ownership, and decision recording

1. Read `CLAUDE.md`, this plan, and the authoritative intent-first plan completely.
2. Capture `git status --short`; identify user-owned changes and freeze file ownership across the team.
3. Run the focused baseline suites before editing:

   ```bash
   cargo test -p relay-providers
   cargo test -p relay-config
   cargo test -p relay-executor
   cargo test -p relay-tests --test capability_security
   cargo test -p relay-tests --test scheduling_concurrency
   ```

4. Record any pre-existing failures verbatim and do not "fix" unrelated failures.
5. Update `CLAUDE.md` narrowly because this is a fundamental engineering direction:
   - Anthropic Messages is the strategic live external provider protocol.
   - `fake` remains the deterministic offline provider.
   - Anthropic Messages is the sole supported live external protocol.
   - `openai-chat`, its default connection, and its capability presets have been removed.
   - Relay owns the transcript and tool loop; provider-hosted tools/state are not authoritative.
6. Do not otherwise change product-direction or ownership instructions.

Exit criterion: clean ownership map, known baseline, and one unambiguous repository-level protocol rule.

### Phase 1: define and test the Relay-owned native-turn contract

1. Introduce ordered input/output content-block types in `relay-providers` without exposing Anthropic wire structs.
2. Represent tool results with call ID, bounded content, and `is_error`.
3. Represent ordered assistant output, completion, usage, and optional opaque continuation data.
4. Extend stream events as needed for block lifecycle and incremental tool arguments. Do not require callers to parse partial JSON; the adapter must either emit validated complete calls or a typed terminal invalid-response error.
5. Redesign `collect_turn` to preserve block order and require a terminal completion/error event.
6. Redesign `ModelTurnRequest` and `ModelTurnResponse` so `relay-executor` no longer discards tool calls. Keep routing and error/fallback diagnostics intact.
7. Add unit tests for:
   - interleaved text and tool blocks;
   - multiple calls and stable ordering;
   - tool-result correlation;
   - incomplete/malformed streamed arguments;
   - missing terminal event;
   - cancellation;
   - usage aggregation without undercounting;
   - bounded opaque continuation blocks.

Do not edit `planner.rs` until the contract compiles and its tests pass. This is the shared-contract checkpoint for downstream workers.

Exit criterion: provider and executor contracts express a complete native tool turn and pass focused unit tests with fake data.

### Phase 2: make the Anthropic Messages adapter conform fully

1. Map Relay instructions and transcript blocks to `/v1/messages` wire requests.
2. Merge adjacent tool results into valid Anthropic user content where required while preserving logical ordering and call IDs.
3. Replay prior assistant text/tool-use blocks exactly enough for a subsequent tool-result turn.
4. Parse SSE block start/delta/stop events, incremental JSON tool input, message usage, stop reasons, and terminal events.
5. Fail closed on:
   - duplicate or missing block indices;
   - malformed tool arguments;
   - tool blocks without IDs/names;
   - unknown required event transitions;
   - stream EOF without `message_stop`;
   - response-size or opaque-state bounds.
6. Preserve existing redaction, bounded error text, timeout, status classification, credential handling, and loopback-only tests.
7. If thinking/redacted-thinking support is added, retain it as bounded opaque continuation data and prove it is never rendered, logged, or persisted as domain evidence.
8. Expand `anthropic_conformance.rs` with offline fixtures covering text-only, tool-only, mixed text/tool, multiple tools, error tool results, malformed fragments, cancellation, rate limiting, authentication failure, incomplete stream, and optional continuation blocks.

Exit criterion: the Anthropic adapter passes an explicit required-capability conformance suite entirely offline.

### Phase 3: carry native tools through routing and the executor

1. Update `ProviderModelClient` and `RoutedModelClient` to pass transcript and tool definitions and return ordered native blocks/tool calls.
2. Preserve deterministic route selection and current fallback policy. A tool-use response from a successful route pins the continuation to that route; do not send provider-specific opaque continuation data or unmatched call IDs to a different fallback route.
3. Define the behavior when a continuation route becomes unavailable:
   - if the transcript is fully Relay-owned and portable, retry only from a safe turn boundary without opaque state;
   - otherwise fail with an actionable normalized error rather than silently switching providers mid-turn.
4. Preserve summed usage and complete `routes_attempted` diagnostics.
5. Add routed-model tests proving that provider selection remains model-independent and that tool continuation cannot cross an incompatible route.

Exit criterion: a routed Anthropic-compatible connection can complete text and native-tool turns through `ModelClient` without text-encoded tool calls.

### Phase 4: replace the planner's synthetic JSON tool protocol

1. Obtain visible `ToolSpec` values from the existing `ToolRunner`/registry after grant filtering. The same schema sent to the model must remain the schema used for runtime validation.
2. Remove tool descriptions and `{"tool_calls": ...}` instructions from the workspace/verifier prompt protocol.
3. Maintain a Relay-owned transcript for the session:
   - initial user objective/context;
   - assistant text/tool-use blocks;
   - Relay-executed tool results marked success/error;
   - follow-up model turns.
4. For every tool call:
   - reject unknown tools;
   - parse and validate arguments with the registry;
   - enforce the immutable capability grant;
   - honor cancellation and budgets;
   - execute only through `ToolRunner`;
   - return bounded/redacted output and `is_error` to the matching call ID;
   - retain the current `CallRecord`/receipt evidence behavior.
5. Preserve deterministic sequential execution of multiple calls unless an existing safe policy already authorizes parallelism. Do not infer parallel safety from the provider request.
6. Keep the final receipt proposal schema strict. Structured final outputs may remain a JSON object in visible assistant text for this phase, but tool calls must be native and must not be accepted from text. A later change may introduce a separate final-output tool/schema after evaluating provider support.
7. Update remediation prompts to address only malformed final output, not tool-call serialization.
8. Reject a turn that mixes a purported final receipt with native tool calls when the semantics are ambiguous; return tool results and request a final turn instead.

Exit criterion: workspace and verifier sessions use native provider tool calls end to end, while final proposals still pass existing typed validation and policy gates.

### Phase 5: preserve model-only execution behavior

1. Keep `ModelExecutor` tool-free. It may use the new transcript/result types but must reject any native tool call with an actionable remediation message.
2. Preserve the accepted final object shape for outputs, evidence, and graph changes.
3. Preserve current retry/remediation counts, budgets, cancellation, receipts, evidence validation, and graph-change proposal policy.
4. Update fake/scripted model doubles to produce native result blocks where appropriate. Avoid test doubles that bypass the public `ModelClient` behavior.
5. Remove the OpenAI provider module, exports, wire structs, conformance tests, capability defaults, and adapter construction branches. Confirm no production or test code can construct an OpenAI Chat request.

Exit criterion: existing intent-first scenarios and offline default behavior remain unchanged except that guided sessions use native tools internally.

### Phase 6: remove OpenAI Chat configuration and define the migration failure

1. Remove `AdapterKind::OpenaiChat`, `openai-chat` from `ADAPTER_KINDS`, OpenAI capability defaults, `OPENAI_API_KEY` defaults, the implicit `providers.openai` connection, and OpenAI-specific presets such as `glm`.
2. Preserve `adapter: anthropic` as the canonical spelling and retain inference for `providers.anthropic` with `ANTHROPIC_API_KEY`.
3. Keep `fake` as the deterministic offline adapter and default route. The supported adapter set becomes `anthropic | fake`.
4. Inventory every place configuration schema version 1 is serialized, persisted, inspected, or used by legacy fixtures. If removal changes the promised accepted language, bump the schema version and implement explicit older-version validation/migration behavior rather than leaving a misleading version number.
5. Recognize the literal obsolete spelling `openai-chat` only at the raw validation boundary to return an error such as: `providers.<name>.adapter: openai-chat was removed; configure an Anthropic Messages-compatible endpoint with adapter: anthropic and select a model supported by that endpoint`. Do not create an OpenAI enum variant or runtime connection.
6. A legacy `providers.openai` entry without `adapter` must no longer infer a protocol. Return a similarly targeted error. Do not silently point it at Anthropic and do not guess a base URL or model mapping.
7. Update golden configuration corpora and compatibility tests intentionally. Preserve immutable historical project fixtures; where they contain removed provider configuration, assert the documented migration error rather than rewriting the fixture.
8. Remove OpenAI-only routes/examples from defaults, README, configuration docs, and examples. Replace useful multi-connection examples with multiple named Anthropic-compatible endpoints.
9. Add validation for new capability fields only when the runtime implements them. Unknown/unsupported capabilities fail with actionable configuration paths.
10. Do not retain dead OpenAI terminology in public inspection output, environment-variable guidance, available-adapter lists, or error suggestions.

Exit criterion: existing config fixtures remain valid and deterministic, and users can configure direct Anthropic-compatible endpoints without changing core routing semantics.

### Phase 7: security and scenario verification

Add cross-crate tests proving:

1. A model-requested path outside the grant is denied before any filesystem action.
2. Unknown tool names and malformed arguments return correlated error tool results and never execute.
3. A denied tool call cannot be converted into evidence of success.
4. Repeated identical calls still trigger the existing repetition/budget protection.
5. Multiple tool calls preserve order and each result matches the correct call ID.
6. Cancellation between stream events and between tool calls stops subsequent execution.
7. Model-only attempts cannot invoke tools.
8. Workspace-changing attempts remain serialized; native provider calls do not weaken scheduling rules.
9. No project writer lock is held during provider calls or tools.
10. Provider output cannot directly mutate the graph, publish an artifact, or authorize delivery.
11. Hidden/opaque provider state is absent from facts, SQLite projections, artifacts, receipts, logs, and user-facing output.
12. The fake default still runs offline with no credentials or network.
13. No `openai-chat` adapter, provider module, construction path, capability preset, conformance test, default connection, or successful configuration path remains.
14. Obsolete OpenAI configurations fail before network or project-state mutation with an actionable Anthropic migration diagnostic.

Exit criterion: native tools satisfy the same or stronger capability-security and scheduling evidence as the synthetic JSON loop.

### Phase 8: documentation, review, and cleanup

1. Update README and configuration documentation to explain:
   - Anthropic Messages is the strategic live protocol;
   - Relay owns state and tool execution;
   - compatible endpoint support is capability-profiled, not assumed;
   - Anthropic Messages is the sole live external protocol;
   - OpenAI Chat support was intentionally removed and obsolete configurations fail with migration guidance;
   - provider-hosted tools/state are not used as Relay authority.
2. Reconcile example edits with the pre-existing untracked `examples/` files; never overwrite user content blindly.
3. Remove the OpenAI adapter and OpenAI-only code/tests/docs completely. Preserve immutable legacy fixtures and frozen crates; adapt fixture expectations to the explicit migration failure rather than altering historical inputs.
4. Review public docs and diagnostics for the terms `provider`, `connection`, `adapter`, and `model`; preserve their existing meanings.
5. Run formatting only on touched Rust files or through the standard workspace formatter after verifying it will not rewrite unrelated user changes.
6. Inspect the final diff and `git status --short`; confirm no user file was lost, staged, committed, or pushed.

Exit criterion: documentation matches the Anthropic-only behavior, obsolete OpenAI configuration guidance is actionable, and the final diff contains no unrelated cleanup.

## Engineering constraints

- Safe Rust only; use the pinned toolchain and committed `Cargo.lock`.
- No Anthropic or other provider SDK dependency. Continue using private wire structs and `reqwest` inside the adapter.
- Provider-neutral public types must not contain Anthropic field names unless they are genuinely protocol-independent concepts.
- The authoritative transcript is Relay-owned. Provider IDs may be retained as bounded opaque correlation/continuation data but cannot replace local history.
- Do not persist API keys, authorization headers, raw HTTP bodies containing secrets, hidden reasoning, or unbounded provider output.
- Bound every text block, tool argument buffer, tool result, opaque block, error, and operational log before retaining it. Reuse central redaction where possible.
- Tool schemas exposed to the provider and validators used at execution must come from the same registry definition.
- Never execute a tool based solely on provider-declared capability. Enforce the immutable attempt grant for every call.
- Never silently downgrade unsupported features. Reject them during capability preflight with an actionable error.
- Do not fall back to another provider in the middle of a provider-specific continuation unless the complete portable transcript is replayed from a safe boundary.
- Preserve deterministic routing, bounded fallback, budgets, retry policy, cancellation, and route-attempt diagnostics.
- Do not change authoritative work-graph facts or persistence formats for operational stream detail.
- Do not keep an `OpenaiChat` runtime enum variant, dormant adapter, feature flag, or test-only implementation. Raw-parser recognition for a migration error is the only allowed remnant of the `openai-chat` adapter spelling.
- Keep default tests offline and deterministic; loopback mock servers must bind only to `127.0.0.1`.
- Do not commit or push.

## Acceptance criteria

1. `relay-executor` can run a workspace attempt where the model emits a native tool call, Relay executes it through the grant-gated registry, returns a correlated tool result, and accepts a later valid final proposal.
2. The planner no longer instructs models to emit `{"tool_calls": ...}` and no longer parses tool calls from assistant text.
3. Ordered visible text and multiple tool calls survive provider normalization and executor routing without loss or reordering.
4. Tool errors, denials, malformed arguments, and unknown tool names are returned as explicit error results and never treated as successful evidence.
5. The Anthropic adapter fails closed on malformed/incomplete SSE streams and has offline conformance coverage for required core behavior.
6. Relay owns and can replay the full portable transcript; execution does not depend on provider-stored conversation state.
7. Provider-hosted tools and remote MCP execution cannot bypass Relay's registry or capability grants.
8. Opaque thinking/continuation data, if implemented, is bounded, same-route-only, non-authoritative, and absent from durable/user-visible surfaces.
9. Model-only attempts remain tool-free and preserve the existing strict final proposal format.
10. Existing routing, fallback, usage accounting, cancellation, repetition limits, scheduling serialization, receipts, evidence, and graph policy behave as before.
11. Updated configuration fixtures, migration diagnostics, offline fake-provider behavior, and Anthropic tests pass; OpenAI Chat conformance tests and runtime code no longer exist.
12. No changes are made to `crates/relay-core`, `crates/relay-runtime`, legacy migration fixtures, or user-owned untracked files.
13. Documentation identifies Anthropic Messages as the only live external protocol, removes OpenAI setup instructions, and gives concise migration guidance for obsolete `openai-chat` configuration.
14. Repository-wide searches find no production support for `/chat/completions`, `OPENAI_API_KEY`, `AdapterKind::OpenaiChat`, or OpenAI capability presets. Any remaining historical mention is explicitly labeled archival evidence or migration-error text.
15. All required focused and workspace-wide verification commands pass, or pre-existing failures are reported with evidence and left unrelated to the implementation.

## Verification commands

Run focused checks during development:

```bash
cargo test -p relay-providers
cargo test -p relay-providers --test anthropic_conformance
cargo test -p relay-config
cargo test -p relay-config --test legacy_compat
cargo test -p relay-executor
cargo test -p relay-tools
cargo test -p relay-tests --test capability_security
cargo test -p relay-tests --test scheduling_concurrency
cargo test -p relay-tests --test intent_first_scenarios
```

If the actual scenario test target differs, identify it with `cargo test -p relay-tests --test <target> -- --list` or the workspace manifest and run the current equivalent; do not invent or silently omit it.

Run the complete repository gates after integration:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo build --workspace --all-targets --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps --exclude relay-core --exclude relay-runtime
```

Run the focused intent-first suites required by `CLAUDE.md`:

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

Because configuration compatibility is intentionally broken for the removed protocol, also run:

```bash
cargo test -p relay-tests --test migration
cargo test -p relay-tests --test semantic_migration
cargo test -p relay-tests --test compatibility
cargo test -p relay-config --test legacy_compat
```

Finally inspect repository state:

```bash
git status --short
git diff --check
git diff --stat
```

Verify the removed protocol has no live implementation surface:

```bash
rg -n "/chat/completions|OPENAI_API_KEY|AdapterKind::OpenaiChat|openai_conformance|preset: glm" crates docs README.md examples
```

Every match must be either an explicit migration diagnostic, immutable historical evidence, or a clearly labeled historical plan. There must be no live adapter, default, example, or successful configuration path.

Do not commit or push. Report completed phases, files changed, focused and full verification results, pre-existing failures, intentional configuration breaks and migration diagnostics, and any unresolved protocol or safety issue.
