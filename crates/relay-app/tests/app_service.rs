//! Application-service integration tests: session open/reopen, the store
//! fact sink, abandoned-attempt reconciliation, and the markdown view
//! publication — all offline and deterministic.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::ok_expect,
    clippy::err_expect
)] // test-only conveniences, house style

use std::sync::Arc;

use relay_app::RuntimeWiring;
use relay_app::commands as app;
use relay_app::session::{ProjectSession, SessionOptions};
use relay_app::sink::StoreFactSink;
use relay_domain::{
    ArtifactDigest, AttemptOutcome, AttemptReceipt, CapabilityGrant, DomainFact, ExecutorClass,
    GrantId, OutputProposal, ScopePath, Usage,
};
use relay_executor::spec::{AttemptSpec, RunContext};
use relay_executor::{
    Executor, ExecutorSet, FactSink, ModelClient, ModelClientError, ModelTurnRequest,
    ModelTurnResponse,
};
use relay_providers::contract::CancelToken;
use relay_workflows::StructuredDoc;

const EPOCH_MS: i64 = 1_791_878_400_000;

/// A model-class executor that publishes a brief for discovery work.
struct DiscoveryExecutor;

impl Executor for DiscoveryExecutor {
    fn class(&self) -> ExecutorClass {
        ExecutorClass::Model
    }

    fn run(&self, spec: &AttemptSpec, context: &RunContext) -> AttemptReceipt {
        let doc = relay_workflows::BriefDoc {
            problem: "reports are stuck".to_string(),
            audience: "analysts".to_string(),
            outcome: "one-click CSV export".to_string(),
            open_questions: Vec::new(),
        };
        AttemptReceipt {
            attempt: spec.attempt.clone(),
            work: spec.work.clone(),
            outcome: AttemptOutcome::Completed,
            completed_at_ms: context.now_ms,
            outputs: vec![OutputProposal {
                artifact: "art_brief".parse().unwrap(),
                title: Some("Opportunity brief".to_string()),
                content: doc.to_content(),
                digest: ArtifactDigest::parse(&format!("{:064x}", 0x00ff)).unwrap(),
            }],
            evidence: Vec::new(),
            graph_changes: Vec::new(),
            usage: Usage {
                input_tokens: 1,
                output_tokens: 1,
                tool_calls: 0,
                duration_ms: 1,
            },
        }
    }
}

struct EchoModel;

impl ModelClient for EchoModel {
    fn complete(
        &self,
        _route: &relay_domain::ModelRoute,
        request: &ModelTurnRequest,
        _signal: &CancelToken,
    ) -> Result<ModelTurnResponse, ModelClientError> {
        Ok(ModelTurnResponse {
            text: format!("echo: {}", request.prompt),
            input_tokens: 1,
            output_tokens: 1,
        })
    }
}

fn wiring() -> RuntimeWiring {
    RuntimeWiring::scripted(
        ExecutorSet::new().with(Arc::new(DiscoveryExecutor)),
        Arc::new(EchoModel),
    )
}

fn options() -> SessionOptions {
    SessionOptions::deterministic(EPOCH_MS)
}

#[test]
fn init_expand_run_round_trip_through_reopen() {
    let dir = tempfile::tempdir().unwrap();
    app::init(dir.path(), "export team reports as CSV", options()).unwrap();

    // Reopening yields the same derivation as the live session.
    let session = ProjectSession::open(dir.path(), options()).unwrap();
    assert_eq!(session.graph().works().count(), 2, "clarify + discover");
    let evaluation = session.evaluation();
    assert_eq!(evaluation.awaiting_human.len(), 1);
    assert_eq!(evaluation.blocked.len(), 1);

    // Resolve the clarification, run one scripted tick, verify acceptance.
    app::resolve_decision(&session, "dec_clarify-unknowns-1", "csv export", None).unwrap();
    let run = relay_app::runner::run_ticks(
        &session,
        &wiring(),
        relay_engine::SchedulingPolicy::default(),
        1,
    )
    .unwrap();
    assert_eq!(run.ticks.len(), 1, "{run:?}");
    assert_eq!(run.ticks[0].accepted.len(), 1, "{run:?}");
    // The accepted brief's Markdown view is auto-published (canonical blob
    // + digest convention), so the store needs no manual `--publish`.
    assert_eq!(run.published_views.len(), 1, "{run:?}");
    assert_eq!(run.published_views[0].artifact, "art_brief");
    assert!(dir.path().join(&run.published_views[0].path).is_file());
    assert!(run.view_notes.is_empty(), "{run:?}");
    // Doctor is clean right after the run — no missing view blobs.
    let doctor = app::doctor_check(dir.path(), &options().store);
    assert!(doctor.healthy, "{doctor:?}");

    // A fresh process sees exactly the same graph: the clarification (by
    // decision) and discovery (by receipt) are both completed.
    let reopened = ProjectSession::open(dir.path(), options()).unwrap();
    assert_eq!(reopened.evaluation().completed.len(), 2);
    assert_eq!(
        reopened.graph().revision().get(),
        session.graph().revision().get()
    );
    assert_eq!(
        reopened.store().revision().unwrap(),
        session.store().revision().unwrap()
    );
}

