//! Scheduling and concurrency suite (Phase 5, runtime/security owner).
//!
//! Covers the orchestrator's execution semantics on plain std threads:
//!
//! - parallel read/model attempts genuinely run concurrently;
//! - workspace writes are serialized (one writer per tick, disjoint
//!   intervals), and overlapping write grants are refused outright;
//! - stale-revision double-finish follows the materially-precise rule
//!   (a receipt is stale only when the work's own node, direct
//!   dependencies, or exactly pinned inputs changed) and an attempt can
//!   never conclude twice;
//! - abandoned attempts (crash between start and conclusion) reconcile as
//!   `Abandoned` — never success — with write-authority abandons surfacing
//!   a human decision instead of an auto-revert;
//! - swapping providers changes model bytes, never accepted facts or
//!   policy outcomes.
//!
//! Offline and deterministic: fake providers, manual or system clocks, no
//! network.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use relay_domain::{
    ArtifactContent, ArtifactDigest, ArtifactId, ArtifactVersionRef, AttemptConclusion, AttemptId,
    AttemptOutcome, AttemptReceipt, Budget, CapabilityGrant, CompletionRule, DomainFact, Evidence,
    EvidenceId, EvidenceKind, EvidenceOutcome, EvidenceTarget, ExecutorClass, GrantId, InputRef,
    ModelRoute, Operation, ScopePath, WorkItem, WorkItemId, WorkKind,
};
use relay_engine::SchedulingPolicy;
use relay_engine::schedule::{Autonomy, recommend};
use relay_executor::{
    ExecutorSet, FactSink, ModelCallFailure, ModelClient, ModelClientError, ModelTurnRequest,
    ModelTurnResponse, Orchestrator, ProviderModelClient, ToolCallRequest, WorkspaceExecutor,
    WorkspacePlanner, grant_from_work, sink::InMemoryFactSink,
};
use relay_providers::fake::{FakeProvider, FakeResponse};
use serde_json::json;

fn route() -> ModelRoute {
    ModelRoute {
        provider: "fake".to_owned(),
        model: "fake-standard".to_owned(),
        role: None,
    }
}

fn model_grants() -> Arc<dyn Fn(&relay_domain::WorkNode) -> CapabilityGrant + Send + Sync> {
    let ids = Arc::new(relay_executor::IdSource::new());
    let route = route();
    Arc::new(move |work| {
        grant_from_work(
            ids.next_grant_id(),
            work,
            Some(route.clone()),
            Budget::default(),
        )
    })
}

fn seed_facts(work: Vec<WorkItem>) -> Vec<DomainFact> {
    let mut facts = vec![
        DomainFact::ProjectCreated {
            project: relay_domain::ProjectId::parse("prj_sched").unwrap(),
        },
        DomainFact::GoalDeclared {
            goal: relay_domain::Goal {
                id: relay_domain::GoalId::parse("goal_sched").unwrap(),
                outcome: "exercise scheduling semantics".into(),
                constraints: Vec::new(),
                success_definition: "the suites pass".into(),
                provenance: relay_domain::Provenance {
                    source: relay_domain::ProvenanceSource::User,
                    statement: None,
                    at_ms: 0,
                },
                declared_at_ms: 0,
            },
        },
    ];
    facts.extend(
        work.into_iter()
            .map(|work| DomainFact::WorkDeclared { work }),
    );
    facts
}

fn model_work(id: &str) -> WorkItem {
    let mut work = WorkItem::new(
        WorkItemId::parse(id).unwrap(),
        WorkKind::parse("model-work").unwrap(),
        ExecutorClass::Model,
        "reason about the thing",
        CompletionRule::Receipt,
    );
    work.read_scopes = vec![ScopePath::root()];
    work
}

fn writer_work(id: &str, scope: &str) -> WorkItem {
    let mut work = WorkItem::new(
        WorkItemId::parse(id).unwrap(),
        WorkKind::parse("write-work").unwrap(),
        ExecutorClass::Workspace,
        format!("change files under {scope}"),
        CompletionRule::Receipt,
    );
    work.read_scopes = vec![ScopePath::root()];
    work.write_scopes = vec![ScopePath::parse(scope).unwrap()];
    work.capability_ceiling.write_scopes = vec![ScopePath::parse(scope).unwrap()];
    work.capability_ceiling.operations =
        vec![Operation::Read, Operation::Write, Operation::Execute];
    work
}

fn read_grant(id: &str) -> CapabilityGrant {
    CapabilityGrant::read_only(GrantId::parse(id).unwrap(), vec![ScopePath::root()])
}

// ---------------------------------------------------------------------------
// Parallel read/model attempts genuinely run concurrently
// ---------------------------------------------------------------------------

/// A model client whose calls rendezvous: each call waits until a second
/// call is in flight, proving concurrency (or times out, failing the
/// attempt).
struct RendezvousModel {
    state: Arc<(Mutex<u32>, Condvar)>,
}

impl RendezvousModel {
    fn new() -> (Self, Arc<(Mutex<u32>, Condvar)>) {
        let state = Arc::new((Mutex::new(0), Condvar::new()));
        (
            RendezvousModel {
                state: Arc::clone(&state),
            },
            state,
        )
    }
}

impl ModelClient for RendezvousModel {
    fn complete(
        &self,
        _route: &ModelRoute,
        _request: &ModelTurnRequest,
        _signal: &relay_providers::contract::CancelToken,
    ) -> Result<ModelTurnResponse, ModelClientError> {
        let (lock, cvar) = &*self.state;
        let mut count = lock.lock().unwrap_or_else(|poison| poison.into_inner());
        *count += 1;
        cvar.notify_all();
        let deadline = Instant::now() + Duration::from_secs(10);
        while *count < 2 {
            let waited = cvar
                .wait_timeout(count, deadline.saturating_duration_since(Instant::now()))
                .unwrap_or_else(|poison| poison.into_inner());
            count = waited.0;
            if waited.1.timed_out() {
                return Err(ModelClientError::Provider(ModelCallFailure::from_message(
                    "rendezvous timed out: attempts did not run concurrently",
                )));
            }
        }
        // A valid (empty-section) final object: the turn itself is the
        // accounting evidence, so the attempts complete.
        Ok(ModelTurnResponse {
            input_tokens: 1,
            output_tokens: 1,
            ..ModelTurnResponse::from_text("{}")
        })
    }
}

#[test]
fn parallel_model_attempts_run_concurrently() {
    let sink = Arc::new(
        InMemoryFactSink::seeded(seed_facts(vec![model_work("work_a"), model_work("work_b")]))
            .expect("seed"),
    );
    let (model, _state) = RendezvousModel::new();
    let orchestrator = Arc::new(
        Orchestrator::new(Arc::clone(&sink) as Arc<_>, relay_executor::system_clock())
            .with_executors(ExecutorSet::new().with(Arc::new(relay_executor::ModelExecutor)))
            .with_grants(model_grants())
            .with_model(Arc::new(model))
            .with_spawn(relay_executor::thread_spawn()),
    );
    let report = orchestrator.tick().expect("tick");
    assert_eq!(report.started.len(), 2, "{report:?}");
    assert_eq!(report.accepted.len(), 2, "{report:?}");
    assert!(report.failed.is_empty(), "{report:?}");
    let graph = sink.graph().expect("graph");
    for work in ["work_a", "work_b"] {
        let id = WorkItemId::parse(work).unwrap();
        let status = relay_engine::work_status_by_id(&graph, &id).expect("status");
        assert!(
            relay_engine::status::work_is_completed(&graph, graph.work(&id).unwrap()),
            "{work} should be completed, status: {}",
            status.name()
        );
    }
}

// ---------------------------------------------------------------------------
// Workspace writes are serialized; overlapping write grants refused
// ---------------------------------------------------------------------------

