//! The four Phase 1 architecture scenarios, as readable narrative tests
//! named after their plan numbers. Each is a hand-built fact sequence —
//! no modes, no back-jumps, no controller special cases anywhere.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::ok_expect,
    clippy::err_expect
)] // test-only conveniences, house style

mod common;

use common::*;
use relay_domain::{
    ArtifactVersion, DecisionSubject, DependencyEdge, DependencyKind, DomainFact, Evidence,
    EvidenceOutcome, EvidenceTarget, GraphChangeProposal,
};
use relay_engine::{self, Autonomy, SchedulingPolicy, WorkStatus};

// ---------------------------------------------------------------------------
// Scenario 1: an ambiguous goal creates discovery work and a focused human
// clarification; the resulting brief can be approved.
// ---------------------------------------------------------------------------

#[test]
fn scenario_1_ambiguous_goal_creates_discovery_and_focused_clarification() {
    let mut facts = vec![
        project("prj_brief"),
        goal("goal_1", "make triaging incoming feature requests painless"),
    ];

    // The goal is ambiguous, so it fans out into model-driven discovery and
    // exactly one focused clarification for a human.
    let discover = model_work(
        "work_discover",
        "discover-opportunity",
        "survey the problem space and draft an opportunity brief",
    );
    let clarify = human_work(
        "work_clarify",
        "clarify-unknown",
        "answer who the primary user is",
        "dec_user",
    );
    facts.push(work_declared(discover));
    facts.push(work_declared(clarify));
    facts.push(decision_opened(
        "dec_user",
        DecisionSubject::WorkItem(work("work_clarify")),
        "Who is the primary user we optimize for?",
        &[
            (
                "support-agents",
                "agents triaging hundreds of requests daily",
            ),
            ("end-customers", "customers submitting the requests"),
        ],
        Some("support-agents"),
    ));
    // Discovery needs the answer before it can shape the brief.
    facts.push(requires("work_discover", "work_clarify"));

    let graph = fold(&facts);
    let evaluation = relay_engine::evaluate(&graph);

    // The clarification is ready — for a human, not for a scheduler.
    assert!(evaluation.ready.is_empty());
    assert_eq!(evaluation.awaiting_human.len(), 1);
    let awaiting = &evaluation.awaiting_human[0];
    assert_eq!(awaiting.work, work("work_clarify"));
    assert_eq!(awaiting.decision.to_string(), "dec_user");
    assert!(awaiting.question.contains("primary user"));
    assert_eq!(awaiting.recommendation.as_deref(), Some("support-agents"));

    // Discovery waits for that answer, and says so.
    assert_eq!(evaluation.blocked.len(), 1);
    assert_eq!(evaluation.blocked[0].work, work("work_discover"));
    let explained = relay_engine::explain(&graph, &work("work_discover"))
        .ok()
        .expect("explainable");
    assert!(explained.detail.contains("work_clarify"));
    assert!(explained.detail.contains("awaiting-human"));

    // The human answers.
    facts.push(decision_resolved(
        "dec_user",
        "support-agents",
        "the support team owns triage throughput",
    ));
    let graph = fold(&facts);
    let evaluation = relay_engine::evaluate(&graph);
    assert!(evaluation.completed.contains(&work("work_clarify")));
    assert_eq!(evaluation.ready.len(), 1, "discovery becomes ready");
    assert_eq!(evaluation.ready[0].work, work("work_discover"));

    // Discovery runs and proposes the brief.
    facts.push(started(
        &graph,
        "att_discover",
        "work_discover",
        grant_read("grant_model", &["."]),
    ));
    let mut graph = fold(&facts);
    let acceptance = relay_engine::accept_receipt(
        &graph,
        &completed_receipt(
            "att_discover",
            "work_discover",
            vec![output(
                "art_brief",
                "Opportunity brief: agent-first triage",
                1,
            )],
            vec![],
            vec![],
        ),
    )
    .ok()
    .expect("discovery receipt accepted");
    apply_all(&mut graph, &acceptance.facts);
    assert_eq!(
        graph.current_version(&"art_brief".parse().ok().expect("artifact id")),
        Some(ArtifactVersion::FIRST)
    );

    // The brief can be approved: approval is human work gated on the exact
    // published version.
    let mut approve = human_work(
        "work_approve",
        "approve-brief",
        "approve the opportunity brief",
        "dec_approve",
    );
    approve.inputs = vec![relay_domain::InputRef::Exact(version_ref("art_brief", 1))];
    apply(&mut graph, &work_declared(approve));
    apply(
        &mut graph,
        &decision_opened(
            "dec_approve",
            DecisionSubject::ArtifactVersion(version_ref("art_brief", 1)),
            "Approve opportunity brief v1 as the basis for requirements?",
            &[
                ("approve", "proceed to requirements"),
                ("revise", "send back to discovery"),
            ],
            None,
        ),
    );

    let evaluation = relay_engine::evaluate(&graph);
    assert!(
        evaluation
            .awaiting_human
            .iter()
            .any(|entry| entry.work == work("work_approve"))
    );
    assert!(evaluation.completed.contains(&work("work_discover")));

    // Approval lands as a resolved decision on that exact version.
    apply(
        &mut graph,
        &decision_resolved("dec_approve", "approve", "the brief matches what we heard"),
    );
    let evaluation = relay_engine::evaluate(&graph);
    assert!(evaluation.completed.contains(&work("work_approve")));
    let explained = relay_engine::explain(&graph, &work("work_approve"))
        .ok()
        .expect("explainable");
    assert!(explained.detail.contains("dec_approve"));
    assert!(explained.detail.contains("approve"));
}

