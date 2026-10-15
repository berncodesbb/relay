//! Capability and executor-boundary security suite (Phase 5,
//! runtime/security owner).
//!
//! Adversarial coverage for the five enforcement layers:
//!
//! - per-tool-call grant escapes: path/scope traversal, symlink layering,
//!   unlisted commands and environment names, delivery without authority,
//!   and patches aimed outside the grant's write scopes;
//! - budget enforcement: token, attempt, and duration ceilings fire the
//!   attempt's cancellation token mid-execution;
//! - executor-cannot-mutate-graph: receipts and proposals only — a
//!   proposing executor never installs graph changes;
//! - cancellation propagation through live tool execution (Unix).
//!
//! Offline and deterministic: no network, no real providers, manual
//! clocks. Process tests are `cfg(unix)`.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;

use relay_domain::{
    Budget, CapabilityGrant, CompletionRule, DomainFact, ExecutorClass, GrantId, ModelRoute,
    Operation, ScopePath, WorkItem, WorkItemId, WorkKind,
};
use relay_executor::{
    BudgetExhausted, BudgetGuard, ExecutorSet, FactSink, ManualClock, Orchestrator, RunContext,
    SystemJob, SystemOutcome, ToolCallRequest, WorkspaceExecutor, WorkspacePlanner,
    grant_from_work, sink::InMemoryFactSink,
};
use relay_providers::contract::CancelToken;
use relay_tools::GrantGate;
use relay_tools::PermissionDecision;
use serde_json::json;

fn ws(prefix: &str) -> PathBuf {
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir()
        .expect("temp workspace")
        .keep()
}

fn grant(mutate: impl FnOnce(&mut CapabilityGrant)) -> CapabilityGrant {
    let mut grant = CapabilityGrant {
        id: GrantId::parse("grant_sec").unwrap(),
        read_paths: vec![ScopePath::root()],
        write_paths: vec![ScopePath::parse("src").unwrap()],
        operations: vec![Operation::Read, Operation::Write, Operation::Execute],
        command_allowlist: vec![relay_domain::CommandPattern {
            program: "/bin/echo".to_owned(),
            args_prefix: vec!["hello".to_owned()],
        }],
        environment_names: vec!["RELAY_CAP_TEST_VAR".to_owned()],
        model_route: Some(ModelRoute {
            provider: "fake".to_owned(),
            model: "fake-standard".to_owned(),
            role: None,
        }),
        budget: Budget::default(),
        delivery: relay_domain::DeliveryAuthority::None,
    };
    mutate(&mut grant);
    grant
}

fn registry() -> Arc<relay_tools::ToolRegistry> {
    Arc::new(
        relay_tools::create_default_tool_registry(&relay_config::ToolSettings::default())
            .expect("registry"),
    )
}

// ---------------------------------------------------------------------------
// Grant escapes: scopes, commands, environment, delivery
// ---------------------------------------------------------------------------

#[test]
fn tool_calls_cannot_escape_the_grant_scopes() {
    let workspace = ws("relay-cap-scopes-");
    let gate = GrantGate::new(grant(|_| {}));
    let tools = registry();
    let context = relay_tools::ToolContext::new(&workspace);

    // Inside the write scope: allowed and effective.
    let inside = gate.call(
        &tools,
        "write-file",
        &json!({ "path": "src/new_file.rs", "content": "pub fn f() {}\n" }),
        &context,
    );
    assert!(inside.ok, "{}", inside.output);
    assert!(workspace.join("src/new_file.rs").exists());

    // Outside every granted scope: denied, nothing written.
    for path in ["docs/escape.md", "../escape.txt", "/tmp/relay-escape.txt"] {
        let denied = gate.call(
            &tools,
            "write-file",
            &json!({ "path": path, "content": "nope" }),
            &context,
        );
        assert!(!denied.ok, "{path} must be denied");
        assert!(
            denied.output.contains("denied by capability grant"),
            "{}: {}",
            path,
            denied.output
        );
    }
    assert!(!workspace.join("docs/escape.md").exists());
    assert!(!workspace.parent().unwrap().join("escape.txt").exists());
}

#[cfg(unix)]
#[test]
fn the_path_guard_still_enforces_symlinks_under_a_permissive_grant() {
    let workspace = ws("relay-cap-symlink-");
    let outside = ws("relay-cap-outside-");
    std::os::unix::fs::symlink(&outside, workspace.join("linked")).expect("symlink");
    let gate = GrantGate::new(grant(|value| {
        // A grant that nominally covers the whole tree.
        value.write_paths = vec![ScopePath::root()];
    }));
    let tools = registry();
    // The gate permits the path lexically; the path guard must still
    // refuse the symlink escape underneath it. Layers, not either/or.
    let result = gate.call(
        &tools,
        "write-file",
        &json!({ "path": "linked/evil.txt", "content": "nope" }),
        &relay_tools::ToolContext::new(&workspace),
    );
    assert!(
        !result.ok,
        "symlink escapes must not survive the path guard"
    );
    assert!(result.output.contains("symlink"), "{}", result.output);
    assert!(!outside.join("evil.txt").exists());
}

#[test]
fn unlisted_commands_and_environment_names_are_denied() {
    let workspace = ws("relay-cap-command-");
    let gate = GrantGate::new(grant(|_| {}));
    let tools = registry();
    let context = relay_tools::ToolContext::new(&workspace);

    let unlisted = gate.call(
        &tools,
        "run-command",
        &json!({ "command": "/bin/rm", "args": ["-rf", "."] }),
        &context,
    );
    assert!(!unlisted.ok);
    assert!(
        unlisted.output.contains("process allowlist"),
        "{}",
        unlisted.output
    );

    let wrong_prefix = gate.call(
        &tools,
        "run-command",
        &json!({ "command": "/bin/echo", "args": ["goodbye"] }),
        &context,
    );
    assert!(
        !wrong_prefix.ok,
        "argument prefixes are part of the allowlist"
    );

    let unlisted_env = gate.call(
        &tools,
        "run-command",
        &json!({
            "command": "/bin/echo",
            "args": ["hello"],
            "env": ["RELAY_NEVER_GRANTED"],
        }),
        &context,
    );
    assert!(!unlisted_env.ok);
    assert!(
        unlisted_env
            .output
            .contains("neither a base variable nor named by"),
        "{}",
        unlisted_env.output
    );

    #[cfg(unix)]
    {
        let allowed = gate.call(
            &tools,
            "run-command",
            &json!({ "command": "/bin/echo", "args": ["hello", "extra"] }),
            &context,
        );
        assert!(allowed.ok, "{}", allowed.output);
    }
    #[cfg(not(unix))]
    {
        let _ = &context;
    }
}