#[test]
fn store_fact_sink_appends_and_reads_the_timeline() {
    let dir = tempfile::tempdir().unwrap();
    app::init(dir.path(), "ship a tiny tool", options()).unwrap();
    let session = ProjectSession::open(dir.path(), options()).unwrap();
    let sink = StoreFactSink::new(session.store());
    let before = sink.facts().unwrap().len();
    // An invalid fact is rejected and nothing is appended.
    let duplicate = sink
        .facts()
        .unwrap()
        .first()
        .cloned()
        .map(|fact| DomainFact::ProjectCreated {
            project: match fact {
                DomainFact::ProjectCreated { project } => project,
                _ => "prj_x".parse().unwrap(),
            },
        })
        .unwrap();
    assert!(sink.append(duplicate).is_err());
    assert_eq!(sink.facts().unwrap().len(), before);
    // The folded graph reads through the sink.
    assert!(sink.graph().unwrap().project_id().is_some());
}

#[test]
fn opening_reconciles_abandoned_attempts_never_as_success() {
    let dir = tempfile::tempdir().unwrap();
    app::init(dir.path(), "export team reports as CSV", options()).unwrap();
    let session = ProjectSession::open(dir.path(), options()).unwrap();
    // Resolve the clarification so discovery is ready, then lease an
    // attempt directly (simulating a crash between lease and conclusion).
    app::resolve_decision(&session, "dec_clarify-unknowns-1", "csv export", None).unwrap();
    let revision = session.store().revision().unwrap();
    session
        .store()
        .append(&[DomainFact::AttemptStarted {
            attempt: "att_99".parse().unwrap(),
            work: "work_discover-opportunity-1".parse().unwrap(),
            expected_revision: revision,
            grant: CapabilityGrant::for_scopes(
                GrantId::parse("grant_99").unwrap(),
                vec![ScopePath::root()],
                Vec::new(),
            ),
            started_at_ms: EPOCH_MS,
        }])
        .unwrap();
    drop(session);

    // Reopening concludes the attempt as abandoned — never accepted.
    let (reopened, report) = ProjectSession::open_or_create(dir.path(), options()).unwrap();
    assert_eq!(report.abandoned.len(), 1, "the crash lease is reconciled");
    assert_eq!(report.abandoned[0].attempt.as_str(), "att_99");
    assert!(!report.abandoned[0].had_write_authority);
    let attempt = reopened
        .graph()
        .attempt(&"att_99".parse().unwrap())
        .cloned()
        .unwrap();
    assert!(attempt.conclusion.is_some(), "concluded at open");
    assert!(
        matches!(
            attempt.conclusion,
            Some(relay_domain::AttemptConclusion::Abandoned { .. })
        ),
        "never treated as success: {:?}",
        attempt.conclusion
    );
    // The work is ready again: the abandoned attempt did not complete it.
    assert!(
        reopened
            .evaluation()
            .ready
            .iter()
            .any(|entry| entry.work.as_str() == "work_discover-opportunity-1")
    );
}