// ---------------------------------------------------------------------------
// Scenario 3: a changed requirement invalidates only work, artifacts, and
// evidence that actually depend on that requirement version.
// ---------------------------------------------------------------------------

#[test]
fn scenario_3_changed_requirement_invalidates_only_dependents() {
    let mut facts = vec![
        project("prj_change"),
        goal("goal_3", "export team reports as CSV"),
    ];

    // A small dependency chain: requirements → design → implementation,
    // each pinning its inputs exactly, plus unrelated research.
    let spec = model_work("work_spec", "define-requirements", "write the requirements");
    let mut design = model_work("work_design", "technical-design", "design the exporter");
    design.inputs = vec![relay_domain::InputRef::Exact(version_ref("art_req", 1))];
    design.dependencies = vec![DependencyEdge::new(
        work("work_design"),
        work("work_spec"),
        DependencyKind::Requires,
    )];
    let mut implement = workspace_work("work_implement", "implement", "build the exporter", "src");
    implement.inputs = vec![relay_domain::InputRef::Exact(version_ref("art_design", 1))];
    implement.dependencies = vec![DependencyEdge::new(
        work("work_implement"),
        work("work_design"),
        DependencyKind::Requires,
    )];
    let unrelated = model_work("work_unrelated", "research", "survey CSV libraries");

    facts.push(work_declared(spec));
    facts.push(work_declared(design));
    facts.push(work_declared(implement));
    facts.push(work_declared(unrelated));

    // Everything runs once and completes.
    let mut graph = fold(&facts);
    for (attempt, work_id, grant, writes, artifact, note) in [
        (
            "att_spec",
            "work_spec",
            "grant_spec",
            &[][..],
            "art_req",
            1u64,
        ),
        (
            "att_design",
            "work_design",
            "grant_design",
            &[][..],
            "art_design",
            2,
        ),
        (
            "att_impl",
            "work_implement",
            "grant_impl",
            &["src"][..],
            "art_impl",
            3,
        ),
        (
            "att_other",
            "work_unrelated",
            "grant_other",
            &[][..],
            "art_other",
            4,
        ),
    ] {
        start(
            &mut graph,
            attempt,
            work_id,
            grant_for_scopes(grant, &["."], writes),
        );
        let acceptance = relay_engine::accept_receipt(
            &graph,
            &completed_receipt(
                attempt,
                work_id,
                vec![output(artifact, "notes", note)],
                vec![],
                vec![],
            ),
        )
        .ok()
        .expect("receipt accepted");
        apply_all(&mut graph, &acceptance.facts);
    }
    // A review attaches evidence to the exact design version.
    apply(
        &mut graph,
        &DomainFact::EvidenceRecorded {
            evidence: Evidence::new(
                evidence_id("ev_review"),
                relay_domain::EvidenceKind::Review,
                EvidenceTarget::ArtifactVersion(version_ref("art_design", 1)),
                EvidenceOutcome::Pass,
                "design covers the export settings",
                4_500,
            ),
        },
    );

    let statuses = status_map(&graph);
    for id in [
        "work_spec",
        "work_design",
        "work_implement",
        "work_unrelated",
    ] {
        assert_eq!(
            statuses[&work(id)],
            WorkStatus::Completed,
            "{id} completed before the change"
        );
    }

    // The requirement changes: importing the edited requirements publishes
    // v2 of exactly that artifact.
    apply(&mut graph, &publish("art_req", 2, 9));

    // Only the actual dependents of art_req@1 are invalid.
    let evaluation = relay_engine::evaluate(&graph);
    let invalidated: Vec<&relay_domain::WorkItemId> = evaluation
        .invalidated
        .iter()
        .map(|entry| &entry.work)
        .collect();
    assert_eq!(
        invalidated.len(),
        2,
        "design and implementation only: {invalidated:?}"
    );
    assert!(invalidated.contains(&&work("work_design")));
    assert!(invalidated.contains(&&work("work_implement")));
    assert!(!invalidated.contains(&&work("work_spec")));
    assert!(!invalidated.contains(&&work("work_unrelated")));

    // The cause chain names the exact version change.
    let explained = relay_engine::explain(&graph, &work("work_design"))
        .ok()
        .expect("explainable");
    assert!(
        explained.detail.contains("art_req@1"),
        "chain names the superseded version: {explained}"
    );
    assert!(
        explained.detail.contains("art_req@2"),
        "chain names the replacing version: {explained}"
    );

    // The design artifact version and its evidence are invalid; the
    // requirement's own history and unrelated work are untouched.
    assert!(matches!(
        relay_engine::status::version_status(&graph, &version_ref("art_design", 1)),
        relay_engine::VersionStatus::Invalid { .. }
    ));
    let review = graph
        .evidence_by_id(&evidence_id("ev_review"))
        .cloned()
        .expect("evidence exists");
    assert!(matches!(
        relay_engine::status::evidence_status(&graph, &review),
        relay_engine::EvidenceStatus::Invalid { .. }
    ));
    assert!(matches!(
        relay_engine::status::version_status(&graph, &version_ref("art_req", 1)),
        relay_engine::VersionStatus::Superseded { .. }
    ));
    assert!(matches!(
        relay_engine::status::version_status(&graph, &version_ref("art_other", 1)),
        relay_engine::VersionStatus::Current
    ));
}

