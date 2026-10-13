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