/// One recorded execution interval: which writer, start, end.
type Interval = (&'static str, Instant, Instant);

#[derive(Clone)]
struct RecordingPlanner {
    label: &'static str,
    log: Arc<Mutex<Vec<Interval>>>,
}

impl WorkspacePlanner for RecordingPlanner {
    fn plan(
        &self,
        _spec: &relay_executor::AttemptSpec,
        _context: &relay_executor::RunContext,
    ) -> Vec<ToolCallRequest> {
        // A short, bounded nap widens the interval so overlap would be
        // visible if writes ever raced.
        std::thread::sleep(Duration::from_millis(60));
        self.log
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .push((self.label, Instant::now(), Instant::now()));
        Vec::new()
    }
}

#[test]
fn workspace_writes_are_serialized() {
    let sink = Arc::new(
        InMemoryFactSink::seeded(seed_facts(vec![
            writer_work("work_writer-a", "src"),
            writer_work("work_writer-b", "src"),
        ]))
        .expect("seed"),
    );
    let log = Arc::new(Mutex::new(Vec::new()));
    let planner = |label: &'static str| RecordingPlanner {
        label,
        log: Arc::clone(&log),
    };
    let executors =
        ExecutorSet::new().with(Arc::new(WorkspaceExecutor::new(Arc::new(planner("a")))));
    // Both writers share one executor class; the planner distinguishes
    // them by work id below.
    let executors = executors.with(Arc::new(WorkspaceExecutor::new(Arc::new(planner("b")))));
    let orchestrator = Arc::new(
        Orchestrator::new(Arc::clone(&sink) as Arc<_>, relay_executor::system_clock())
            .with_executors(executors)
            .with_grants(Arc::new(|work: &relay_domain::WorkNode| {
                grant_from_work(
                    GrantId::parse("grant_write").unwrap(),
                    work,
                    None,
                    Budget::default(),
                )
            }))
            .with_spawn(relay_executor::thread_spawn()),
    );

    // The engine recommends overlapping writers one-at-a-time: one is
    // serialized, the second is explicitly deferred by the writer cap.
    let recommendation = recommend(&sink.graph().expect("graph"), &SchedulingPolicy::default());
    assert_eq!(recommendation.parallel.len(), 0);
    assert_eq!(recommendation.serialized.len(), 1, "{recommendation:?}");
    assert!(
        recommendation.serialized.iter().all(|entry| entry
            .write_scopes
            .iter()
            .any(|scope| scope.as_str() == "src")),
        "the writer declares its write scope: {recommendation:?}"
    );
    assert!(
        recommendation
            .deferred
            .iter()
            .any(|deferred| deferred.work.as_str() == "work_writer-b"
                && deferred.reason.contains("serialize")),
        "the overlapping writer waits: {recommendation:?}"
    );

    // One tick runs exactly one writer.
    let first = orchestrator.tick().expect("tick");
    assert_eq!(first.started.len(), 1, "{first:?}");
    let second = orchestrator.tick().expect("tick");
    assert_eq!(second.started.len(), 1, "{second:?}");
    assert!(first.accepted.len() + second.accepted.len() >= 1);

    let intervals = log.lock().unwrap_or_else(|poison| poison.into_inner());
    assert!(
        intervals.len() >= 2,
        "both writers should have run: {intervals:?}"
    );
    let mut sorted = intervals.clone();
    sorted.sort_by_key(|(_, start, _)| *start);
    for pair in sorted.windows(2) {
        let (_, _, earlier_end) = pair[0];
        let (_, later_start, _) = pair[1];
        assert!(
            earlier_end <= later_start,
            "write intervals must never overlap: {sorted:?}"
        );
    }
}

#[test]
fn overlapping_write_grants_are_refused() {
    // A writer is already active with the `src` scope; a second writer to
    // `src` must be refused by the orchestrator's independent guard.
    let sink = Arc::new(
        InMemoryFactSink::seeded(seed_facts(vec![
            writer_work("work_active-writer", "src"),
            writer_work("work_waiting-writer", "src"),
        ]))
        .expect("seed"),
    );
    // Start the first writer manually and leave it unterminated (as if
    // running), so the second must be refused.
    let graph = sink.graph().expect("graph");
    sink.append(DomainFact::AttemptStarted {
        attempt: AttemptId::parse("att_active").unwrap(),
        work: WorkItemId::parse("work_active-writer").unwrap(),
        expected_revision: graph.revision(),
        grant: write_grant("grant_active", "src"),
        started_at_ms: 0,
    })
    .expect("start");

    let waiting = sink.graph().expect("graph");
    let waiting_node = waiting
        .work(&WorkItemId::parse("work_waiting-writer").unwrap())
        .unwrap();
    let candidate = grant_from_work(
        GrantId::parse("grant_waiting").unwrap(),
        waiting_node,
        None,
        Budget::default(),
    );
    let conflict = relay_executor::overlapping_write_conflict(&waiting, &candidate);
    assert!(
        conflict
            .as_deref()
            .is_some_and(|reason| reason.contains("overlapping")),
        "overlapping write authority must be refused: {conflict:?}"
    );
    // Disjoint scopes do not conflict.
    let disjoint = CapabilityGrant {
        write_paths: vec![ScopePath::parse("docs").unwrap()],
        ..candidate.clone()
    };
    assert!(relay_executor::overlapping_write_conflict(&waiting, &disjoint).is_none());

    // And the orchestrator refuses to start the waiting writer this tick.
    let log = Arc::new(Mutex::new(Vec::new()));
    let orchestrator =
        Arc::new(
            Orchestrator::new(Arc::clone(&sink) as Arc<_>, relay_executor::system_clock())
                .with_executors(ExecutorSet::new().with(Arc::new(WorkspaceExecutor::new(
                    Arc::new(RecordingPlanner {
                        label: "waiting",
                        log: Arc::clone(&log),
                    }),
                ))))
                .with_grants(Arc::new(|work: &relay_domain::WorkNode| {
                    grant_from_work(
                        GrantId::parse("grant_tick").unwrap(),
                        work,
                        None,
                        Budget::default(),
                    )
                })),
        );
    let report = orchestrator.tick().expect("tick");
    assert!(
        report
            .refusals
            .iter()
            .any(|refusal| refusal.work.as_str() == "work_waiting-writer"
                && refusal.reason.contains("overlapping")),
        "{report:?}"
    );
    assert!(
        log.lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .is_empty()
    );
}

fn write_grant(id: &str, scope: &str) -> CapabilityGrant {
    CapabilityGrant {
        id: GrantId::parse(id).unwrap(),
        read_paths: vec![ScopePath::root()],
        write_paths: vec![ScopePath::parse(scope).unwrap()],
        operations: vec![Operation::Read, Operation::Write, Operation::Execute],
        command_allowlist: Vec::new(),
        environment_names: Vec::new(),
        model_route: None,
        budget: Budget::default(),
        delivery: relay_domain::DeliveryAuthority::None,
    }
}

// ---------------------------------------------------------------------------
// Stale-revision double-finish (materially-precise rule)
// ---------------------------------------------------------------------------

fn stale_revision_fixture() -> InMemoryFactSink {
    let artifact = ArtifactId::parse("art_brief").unwrap();
    let mut work = model_work("work_stale");
    work.inputs = vec![InputRef::Exact(ArtifactVersionRef::new(
        artifact.clone(),
        relay_domain::ArtifactVersion::FIRST,
    ))];
    let sink = InMemoryFactSink::seeded(vec![
        DomainFact::ProjectCreated {
            project: relay_domain::ProjectId::parse("prj_stale").unwrap(),
        },
        DomainFact::GoalDeclared {
            goal: relay_domain::Goal {
                id: relay_domain::GoalId::parse("goal_stale").unwrap(),
                outcome: "test staleness".into(),
                constraints: Vec::new(),
                success_definition: "suites pass".into(),
                provenance: relay_domain::Provenance {
                    source: relay_domain::ProvenanceSource::User,
                    statement: None,
                    at_ms: 0,
                },
                declared_at_ms: 0,
            },
        },
        DomainFact::ArtifactVersionPublished {
            artifact: artifact.clone(),
            version: relay_domain::ArtifactVersion::FIRST,
            title: None,
            content: ArtifactContent::new(json!({ "v": 1 })),
            digest: ArtifactDigest::parse("a1b2c3d4e5f60718").unwrap(),
            derived_from: Vec::new(),
            published_at_ms: 0,
        },
        DomainFact::WorkDeclared { work },
    ])
    .expect("seed");
    // The attempt starts against the revision after its work was declared.
    let revision = sink.graph().expect("graph").revision();
    sink.append(DomainFact::AttemptStarted {
        attempt: AttemptId::parse("att_stale").unwrap(),
        work: WorkItemId::parse("work_stale").unwrap(),
        expected_revision: revision,
        grant: read_grant("grant_stale"),
        started_at_ms: 1_000,
    })
    .expect("start");
    // A material change lands: a new version of the exactly pinned input.
    sink.append(DomainFact::ArtifactVersionPublished {
        artifact,
        version: relay_domain::ArtifactVersion::new(2).unwrap(),
        title: None,
        content: ArtifactContent::new(json!({ "v": 2 })),
        digest: ArtifactDigest::parse("b2c3d4e5f60718a9").unwrap(),
        derived_from: Vec::new(),
        published_at_ms: 2_000,
    })
    .expect("publish");
    sink
}

