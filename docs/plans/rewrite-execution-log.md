# Rust rewrite execution log (lead integrator working notes)

Working notes for the in-session execution of `docs/plans/2026-10-13-rust-rewrite-implementation.md`.
Not a plan; delete or archive at rewrite completion.

## Environment

- Rust: Homebrew 1.99.0 at /opt/homebrew/bin (includes rustfmt+clippy). On PATH; agents need no setup.
- Inert rustup copy at ~/.cargo (do not use; user chose Homebrew).
- Toolchain pin: rust-toolchain.toml channel 1.99.0 (documentation/MSRV for rustup users; brew ignores).
- cargo-deny NOT selected (not installed; decision recorded in final report).
- MSRV declared 1.99 (workspace.package.rust-version); lowering requires dedicated testing — deferred decision.

## Ownership map (one editor per area)

- Lead (me): root Cargo.toml, rust-toolchain.toml, .gitignore, .github/workflows/ci.yml, CLAUDE.md, docs/event-format.md, docs/migration-to-rust.md, README.md, docs/architecture.md, docs/configuration.md (last three at cutover), crates/relay-tests coordination.
- Core/compat agent: crates/relay-core, scripts/export-*.mjs, test/compatibility/**, tests/fixtures/**, crates/relay-tests/tests/compatibility.rs.
- Storage agent: crates/relay-store, crates/relay-tests/tests/{migration,fault_injection}.rs. Follow-up assignment (not started): crates/relay-artifacts.
- Runtime/security agent: crates/relay-config (wave 1), then relay-providers, relay-tools, relay-runtime (wave 2, sequential), crates/relay-tests/tests/security.rs.
- CLI/UI agent (wave 2, spawns after core lands): crates/relay-ui, crates/relay-cli, relay-cli/tests/cli_compatibility.rs (CLI-view compat tests; compatibility.rs itself is core agent's).

## Phase status

- Phase 0 (freeze TS baseline): DONE except fixture corpus (core agent milestone 1).
  - Routing plan verified complete: 560/560 tests, --role works (exit 2 unknown role, no state mutation), config show/get verified, help/version captured (0.1.0, usage exit 2, status 0).
  - npm typecheck/test/build/pack --dry-run/git diff --check all pass. User changes intact.
  - docs/event-format.md written (normative). CLAUDE.md dual-runtime update done. docs/migration-to-rust.md written.
  - Exit codes: 0 success, 1 runtime, 2 usage/config. Error-name → code map in src/cli/errors.ts.
- Phase 1 (workspace + relay-core): workspace scaffold DONE (10 crates, builds clean, workspace lints: forbid unsafe, deny unwrap/expect/panic, warn missing_docs). relay-core in progress (core agent).

## 2026-10-13 ~10:30 — rate-limit interruption

- 5-hour API usage limit hit at ~10:27 (reset 11:31): core agent (mid relay-core test phase) and storage agent (mid envelope/canonical delegation refactor to relay-core) both stopped. Config agent had already completed.
- One-shot cron 2a350185 scheduled for 11:36 to resume both agents (session-only).
- On-disk state verified: `cargo build --workspace` PASSES; `cargo test -p relay-core` = 83 passed / 5 failed (ids uuid-shaped parse, policies denied-case patterns, reducer gap/duplicate message wording, transitions forward-jump + paused-rejection — paused-state ordering issue suspected in evaluateTransition mode-complete branch). These 5 are the core agent's known remaining work.
- relay-config: COMPLETE (61 tests, fmt/clippy/test/build pass; differentially verified byte-identical diagnostics vs TS oracle). Decisions: serde_yaml 0.9 + indexmap 2 crate-local; effective dump is serde_yaml Value (insertion order is contract); relay-config has its own frozen mode-name constants (WorkflowMode unification with relay-core = optional coordinated follow-up); provider registration not modeled (TS checks at buildProviders); config show sequence indentation diverges cosmetically from `yaml` npm (CLI/UI owner to decide emitter tweak).
- Fixture corpus COMPLETE (core agent M1): 24 cases + config corpus + cli corpus; deterministic; 621 TS tests green. e2e-demo-run includes seven artifacts byte-for-byte. export-legacy-project.mjs working.

## Key contracts I reviewed (for arbitration)

- Provider contract: src/providers/provider.ts (ModelMessage union, ProviderStreamEvent, ModelRequest/TurnResult, ProviderFailure kinds, ProviderName = openai|anthropic|fake).
- Tool contract: src/tools/registry.ts (ToolDefinition kebab-case, capability read|write|execute, timeoutMs, maxOutputChars, PermissionGate, ToolResult{ok,outcome,output,data,affectedFiles}).
- Controller: src/runtime/controller.ts — append→settle pattern, runStep shortcut (approved artifact + no pending note + not enteredByUser), required-input revision guard, draft parse with ONE corrective re-ask, artifact created-vs-revised, verification.completed after verification-report draft, applyTransitionOutcome (approval pause / delivery hold / ADVANCE user-authorization / REVISE), resolveRoutes precedence (configured → --role → --provider filter → --model rewrite), pendingUserNote semantics.
- Agent loop: routes in order, withRetries per route, permission gate BEFORE args validation... (order: gate deny → args schema → repeated-action sha1(name+canonical-args) limit 3 → execute with timeout/cancel race), redaction regexes sk-ant-/sk-/Bearer, finishReason length notice, unknown-tool/invalid-args feedback strings, budget check after each tool.
- Router: purposeChains (intent + 7 modes as keys), roleChains, legacyFallback; attachProviders (primary must be registered else error, fallbacks filtered), exact error messages matter.
- Views: src/runtime/controller-views.ts (StatusView/WhyView shapes; routing tail 40 events, transitions tail 10, blockers 8, role-aware failure matching).
- CLI: parse-arguments.ts grammar (long options only, --name value|--name=value|bool; value-flags workspace/provider/model/role/auto/goal; join-positionals semantics for init/pause message/run message), USAGE text frozen, main.ts dispatch (help/version bypass JSON writer; stateful commands need .relay else exit 1 "No Relay project in <ws>. Run relay init.").

## Verification status

- npm suite: PASS (560 tests) at Phase 0 close.
- cargo build --workspace on skeletons: PASS.
- Full cargo gates: run at each wave integration point (lead).