#[test]
fn delivery_preparation_requires_explicit_authority() {
    let gate = GrantGate::new(grant(|_| {}));
    assert!(matches!(
        gate.permits_delivery_prep(),
        PermissionDecision::Deny { .. }
    ));
    let gate = GrantGate::new(grant(|value| {
        value.delivery = relay_domain::DeliveryAuthority::PreparePackage;
    }));
    assert_eq!(gate.permits_delivery_prep(), PermissionDecision::Allow);
}

#[test]
fn patches_aimed_outside_the_write_scopes_are_denied_before_parsing_writes() {
    let workspace = ws("relay-cap-patch-");
    let gate = GrantGate::new(grant(|_| {}));
    let tools = registry();
    let patch = "--- a/docs/target.md\n+++ b/docs/target.md\n@@ -1 +1 @@\n-a\n+b\n";
    let denied = gate.call(
        &tools,
        "apply-patch",
        &json!({ "patch": patch }),
        &relay_tools::ToolContext::new(&workspace),
    );
    assert!(!denied.ok);
    assert!(denied.output.contains("outside grant"), "{}", denied.output);
    assert!(!workspace.join("docs/target.md").exists());
}

/// A grant-denied call can never be converted into evidence of success:
/// the verifier executor accounts a denied observation as Fail evidence
/// carrying the denial, the attempt does not complete the work, and the
/// denial text is the record — never a fabricated success observation.
#[test]
fn a_denied_tool_call_cannot_become_evidence_of_success() {
    let workspace = ws("relay-cap-denied-");
    let sink = Arc::new(
        InMemoryFactSink::seeded(seed_facts("work_denied", ExecutorClass::Verifier)).expect("seed"),
    );
    // A verifier grant is read/execute-only, so a write call is denied by
    // the grant itself before any filesystem action.
    let grants = test_grants(None, Budget::default());
    let orchestrator = Arc::new(
        Orchestrator::new(
            Arc::clone(&sink) as Arc<_>,
            Arc::new(ManualClock::new(1_000)).into_clock(),
        )
        .with_executors(ExecutorSet::new().with(Arc::new(
            relay_executor::VerifierExecutor::scripted(vec![ToolCallRequest {
                tool: "write-file".to_owned(),
                args: json!({
                    "path": "src/smuggled.rs",
                    "content": "pub const OK: bool = true;\n",
                }),
            }]),
        )))
        .with_grants(grants)
        .with_tools(registry())
        .with_workspace_root(workspace.clone()),
    );
    let report = orchestrator.tick().expect("tick");
    assert_eq!(report.started.len(), 1, "{report:?}");

    // The denial is durable evidence, and it is failure evidence.
    let graph = sink.graph().expect("graph");
    let mut denials = 0;
    for (_, evidence) in graph.evidence() {
        let payload = serde_json::to_string(&evidence.payload).unwrap_or_default();
        if payload.contains("write-file") || evidence.summary.contains("write-file") {
            denials += 1;
            assert_eq!(
                evidence.outcome,
                relay_domain::EvidenceOutcome::Fail,
                "a denied call must never read as success: {evidence:?}"
            );
            assert!(
                evidence.summary.contains("denied by capability grant"),
                "{}",
                evidence.summary
            );
        }
    }
    assert_eq!(denials, 1, "the denied call must be accounted exactly once");
    assert!(
        !workspace.join("src/smuggled.rs").exists(),
        "the denied write never touched the workspace"
    );
    assert!(
        !relay_engine::status::work_is_completed(
            &graph,
            graph
                .work(&WorkItemId::parse("work_denied").unwrap())
                .unwrap()
        ),
        "a denied observation must never complete the work"
    );
}

// ---------------------------------------------------------------------------
// Budget enforcement: ceilings fire the attempt's cancel token
// ---------------------------------------------------------------------------

#[test]
fn budget_ceilings_fire_the_attempt_cancel_token() {
    let clock = Arc::new(ManualClock::new(1_000));
    let signal = CancelToken::new();
    let guard = BudgetGuard::new(
        Budget {
            max_input_tokens: Some(100),
            max_attempts: Some(3),
            max_duration_ms: Some(2_000),
            ..Budget::default()
        },
        1_000,
        Arc::clone(&clock).into_clock(),
        signal.clone(),
    );
    assert!(guard.record_attempt().is_ok());
    assert!(guard.record_model_usage(40, 10).is_ok());
    let tokens = guard.record_model_usage(80, 0);
    assert!(matches!(tokens, Err(BudgetExhausted::Tokens { .. })));
    assert!(signal.is_cancelled());

    let clock = Arc::new(ManualClock::new(1_000));
    let signal = CancelToken::new();
    let guard = BudgetGuard::new(
        Budget {
            max_duration_ms: Some(500),
            ..Budget::default()
        },
        1_000,
        Arc::clone(&clock).into_clock(),
        signal.clone(),
    );
    clock.advance(501);
    assert!(matches!(
        guard.enforce(),
        Err(BudgetExhausted::Duration { .. })
    ));
    assert!(signal.is_cancelled());
}

/// A planner that burns the duration budget before its tool call runs,
/// proving the fired token stops the batch mid-execution.
struct ExhaustingPlanner {
    clock: Arc<ManualClock>,
}

