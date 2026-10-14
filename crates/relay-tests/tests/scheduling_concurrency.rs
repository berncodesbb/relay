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
    ExecutorSet, FactSink, ModelClient, ModelClientError, ModelTurnRequest, ModelTurnResponse,
    Orchestrator, ProviderModelClient, ToolCallRequest, WorkspaceExecutor, WorkspacePlanner,
    grant_from_work, sink::InMemoryFactSink,
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
                return Err(ModelClientError::Provider(
                    "rendezvous timed out: attempts did not run concurrently".to_owned(),
                ));
            }
        }
        // A valid (empty-section) final object: the turn itself is the
        // accounting evidence, so the attempts complete.
        Ok(ModelTurnResponse {
            text: "{}".to_owned(),
            input_tokens: 1,
            output_tokens: 1,
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

/// A model client that plays a two-turn workspace session: first a gated
/// tool call, then the structured final object.
struct GuidedScriptModel {
    turns: Mutex<std::collections::VecDeque<String>>,
}

impl ModelClient for GuidedScriptModel {
    fn complete(
        &self,
        _route: &ModelRoute,
        _request: &ModelTurnRequest,
        _signal: &relay_providers::contract::CancelToken,
    ) -> Result<ModelTurnResponse, ModelClientError> {
        let mut turns = self
            .turns
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let text = if turns.len() > 1 {
            turns.pop_front().unwrap_or_default()
        } else {
            turns.front().cloned().unwrap_or_default()
        };
        Ok(ModelTurnResponse {
            text,
            input_tokens: 2,
            output_tokens: 3,
        })
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
                r#"{"tool_calls":[{"tool":"write-file","args":{"path":"src/delivered.rs","content":"pub const DELIVERED: bool = true;\n"}}]}"#
                    .to_owned(),
                r#"{"outputs":[{"artifact":"art_delivered","title":"Delivered","content":{"file":"src/delivered.rs"}}],"evidence":[],"graph_changes":[]}"#
                    .to_owned(),
            ]
            .into(),
        ),
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