#[test]
fn publish_artifact_view_writes_the_convention_blob_and_evidence() {
    let dir = tempfile::tempdir().unwrap();
    app::init(dir.path(), "export team reports as CSV", options()).unwrap();
    let session = ProjectSession::open(dir.path(), options()).unwrap();
    app::resolve_decision(&session, "dec_clarify-unknowns-1", "csv export", None).unwrap();
    let run = relay_app::runner::run_ticks(
        &session,
        &wiring(),
        relay_engine::SchedulingPolicy::default(),
        1,
    )
    .unwrap();

    // The run auto-published the view; verify the convention from the run's
    // own report and on disk.
    let published = run
        .published_views
        .iter()
        .find(|view| view.artifact == "art_brief")
        .cloned()
        .unwrap();
    assert_eq!(published.status, "published");
    assert_eq!(published.version, 1);
    assert!(
        published
            .path
            .starts_with(".relay-graph/artifacts/art_brief/1-")
    );
    assert!(published.path.ends_with(".blob"));
    assert!(dir.path().join(&published.path).is_file());
    assert_eq!(published.sha256.len(), 64);
    // The publication is durable evidence attached to the version.
    let evidence = app::evidence(&session, "art_brief").unwrap();
    assert!(
        evidence
            .evidence
            .iter()
            .any(|item| item.id == published.evidence),
        "{evidence:?}"
    );
    // A manual publish of the same version is idempotent: the existing
    // blob is reported, not rewritten, and no duplicate evidence lands.
    let revision_before = session.graph().revision().get();
    let again = app::publish_artifact_view(&session, "art_brief", None).unwrap();
    assert_eq!(again.status, "already-published");
    assert_eq!(again.path, published.path);
    assert_eq!(session.graph().revision().get(), revision_before);
}

#[test]
fn import_prd_revision_declares_the_revise_work() {
    let dir = tempfile::tempdir().unwrap();
    app::init(dir.path(), "export team reports as CSV", options()).unwrap();
    // One session throughout: distinct wiring keys keep the two scripted
    // executors (and their attempt-id sequences) separate.
    let session = ProjectSession::open(dir.path(), options()).unwrap();
    app::resolve_decision(&session, "dec_clarify-unknowns-1", "csv export", None).unwrap();
    relay_app::runner::run_ticks(
        &session,
        &requirements_wiring(),
        relay_engine::SchedulingPolicy::default(),
        1,
    )
    .unwrap();
    app::expand(&session).unwrap();
    let decisions = app::decisions(&session).unwrap();
    let approval = decisions
        .decisions
        .iter()
        .find(|decision| decision.question.contains("Approve opportunity brief"))
        .unwrap();
    app::resolve_decision(&session, &approval.id, "approve", None).unwrap();
    app::expand(&session).unwrap();
    // Run the requirements work through a scripted model executor that
    // publishes one requirement + criterion.
    relay_app::runner::run_ticks(
        &session,
        &requirements_wiring(),
        relay_engine::SchedulingPolicy::default(),
        1,
    )
    .unwrap();
    let markdown = "# Product Requirements — export team reports as CSV\n\nGoal: export team reports as CSV\n\nBrief: art_brief@1\n\n## Requirement: csv-export — art_req-csv@1\n\nEvery report exports as valid CSV, revised.\n\nRationale: analysts.\n\n### Criterion: valid-rows — art_cri-valid@1 [pass]\n\nNo malformed rows.\n\nMethod: automated-test\n";
    let imported = app::import_prd_revision(&session, markdown).unwrap();
    assert!(
        imported.work.starts_with("work_revise-artifact-1"),
        "{:?}",
        imported.work
    );
    assert!(
        imported
            .documents
            .iter()
            .any(|document| document.artifact == "art_req-csv"),
        "{:?}",
        imported.documents
    );
}

/// A model executor that publishes one requirement and one criterion for
/// requirements work (and the brief for discovery work — the shared
/// executor set keeps one attempt-id sequence per session).
struct RequirementsExecutor;

impl RequirementsExecutor {
    fn discovery_receipt(spec: &AttemptSpec, context: &RunContext) -> AttemptReceipt {
        let doc = relay_workflows::BriefDoc {
            problem: "reports are stuck".to_string(),
            audience: "analysts".to_string(),
            outcome: "one-click CSV export".to_string(),
            open_questions: Vec::new(),
        };
        AttemptReceipt {
            attempt: spec.attempt.clone(),
            work: spec.work.clone(),
            outcome: AttemptOutcome::Completed,
            completed_at_ms: context.now_ms,
            outputs: vec![OutputProposal {
                artifact: "art_brief".parse().unwrap(),
                title: Some("Opportunity brief".to_string()),
                content: doc.to_content(),
                digest: ArtifactDigest::parse(&format!("{:064x}", 0x00ff)).unwrap(),
            }],
            evidence: Vec::new(),
            graph_changes: Vec::new(),
            usage: Usage::default(),
        }
    }
}