fn stale_receipt() -> AttemptReceipt {
    AttemptReceipt {
        attempt: AttemptId::parse("att_stale").unwrap(),
        work: WorkItemId::parse("work_stale").unwrap(),
        outcome: AttemptOutcome::Completed,
        completed_at_ms: 3_000,
        outputs: Vec::new(),
        evidence: vec![Evidence {
            id: EvidenceId::parse("ev_stale").unwrap(),
            kind: EvidenceKind::Inspection,
            target: EvidenceTarget::WorkItem(WorkItemId::parse("work_stale").unwrap()),
            outcome: EvidenceOutcome::Info,
            summary: "finished, but against stale inputs".to_owned(),
            payload: json!({}),
            recorded_at_ms: 3_000,
        }],
        graph_changes: Vec::new(),
        usage: relay_domain::Usage {
            duration_ms: 2_000,
            ..relay_domain::Usage::default()
        },
    }
}

#[test]
fn a_stale_receipt_is_rejected_and_concluded_failed_never_completed() {
    let sink = Arc::new(stale_revision_fixture());
    let graph = sink.graph().expect("graph");
    let rejection = relay_engine::accept_receipt(&graph, &stale_receipt())
        .expect_err("stale receipt must be rejected");
    match &rejection {
        relay_engine::Rejection::StaleRevision { changes, .. } => {
            assert!(
                changes.iter().any(|change| change.contains("art_brief")),
                "the exactly pinned input's change is material: {changes:?}"
            );
        }
        other => panic!("expected staleness, got {other:?}"),
    }
    // The orchestrator's response: a Failed conclusion, never completion.
    sink.append(DomainFact::AttemptConcluded {
        attempt: AttemptId::parse("att_stale").unwrap(),
        conclusion: AttemptConclusion::Failed {
            reason: rejection.to_string(),
        },
    })
    .expect("conclude");
    let graph = sink.graph().expect("graph");
    assert!(!relay_engine::status::work_is_completed(
        &graph,
        graph
            .work(&WorkItemId::parse("work_stale").unwrap())
            .unwrap()
    ));
    // An attempt can never conclude twice.
    assert!(matches!(
        sink.append(DomainFact::AttemptConcluded {
            attempt: AttemptId::parse("att_stale").unwrap(),
            conclusion: AttemptConclusion::Failed {
                reason: "second finish".to_owned(),
            },
        }),
        Err(relay_executor::FactSinkError::Rejected(_))
    ));
}

#[test]
fn unrelated_facts_are_not_material_for_staleness() {
    // Staleness is materially-precise in the other direction too: a fact
    // that does not touch the work's own node, its dependencies, or its
    // pinned inputs (here: publishing a brand-new unrelated artifact)
    // does not stale an attempt started before it.
    let sink = InMemoryFactSink::seeded(seed_facts(vec![model_work("work_fresh")])).expect("seed");
    let revision = sink.graph().expect("graph").revision();
    sink.append(DomainFact::AttemptStarted {
        attempt: AttemptId::parse("att_fresh").unwrap(),
        work: WorkItemId::parse("work_fresh").unwrap(),
        expected_revision: revision,
        grant: read_grant("grant_fresh"),
        started_at_ms: 2_500,
    })
    .expect("start");
    sink.append(DomainFact::ArtifactVersionPublished {
        artifact: ArtifactId::parse("art_other").unwrap(),
        version: relay_domain::ArtifactVersion::FIRST,
        title: None,
        content: ArtifactContent::new(json!({ "note": "unrelated" })),
        digest: ArtifactDigest::parse("c3d4e5f60718a9b0").unwrap(),
        derived_from: Vec::new(),
        published_at_ms: 2_600,
    })
    .expect("publish");
    let graph = sink.graph().expect("graph");
    assert_ne!(graph.revision(), revision, "the revision must have moved");
    let receipt = AttemptReceipt {
        attempt: AttemptId::parse("att_fresh").unwrap(),
        work: WorkItemId::parse("work_fresh").unwrap(),
        outcome: AttemptOutcome::Completed,
        completed_at_ms: 3_000,
        outputs: Vec::new(),
        evidence: vec![Evidence {
            id: EvidenceId::parse("ev_fresh").unwrap(),
            kind: EvidenceKind::Inspection,
            target: EvidenceTarget::WorkItem(WorkItemId::parse("work_fresh").unwrap()),
            outcome: EvidenceOutcome::Info,
            summary: "finished".to_owned(),
            payload: json!({}),
            recorded_at_ms: 3_000,
        }],
        graph_changes: Vec::new(),
        usage: relay_domain::Usage::default(),
    };
    let acceptance = relay_engine::accept_receipt(&graph, &receipt);
    assert!(acceptance.is_ok(), "{acceptance:?}");
    for fact in acceptance.unwrap().facts {
        sink.append(fact).expect("append");
    }
    let graph = sink.graph().expect("graph");
    assert!(relay_engine::status::work_is_completed(
        &graph,
        graph
            .work(&WorkItemId::parse("work_fresh").unwrap())
            .unwrap()
    ));
}

// ---------------------------------------------------------------------------
// Abandoned-attempt reconciliation (crash between start and conclusion)
// ---------------------------------------------------------------------------

#[test]
fn abandoned_attempts_reconcile_as_abandoned_never_success() {
    let sink = Arc::new(
        InMemoryFactSink::seeded(seed_facts(vec![writer_work("work_abandoned", "src")]))
            .expect("seed"),
    );
    let revision = sink.graph().expect("graph").revision();
    // Crash between start and conclusion: the attempt never finished.
    sink.append(DomainFact::AttemptStarted {
        attempt: AttemptId::parse("att_crashed").unwrap(),
        work: WorkItemId::parse("work_abandoned").unwrap(),
        expected_revision: revision,
        grant: write_grant("grant_crashed", "src"),
        started_at_ms: 1_000,
    })
    .expect("start");

    let orchestrator =
        Orchestrator::new(Arc::clone(&sink) as Arc<_>, relay_executor::system_clock());
    let reports = orchestrator.reconcile_abandoned().expect("reconcile");
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].attempt.as_str(), "att_crashed");
    assert!(
        reports[0].had_write_authority,
        "write-authority abandons must be surfaced for a human decision"
    );

    let graph = sink.graph().expect("graph");
    let attempt = graph
        .attempt(&AttemptId::parse("att_crashed").unwrap())
        .unwrap();
    assert!(matches!(
        attempt.conclusion.as_ref(),
        Some(AttemptConclusion::Abandoned { .. })
    ));
    assert!(!relay_engine::status::work_is_completed(
        &graph,
        graph
            .work(&WorkItemId::parse("work_abandoned").unwrap())
            .unwrap()
    ));

    // Never auto-revert: the app surfaces a human decision instead, and
    // that proposal validates through the engine.
    let decision = relay_domain::GraphChangeProposal::OpenDecision {
        decision: relay_domain::Decision {
            id: relay_domain::DecisionId::parse("dec_dirty").unwrap(),
            subject: relay_domain::DecisionSubject::WorkItem(
                WorkItemId::parse("work_abandoned").unwrap(),
            ),
            question: "Attempt att_crashed held write authority when the process died. \
                       Keep its partial workspace changes or reset them?"
                .to_owned(),
            options: vec![
                relay_domain::DecisionOption {
                    label: "keep".to_owned(),
                    description: "Keep the partial changes and continue".to_owned(),
                },
                relay_domain::DecisionOption {
                    label: "reset".to_owned(),
                    description: "Discard the partial changes".to_owned(),
                },
            ],
            recommendation: None,
            evidence: Vec::new(),
            consequences: "Decides whether unverified partial writes survive".to_owned(),
            authority: relay_domain::Authority::Human,
            opened_at_ms: 2_000,
        },
    };
    assert!(relay_engine::validate_graph_change(&graph, &decision).is_ok());
}

// ---------------------------------------------------------------------------
// Provider swap: different bytes, identical policy outcomes
// ---------------------------------------------------------------------------

fn run_with_provider(provider: FakeProvider) -> (Vec<&'static str>, usize, String, bool) {
    let sink = Arc::new(
        InMemoryFactSink::seeded(seed_facts(vec![model_work("work_swap")])).expect("seed"),
    );
    let orchestrator = Arc::new(
        Orchestrator::new(Arc::clone(&sink) as Arc<_>, relay_executor::system_clock())
            .with_executors(ExecutorSet::new().with(Arc::new(relay_executor::ModelExecutor)))
            .with_grants(model_grants())
            .with_model(Arc::new(ProviderModelClient::new(Arc::new(provider)))),
    );
    let report = orchestrator.tick().expect("tick");
    assert_eq!(report.accepted.len(), 1, "{report:?}");
    let graph = sink.graph().expect("graph");
    let work = WorkItemId::parse("work_swap").unwrap();
    let completed = relay_engine::status::work_is_completed(&graph, graph.work(&work).unwrap());
    let evidence: Vec<&Evidence> = graph.evidence().map(|(_, evidence)| evidence).collect();
    let text = evidence
        .first()
        .map(|evidence| {
            // The model executor records each turn's text in the payload.
            evidence.payload["turns"]
                .as_array()
                .and_then(|turns| turns.first())
                .and_then(|turn| turn.as_str())
                .unwrap_or_default()
                .to_owned()
        })
        .unwrap_or_default();
    let kinds = sink
        .facts()
        .expect("facts")
        .iter()
        .map(|fact| fact.kind())
        .collect();
    (kinds, evidence.len(), text, completed)
}