// ---------------------------------------------------------------------------
// Scenario 4: technical research and repository inspection become ready
// together and run concurrently because both are read-only.
// ---------------------------------------------------------------------------

#[test]
fn scenario_4_readonly_work_becomes_ready_together_and_runs_concurrently() {
    let mut facts = vec![
        project("prj_parallel"),
        goal(
            "goal_4",
            "ground the design in research and repository facts",
        ),
    ];
    let mut research = model_work("work_research", "research", "survey prior approaches");
    research.read_scopes = vec![scope("docs")];
    let mut inspection = model_work(
        "work_inspect",
        "inspect-repository",
        "map the current modules",
    );
    inspection.executor = relay_domain::ExecutorClass::Verifier;
    inspection.read_scopes = vec![scope("src")];
    facts.push(work_declared(research));
    facts.push(work_declared(inspection));
    let graph = fold(&facts);

    // Both are ready at the same time.
    let evaluation = relay_engine::evaluate(&graph);
    let ready_ids: Vec<&relay_domain::WorkItemId> =
        evaluation.ready.iter().map(|e| &e.work).collect();
    assert_eq!(ready_ids.len(), 2);
    assert!(ready_ids.contains(&&work("work_research")));
    assert!(ready_ids.contains(&&work("work_inspect")));

    // The scheduler puts both in the parallel batch — nothing writes.
    let recommendation = relay_engine::recommend(
        &graph,
        &SchedulingPolicy {
            autonomy: Autonomy::Safe,
            max_concurrent_writers: 1,
        },
    );
    assert_eq!(recommendation.parallel.len(), 2);
    assert!(recommendation.serialized.is_empty());
    assert!(
        recommendation
            .parallel
            .iter()
            .all(|entry| entry.write_scopes.is_empty())
    );
    assert!(recommendation.rationale.contains("2 parallelizable"));

    // Both attempts start; the second start lands one revision later and
    // that is fine — durable attempt facts are not conflicts.
    let mut graph = graph;
    start(
        &mut graph,
        "att_research",
        "work_research",
        grant_read("grant_r", &["docs", "."]),
    );
    start(
        &mut graph,
        "att_inspect",
        "work_inspect",
        grant_read("grant_i", &["src", "."]),
    );
    let evaluation = relay_engine::evaluate(&graph);
    assert_eq!(evaluation.active_attempts.len(), 2);
    assert!(
        evaluation
            .active_attempts
            .iter()
            .all(|attempt| !attempt.inputs_stale)
    );

    // Research finishes first and its accepted facts bump the revision.
    let acceptance = relay_engine::accept_receipt(
        &graph,
        &completed_receipt(
            "att_research",
            "work_research",
            vec![output("art_research", "prior approaches", 5)],
            vec![],
            vec![],
        ),
    )
    .ok()
    .expect("research receipt accepted");
    apply_all(&mut graph, &acceptance.facts);
    let inspection_attempt = graph
        .attempt(&attempt_id("att_inspect"))
        .cloned()
        .expect("attempt exists");
    assert!(
        graph.revision() > inspection_attempt.expected_revision,
        "facts landed while the inspection attempt was in flight"
    );

    // The inspection still completes: the intervening facts are immaterial
    // to it, and read-only work never conflicts.
    let acceptance = relay_engine::accept_receipt(
        &graph,
        &completed_receipt(
            "att_inspect",
            "work_inspect",
            vec![output("art_findings", "module map", 6)],
            vec![evidence(
                "ev_inspect",
                EvidenceTarget::WorkItem(work("work_inspect")),
                EvidenceOutcome::Info,
                "14 modules, 3 cyclic imports",
            )],
            vec![],
        ),
    )
    .ok()
    .expect("inspection receipt accepted despite intervening facts");
    apply_all(&mut graph, &acceptance.facts);

    let evaluation = relay_engine::evaluate(&graph);
    assert!(evaluation.completed.contains(&work("work_research")));
    assert!(evaluation.completed.contains(&work("work_inspect")));
    assert!(evaluation.ready.is_empty());
}