impl Executor for RequirementsExecutor {
    fn class(&self) -> ExecutorClass {
        ExecutorClass::Model
    }

    fn run(&self, spec: &AttemptSpec, context: &RunContext) -> AttemptReceipt {
        if spec.work.as_str().starts_with("work_discover") {
            return Self::discovery_receipt(spec, context);
        }
        let requirement = relay_workflows::RequirementDoc {
            slug: "csv-export".to_string(),
            statement: "Every report exports as valid CSV.".to_string(),
            rationale: "analysts live in spreadsheets".to_string(),
            brief: relay_domain::ArtifactVersionRef::new(
                "art_brief".parse().unwrap(),
                relay_domain::ArtifactVersion::FIRST,
            ),
        };
        let criterion = relay_workflows::CriterionDoc {
            slug: "valid-rows".to_string(),
            requirement: relay_domain::ArtifactVersionRef::new(
                "art_req-csv".parse().unwrap(),
                relay_domain::ArtifactVersion::FIRST,
            ),
            statement: "No malformed rows.".to_string(),
            method: relay_workflows::VerificationMethod::AutomatedTest,
        };
        AttemptReceipt {
            attempt: spec.attempt.clone(),
            work: spec.work.clone(),
            outcome: AttemptOutcome::Completed,
            completed_at_ms: context.now_ms,
            outputs: vec![
                OutputProposal {
                    artifact: "art_req-csv".parse().unwrap(),
                    title: Some("Requirement".to_string()),
                    content: requirement.to_content(),
                    digest: ArtifactDigest::parse(&format!("{:064x}", 0x01)).unwrap(),
                },
                OutputProposal {
                    artifact: "art_cri-valid".parse().unwrap(),
                    title: Some("Criterion".to_string()),
                    content: criterion.to_content(),
                    digest: ArtifactDigest::parse(&format!("{:064x}", 0x02)).unwrap(),
                },
            ],
            evidence: Vec::new(),
            graph_changes: Vec::new(),
            usage: Usage::default(),
        }
    }
}

/// A verifier-class executor that publishes research findings for
/// inspect-repository work.
struct ResearchExecutor;

impl Executor for ResearchExecutor {
    fn class(&self) -> ExecutorClass {
        ExecutorClass::Verifier
    }

    fn run(&self, spec: &AttemptSpec, context: &RunContext) -> AttemptReceipt {
        let doc = relay_workflows::ResearchDoc {
            question: spec.objective.clone(),
            findings: vec!["the repository is small".to_string()],
            references: vec!["src/".to_string()],
        };
        AttemptReceipt {
            attempt: spec.attempt.clone(),
            work: spec.work.clone(),
            outcome: AttemptOutcome::Completed,
            completed_at_ms: context.now_ms,
            outputs: vec![OutputProposal {
                artifact: "art_research".parse().unwrap(),
                title: Some("Research".to_string()),
                content: doc.to_content(),
                digest: ArtifactDigest::parse(&format!("{:064x}", 0x03)).unwrap(),
            }],
            evidence: Vec::new(),
            graph_changes: Vec::new(),
            usage: Usage::default(),
        }
    }
}

fn requirements_wiring() -> RuntimeWiring {
    RuntimeWiring::scripted(
        ExecutorSet::new()
            .with(Arc::new(RequirementsExecutor))
            .with(Arc::new(ResearchExecutor)),
        Arc::new(EchoModel),
    )
}