#[test]
fn swapping_providers_changes_bytes_not_outcomes() {
    let (kinds_a, count_a, text_a, completed_a) = run_with_provider(FakeProvider::with_fallback(
        Vec::new(),
        FakeResponse {
            text: Some(
                r#"{"evidence":[{"kind":"inspection","target":{"work_item":"work_swap"},"outcome":"info","summary":"provider A answered"}]}"#
                    .to_owned(),
            ),
            ..FakeResponse::default()
        },
    ));
    let (kinds_b, count_b, text_b, completed_b) = run_with_provider(FakeProvider::with_fallback(
        Vec::new(),
        FakeResponse {
            text: Some(
                r#"{"evidence":[{"kind":"inspection","target":{"work_item":"work_swap"},"outcome":"info","summary":"provider B answered differently"}]}"#
                    .to_owned(),
            ),
            ..FakeResponse::default()
        },
    ));
    assert_ne!(text_a, text_b, "providers must produce different bytes");
    assert_eq!(
        kinds_a, kinds_b,
        "accepted fact sequences must be identical"
    );
    assert_eq!(count_a, count_b);
    // Turn accounting evidence plus the model-proposed evidence entry.
    assert_eq!(count_a, 2);
    assert!(
        completed_a && completed_b,
        "policy outcomes must be identical"
    );
}

/// Autonomy bounds hold scheduling back without changing semantics.
#[test]
fn manual_autonomy_defers_everything() {
    let sink = Arc::new(
        InMemoryFactSink::seeded(seed_facts(vec![model_work("work_manual")])).expect("seed"),
    );
    let orchestrator = Arc::new(
        Orchestrator::new(Arc::clone(&sink) as Arc<_>, relay_executor::system_clock())
            .with_executors(ExecutorSet::new().with(Arc::new(relay_executor::ModelExecutor)))
            .with_grants(model_grants())
            .with_policy(SchedulingPolicy {
                autonomy: Autonomy::Manual,
                max_concurrent_writers: 1,
            }),
    );
    let report = orchestrator.tick().expect("tick");
    assert!(report.started.is_empty(), "{report:?}");
    // Manual autonomy defers work in the engine's recommendation; nothing
    // reaches the orchestrator's refusals because nothing was scheduled.
    let recommendation = recommend(
        &sink.graph().expect("graph"),
        &SchedulingPolicy {
            autonomy: Autonomy::Manual,
            max_concurrent_writers: 1,
        },
    );
    assert!(recommendation.parallel.is_empty());
    assert!(recommendation.serialized.is_empty());
    assert!(
        recommendation
            .deferred
            .iter()
            .any(|deferred| deferred.reason.contains("manual")),
        "{recommendation:?}"
    );
}

// ---------------------------------------------------------------------------
// Model-guided planning end to end (Phase 5, wave 3)
// ---------------------------------------------------------------------------

/// A model client that plays a two-turn native workspace session: first a
/// native tool-use block (the gated call), then the structured final
/// object as visible text. Records every request it served so tests can
/// assert on the transcript the executor actually sent.
struct GuidedScriptModel {
    turns: Mutex<std::collections::VecDeque<ModelTurnResponse>>,
    requests: Mutex<Vec<ModelTurnRequest>>,
}

impl GuidedScriptModel {
    /// The requests served so far, oldest first.
    fn served_requests(&self) -> Vec<ModelTurnRequest> {
        self.requests
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }
}