impl WorkspacePlanner for ExhaustingPlanner {
    fn plan(
        &self,
        _spec: &relay_executor::AttemptSpec,
        _context: &RunContext,
    ) -> Vec<ToolCallRequest> {
        // Cross the duration ceiling, firing the attempt's token.
        self.clock.advance(10_000);
        vec![ToolCallRequest {
            tool: "write-file".to_owned(),
            args: json!({ "path": "src/budgeted.rs", "content": "never written\n" }),
        }]
    }
}

#[test]
fn duration_exhaustion_stops_the_tool_batch_mid_execution() {
    let workspace = ws("relay-cap-budget-");
    let clock = Arc::new(ManualClock::new(1_000));
    let sink = Arc::new(
        InMemoryFactSink::seeded(seed_facts("work_budget", ExecutorClass::Workspace))
            .expect("seed"),
    );
    let grants = test_grants(
        None,
        Budget {
            max_duration_ms: Some(500),
            ..Budget::default()
        },
    );
    let orchestrator =
        Arc::new(
            Orchestrator::new(Arc::clone(&sink) as Arc<_>, Arc::clone(&clock).into_clock())
                .with_executors(ExecutorSet::new().with(Arc::new(WorkspaceExecutor::new(
                    Arc::new(ExhaustingPlanner {
                        clock: Arc::clone(&clock),
                    }),
                ))))
                .with_grants(grants)
                .with_tools(registry())
                .with_workspace_root(workspace.clone()),
        );
    let report = orchestrator.tick().expect("tick");
    assert_eq!(report.started.len(), 1);
    assert_eq!(report.canceled.len(), 1, "{report:?}");
    assert_eq!(report.accepted.len(), 0);
    // The budget-stopped call never touched the workspace.
    assert!(!workspace.join("src/budgeted.rs").exists());
}

/// Repeated identical calls must not run away: every tool call is one
/// budget attempt, so a model (or script) repeating the same call drains
/// the attempt budget and the batch stops before the later calls execute.
#[test]
fn repeated_identical_calls_drain_the_budget_and_stop_the_batch() {
    let workspace = ws("relay-cap-repeat-");
    std::fs::create_dir_all(workspace.join("src")).expect("src dir");
    std::fs::write(workspace.join("src/target.txt"), "stable content\n").expect("seed file");
    let sink = Arc::new(
        InMemoryFactSink::seeded(seed_facts("work_repeat", ExecutorClass::Workspace))
            .expect("seed"),
    );
    // Room for exactly two calls.
    let grants = test_grants(
        None,
        Budget {
            max_attempts: Some(2),
            ..Budget::default()
        },
    );
    let read_target = || ToolCallRequest {
        tool: "read-file".to_owned(),
        args: json!({ "path": "src/target.txt" }),
    };
    let orchestrator = Arc::new(
        Orchestrator::new(
            Arc::clone(&sink) as Arc<_>,
            Arc::new(ManualClock::new(1_000)).into_clock(),
        )
        .with_executors(
            ExecutorSet::new().with(Arc::new(WorkspaceExecutor::scripted(vec![
                read_target(),
                read_target(),
                read_target(),
                read_target(),
            ]))),
        )
        .with_grants(grants)
        .with_tools(registry())
        .with_workspace_root(workspace.clone()),
    );
    let report = orchestrator.tick().expect("tick");
    assert_eq!(report.started.len(), 1);
    assert_eq!(report.canceled.len(), 1, "{report:?}");
    assert_eq!(report.accepted.len(), 0, "{report:?}");

    // The durable conclusion names the attempt-count exhaustion exactly:
    // two calls ran, the third tripped the ceiling and fired the attempt
    // token, and the fourth never executed. Reads left no workspace trace,
    // so the budget arithmetic is the observable: attempts 3 > max 2.
    let graph = sink.graph().expect("graph");
    let conclusion = graph
        .attempts()
        .map(|(_, attempt)| attempt)
        .find(|attempt| attempt.conclusion.is_some())
        .and_then(|attempt| attempt.conclusion.as_ref())
        .expect("concluded attempt");
    match conclusion {
        relay_domain::AttemptConclusion::Canceled { reason } => {
            assert!(
                reason.contains("attempt-count budget exhausted"),
                "{reason}"
            );
        }
        other => panic!("budget exhaustion must cancel, not conclude {other:?}"),
    }
    assert!(
        !relay_engine::status::work_is_completed(
            &graph,
            graph
                .work(&WorkItemId::parse("work_repeat").unwrap())
                .unwrap()
        ),
        "repetition must never complete the work"
    );
}

// ---------------------------------------------------------------------------
// Executors propose; they never mutate the graph
// ---------------------------------------------------------------------------

/// A system job that tries to "helpfully" declare new work.
struct ProposingJob;

impl SystemJob for ProposingJob {
    fn run(&self, spec: &relay_executor::AttemptSpec, _context: &RunContext) -> SystemOutcome {
        let work = WorkItem::new(
            WorkItemId::parse("work_sneaky").unwrap(),
            WorkKind::parse("sneaky-extra").unwrap(),
            ExecutorClass::Model,
            "work the system executor tried to install directly",
            CompletionRule::Receipt,
        );
        SystemOutcome {
            outputs: Vec::new(),
            proposals: vec![relay_domain::GraphChangeProposal::DeclareWork { work }],
            evidence: vec![relay_domain::Evidence {
                id: relay_domain::EvidenceId::parse(&format!(
                    "ev_{}-sys",
                    spec.attempt.as_str().strip_prefix("att_").unwrap_or("x")
                ))
                .unwrap(),
                kind: relay_domain::EvidenceKind::Inspection,
                target: relay_domain::EvidenceTarget::WorkItem(spec.work.clone()),
                outcome: relay_domain::EvidenceOutcome::Info,
                summary: "system job accounted".to_owned(),
                payload: json!({ "note": "proposing" }),
                recorded_at_ms: 0,
            }],
            note: None,
        }
    }
}