#[test]
fn repeated_runs_never_mint_duplicate_attempt_ids() {
    let dir = tempfile::tempdir().unwrap();
    app::init(dir.path(), "export team reports as CSV", options()).unwrap();
    let session = ProjectSession::open(dir.path(), options()).unwrap();
    app::resolve_decision(&session, "dec_clarify-unknowns-1", "csv export", None).unwrap();
    let first = relay_app::runner::run_ticks(
        &session,
        &requirements_wiring(),
        relay_engine::SchedulingPolicy::default(),
        1,
    )
    .unwrap();
    assert_eq!(first.ticks[0].accepted.len(), 1);
    // The second run seeds its id source past everything the graph holds,
    // so its attempts continue the sequence instead of colliding on att_1.
    app::expand(&session).unwrap();
    let decisions = app::decisions(&session).unwrap();
    let approval = decisions
        .decisions
        .iter()
        .find(|decision| decision.question.contains("Approve opportunity brief"))
        .unwrap();
    app::resolve_decision(&session, &approval.id, "approve", None).unwrap();
    app::expand(&session).unwrap();
    let second = relay_app::runner::run_ticks(
        &session,
        &requirements_wiring(),
        relay_engine::SchedulingPolicy::default(),
        1,
    )
    .unwrap();
    assert!(
        !second.ticks.is_empty() && !second.ticks[0].started.is_empty(),
        "the requirements round ran: {second:?}"
    );
    assert_ne!(
        second.ticks[0].started, first.ticks[0].started,
        "attempt ids continue the session's sequence"
    );
}

#[test]
fn migrate_reports_availability_and_plans_a_legacy_copy() {
    let empty = tempfile::tempdir().unwrap();
    let view = app::migrate(empty.path(), true, &options().store).unwrap();
    assert!(!view.available);
    assert!(view.detail.contains("no legacy Relay project found"));

    let legacy = tempfile::tempdir().unwrap();
    let relay = legacy.path().join(".relay");
    std::fs::create_dir_all(relay.join("artifacts")).unwrap();
    std::fs::write(
        relay.join("events.jsonl"),
        "{\"causationId\":null,\"correlationId\":\"cor_1\",\"envelopeVersion\":1,\"hash\":\"x\",\"id\":\"evt_1\",\"recordedAt\":\"2026-01-01T00:00:00.000Z\",\"sequence\":1,\"type\":\"project.initialized\",\"schemaVersion\":1,\"payload\":{\"projectId\":\"prj_demo\"}}\n",
    )
    .unwrap();
    // Whatever the reference decoder concludes, the dry run never writes.
    let _ = app::migrate(legacy.path(), true, &options().store);
    assert!(!legacy.path().join(".relay-graph").exists());
}

// ---------------------------------------------------------------------------
// The full pipeline through the application boundary: discovery to a
// prepared delivery package gated by an explicit human authorization
// decision (product scenario 12).
// ---------------------------------------------------------------------------

/// Attempt-number-derived digest salt so repeated scripted publications
/// never collide on content digests.
fn attempt_digest(spec: &AttemptSpec) -> ArtifactDigest {
    let number: u64 = spec
        .attempt
        .as_str()
        .trim_start_matches("att_")
        .parse()
        .unwrap_or(1);
    ArtifactDigest::parse(&format!("{number:064x}")).unwrap()
}

fn vref(text: &str) -> relay_domain::ArtifactVersionRef {
    let (artifact, version) = text.split_once('@').unwrap();
    relay_domain::ArtifactVersionRef::new(
        artifact.parse().unwrap(),
        relay_domain::ArtifactVersion::new(version.parse().unwrap()).unwrap(),
    )
}

/// The model round: discovery, requirements, design, plan.
struct PipelineModelExecutor;

impl Executor for PipelineModelExecutor {
    fn class(&self) -> ExecutorClass {
        ExecutorClass::Model
    }