impl ModelClient for GuidedScriptModel {
    fn complete(
        &self,
        _route: &ModelRoute,
        request: &ModelTurnRequest,
        _signal: &relay_providers::contract::CancelToken,
    ) -> Result<ModelTurnResponse, ModelClientError> {
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

#[test]
fn a_model_guided_workspace_attempt_publishes_through_acceptance() {
    let workspace = tempfile::Builder::new()
        .prefix("relay-sched-guided-")
        .tempdir()
        .expect("temp workspace")
        .keep();
    let sink = Arc::new(
        InMemoryFactSink::seeded(seed_facts(vec![writer_work("work_guided", "src")]))
            .expect("seed"),
    );
    let model = GuidedScriptModel {
        turns: Mutex::new(
            vec![
                // Turn 1: one native write-file call.
                ModelTurnResponse {
                    blocks: vec![relay_providers::contract::AssistantContent::ToolCall {
                        call: relay_providers::contract::ToolCall {
                            id: "toolu_deliver".to_owned(),
                            name: "write-file".to_owned(),
                            arguments: json!({
                                "path": "src/delivered.rs",
                                "content": "pub const DELIVERED: bool = true;\n",
                            }),
                        },
                    }],
                    input_tokens: 2,
                    output_tokens: 3,
                    routes_attempted: Vec::new(),
                },
                // Turn 2: the structured final object.
                ModelTurnResponse {
                    input_tokens: 2,
                    output_tokens: 3,
                    ..ModelTurnResponse::from_text(
                        r#"{"outputs":[{"artifact":"art_delivered","title":"Delivered","content":{"file":"src/delivered.rs"}}],"evidence":[],"graph_changes":[]}"#,
                    )
                },
            ]
            .into(),
        ),
        requests: Mutex::new(Vec::new()),
    };
    let orchestrator =
        Arc::new(
            Orchestrator::new(Arc::clone(&sink) as Arc<_>, relay_executor::system_clock())
                .with_executors(ExecutorSet::new().with(Arc::new(WorkspaceExecutor::new(
                    Arc::new(relay_executor::ModelGuidedWorkspacePlanner::new()),
                ))))
                .with_grants(Arc::new(|work: &relay_domain::WorkNode| {
                    grant_from_work(
                        GrantId::parse("grant_guided").unwrap(),
                        work,
                        Some(route()),
                        Budget::default(),
                    )
                }))
                .with_model(Arc::new(model))
                .with_workspace_root(workspace.clone()),
        );
    let report = orchestrator.tick().expect("tick");
    assert_eq!(report.accepted.len(), 1, "{report:?}");

    // The gated tool call really ran inside the workspace.
    assert!(workspace.join("src/delivered.rs").exists());

    // The extracted output flowed through acceptance: the engine assigned
    // the artifact's first version and the work completed.
    let graph = sink.graph().expect("graph");
    let artifact = graph
        .artifact(&ArtifactId::parse("art_delivered").unwrap())
        .expect("artifact published");
    assert_eq!(artifact.current_version().get(), 1);
    let version = artifact
        .versions
        .get(&relay_domain::ArtifactVersion::FIRST)
        .expect("version record");
    assert_eq!(version.digest.as_str().len(), 64);
    assert!(relay_engine::status::work_is_completed(
        &graph,
        graph
            .work(&WorkItemId::parse("work_guided").unwrap())
            .unwrap()
    ));
}

// ---------------------------------------------------------------------------
// Routing seam (interface owner, Phase 4/5): the granted route reaches the
// model client verbatim — the connection name the grant carries is the
// name the client resolves, with no rewriting between grant and call.
// ---------------------------------------------------------------------------

/// A model client that records the route of every call it served.
struct RouteRecordingModel {
    routes: Mutex<Vec<ModelRoute>>,
}

impl ModelClient for RouteRecordingModel {
    fn complete(
        &self,
        route: &ModelRoute,
        _request: &ModelTurnRequest,
        _signal: &relay_providers::contract::CancelToken,
    ) -> Result<ModelTurnResponse, ModelClientError> {
        self.routes
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .push(route.clone());
        Ok(ModelTurnResponse {
            input_tokens: 1,
            output_tokens: 1,
            routes_attempted: vec![route.clone()],
            ..ModelTurnResponse::from_text("{}")
        })
    }
}

#[test]
fn the_granted_routes_connection_name_reaches_the_model_client_verbatim() {
    let sink = Arc::new(
        InMemoryFactSink::seeded(seed_facts(vec![model_work("work_route")])).expect("seed"),
    );
    // A user-chosen connection name with a role, exactly as the
    // application's purpose-resolved grants carry them.
    let granted = ModelRoute {
        provider: "glm".to_owned(),
        model: "glm-4.6".to_owned(),
        role: Some("coding".to_owned()),
    };
    let ids = Arc::new(relay_executor::IdSource::new());
    let route = granted.clone();
    let grants: Arc<dyn Fn(&relay_domain::WorkNode) -> CapabilityGrant + Send + Sync> =
        Arc::new(move |work| {
            grant_from_work(
                ids.next_grant_id(),
                work,
                Some(route.clone()),
                Budget::default(),
            )
        });
    let model = Arc::new(RouteRecordingModel {
        routes: Mutex::new(Vec::new()),
    });
    let orchestrator = Arc::new(
        Orchestrator::new(Arc::clone(&sink) as Arc<_>, relay_executor::system_clock())
            .with_executors(ExecutorSet::new().with(Arc::new(relay_executor::ModelExecutor)))
            .with_grants(grants)
            .with_model(model.clone() as Arc<_>)
            .with_spawn(relay_executor::inline_spawn()),
    );
    let report = orchestrator.tick().expect("tick");
    assert_eq!(report.accepted.len(), 1, "{report:?}");

    // The client saw the grant's route verbatim, and the started attempt's
    // durable grant carries the same connection name.
    let seen = model
        .routes
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .clone();
    assert_eq!(seen, vec![granted.clone()]);
    let graph = sink.graph().expect("graph");
    for (_, attempt) in graph.attempts() {
        assert_eq!(attempt.grant.model_route.as_ref(), Some(&granted));
    }
}

// ---------------------------------------------------------------------------
// Provider-stream hygiene through the executor model client (integration
// owner, native tool-loop plan Phase 7): failures surface as typed,
// bounded client errors; a stream that ends without a terminal event is
// never an implicit success; cancellation between stream events fails the
// turn instead of falling back to one.
// ---------------------------------------------------------------------------

/// A provider double that replays one scripted event list as its stream.
struct StreamScriptProvider {
    events: Vec<relay_providers::contract::ProviderStreamEvent>,
}

impl StreamScriptProvider {
    /// A well-formed text turn: one text delta, then the terminal event.
    fn completing_text(text: &str) -> Self {
        use relay_providers::contract::{FinishReason, ProviderStreamEvent, TokenUsage};
        StreamScriptProvider {
            events: vec![
                ProviderStreamEvent::TextDelta {
                    text: text.to_owned(),
                },
                ProviderStreamEvent::Completed {
                    finish_reason: FinishReason::Stop,
                    usage: TokenUsage {
                        input_tokens: 1,
                        output_tokens: 1,
                    },
                },
            ],
        }
    }
}

impl relay_providers::contract::ModelProvider for StreamScriptProvider {
    fn name(&self) -> relay_providers::contract::AdapterKind {
        relay_providers::contract::AdapterKind::Fake
    }

    fn models(&self) -> Vec<String> {
        vec!["fake-standard".to_owned()]
    }

    fn stream(
        &self,
        _request: &relay_providers::contract::ModelRequest,
    ) -> relay_providers::contract::ProviderEventStream<'_> {
        Box::new(self.events.clone().into_iter())
    }
}

fn fake_route() -> ModelRoute {
    ModelRoute {
        provider: "fake".to_owned(),
        model: "fake-standard".to_owned(),
        role: None,
    }
}

#[test]
fn provider_stream_error_events_fail_the_turn_with_typed_bounded_diagnostics() {
    use relay_providers::contract::ProviderErrorKind;

    let provider = Arc::new(FakeProvider::with_fallback(
        Vec::new(),
        FakeResponse {
            error: Some(relay_providers::fake::FakeErrorPlan {
                kind: ProviderErrorKind::RateLimit,
                retryable: true,
                message: "throttled by the upstream endpoint".to_owned(),
                fail_times: None,
            }),
            ..FakeResponse::default()
        },
    ));
    let client = ProviderModelClient::new(provider);
    let route = ModelRoute {
        provider: "upstream".to_owned(),
        model: "m1".to_owned(),
        role: None,
    };
    let error = client
        .complete(
            &route,
            &ModelTurnRequest::from_prompt("x"),
            &relay_providers::contract::CancelToken::new(),
        )
        .expect_err("error events fail the turn");
    let ModelClientError::Provider(failure) = &error else {
        panic!("expected a provider failure, got {error}");
    };
    let attempt = failure.terminal().expect("one attempt");
    assert_eq!(attempt.kind, ProviderErrorKind::RateLimit);
    assert_eq!(attempt.route, route);
    assert_eq!(attempt.message, "throttled by the upstream endpoint");
    // Nothing credential-shaped rides the failure diagnostics.
    for shape in ["sk-", "sk-ant-", "Bearer "] {
        assert!(!error.to_string().contains(shape), "{error}");
    }

    // Defense in depth: an oversized provider message is bounded before it
    // can reach executor diagnostics.
    let loud = Arc::new(FakeProvider::with_fallback(
        Vec::new(),
        FakeResponse {
            error: Some(relay_providers::fake::FakeErrorPlan {
                kind: ProviderErrorKind::Server,
                retryable: false,
                message: "x".repeat(5_000),
                fail_times: None,
            }),
            ..FakeResponse::default()
        },
    ));
    let client = ProviderModelClient::new(loud);
    let error = client
        .complete(
            &fake_route(),
            &ModelTurnRequest::from_prompt("x"),
            &relay_providers::contract::CancelToken::new(),
        )
        .expect_err("fails");
    let ModelClientError::Provider(failure) = &error else {
        panic!("expected a provider failure, got {error}");
    };
    assert!(
        failure
            .terminal()
            .expect("one attempt")
            .message
            .chars()
            .count()
            <= 300,
        "failure messages must be bounded, got {} chars",
        failure.terminal().unwrap().message.chars().count()
    );
}

#[test]
fn a_stream_without_a_terminal_event_fails_closed_through_the_model_client() {
    use relay_providers::contract::{ProviderErrorKind, ProviderStreamEvent};

    // A text delta then silence: no Completed, no Error. The fold must
    // reject it as an invalid response, never an implicit success.
    let client = ProviderModelClient::new(Arc::new(StreamScriptProvider {
        events: vec![ProviderStreamEvent::TextDelta {
            text: "partial".to_owned(),
        }],
    }));
    let error = client
        .complete(
            &fake_route(),
            &ModelTurnRequest::from_prompt("x"),
            &relay_providers::contract::CancelToken::new(),
        )
        .expect_err("missing terminal event fails");
    let ModelClientError::Provider(failure) = &error else {
        panic!("expected a provider failure, got {error}");
    };
    let attempt = failure.terminal().expect("one attempt");
    assert_eq!(attempt.kind, ProviderErrorKind::InvalidResponse);
    assert!(attempt.message.contains("terminal"), "{}", attempt.message);

    // An entirely empty stream is equally invalid.
    let client = ProviderModelClient::new(Arc::new(StreamScriptProvider { events: Vec::new() }));
    let error = client
        .complete(
            &fake_route(),
            &ModelTurnRequest::from_prompt("x"),
            &relay_providers::contract::CancelToken::new(),
        )
        .expect_err("empty stream fails");
    assert!(
        matches!(
            &error,
            ModelClientError::Provider(failure)
                if failure.terminal().unwrap().kind == ProviderErrorKind::InvalidResponse
        ),
        "{error}"
    );
}

/// A provider that cancels a shared token between its first and second
/// stream events, then emits the normalized canceled error a real adapter
/// emits when it observes cancellation mid-stream.
struct MidStreamCancelProvider {
    signal: relay_providers::contract::CancelToken,
}

impl relay_providers::contract::ModelProvider for MidStreamCancelProvider {
    fn name(&self) -> relay_providers::contract::AdapterKind {
        relay_providers::contract::AdapterKind::Fake
    }

    fn models(&self) -> Vec<String> {
        vec!["fake-standard".to_owned()]
    }

    fn stream(
        &self,
        _request: &relay_providers::contract::ModelRequest,
    ) -> relay_providers::contract::ProviderEventStream<'_> {
        use relay_providers::contract::{ProviderErrorKind, ProviderFailure, ProviderStreamEvent};
        let signal = self.signal.clone();
        Box::new(
            vec![ProviderStreamEvent::TextDelta {
                text: "partial".to_owned(),
            }]
            .into_iter()
            .chain(std::iter::once_with(move || {
                signal.cancel("aborted between stream events");
                ProviderStreamEvent::Error {
                    error: ProviderFailure::new(
                        ProviderErrorKind::Canceled,
                        "request aborted between stream events",
                    ),
                }
            })),
        )
    }
}