#[test]
fn executors_cannot_install_graph_changes_directly() {
    let sink = Arc::new(
        InMemoryFactSink::seeded(seed_facts("work_system", ExecutorClass::System)).expect("seed"),
    );
    let orchestrator = Arc::new(
        Orchestrator::new(
            Arc::clone(&sink) as Arc<_>,
            Arc::new(ManualClock::new(1_000)).into_clock(),
        )
        .with_executors(
            ExecutorSet::new().with(Arc::new(relay_executor::SystemExecutor::new(Arc::new(
                ProposingJob,
            )))),
        )
        .with_grants(test_grants(None, Budget::default())),
    );
    let report = orchestrator.tick().expect("tick");
    assert_eq!(report.accepted.len(), 1, "{report:?}");
    let graph = sink.graph().expect("graph");
    // The proposed work was NOT installed by execution.
    assert!(
        graph
            .work(&WorkItemId::parse("work_sneaky").unwrap())
            .is_none(),
        "executor proposals must never install graph changes"
    );
    // The only work is the seeded one; the proposal rides in the receipt
    // awaiting separate validation.
    assert_eq!(graph.works().count(), 1);
}

#[test]
fn executor_proposals_still_validate_through_the_engine_when_submitted() {
    let sink = Arc::new(
        InMemoryFactSink::seeded(seed_facts("work_system2", ExecutorClass::System)).expect("seed"),
    );
    let orchestrator = Arc::new(
        Orchestrator::new(
            Arc::clone(&sink) as Arc<_>,
            Arc::new(ManualClock::new(1_000)).into_clock(),
        )
        .with_executors(
            ExecutorSet::new().with(Arc::new(relay_executor::SystemExecutor::new(Arc::new(
                ProposingJob,
            )))),
        )
        .with_grants(test_grants(None, Budget::default())),
    );
    orchestrator.tick().expect("tick");
    let graph = sink.graph().expect("graph");
    // The accepted conclusion wraps a receipt carrying the proposal.
    let concluded = graph
        .attempts()
        .map(|(_, attempt)| attempt)
        .find(|attempt| attempt.conclusion.is_some())
        .expect("concluded attempt");
    match concluded.conclusion.as_ref().unwrap() {
        relay_domain::AttemptConclusion::Accepted { receipt } => {
            assert_eq!(receipt.graph_changes.len(), 1);
            // The proposal validates cleanly when the policy submits it.
            let outcome = relay_engine::validate_graph_change(&graph, &receipt.graph_changes[0]);
            assert!(outcome.is_ok(), "{outcome:?}");
        }
        other => panic!("expected acceptance, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Cancellation propagation through a live tool (Unix)
// ---------------------------------------------------------------------------

/// A planner whose run-command sleeps; the attempt token fires mid-tool.
struct SleepingPlanner;

impl WorkspacePlanner for SleepingPlanner {
    fn plan(
        &self,
        _spec: &relay_executor::AttemptSpec,
        _context: &RunContext,
    ) -> Vec<ToolCallRequest> {
        vec![ToolCallRequest {
            tool: "run-command".to_owned(),
            args: json!({ "command": "/bin/sleep", "args": ["30"] }),
        }]
    }
}

#[cfg(unix)]
#[test]
fn cancellation_propagates_into_live_tool_execution() {
    let workspace = ws("relay-cap-cancel-");
    let sink = Arc::new(
        InMemoryFactSink::seeded(seed_facts("work_cancel", ExecutorClass::Workspace))
            .expect("seed"),
    );
    // The grant allows the sleeping command; the duration budget is the
    // production cancellation path (watchdog fires the attempt token).
    let grants: Arc<dyn Fn(&relay_domain::WorkNode) -> CapabilityGrant + Send + Sync> =
        Arc::new(move |work: &relay_domain::WorkNode| {
            let mut grant = grant_from_work(
                GrantId::parse("grant_cancel").unwrap(),
                work,
                None,
                Budget {
                    max_duration_ms: Some(300),
                    ..Budget::default()
                },
            );
            grant.command_allowlist = vec![relay_domain::CommandPattern {
                program: "/bin/sleep".to_owned(),
                args_prefix: vec!["30".to_owned()],
            }];
            grant
        });
    let orchestrator = Arc::new(
        Orchestrator::new(Arc::clone(&sink) as Arc<_>, relay_executor::system_clock())
            .with_executors(
                ExecutorSet::new()
                    .with(Arc::new(WorkspaceExecutor::new(Arc::new(SleepingPlanner)))),
            )
            .with_grants(grants)
            .with_tools(registry())
            .with_workspace_root(workspace)
            .with_spawn(relay_executor::thread_spawn())
            .with_duration_watchdogs(true),
    );
    let started = std::time::Instant::now();
    let report = orchestrator.tick().expect("tick");
    assert_eq!(report.canceled.len(), 1, "{report:?}");
    assert_eq!(report.accepted.len(), 0, "{report:?}");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(10),
        "the watchdog must kill the sleeping process group promptly, took {:?}",
        started.elapsed()
    );
}

// ---------------------------------------------------------------------------
// Model-only attempts stay tool-free
// ---------------------------------------------------------------------------

/// A scripted model client that records every request it served and always
/// replies with one native tool call (never a valid final object).
struct ToolCallingModel {
    requests: Mutex<Vec<relay_executor::ModelTurnRequest>>,
}

impl relay_executor::ModelClient for ToolCallingModel {
    fn complete(
        &self,
        _route: &ModelRoute,
        request: &relay_executor::ModelTurnRequest,
        _signal: &relay_providers::contract::CancelToken,
    ) -> Result<relay_executor::ModelTurnResponse, relay_executor::ModelClientError> {
        self.requests
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .push(request.clone());
        // A native tool-use block: the honest adversarial probe for
        // model-class work.
        Ok(relay_executor::ModelTurnResponse {
            blocks: vec![relay_providers::contract::AssistantContent::ToolCall {
                call: relay_providers::contract::ToolCall {
                    id: "toolu_notools".to_owned(),
                    name: "write-file".to_owned(),
                    arguments: json!({
                        "path": "src/model-written.rs",
                        "content": "never\n",
                    }),
                },
            }],
            input_tokens: 1,
            output_tokens: 1,
            routes_attempted: Vec::new(),
        })
    }
}

/// Model-class attempts have no tools: a reply that tries to invoke one is
/// rejected with an actionable failure, the request the model saw carried no
/// tool definitions, and nothing is executed in the workspace.
#[test]
fn model_only_attempts_reject_tool_calls_and_execute_nothing() {
    let workspace = ws("relay-cap-notools-");
    let sink = Arc::new(
        InMemoryFactSink::seeded(seed_facts("work_notools", ExecutorClass::Model)).expect("seed"),
    );
    let route = ModelRoute {
        provider: "fake".to_owned(),
        model: "fake-standard".to_owned(),
        role: None,
    };
    let model = Arc::new(ToolCallingModel {
        requests: Mutex::new(Vec::new()),
    });
    let orchestrator = Arc::new(
        Orchestrator::new(
            Arc::clone(&sink) as Arc<_>,
            Arc::new(ManualClock::new(1_000)).into_clock(),
        )
        .with_executors(ExecutorSet::new().with(Arc::new(relay_executor::ModelExecutor)))
        .with_grants(test_grants(Some(route), Budget::default()))
        .with_model(Arc::clone(&model) as Arc<_>)
        .with_tools(registry())
        .with_workspace_root(workspace.clone()),
    );
    let report = orchestrator.tick().expect("tick");
    assert_eq!(report.started.len(), 1);
    assert_eq!(report.accepted.len(), 1, "{report:?}");
    // The accepted receipt concludes the attempt FAILED, naming the
    // tool-free rule so the failure is actionable.
    let graph = sink.graph().expect("graph");
    let conclusion = graph
        .attempts()
        .map(|(_, attempt)| attempt)
        .find(|attempt| attempt.conclusion.is_some())
        .and_then(|attempt| attempt.conclusion.as_ref())
        .expect("concluded attempt");
    match conclusion {
        relay_domain::AttemptConclusion::Accepted { receipt } => match receipt.outcome {
            relay_domain::AttemptOutcome::Failed { ref reason } => {
                assert!(reason.contains("model-class work has no tools"), "{reason}");
            }
            ref outcome => panic!("a tool-calling reply must fail the attempt, got {outcome:?}"),
        },
        other => panic!("expected an accepted-but-failed receipt, got {other:?}"),
    }

    // The model was never offered tools, and its tool-call attempt reached
    // no filesystem action.
    let served = model
        .requests
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    assert!(!served.is_empty());
    assert!(
        served.iter().all(|request| request.tools.is_none()),
        "model-only turns must be tool-free"
    );
    assert!(!workspace.join("src/model-written.rs").exists());
    let graph = sink.graph().expect("graph");
    assert!(
        !relay_engine::status::work_is_completed(
            &graph,
            graph
                .work(&WorkItemId::parse("work_notools").unwrap())
                .unwrap()
        ),
        "a tool-call reply must never complete model-class work"
    );
}

// ---------------------------------------------------------------------------
// Native tool-call security (Phase 7 second pass): model-requested calls
// through the real guided loop — grant escapes denied before any
// filesystem action, unknown/malformed calls answered with correlated
// error results and never executed.
// ---------------------------------------------------------------------------

/// A scripted model client for native sessions: serves queued turns in
/// order (the last one repeats) and records every request it served, so
/// tests can assert on the transcript the executor actually sent back.
struct NativeScriptModel {
    turns: Mutex<std::collections::VecDeque<relay_executor::ModelTurnResponse>>,
    requests: Mutex<Vec<relay_executor::ModelTurnRequest>>,
}

impl NativeScriptModel {
    fn served_requests(&self) -> Vec<relay_executor::ModelTurnRequest> {
        self.requests
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }
}

impl relay_executor::ModelClient for NativeScriptModel {
    fn complete(
        &self,
        _route: &ModelRoute,
        request: &relay_executor::ModelTurnRequest,
        _signal: &relay_providers::contract::CancelToken,
    ) -> Result<relay_executor::ModelTurnResponse, relay_executor::ModelClientError> {
        self.requests
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .push(request.clone());
        let mut turns = self
            .turns
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let response = if turns.len() > 1 {
            turns.pop_front().expect("scripted turn")
        } else {
            turns.front().cloned().expect("scripted turn")
        };
        Ok(response)
    }
}

/// One native tool-use block.
fn native_call(
    id: &str,
    name: &str,
    arguments: serde_json::Value,
) -> relay_providers::contract::AssistantContent {
    relay_providers::contract::AssistantContent::ToolCall {
        call: relay_providers::contract::ToolCall {
            id: id.to_owned(),
            name: name.to_owned(),
            arguments,
        },
    }
}

/// A native turn carrying exactly the given blocks.
fn native_turn(
    blocks: Vec<relay_providers::contract::AssistantContent>,
) -> relay_executor::ModelTurnResponse {
    relay_executor::ModelTurnResponse {
        blocks,
        input_tokens: 2,
        output_tokens: 3,
        routes_attempted: Vec::new(),
    }
}

/// The tool results the executor sent back on the follow-up turn, as
/// (call id, content, is_error) in transcript order.
fn tool_results_of(request: &relay_executor::ModelTurnRequest) -> Vec<(String, String, bool)> {
    request
        .messages
        .iter()
        .filter_map(|message| match message {
            relay_providers::contract::ModelMessage::ToolResult {
                tool_call_id,
                content,
                is_error,
            } => Some((tool_call_id.clone(), content.clone(), *is_error)),
            _ => None,
        })
        .collect()
}

/// The tool results of the last turn the model was asked for.
fn last_turn_tool_results(model: &NativeScriptModel) -> Vec<(String, String, bool)> {
    tool_results_of(
        model
            .served_requests()
            .last()
            .expect("at least one served request"),
    )
}

#[test]
fn native_tool_calls_aimed_outside_the_grant_are_denied_and_answered_with_error_results() {
    let workspace = ws("relay-cap-native-scope-");
    let sink = Arc::new(
        InMemoryFactSink::seeded(seed_facts("work_native-scope", ExecutorClass::Workspace))
            .expect("seed"),
    );
    let route = ModelRoute {
        provider: "fake".to_owned(),
        model: "fake-standard".to_owned(),
        role: None,
    };
    let model = Arc::new(NativeScriptModel {
        turns: Mutex::new(
            vec![
                // One call inside an ungranted directory, one escaping the
                // workspace entirely.
                native_turn(vec![
                    native_call(
                        "toolu_docs",
                        "write-file",
                        json!({ "path": "docs/escape.md", "content": "nope\n" }),
                    ),
                    native_call(
                        "toolu_parent",
                        "write-file",
                        json!({ "path": "../escape.txt", "content": "nope\n" }),
                    ),
                ]),
                // After the denials the model gives up with a plain final
                // evidence object (no outputs: nothing was ever written).
                relay_executor::ModelTurnResponse {
                    input_tokens: 2,
                    output_tokens: 3,
                    ..relay_executor::ModelTurnResponse::from_text(
                        r#"{"evidence":[{"kind":"inspection","target":{"work_item":"work_native-scope"},"outcome":"info","summary":"nothing escaped"}]}"#,
                    )
                },
            ]
            .into(),
        ),
        requests: Mutex::new(Vec::new()),
    });
    let orchestrator = Arc::new(
        Orchestrator::new(
            Arc::clone(&sink) as Arc<_>,
            Arc::new(ManualClock::new(1_000)).into_clock(),
        )
        .with_executors(
            ExecutorSet::new().with(Arc::new(WorkspaceExecutor::new(Arc::new(
                relay_executor::ModelGuidedWorkspacePlanner::new(),
            )))),
        )
        .with_grants(test_grants(Some(route), Budget::default()))
        .with_model(Arc::clone(&model) as Arc<_>)
        .with_tools(registry())
        .with_workspace_root(workspace.clone()),
    );
    let report = orchestrator.tick().expect("tick");
    assert_eq!(report.started.len(), 1, "{report:?}");

    // Denied before any filesystem action: neither target exists.
    assert!(!workspace.join("docs/escape.md").exists());
    assert!(!workspace.parent().unwrap().join("escape.txt").exists());

    // Both calls were answered by correlated error tool results naming the
    // grant as the reason — never a success observation.
    let results = last_turn_tool_results(&model);
    assert_eq!(results.len(), 2, "{results:?}");
    assert_eq!(results[0].0, "toolu_docs");
    assert_eq!(results[1].0, "toolu_parent");
    for (id, content, is_error) in &results {
        assert!(is_error, "{id} must be an error result: {content}");
        assert!(
            content.contains("denied by capability grant"),
            "{id}: {content}"
        );
    }

    // The attempt itself failed and the work never completed.
    let graph = sink.graph().expect("graph");
    let conclusion = graph
        .attempts()
        .map(|(_, attempt)| attempt)
        .find(|attempt| attempt.conclusion.is_some())
        .and_then(|attempt| attempt.conclusion.as_ref())
        .expect("concluded attempt");
    match conclusion {
        relay_domain::AttemptConclusion::Accepted { receipt } => {
            assert!(
                matches!(receipt.outcome, relay_domain::AttemptOutcome::Failed { .. }),
                "a grant-escaping turn must fail: {:?}",
                receipt.outcome
            );
        }
        other => panic!("expected a receipted conclusion, got {other:?}"),
    }
    assert!(
        !relay_engine::status::work_is_completed(
            &graph,
            graph
                .work(&WorkItemId::parse("work_native-scope").unwrap())
                .unwrap()
        ),
        "a denied native call must never complete the work"
    );
}

#[test]
fn unknown_and_malformed_native_tool_calls_get_error_results_and_never_execute() {
    let workspace = ws("relay-cap-native-shape-");
    std::fs::create_dir_all(workspace.join("src")).expect("src dir");
    std::fs::write(workspace.join("src/keep.txt"), "keep me\n").expect("seed file");
    let sink = Arc::new(
        InMemoryFactSink::seeded(seed_facts("work_native-shape", ExecutorClass::Workspace))
            .expect("seed"),
    );
    let route = ModelRoute {
        provider: "fake".to_owned(),
        model: "fake-standard".to_owned(),
        role: None,
    };
    let model = Arc::new(NativeScriptModel {
        turns: Mutex::new(
            vec![
                native_turn(vec![
                    // A hallucinated tool name.
                    native_call(
                        "toolu_unknown",
                        "levitate-file",
                        json!({ "path": "src/gone.txt" }),
                    ),
                    // A real tool with schema-invalid arguments (content is
                    // not a string).
                    native_call(
                        "toolu_malformed",
                        "write-file",
                        json!({ "path": "src/bad.rs", "content": 42 }),
                    ),
                ]),
                relay_executor::ModelTurnResponse {
                    input_tokens: 2,
                    output_tokens: 3,
                    ..relay_executor::ModelTurnResponse::from_text(
                        r#"{"evidence":[{"kind":"inspection","target":{"work_item":"work_native-shape"},"outcome":"info","summary":"nothing ran"}]}"#,
                    )
                },
            ]
            .into(),
        ),
        requests: Mutex::new(Vec::new()),
    });
    let orchestrator = Arc::new(
        Orchestrator::new(
            Arc::clone(&sink) as Arc<_>,
            Arc::new(ManualClock::new(1_000)).into_clock(),
        )
        .with_executors(
            ExecutorSet::new().with(Arc::new(WorkspaceExecutor::new(Arc::new(
                relay_executor::ModelGuidedWorkspacePlanner::new(),
            )))),
        )
        .with_grants(test_grants(Some(route), Budget::default()))
        .with_model(Arc::clone(&model) as Arc<_>)
        .with_tools(registry())
        .with_workspace_root(workspace.clone()),
    );
    let report = orchestrator.tick().expect("tick");
    assert_eq!(report.started.len(), 1, "{report:?}");

    // Neither call executed: no new file, and the pre-existing file is
    // untouched.
    assert!(!workspace.join("src/gone.txt").exists());
    assert!(!workspace.join("src/bad.rs").exists());
    assert_eq!(
        std::fs::read_to_string(workspace.join("src/keep.txt")).unwrap(),
        "keep me\n"
    );

    // Correlated error results: the unknown tool names the visible surface;
    // the malformed arguments are rejected with a validation message.
    let results = last_turn_tool_results(&model);
    assert_eq!(results.len(), 2, "{results:?}");
    let (unknown_id, unknown_content, unknown_error) = &results[0];
    assert_eq!(unknown_id, "toolu_unknown");
    assert!(unknown_error, "an unknown tool is an error result");
    assert!(
        unknown_content.contains("unknown tool \"levitate-file\""),
        "{unknown_content}"
    );
    assert!(
        unknown_content.contains("write-file"),
        "the error must name a usable alternative: {unknown_content}"
    );
    let (malformed_id, malformed_content, malformed_error) = &results[1];
    assert_eq!(malformed_id, "toolu_malformed");
    assert!(malformed_error, "invalid arguments are an error result");
    assert!(
        malformed_content.to_lowercase().contains("content"),
        "the validation message must name the offending field: {malformed_content}"
    );

    let graph = sink.graph().expect("graph");
    assert!(
        !relay_engine::status::work_is_completed(
            &graph,
            graph
                .work(&WorkItemId::parse("work_native-shape").unwrap())
                .unwrap()
        ),
        "never-executed calls must never complete the work"
    );
}

// ---------------------------------------------------------------------------
// OpenAI removal (Phase 7 items 13/14): no openai-chat surface survives,
// and obsolete configurations fail at the raw validation boundary with
// the targeted Anthropic migration diagnostic, before any network attempt
// or project-state creation.
// ---------------------------------------------------------------------------

#[test]
fn the_openai_chat_adapter_is_gone_from_every_public_surface() {
    use relay_executor::routed_model::ProviderRegistry;
    use relay_providers::contract::{ADAPTER_KINDS, AdapterKind};

    // The spelling is not parseable anywhere: neither protocol nor vendor
    // shorthand resurrects an OpenAI adapter.
    assert!(AdapterKind::parse("openai-chat").is_none());
    assert!(AdapterKind::parse("openai").is_none());
    assert_eq!(ADAPTER_KINDS, [AdapterKind::Anthropic, AdapterKind::Fake]);

    // No capability preset survives the removal.
    assert!(
        relay_providers::contract::PRESET_NAMES.is_empty(),
        "no capability presets may remain: {:?}",
        relay_providers::contract::PRESET_NAMES
    );

    // The shipped default profile carries exactly the offline fake and the
    // anthropic connection — no implicit openai.
    assert_eq!(
        relay_config::DEFAULT_CONNECTION_NAMES,
        ["fake", "anthropic"]
    );
    let config = relay_config::default_config();
    assert!(config.providers.0.contains_key("fake"));
    assert!(config.providers.0.contains_key("anthropic"));
    assert!(
        !config.providers.0.contains_key("openai"),
        "the implicit openai connection must be gone"
    );

    // And a registry built from those defaults knows only these names.
    let registry = ProviderRegistry::from_effective_config(&config);
    assert_eq!(registry.names(), ["fake", "anthropic"]);
}

#[test]
fn obsolete_openai_configurations_fail_with_migration_diagnostics_before_any_side_effect() {
    // The failure happens in configuration validation: no provider is
    // constructed, no network is attempted, and no project state is
    // created (the workspace below must stay empty).
    let workspace = ws("relay-cap-migrate-");

    // 1. The removed adapter spelling on any connection.
    let yaml = r#"version: 1
budgets: {}
providers:
  gateway:
    adapter: openai-chat
routing:
  intent: { provider: fake, model: fake-standard }
  discovery: { provider: fake, model: fake-standard }
  specification: { provider: fake, model: fake-standard }
  technical-design: { provider: fake, model: fake-standard }
  planning: { provider: fake, model: fake-standard }
  implementation: { provider: fake, model: fake-standard }
  verification: { provider: fake, model: fake-standard }
  delivery: { provider: fake, model: fake-standard }"#;
    let error = relay_config::parse_yaml_layer("project config", yaml)
        .expect_err("openai-chat must fail configuration validation");
    match &error {
        relay_config::ConfigError::Validation(validation) => {
            assert!(
                validation.issues.iter().any(|issue| issue.contains(
                    "providers.gateway.adapter: openai-chat was removed; configure an Anthropic \
                     Messages-compatible endpoint with adapter: anthropic and select a model \
                     supported by that endpoint"
                )),
                "{}",
                error
            );
        }
        other => panic!("expected a validation failure, got {other}"),
    }

    // 2. The legacy implicit `providers.openai` entry without an adapter.
    let yaml = r#"version: 1
budgets: {}
providers:
  openai:
    apiKeyEnv: MY_OPENAI_KEY
routing:
  intent: { provider: fake, model: fake-standard }
  discovery: { provider: fake, model: fake-standard }
  specification: { provider: fake, model: fake-standard }
  technical-design: { provider: fake, model: fake-standard }
  planning: { provider: fake, model: fake-standard }
  implementation: { provider: fake, model: fake-standard }
  verification: { provider: fake, model: fake-standard }
  delivery: { provider: fake, model: fake-standard }"#;
    let error = relay_config::parse_yaml_layer("project config", yaml)
        .expect_err("the implicit openai connection must fail configuration validation");
    match &error {
        relay_config::ConfigError::Validation(validation) => {
            assert!(
                validation.issues.iter().any(|issue| issue.contains(
                    "providers.openai: the implicit openai connection was removed with the \
                     openai-chat adapter; configure an Anthropic Messages-compatible connection \
                     (adapter: anthropic) and update routes that name openai"
                )),
                "{}",
                error
            );
        }
        other => panic!("expected a validation failure, got {other}"),
    }

    // Nothing was created on disk: configuration failure precedes any
    // project-state mutation or connection construction.
    assert!(
        !workspace.join(".relay-graph").exists(),
        "no project state may appear from a failed configuration load"
    );
    assert!(!workspace.join(".relay").exists());
    assert!(
        std::fs::read_dir(&workspace).unwrap().count() == 0,
        "the workspace stays empty"
    );
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn seed_facts(work_id: &str, class: ExecutorClass) -> Vec<DomainFact> {
    let mut work = WorkItem::new(
        WorkItemId::parse(work_id).unwrap(),
        WorkKind::parse("test-work").unwrap(),
        class,
        "do the thing",
        CompletionRule::Receipt,
    );
    work.read_scopes = vec![ScopePath::root()];
    match class {
        ExecutorClass::Workspace => {
            work.write_scopes = vec![ScopePath::parse("src").unwrap()];
            work.capability_ceiling.write_scopes = vec![ScopePath::parse("src").unwrap()];
            work.capability_ceiling.operations =
                vec![Operation::Read, Operation::Write, Operation::Execute];
        }
        ExecutorClass::Verifier => {
            work.capability_ceiling.operations = vec![Operation::Read, Operation::Execute];
        }
        _ => {}
    }
    vec![
        DomainFact::ProjectCreated {
            project: relay_domain::ProjectId::parse("prj_cap").unwrap(),
        },
        seed_goal(),
        DomainFact::WorkDeclared { work },
    ]
}

fn seed_goal() -> DomainFact {
    DomainFact::GoalDeclared {
        goal: relay_domain::Goal {
            id: relay_domain::GoalId::parse("goal_cap").unwrap(),
            outcome: "test capability enforcement".into(),
            constraints: Vec::new(),
            success_definition: "the suites pass".into(),
            provenance: relay_domain::Provenance {
                source: relay_domain::ProvenanceSource::User,
                statement: None,
                at_ms: 0,
            },
            declared_at_ms: 0,
        },
    }
}

fn test_grants(
    route: Option<ModelRoute>,
    budget: Budget,
) -> Arc<dyn Fn(&relay_domain::WorkNode) -> CapabilityGrant + Send + Sync> {
    let ids = Arc::new(relay_executor::IdSource::new());
    Arc::new(move |work| grant_from_work(ids.next_grant_id(), work, route.clone(), budget))
}

// ---------------------------------------------------------------------------
// Credential hygiene across the routing seam (interface owner, Phase 5)
// ---------------------------------------------------------------------------

/// Shapes a credential-shaped substring must never appear in: a missing
/// credential's diagnostic names the environment VARIABLE and nothing else,
/// and no credential-shaped string reaches a grant, a fact, or the wire
/// diagnostics the routed client synthesizes. Proven on the Anthropic
/// adapter (the strategic live protocol), so the suite survives the
/// openai-chat removal without touching an OpenAI construction path.
#[test]
fn missing_credential_diagnostics_name_the_variable_never_a_value() {
    use relay_executor::routed_model::{ConnectionState, ProviderRegistry, ResolvedRoutePlan};
    use relay_providers::anthropic::{AnthropicProvider, AnthropicProviderOptions};

    let never_set = "RELAY_CAPSEC_TEST_NEVER_SET_KEY_2026_10_15";
    let mut config = relay_config::default_config();
    config.providers.0.insert(
        "anthropic-missing".to_owned(),
        relay_config::ProviderConnection {
            adapter: relay_config::AdapterKind::Anthropic,
            api_key_env: Some(never_set.to_owned()),
            base_url: Some("https://example.invalid/anthropic".to_owned()),
            preset: None,
            capabilities: relay_config::CapabilityOverrides::default(),
        },
    );

    // The registry records the connection unavailable with the adapter's
    // bounded reason: the variable NAME, never a value.
    let registry = ProviderRegistry::from_effective_config(&config);
    let reason = match registry.connection("anthropic-missing") {
        Some(ConnectionState::Unavailable { reason, .. }) => reason.clone(),
        Some(ConnectionState::Ready { .. }) => {
            panic!("anthropic-missing must not be ready without its variable")
        }
        None => panic!("anthropic-missing must be registered"),
    };
    assert!(reason.contains(never_set), "{reason}");
    for credential_shape in ["sk-", "sk-ant-", "Bearer "] {
        assert!(!reason.contains(credential_shape), "{reason}");
    }
    assert!(registry.ready("anthropic-missing").is_none());

    // The adapter's own construction failure carries the same hygiene.
    let failure = match AnthropicProvider::new(AnthropicProviderOptions {
        api_key: None,
        api_key_env: Some(never_set.to_owned()),
        base_url: None,
        capabilities: None,
    }) {
        Ok(_) => panic!("construction without the variable must fail"),
        Err(failure) => failure,
    };
    assert!(failure.message.contains(never_set), "{}", failure.message);
    for credential_shape in ["sk-", "sk-ant-", "Bearer "] {
        assert!(
            !failure.message.contains(credential_shape),
            "{}",
            failure.message
        );
    }

    // A grant's model route carries only the connection name — the fact
    // surface has no room for credential-shaped strings, and the persisted
    // JSON proves it.
    let plan = ResolvedRoutePlan::from_normalized(
        &relay_config::normalize_routing(&config),
        &Default::default(),
    );
    let route = plan
        .primary("intent")
        .expect("intent resolves")
        .route
        .clone();
    assert_eq!(route.provider, "fake", "the offline default stays usable");
    let grant = grant(|grant| grant.model_route = Some(route));
    let serialized = serde_json::to_string(&grant).unwrap();
    assert!(!serialized.contains("sk-"), "{serialized}");
    assert!(
        !serialized.contains(never_set),
        "only the connection name is recorded: {serialized}"
    );
}