    fn run(&self, spec: &AttemptSpec, context: &RunContext) -> AttemptReceipt {
        let outputs = if spec.work.as_str().starts_with("work_discover") {
            let doc = relay_workflows::BriefDoc {
                problem: "reports are stuck".to_string(),
                audience: "analysts".to_string(),
                outcome: "one-click CSV export".to_string(),
                open_questions: Vec::new(),
            };
            vec![output("art_brief", "Opportunity brief", doc, spec)]
        } else if spec.work.as_str().starts_with("work_define-requirements") {
            let requirement = relay_workflows::RequirementDoc {
                slug: "csv-export".to_string(),
                statement: "Every report exports as valid CSV.".to_string(),
                rationale: "analysts live in spreadsheets".to_string(),
                brief: vref("art_brief@1"),
            };
            let valid = relay_workflows::CriterionDoc {
                slug: "valid-rows".to_string(),
                requirement: vref("art_req-csv@1"),
                statement: "No malformed rows.".to_string(),
                method: relay_workflows::VerificationMethod::AutomatedTest,
            };
            vec![
                output("art_req-csv", "Requirement", requirement, spec),
                output("art_cri-valid", "Criterion", valid, spec),
            ]
        } else if spec.work.as_str().starts_with("work_design") {
            let design = relay_workflows::DesignDoc {
                approach: "stream rows through a validating writer".to_string(),
                components: vec!["src/csv/writer.rs".to_string()],
                risks: vec!["quoting edge cases".to_string()],
            };
            let decision = relay_workflows::DesignDecisionDoc {
                slug: "csv-library".to_string(),
                question: "Which CSV writer?".to_string(),
                choice: "hand-rolled validating writer".to_string(),
                rationale: "full control over quoting".to_string(),
                consequences: "we own the edge cases".to_string(),
            };
            vec![
                output("art_design", "Design", design, spec),
                output("art_dec-csv", "Design decision", decision, spec),
            ]
        } else if spec.work.as_str().starts_with("work_decompose") {
            let plan = relay_workflows::PlanDoc {
                tasks: vec![relay_workflows::PlanTask {
                    slug: "csv-writer".to_string(),
                    kind: relay_workflows::TaskKind::Implement,
                    objective: "Implement the CSV writer".to_string(),
                    requirement_refs: vec![vref("art_req-csv@1")],
                    write_scopes: vec!["src".to_string()],
                }],
            };
            vec![output("art_plan", "Plan", plan, spec)]
        } else {
            return failed_receipt(spec, context, "scripted model round has nothing for this");
        };
        completed(spec, context, outputs)
    }
}

/// The verifier round: research and criterion evidence.
struct PipelineVerifierExecutor;

impl Executor for PipelineVerifierExecutor {
    fn class(&self) -> ExecutorClass {
        ExecutorClass::Verifier
    }

    fn run(&self, spec: &AttemptSpec, context: &RunContext) -> AttemptReceipt {
        if spec.work.as_str().starts_with("work_verify") {
            // The criterion version is the work's pinned input.
            let Some(criterion) = spec.inputs.first().cloned() else {
                return failed_receipt(spec, context, "verification work pins a criterion");
            };
            let evidence = relay_domain::Evidence::new(
                relay_domain::EvidenceId::parse(&format!(
                    "ev_{}",
                    spec.attempt.as_str().trim_start_matches("att_")
                ))
                .unwrap(),
                relay_domain::EvidenceKind::TestRun,
                relay_domain::EvidenceTarget::ArtifactVersion(criterion),
                relay_domain::EvidenceOutcome::Pass,
                "csv export test suite: all rows valid",
                context.now_ms,
            );
            return AttemptReceipt {
                attempt: spec.attempt.clone(),
                work: spec.work.clone(),
                outcome: AttemptOutcome::Completed,
                completed_at_ms: context.now_ms,
                outputs: Vec::new(),
                evidence: vec![evidence],
                graph_changes: Vec::new(),
                usage: Usage::default(),
            };
        }
        let doc = relay_workflows::ResearchDoc {
            question: spec.objective.clone(),
            findings: vec!["the repository is small".to_string()],
            references: vec!["src/".to_string()],
        };
        completed(
            spec,
            context,
            vec![output("art_research", "Research", doc, spec)],
        )
    }
}

/// The workspace round: implementation and integration.
struct PipelineWorkspaceExecutor;

impl Executor for PipelineWorkspaceExecutor {
    fn class(&self) -> ExecutorClass {
        ExecutorClass::Workspace
    }

    fn run(&self, spec: &AttemptSpec, context: &RunContext) -> AttemptReceipt {
        let doc = relay_workflows::ResearchDoc {
            question: "what changed?".to_string(),
            findings: vec!["added the validating CSV writer".to_string()],
            references: vec!["src/csv/writer.rs".to_string()],
        };
        let artifact = if spec.work.as_str().starts_with("work_implement") {
            "art_task-notes"
        } else {
            "art_integration-notes"
        };
        completed(spec, context, vec![output(artifact, "Notes", doc, spec)])
    }
}