#[test]
fn cancellation_between_stream_events_fails_the_turn_and_never_succeeds() {
    use relay_providers::contract::{CancelToken, ProviderErrorKind};

    let signal = CancelToken::new();
    let client = ProviderModelClient::new(Arc::new(MidStreamCancelProvider {
        signal: signal.clone(),
    }));
    let error = client
        .complete(&fake_route(), &ModelTurnRequest::from_prompt("x"), &signal)
        .expect_err("mid-stream cancellation must fail the turn");
    match &error {
        ModelClientError::Provider(failure) => {
            assert_eq!(
                failure.terminal().expect("one attempt").kind,
                ProviderErrorKind::Canceled,
                "{error}"
            );
        }
        other => panic!("expected a provider failure carrying the cancel, got {other}"),
    }
    assert!(
        signal.is_cancelled(),
        "the shared token reflects the mid-stream abort"
    );

    // A token already canceled before the call never yields a turn either,
    // even from a provider whose stream completes normally.
    let presignaled = CancelToken::new();
    presignaled.cancel("user pressed ctrl-c");
    let client =
        ProviderModelClient::new(Arc::new(StreamScriptProvider::completing_text("too late")));
    let error = client
        .complete(
            &fake_route(),
            &ModelTurnRequest::from_prompt("x"),
            &presignaled,
        )
        .expect_err("a canceled call must not succeed");
    assert!(matches!(error, ModelClientError::Canceled(_)), "{error}");
}

// ---------------------------------------------------------------------------
// No project writer lock across provider calls (integration owner): the
// storage writer lock is held per fact append only. While a model call is
// in flight inside an attempt, the lock must be acquirable.
// ---------------------------------------------------------------------------

enum LockProbe {
    NotYetProbed,
    AcquiredFreely,
    TimedOut(String),
}

/// A model client that probes the store's writer lock from inside the
/// provider call: if any layer held the lock across executor work, the
/// bounded acquisition would time out and this double fails the attempt.
struct LockProbingModel {
    lock_path: std::path::PathBuf,
    recovery_dir: std::path::PathBuf,
    probe: Mutex<LockProbe>,
}

impl ModelClient for LockProbingModel {
    fn complete(
        &self,
        _route: &ModelRoute,
        _request: &ModelTurnRequest,
        _signal: &relay_providers::contract::CancelToken,
    ) -> Result<ModelTurnResponse, ModelClientError> {
        let clock = relay_store::FixedClock::new(common::CLOCK_MS);
        let probe = relay_store::lock::acquire(
            &self.lock_path,
            &self.recovery_dir,
            Duration::from_millis(300),
            &clock,
        );
        let mut slot = self
            .probe
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        match probe {
            Ok(_guard) => {
                *slot = LockProbe::AcquiredFreely;
                Ok(ModelTurnResponse {
                    input_tokens: 1,
                    output_tokens: 1,
                    ..ModelTurnResponse::from_text(
                        r#"{"evidence":[{"kind":"inspection","target":{"work_item":"work_lock"},"outcome":"info","summary":"lock was free during the call"}]}"#,
                    )
                })
            }
            Err(error) => {
                *slot = LockProbe::TimedOut(error.to_string());
                Err(ModelClientError::Provider(ModelCallFailure::from_message(
                    error.to_string(),
                )))
            }
        }
    }
}

#[test]
fn no_project_writer_lock_is_held_across_a_model_call() {
    let (dir, store) = common::graph_store().expect("store");
    store
        .append(&common::seed_history(&"prj_crash".parse().expect("project id")).expect("seed"))
        .expect("seed history");
    store
        .append(&[DomainFact::WorkDeclared {
            work: model_work("work_lock"),
        }])
        .expect("declare work");
    let paths = relay_store::FactPaths::new(dir.path());
    let model = Arc::new(LockProbingModel {
        lock_path: paths.writer_lock(),
        recovery_dir: paths.recovery_dir.clone(),
        probe: Mutex::new(LockProbe::NotYetProbed),
    });
    let sink = Arc::new(common::StoreFactSink::new(Arc::new(store)));
    let orchestrator = Arc::new(
        Orchestrator::new(Arc::clone(&sink) as Arc<_>, relay_executor::system_clock())
            .with_executors(ExecutorSet::new().with(Arc::new(relay_executor::ModelExecutor)))
            .with_grants(model_grants())
            .with_model(Arc::clone(&model) as Arc<_>),
    );

    // The attempt runs a full model turn over the durable store and the
    // probe inside the provider call found the writer lock free.
    let report = orchestrator.tick().expect("tick");
    assert_eq!(report.started.len(), 1, "{report:?}");
    assert_eq!(report.accepted.len(), 1, "{report:?}");
    let probe = model
        .probe
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    match &*probe {
        LockProbe::AcquiredFreely => {}
        LockProbe::TimedOut(reason) => {
            panic!("the writer lock must not be held across a model call: {reason}")
        }
        LockProbe::NotYetProbed => panic!("the model call never ran"),
    }
}

// ---------------------------------------------------------------------------
// Offline default (integration owner): the shipped configuration resolves
// every routing purpose to the credential-less fake and completes turns
// with no keys and no network.
// ---------------------------------------------------------------------------

#[test]
fn the_offline_default_serves_every_purpose_without_credentials_or_network() {
    use relay_executor::routed_model::{ProviderRegistry, ResolvedRoutePlan, RoutedModelClient};

    let config = relay_config::default_config();
    // The offline connection carries no credential variable at all.
    let fake_connection = config
        .providers
        .0
        .get("fake")
        .expect("the default profile defines the fake connection");
    assert_eq!(
        fake_connection.api_key_env, None,
        "the offline default must not need any credential"
    );

    let registry = ProviderRegistry::from_effective_config(&config);
    assert!(registry.ready("fake").is_some());
    let plan = ResolvedRoutePlan::from_normalized(
        &relay_config::normalize_routing(&config),
        &Default::default(),
    );
    for purpose in [
        "intent",
        "discovery",
        "specification",
        "technical-design",
        "planning",
        "implementation",
        "verification",
        "delivery",
    ] {
        let primary = plan.primary(purpose).expect(purpose).route.clone();
        assert_eq!(primary.provider, "fake", "{purpose} must stay offline");
    }

    // The default chain completes a turn entirely offline: the fake serves
    // it, and no other connection is ever attempted (nothing falls back
    // toward a live connection, keys or not).
    let route = plan
        .primary("intent")
        .expect("intent resolves")
        .route
        .clone();
    let client = RoutedModelClient::new(registry, plan);
    let response = client
        .complete(
            &route,
            &ModelTurnRequest::from_prompt("ping"),
            &relay_providers::contract::CancelToken::new(),
        )
        .expect("the offline default completes turns");
    assert_eq!(response.text(), "ping");
    assert_eq!(response.routes_attempted, vec![route]);
}

// ---------------------------------------------------------------------------
// Native tool-loop semantics (Phase 7 second pass): multiple native calls
// keep their order, every result correlates with its call id, and native
// sessions do not weaken write serialization.
// ---------------------------------------------------------------------------