// ---------------------------------------------------------------------------
// Scenario 8: verification attaches evidence to acceptance criteria; a
// failure creates targeted corrective work and later makes verification
// ready again.
// ---------------------------------------------------------------------------

#[test]
fn scenario_8_verification_evidence_failure_and_corrective_work() {
    let mut facts = vec![
        project("prj_verify"),
        goal("goal_8", "the exporter produces valid CSV for every report"),
    ];
    // One acceptance criterion, published once.
    facts.push(publish("art_criterion", 1, 1));

    let mut implement = workspace_work("work_implement8", "implement", "build the exporter", "src");
    implement.required = true;
    let mut verify = verifier_work(
        "work_verify",
        "verify-criterion",
        "check the exporter against the criterion",
        ("art_criterion", 1),
    );
    verify.inputs = vec![relay_domain::InputRef::Exact(version_ref(
        "art_criterion",
        1,
    ))];
    verify.dependencies = vec![DependencyEdge::new(
        work("work_verify"),
        work("work_implement8"),
        DependencyKind::Validates,
    )];
    facts.push(work_declared(implement));
    facts.push(work_declared(verify));
    let mut graph = fold(&facts);

    // Verification is blocked until the implementation completes.
    let evaluation = relay_engine::evaluate(&graph);
    assert_eq!(
        evaluation.ready.len(),
        1,
        "only the implementation is ready"
    );
    assert_eq!(evaluation.ready[0].work, work("work_implement8"));

    // The implementation runs once.
    start(
        &mut graph,
        "att_impl8",
        "work_implement8",
        grant_for_scopes("grant_impl8", &["."], &["src"]),
    );
    let acceptance = relay_engine::accept_receipt(
        &graph,
        &completed_receipt(
            "att_impl8",
            "work_implement8",
            vec![output("art_impl8", "implementation notes", 7)],
            vec![],
            vec![],
        ),
    )
    .ok()
    .expect("implementation receipt accepted");
    apply_all(&mut graph, &acceptance.facts);

    // Now verification is ready and runs — the run itself completes, but
    // the evidence it attaches refutes the criterion.
    let evaluation = relay_engine::evaluate(&graph);
    assert_eq!(evaluation.ready.len(), 1);
    assert_eq!(evaluation.ready[0].work, work("work_verify"));
    start(
        &mut graph,
        "att_verify1",
        "work_verify",
        grant_read("grant_verify", &["."]),
    );
    let failing_run = evidence(
        "ev_run1",
        EvidenceTarget::ArtifactVersion(version_ref("art_criterion", 1)),
        EvidenceOutcome::Fail,
        "2 of 5 report shapes export malformed rows",
    );
    let corrective = workspace_work(
        "work_fix",
        "correct-implementation",
        "fix the malformed row export",
        "src",
    );
    let acceptance = relay_engine::accept_receipt(
        &graph,
        &completed_receipt(
            "att_verify1",
            "work_verify",
            vec![],
            vec![failing_run],
            vec![
                GraphChangeProposal::DeclareWork { work: corrective },
                GraphChangeProposal::DeclareDependency {
                    edge: DependencyEdge::new(
                        work("work_verify"),
                        work("work_fix"),
                        DependencyKind::Requires,
                    ),
                },
            ],
        ),
    )
    .ok()
    .expect("verification receipt accepted; the failure lives in the evidence");
    apply_all(&mut graph, &acceptance.facts);

    // The verifier's proposals become facts only through policy.
    for proposal in &acceptance.proposals {
        let applied = relay_engine::validate_graph_change(&graph, proposal)
            .ok()
            .expect("proposal accepted");
        apply_all(&mut graph, &applied.facts);
    }

    // The criterion is refuted, so verification did not complete — and it
    // is now blocked on exactly the corrective work it caused.
    let statuses = status_map(&graph);
    assert_ne!(statuses[&work("work_verify")], WorkStatus::Completed);
    let explained = relay_engine::explain(&graph, &work("work_verify"))
        .ok()
        .expect("explainable");
    assert!(
        explained.detail.contains("work_fix"),
        "blocked on the corrective work: {explained}"
    );
    assert!(!relay_engine::status::criterion_satisfied(
        &graph,
        &version_ref("art_criterion", 1)
    ));

    // The corrective work runs and completes.
    start(
        &mut graph,
        "att_fix",
        "work_fix",
        grant_for_scopes("grant_fix", &["."], &["src"]),
    );
    let acceptance = relay_engine::accept_receipt(
        &graph,
        &completed_receipt(
            "att_fix",
            "work_fix",
            vec![output("art_fix", "correction notes", 8)],
            vec![],
            vec![],
        ),
    )
    .ok()
    .expect("correction receipt accepted");
    apply_all(&mut graph, &acceptance.facts);

    // Verification is ready again — the plan's "later makes verification
    // ready again", with the failing evidence still standing against the
    // criterion.
    let evaluation = relay_engine::evaluate(&graph);
    assert_eq!(evaluation.ready.len(), 1);
    assert_eq!(evaluation.ready[0].work, work("work_verify"));
    let explained = relay_engine::explain(&graph, &work("work_verify"))
        .ok()
        .expect("explainable");
    assert!(
        explained.detail.contains("ev_run1"),
        "the ready explanation still names the failing evidence: {explained}"
    );

    // The failing evidence tested the implementation the corrective work
    // just replaced; the system executor proposes retiring it, and policy
    // validates the declaration.
    let applied = relay_engine::validate_graph_change(
        &graph,
        &GraphChangeProposal::DeclareInvalidation {
            target: relay_domain::InvalidationTarget::Evidence(evidence_id("ev_run1")),
            cause: relay_domain::InvalidationCause::Declared {
                note: "observed against the implementation the corrective work replaced"
                    .to_string(),
            },
            changed_input: None,
        },
    )
    .ok()
    .expect("evidence invalidation accepted");
    apply_all(&mut graph, &applied.facts);

    // A second verification run passes; only now is the work completed.
    start(
        &mut graph,
        "att_verify2",
        "work_verify",
        grant_read("grant_verify2", &["."]),
    );
    let passing_run = evidence(
        "ev_run2",
        EvidenceTarget::ArtifactVersion(version_ref("art_criterion", 1)),
        EvidenceOutcome::Pass,
        "all 5 report shapes export valid CSV",
    );
    let acceptance = relay_engine::accept_receipt(
        &graph,
        &completed_receipt(
            "att_verify2",
            "work_verify",
            vec![],
            vec![passing_run],
            vec![],
        ),
    )
    .ok()
    .expect("second verification receipt accepted");
    apply_all(&mut graph, &acceptance.facts);

    let statuses = status_map(&graph);
    assert_eq!(statuses[&work("work_verify")], WorkStatus::Completed);
    let explained = relay_engine::explain(&graph, &work("work_verify"))
        .ok()
        .expect("explainable");
    assert!(
        explained.detail.contains("ev_run2"),
        "completion cites the passing evidence: {explained}"
    );
    // And with the required implementation and satisfied criterion, the
    // project completes.
    let evaluation = relay_engine::evaluate(&graph);
    assert!(matches!(
        evaluation.completion,
        relay_engine::ProjectCompletion::Complete
    ));
}

fn scope(root: &str) -> relay_domain::ScopePath {
    relay_domain::ScopePath::parse(root)
        .ok()
        .expect("valid scope")
}