fn output<D: StructuredDoc>(
    artifact: &str,
    title: &str,
    doc: D,
    spec: &AttemptSpec,
) -> OutputProposal {
    OutputProposal {
        artifact: artifact.parse().unwrap(),
        title: Some(title.to_string()),
        content: doc.to_content(),
        digest: attempt_digest(spec),
    }
}

fn completed(
    spec: &AttemptSpec,
    context: &RunContext,
    outputs: Vec<OutputProposal>,
) -> AttemptReceipt {
    AttemptReceipt {
        attempt: spec.attempt.clone(),
        work: spec.work.clone(),
        outcome: AttemptOutcome::Completed,
        completed_at_ms: context.now_ms,
        outputs,
        evidence: Vec::new(),
        graph_changes: Vec::new(),
        usage: Usage::default(),
    }
}

fn failed_receipt(spec: &AttemptSpec, context: &RunContext, reason: &str) -> AttemptReceipt {
    AttemptReceipt {
        attempt: spec.attempt.clone(),
        work: spec.work.clone(),
        outcome: AttemptOutcome::Failed {
            reason: reason.to_string(),
        },
        completed_at_ms: context.now_ms,
        outputs: Vec::new(),
        evidence: Vec::new(),
        graph_changes: Vec::new(),
        usage: Usage::default(),
    }
}

/// The full pipeline wiring, including the REAL delivery-prep system job.
fn pipeline_wiring() -> RuntimeWiring {
    RuntimeWiring::scripted(
        ExecutorSet::new()
            .with(Arc::new(PipelineModelExecutor))
            .with(Arc::new(PipelineVerifierExecutor))
            .with(Arc::new(PipelineWorkspaceExecutor))
            .with(Arc::new(relay_executor::SystemExecutor::new(Arc::new(
                relay_app::jobs::DeliveryPrepJob,
            )))),
        Arc::new(EchoModel),
    )
}

/// Drive the pipeline one (expand, resolve, run) round at a time, leaving
/// the delivery authorization for the human, until a round neither proposes
/// nor runs anything.
fn drive_pipeline(session: &ProjectSession) -> Vec<relay_app::views::RunView> {
    let mut runs = Vec::new();
    for _ in 0..24 {
        let before = session.graph().revision().get();
        app::expand(session).unwrap();
        let expanded = session.graph().revision().get() > before;
        // Resolve every open decision except the delivery authorization,
        // which only a human may answer (this harness leaves it open).
        let decisions = app::decisions(session).unwrap();
        for decision in decisions
            .decisions
            .iter()
            .filter(|decision| decision.status == "open")
        {
            if decision
                .question
                .contains("Authorize delivery of the package")
            {
                continue;
            }
            let choice = decision
                .options
                .first()
                .map(|option| option.label.clone())
                .unwrap_or_else(|| "csv export".to_string());
            app::resolve_decision(session, &decision.id, &choice, Some("pipeline harness"))
                .unwrap();
        }
        let run = relay_app::runner::run_ticks(
            session,
            &pipeline_wiring(),
            relay_engine::SchedulingPolicy::default(),
            1,
        )
        .unwrap();
        let progressed = run.ticks.iter().any(|tick| !tick.started.is_empty());
        runs.push(run);
        if !expanded && !progressed {
            break;
        }
    }
    runs
}