#[test]
fn native_calls_preserve_order_and_results_correlate_by_call_id() {
    let workspace = tempfile::Builder::new()
        .prefix("relay-sched-native-order-")
        .tempdir()
        .expect("temp workspace")
        .keep();
    let sink = Arc::new(
        InMemoryFactSink::seeded(seed_facts(vec![writer_work("work_order", "src")])).expect("seed"),
    );
    let model = Arc::new(GuidedScriptModel {
        turns: Mutex::new(
            vec![
                // Turn 1: text, call A, text, call B, call C — interleaved
                // on purpose. C reads the file A writes, so execution
                // order is observable through the results.
                ModelTurnResponse {
                    blocks: vec![
                        relay_providers::contract::AssistantContent::Text {
                            text: "checking ".to_owned(),
                        },
                        relay_providers::contract::AssistantContent::ToolCall {
                            call: relay_providers::contract::ToolCall {
                                id: "toolu_a".to_owned(),
                                name: "write-file".to_owned(),
                                arguments: json!({
                                    "path": "src/a.rs",
                                    "content": "pub fn a() {}\n",
                                }),
                            },
                        },
                        relay_providers::contract::AssistantContent::Text {
                            text: "then".to_owned(),
                        },
                        relay_providers::contract::AssistantContent::ToolCall {
                            call: relay_providers::contract::ToolCall {
                                id: "toolu_b".to_owned(),
                                name: "write-file".to_owned(),
                                arguments: json!({
                                    "path": "src/b.rs",
                                    "content": "pub fn b() {}\n",
                                }),
                            },
                        },
                        relay_providers::contract::AssistantContent::ToolCall {
                            call: relay_providers::contract::ToolCall {
                                id: "toolu_c".to_owned(),
                                name: "read-file".to_owned(),
                                arguments: json!({ "path": "src/a.rs" }),
                            },
                        },
                    ],
                    input_tokens: 2,
                    output_tokens: 3,
                    routes_attempted: Vec::new(),
                },
                // Turn 2: the structured final object.
                ModelTurnResponse {
                    input_tokens: 2,
                    output_tokens: 3,
                    ..ModelTurnResponse::from_text(
                        r#"{"outputs":[{"artifact":"art_ordered","title":"Ordered","content":{"note":"a, b, c in order"}}],"evidence":[],"graph_changes":[]}"#,
                    )
                },
            ]
            .into(),
        ),
        requests: Mutex::new(Vec::new()),
    });
    let orchestrator = Arc::new(
        Orchestrator::new(Arc::clone(&sink) as Arc<_>, relay_executor::system_clock())
            .with_executors(
                ExecutorSet::new().with(Arc::new(WorkspaceExecutor::new(Arc::new(
                    relay_executor::ModelGuidedWorkspacePlanner::new(),
                )))),
            )
            .with_grants(Arc::new(|work: &relay_domain::WorkNode| {
                grant_from_work(
                    GrantId::parse("grant_order").unwrap(),
                    work,
                    Some(route()),
                    Budget::default(),
                )
            }))
            .with_model(Arc::clone(&model) as Arc<_>)
            .with_tools(Arc::new(
                relay_tools::create_default_tool_registry(&relay_config::ToolSettings::default())
                    .expect("registry"),
            ))
            .with_workspace_root(workspace.clone()),
    );
    let report = orchestrator.tick().expect("tick");
    assert_eq!(report.accepted.len(), 1, "{report:?}");

    // Every call executed in arrival order: A's file exists (and C read
    // it back), B's file exists.
    let a_body = std::fs::read_to_string(workspace.join("src/a.rs")).expect("a.rs written");
    assert!(workspace.join("src/b.rs").exists());

    // The opening request carried grant-filtered native tool definitions,
    // separate from prose.
    let served = model.served_requests();
    assert_eq!(served.len(), 2);
    let opening_tools = served[0].tools.as_ref().expect("tools offered");
    let offered: Vec<&str> = opening_tools.iter().map(|t| t.name.as_str()).collect();
    assert!(offered.contains(&"write-file"), "{offered:?}");
    assert!(offered.contains(&"read-file"), "{offered:?}");

    // The follow-up transcript preserved the assistant turn verbatim —
    // block order intact — followed by one correlated tool result per
    // call, in arrival order.
    let followup = &served[1];
    let relay_providers::contract::ModelMessage::Assistant { content } = &followup.messages[1]
    else {
        panic!(
            "the assistant turn is replayed verbatim: {:?}",
            followup.messages
        );
    };
    let kinds: Vec<&str> = content
        .iter()
        .map(|block| match block {
            relay_providers::contract::AssistantContent::Text { .. } => "text",
            relay_providers::contract::AssistantContent::ToolCall { .. } => "tool-call",
            relay_providers::contract::AssistantContent::Continuation { .. } => "continuation",
        })
        .collect();
    assert_eq!(
        kinds,
        ["text", "tool-call", "text", "tool-call", "tool-call"]
    );

    let results: Vec<(String, String, bool)> = followup
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
        .collect();
    assert_eq!(
        results
            .iter()
            .map(|(id, _, _)| id.as_str())
            .collect::<Vec<_>>(),
        ["toolu_a", "toolu_b", "toolu_c"],
        "results correlate and keep arrival order: {results:?}"
    );
    assert!(results.iter().all(|(_, _, is_error)| !is_error));
    // C's result carries A's content: C executed after A in the same turn.
    assert!(
        results[2].1.contains(&a_body),
        "the read must observe the earlier write: {}",
        results[2].1
    );

    // And the final proposal published through acceptance.
    let graph = sink.graph().expect("graph");
    assert!(
        graph
            .artifact(&ArtifactId::parse("art_ordered").unwrap())
            .is_some()
    );
}

/// A shared model double for serialization: every session is one native
/// write call followed by a final object, and each model call records its
/// interval so overlapping native sessions would be visible.
struct NativeWriterModel {
    calls: std::sync::atomic::AtomicUsize,
    intervals: Mutex<Vec<(usize, Instant, Instant)>>,
}

impl ModelClient for NativeWriterModel {
    fn complete(
        &self,
        _route: &ModelRoute,
        _request: &ModelTurnRequest,
        _signal: &relay_providers::contract::CancelToken,
    ) -> Result<ModelTurnResponse, ModelClientError> {
        use std::sync::atomic::Ordering;
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let session = call / 2;
        let opening = call.is_multiple_of(2);
        let started = Instant::now();
        // A bounded nap widens the interval: overlap would be visible if
        // two native write sessions ever raced.
        std::thread::sleep(Duration::from_millis(60));
        self.intervals
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .push((session, started, Instant::now()));
        let response = if opening {
            ModelTurnResponse {
                blocks: vec![relay_providers::contract::AssistantContent::ToolCall {
                    call: relay_providers::contract::ToolCall {
                        id: format!("toolu_serial_{session}"),
                        name: "write-file".to_owned(),
                        arguments: json!({
                            "path": format!("src/serial-{session}.rs"),
                            "content": "pub fn s() {}\n",
                        }),
                    },
                }],
                input_tokens: 2,
                output_tokens: 3,
                routes_attempted: Vec::new(),
            }
        } else {
            ModelTurnResponse {
                input_tokens: 2,
                output_tokens: 3,
                ..ModelTurnResponse::from_text(format!(
                    r#"{{"outputs":[{{"artifact":"art_serial-{session}","title":"Serial","content":{{"note":"session {session}"}}}}],"evidence":[],"graph_changes":[]}}"#
                ))
            }
        };
        Ok(response)
    }
}

#[test]
fn native_write_sessions_remain_serialized() {
    let workspace = tempfile::Builder::new()
        .prefix("relay-sched-native-serial-")
        .tempdir()
        .expect("temp workspace")
        .keep();
    let sink = Arc::new(
        InMemoryFactSink::seeded(seed_facts(vec![
            writer_work("work_serial-a", "src"),
            writer_work("work_serial-b", "src"),
        ]))
        .expect("seed"),
    );
    let model = Arc::new(NativeWriterModel {
        calls: std::sync::atomic::AtomicUsize::new(0),
        intervals: Mutex::new(Vec::new()),
    });
    let orchestrator = Arc::new(
        Orchestrator::new(Arc::clone(&sink) as Arc<_>, relay_executor::system_clock())
            .with_executors(
                ExecutorSet::new()
                    .with(Arc::new(WorkspaceExecutor::new(Arc::new(
                        relay_executor::ModelGuidedWorkspacePlanner::new(),
                    ))))
                    .with(Arc::new(WorkspaceExecutor::new(Arc::new(
                        relay_executor::ModelGuidedWorkspacePlanner::new(),
                    )))),
            )
            .with_grants(Arc::new(|work: &relay_domain::WorkNode| {
                grant_from_work(
                    GrantId::parse("grant_native-serial").unwrap(),
                    work,
                    Some(route()),
                    Budget::default(),
                )
            }))
            .with_model(Arc::clone(&model) as Arc<_>)
            .with_tools(Arc::new(
                relay_tools::create_default_tool_registry(&relay_config::ToolSettings::default())
                    .expect("registry"),
            ))
            .with_workspace_root(workspace.clone())
            .with_spawn(relay_executor::thread_spawn()),
    );

    // The engine still defers the second overlapping writer.
    let recommendation = recommend(&sink.graph().expect("graph"), &SchedulingPolicy::default());
    assert_eq!(recommendation.parallel.len(), 0);
    assert!(
        recommendation
            .deferred
            .iter()
            .any(|deferred| deferred.reason.contains("serialize")),
        "{recommendation:?}"
    );

    // One native session per tick; both complete across two ticks.
    let first = orchestrator.tick().expect("tick");
    assert_eq!(first.started.len(), 1, "{first:?}");
    let second = orchestrator.tick().expect("tick");
    assert_eq!(second.started.len(), 1, "{second:?}");
    assert_eq!(first.accepted.len() + second.accepted.len(), 2);

    // Both writes landed and the model-call intervals never overlapped:
    // native sessions do not weaken write serialization.
    assert!(workspace.join("src/serial-0.rs").exists());
    assert!(workspace.join("src/serial-1.rs").exists());
    let intervals = model
        .intervals
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .clone();
    assert!(intervals.len() >= 4, "{intervals:?}");
    let mut sorted = intervals.clone();
    sorted.sort_by_key(|(_, start, _)| *start);
    for pair in sorted.windows(2) {
        let (_, _, earlier_end) = pair[0];
        let (_, later_start, _) = pair[1];
        assert!(
            earlier_end <= later_start,
            "native model sessions must never overlap: {sorted:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// No project writer lock across tool execution (Phase 7 item 9): a
// registered tool probes the storage writer lock from inside its own
// execution.
// ---------------------------------------------------------------------------

#[test]
fn no_project_writer_lock_is_held_during_tool_execution() {
    let (dir, store) = common::graph_store().expect("store");
    store
        .append(&common::seed_history(&"prj_crash".parse().expect("project id")).expect("seed"))
        .expect("seed history");
    store
        .append(&[DomainFact::WorkDeclared {
            work: writer_work("work_probe", "src"),
        }])
        .expect("declare work");

    // A custom tool whose body acquires the store's writer lock: if any
    // layer held the lock across tool execution, the bounded acquisition
    // would time out and the tool would fail.
    let paths = relay_store::FactPaths::new(dir.path());
    let lock_path = paths.writer_lock();
    let recovery_dir = paths.recovery_dir.clone();
    let probe_log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let tool_log = Arc::clone(&probe_log);
    let mut tools =
        relay_tools::create_default_tool_registry(&relay_config::ToolSettings::default())
            .expect("registry");
    tools
        .register(relay_tools::ToolDefinition {
            name: "probe-writer-lock".to_owned(),
            title: "Probe the storage writer lock".to_owned(),
            description: "Acquires the project writer lock to prove tools run unlocked".to_owned(),
            capability: relay_tools::ToolCapability::Write,
            args_schema: Arc::new(
                relay_tools::ToolArgsSchema::new(json!({ "type": "object" }), None)
                    .expect("schema"),
            ),
            timeout_ms: 5_000,
            max_output_chars: 1_000,
            execute: Arc::new(move |_args, _context| {
                let clock = relay_store::FixedClock::new(common::CLOCK_MS);
                match relay_store::lock::acquire(
                    &lock_path,
                    &recovery_dir,
                    Duration::from_millis(300),
                    &clock,
                ) {
                    Ok(_guard) => {
                        tool_log
                            .lock()
                            .unwrap_or_else(|poison| poison.into_inner())
                            .push("free".to_owned());
                        relay_tools::ToolResult::done(
                            "writer lock was free during tool execution",
                            Vec::new(),
                        )
                    }
                    Err(error) => {
                        tool_log
                            .lock()
                            .unwrap_or_else(|poison| poison.into_inner())
                            .push(format!("held: {error}"));
                        relay_tools::ToolResult::fail("the writer lock was held across tools")
                    }
                }
            }),
        })
        .expect("register probe tool");

    let sink = Arc::new(common::StoreFactSink::new(Arc::new(store)));
    let orchestrator = Arc::new(
        Orchestrator::new(Arc::clone(&sink) as Arc<_>, relay_executor::system_clock())
            .with_executors(
                ExecutorSet::new().with(Arc::new(WorkspaceExecutor::scripted(vec![
                    ToolCallRequest {
                        tool: "probe-writer-lock".to_owned(),
                        args: json!({}),
                    },
                ]))),
            )
            .with_grants(Arc::new(|work: &relay_domain::WorkNode| {
                grant_from_work(
                    GrantId::parse("grant_probe").unwrap(),
                    work,
                    None,
                    Budget::default(),
                )
            }))
            .with_tools(Arc::new(tools))
            .with_workspace_root(dir.path().to_path_buf()),
    );
    let report = orchestrator.tick().expect("tick");
    assert_eq!(report.accepted.len(), 1, "{report:?}");

    let log = probe_log
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .clone();
    assert_eq!(log, ["free".to_owned()], "the probe ran exactly once");
}

// ---------------------------------------------------------------------------
// Opaque continuation blocks (Phase 7 item 11): hidden provider state is
// replayed only inside the in-memory transcript and never reaches the
// durable surfaces — JSONL facts, SQLite projections, artifacts, or
// receipts.
// ---------------------------------------------------------------------------

#[test]
fn opaque_continuation_data_never_reaches_durable_surfaces() {
    let (dir, store) = common::graph_store().expect("store");
    store
        .append(&common::seed_history(&"prj_crash".parse().expect("project id")).expect("seed"))
        .expect("seed history");
    store
        .append(&[DomainFact::WorkDeclared {
            work: writer_work("work_opaque", "src"),
        }])
        .expect("declare work");

    let payload = "OPAQUE-CONTINUATION-PAYLOAD-7f3a9c";
    let discriminator = "redacted-thinking";
    let model = Arc::new(GuidedScriptModel {
        turns: Mutex::new(
            vec![
                // Turn 1: an opaque continuation block plus a native call.
                ModelTurnResponse {
                    blocks: vec![
                        relay_providers::contract::AssistantContent::Continuation {
                            discriminator: discriminator.to_owned(),
                            data: payload.to_owned(),
                        },
                        relay_providers::contract::AssistantContent::ToolCall {
                            call: relay_providers::contract::ToolCall {
                                id: "toolu_think".to_owned(),
                                name: "write-file".to_owned(),
                                arguments: json!({
                                    "path": "src/with-continuation.rs",
                                    "content": "pub const X: u8 = 1;\n",
                                }),
                            },
                        },
                    ],
                    input_tokens: 2,
                    output_tokens: 3,
                    routes_attempted: Vec::new(),
                },
                // Turn 2: the structured final object.
                ModelTurnResponse {
                    input_tokens: 2,
                    output_tokens: 3,
                    ..ModelTurnResponse::from_text(
                        r#"{"outputs":[{"artifact":"art_opaque","title":"Opaque-free","content":{"file":"src/with-continuation.rs"}}],"evidence":[],"graph_changes":[]}"#,
                    )
                },
            ]
            .into(),
        ),
        requests: Mutex::new(Vec::new()),
    });
    let sink = Arc::new(common::StoreFactSink::new(Arc::new(store)));
    let orchestrator = Arc::new(
        Orchestrator::new(Arc::clone(&sink) as Arc<_>, relay_executor::system_clock())
            .with_executors(
                ExecutorSet::new().with(Arc::new(WorkspaceExecutor::new(Arc::new(
                    relay_executor::ModelGuidedWorkspacePlanner::new(),
                )))),
            )
            .with_grants(Arc::new(|work: &relay_domain::WorkNode| {
                grant_from_work(
                    GrantId::parse("grant_opaque").unwrap(),
                    work,
                    Some(route()),
                    Budget::default(),
                )
            }))
            .with_model(Arc::clone(&model) as Arc<_>)
            .with_tools(Arc::new(
                relay_tools::create_default_tool_registry(&relay_config::ToolSettings::default())
                    .expect("registry"),
            ))
            .with_workspace_root(dir.path().to_path_buf()),
    );
    let report = orchestrator.tick().expect("tick");
    assert_eq!(report.accepted.len(), 1, "{report:?}");

    // The block was really used: the follow-up transcript the model saw
    // replayed the continuation verbatim (in-memory only).
    let served = model.served_requests();
    assert_eq!(served.len(), 2, "a two-turn session");
    let replayed = serde_json::to_string(&served[1].messages).expect("serializable");
    assert!(
        replayed.contains(payload),
        "the continuation must be replayed to the same session"
    );

    // The gated write and the artifact are durable...
    assert!(dir.path().join("src/with-continuation.rs").exists());
    let (reopened, _) =
        relay_store::GraphFactStore::open(dir.path(), common::options()).expect("reopen");
    let graph = reopened.project_graph().expect("graph");
    assert!(
        graph
            .artifact(&ArtifactId::parse("art_opaque").unwrap())
            .is_some()
    );

    // ...and the opaque state is absent from every durable surface: the
    // raw JSONL log, the folded facts, and the SQLite projection rows.
    let mut raw_log = String::new();
    let paths = relay_store::FactPaths::new(dir.path());
    for entry in std::fs::read_dir(&paths.facts_dir).expect("facts dir") {
        let path = entry.expect("entry").path();
        if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            raw_log.push_str(&std::fs::read_to_string(&path).unwrap_or_default());
        }
    }
    raw_log.push_str(&std::fs::read_to_string(&paths.manifest).unwrap_or_default());
    let facts_json = reopened
        .read_facts()
        .expect("facts")
        .iter()
        .map(|fact| serde_json::to_string(fact).expect("serializable"))
        .collect::<Vec<_>>()
        .join("\n");
    let attempts = reopened
        .projection(|conn| relay_store::attempts(conn, None, None))
        .expect("attempts");
    let works = reopened
        .projection(|conn| relay_store::works(conn, &Default::default()))
        .expect("works");
    let artifacts = reopened
        .projection(|conn| relay_store::artifact_versions(conn, None))
        .expect("artifacts");
    let evidence_rows = reopened
        .projection(|conn| relay_store::evidence(conn, None, None, None))
        .expect("evidence");
    let projection_dump = format!(
        "{:?}{:?}{:?}{:?}",
        attempts, works, artifacts, evidence_rows
    );
    for (label, surface) in [
        ("jsonl log", raw_log.as_str()),
        ("folded facts", facts_json.as_str()),
        ("sqlite projection", projection_dump.as_str()),
    ] {
        assert!(
            !surface.contains(payload),
            "opaque continuation data leaked into the {label}"
        );
        assert!(
            !surface.contains(discriminator),
            "the continuation discriminator leaked into the {label}"
        );
    }
}