#[test]
fn full_pipeline_prepares_delivery_and_gates_it_behind_a_human() {
    let dir = tempfile::tempdir().unwrap();
    app::init(dir.path(), "export team reports as CSV", options()).unwrap();
    let session = ProjectSession::open(dir.path(), options()).unwrap();
    let runs = drive_pipeline(&session);
    assert!(
        runs.iter()
            .any(|run| run.ticks.iter().any(|tick| !tick.started.is_empty())),
        "the pipeline ran work"
    );

    // The pipeline reached a prepared delivery package.
    let delivery = app::artifact_detail(&session, "art_delivery-report", None, true).unwrap();
    assert_eq!(delivery.schema, Some("relay.delivery@1".to_string()));
    assert!(
        delivery
            .markdown
            .as_deref()
            .unwrap_or_default()
            .contains("# Delivery Report"),
        "{}",
        delivery.markdown.as_deref().unwrap_or_default()
    );
    assert!(
        delivery
            .markdown
            .as_deref()
            .unwrap_or_default()
            .contains("requires an explicit human authorization"),
        "the report itself states the gate: {}",
        delivery.markdown.as_deref().unwrap_or_default()
    );

    // The delivery report's view was auto-published with the rest.
    assert!(
        runs.iter().any(|run| run
            .published_views
            .iter()
            .any(|view| view.artifact == "art_delivery-report")),
        "the delivery report view auto-published: {:?}",
        runs.last().map(|run| &run.published_views)
    );

    // The explicit human authorization decision is open and gating.
    let decisions = app::decisions(&session).unwrap();
    let authorization = decisions
        .decisions
        .iter()
        .find(|decision| {
            decision
                .question
                .contains("Authorize delivery of the package")
        })
        .unwrap_or_else(|| panic!("authorization decision missing: {decisions:?}"));
    assert_eq!(authorization.status, "open");
    assert_eq!(
        authorization
            .options
            .iter()
            .map(|option| option.label.as_str())
            .collect::<Vec<_>>(),
        vec!["authorize", "hold"]
    );
    let summary = app::summary(&session).unwrap();
    assert!(summary.open_decisions.contains(&authorization.id));
    assert!(
        summary
            .recommendation
            .next
            .detail
            .contains("resolve decision"),
        "the recommendation points at the human gate: {:?}",
        summary.recommendation.next
    );

    // No attempt anywhere carries delivery authority: preparation is not
    // delivery.
    for (_, attempt) in session.graph().attempts() {
        assert_eq!(
            attempt.grant.delivery,
            relay_domain::DeliveryAuthority::None,
            "{}",
            attempt.id
        );
    }

    // The store is healthy end to end: every published artifact's view
    // blob exists at the canonical convention.
    let doctor = app::doctor_check(dir.path(), &options().store);
    assert!(doctor.healthy, "{doctor:?}");
}

#[test]
fn a_fake_provider_run_publishes_the_brief_end_to_end() {
    // The model executor's strict final-object decode, end to end through a
    // real provider adapter: the fake provider replies with the protocol's
    // final object, the executor decodes it, acceptance publishes the
    // artifact version, and the run auto-publishes its Markdown view.
    let final_object = serde_json::json!({
        "outputs": [{
            "artifact": "art_brief",
            "title": "Opportunity brief",
            "content": {
                "schema": "relay.brief@1",
                "problem": "reports are stuck inside the tool",
                "audience": "analysts",
                "outcome": "one-click CSV export",
                "open_questions": []
            }
        }]
    });
    let provider =
        relay_providers::fake::FakeProvider::new(vec![relay_providers::fake::FakeStep {
            r#match: relay_providers::fake::FakeMatch::default(),
            response: relay_providers::fake::FakeResponse {
                text: Some(final_object.to_string()),
                ..Default::default()
            },
        }]);
    let route = relay_domain::ModelRoute {
        provider: "fake".to_string(),
        model: "fake-standard".to_string(),
        role: None,
    };
    let wiring = RuntimeWiring::production(Arc::new(provider), route);

    let dir = tempfile::tempdir().unwrap();
    app::init(
        dir.path(),
        "export team reports as CSV",
        SessionOptions::production(),
    )
    .unwrap();
    let session = ProjectSession::open(dir.path(), SessionOptions::production()).unwrap();
    app::resolve_decision(&session, "dec_clarify-unknowns-1", "csv export", None).unwrap();
    let run = relay_app::runner::run_ticks(
        &session,
        &wiring,
        relay_engine::SchedulingPolicy::default(),
        1,
    )
    .unwrap();

    assert_eq!(run.ticks[0].accepted.len(), 1, "{run:?}");
    assert!(
        run.ticks[0].failed.is_empty(),
        "the strict decode accepted the final object: {run:?}"
    );
    // The brief artifact exists with the provider's structured content.
    let brief = app::artifact_detail(&session, "art_brief", None, true).unwrap();
    assert_eq!(brief.schema, Some("relay.brief@1".to_string()));
    assert_eq!(brief.content["outcome"], "one-click CSV export");
    // Its view was auto-published and the store is healthy.
    assert!(
        run.published_views
            .iter()
            .any(|view| view.artifact == "art_brief"),
        "{run:?}"
    );
    let doctor = app::doctor_check(dir.path(), &SessionOptions::production().store);
    assert!(doctor.healthy, "{doctor:?}");
}
