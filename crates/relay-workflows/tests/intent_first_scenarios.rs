//! All twelve defining product scenarios of the intent-first plan, played
//! end-to-end through the built-in workflow, the engine, and the
//! fake-executor harness. No modes, no pause state, no controller special
//! cases: every step is facts, derivation, and policy.
//!
//! Each scenario owns one [`ProjectGraph`] it mutates through the harness;
//! scenario 9 additionally keeps the fact history to simulate crash
//! recovery by re-folding a prefix.

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
    CompletionRule, DependencyEdge, DependencyKind, DomainFact, EvidenceOutcome,
    GraphChangeProposal, InvalidationCause, InvalidationTarget, ProjectGraph, ScopePath, WorkItem,
    WorkItemId,
};
use relay_engine::{self, Autonomy, Rejected, SchedulingPolicy, WorkStatus};
use relay_workflows::{
    self as wf, BriefDoc, CriterionDoc, DeliveryDoc, DesignDecisionDoc, DesignDoc, PlanDoc,
    PlanTask, RequirementDoc, StructuredDoc, TaskKind, VerificationMethod, WorkflowCatalog,
};

// ---------------------------------------------------------------------------
// Shared pipeline helpers (each scenario starts from the piece it needs)
// ---------------------------------------------------------------------------

fn boot(goal_text: &str) -> ProjectGraph {
    let facts = vec![project("prj_wf"), goal("goal_wf", goal_text)];
    fold(&facts)
}

fn csv_requirement(brief: u32) -> RequirementDoc {
    RequirementDoc {
        slug: "csv-export".to_string(),
        statement: "Every team report exports as valid CSV.".to_string(),
        rationale: "Analysts live in spreadsheets.".to_string(),
        brief: vref("art_brief", brief),
    }
}

fn valid_criterion(requirement: u32) -> CriterionDoc {
    CriterionDoc {
        slug: "valid-rows".to_string(),
        requirement: vref("art_req-csv", requirement),
        statement: "No malformed rows in any export.".to_string(),
        method: VerificationMethod::AutomatedTest,
    }
}

fn headers_criterion(requirement: u32) -> CriterionDoc {
    CriterionDoc {
        slug: "headers-row".to_string(),
        requirement: vref("art_req-csv", requirement),
        statement: "Exports carry a stable header row.".to_string(),
        method: VerificationMethod::Inspection,
    }
}

fn design_doc() -> DesignDoc {
    DesignDoc {
        approach: "Stream rows through a validating CSV writer module".to_string(),
        components: vec!["src/csv/writer.rs".to_string()],
        risks: vec!["quoting edge cases".to_string()],
    }
}

fn csv_plan() -> PlanDoc {
    PlanDoc {
        tasks: vec![PlanTask {
            slug: "csv-writer".to_string(),
            kind: TaskKind::Implement,
            objective: "Implement the CSV writer for report rows".to_string(),
            requirement_refs: vec![vref("art_req-csv", 1)],
            write_scopes: vec!["src".to_string()],
        }],
    }
}

/// Goal → clarification → discovery → brief → approval.
fn run_to_approved_brief(provider: FakeProvider) -> ProjectGraph {
    let mut graph = boot("export team reports as CSV");

    let applied = expand(&mut graph);
    assert_eq!(
        applied.len(),
        3,
        "clarify work + its decision + discovery work"
    );
    let clarify = sole_work_of_kind(&graph, wf::KIND_CLARIFY);
    let discover = sole_work_of_kind(&graph, wf::KIND_DISCOVER);

    let fact = human_resolves(
        &graph,
        clarify.as_str(),
        "CSV export for analysts",
        "the concrete outcome",
    );
    apply(&mut graph, &fact);
    assert_eq!(status_of(&graph, discover.as_str()), WorkStatus::Ready);

    start(&mut graph, "att_discover", discover.as_str());
    executor_delivers(
        &mut graph,
        receipt(
            "att_discover",
            discover.as_str(),
            vec![provider.output(
                "art_brief",
                "Opportunity brief",
                &BriefDoc {
                    problem: "reports are stuck inside the tool".to_string(),
                    audience: "analysts".to_string(),
                    outcome: "one-click CSV export".to_string(),
                    open_questions: vec![],
                },
                1,
            )],
            vec![],
            vec![],
        ),
    );
    assert_eq!(status_of(&graph, discover.as_str()), WorkStatus::Completed);

    let applied = expand(&mut graph);
    assert_eq!(applied.len(), 2, "approval work + its decision");
    let approval = sole_work_of_kind(&graph, wf::KIND_APPROVE);
    let fact = human_resolves(
        &graph,
        approval.as_str(),
        "approve",
        "the brief matches what we heard",
    );
    apply(&mut graph, &fact);
    assert_eq!(status_of(&graph, approval.as_str()), WorkStatus::Completed);
    graph
}

/// Approved brief → requirements and criteria published.
fn run_to_requirements(provider: FakeProvider) -> ProjectGraph {
    let mut graph = run_to_approved_brief(provider);
    let applied = expand(&mut graph);
    assert_eq!(
        applied.len(),
        2,
        "requirements work plus read-only research"
    );
    let requirements = sole_work_of_kind(&graph, wf::KIND_REQUIREMENTS);
    assert_eq!(status_of(&graph, requirements.as_str()), WorkStatus::Ready);

    start(&mut graph, "att_reqs", requirements.as_str());
    executor_delivers(
        &mut graph,
        receipt(
            "att_reqs",
            requirements.as_str(),
            vec![
                provider.output("art_req-csv", "Requirement", &csv_requirement(1), 2),
                provider.output("art_cri-valid", "Criterion", &valid_criterion(1), 3),
                provider.output("art_cri-headers", "Criterion", &headers_criterion(1), 4),
            ],
            vec![],
            vec![],
        ),
    );
    assert_eq!(
        status_of(&graph, requirements.as_str()),
        WorkStatus::Completed
    );
    graph
}

/// Requirements → design → plan → one implemented task, with integration
/// and verification work proposed.
fn run_to_implemented(provider: FakeProvider) -> ProjectGraph {
    let mut graph = run_to_requirements(provider);

    let applied = expand(&mut graph);
    assert_eq!(applied.len(), 1, "design-approach proposed");
    let design = sole_work_of_kind(&graph, wf::KIND_DESIGN);
    start(&mut graph, "att_design", design.as_str());
    executor_delivers(
        &mut graph,
        receipt(
            "att_design",
            design.as_str(),
            vec![
                provider.output("art_design", "Design", &design_doc(), 5),
                provider.output(
                    "art_dec-csv",
                    "Design decision",
                    &DesignDecisionDoc {
                        slug: "csv-library".to_string(),
                        question: "Which CSV writer?".to_string(),
                        choice: "hand-rolled validating writer".to_string(),
                        rationale: "full control over quoting".to_string(),
                        consequences: "we own the edge cases".to_string(),
                    },
                    6,
                ),
            ],
            vec![],
            vec![],
        ),
    );

    let applied = expand(&mut graph);
    assert_eq!(applied.len(), 1, "decompose-plan proposed");
    let plan = sole_work_of_kind(&graph, wf::KIND_PLAN);
    start(&mut graph, "att_plan", plan.as_str());
    executor_delivers(
        &mut graph,
        receipt(
            "att_plan",
            plan.as_str(),
            vec![provider.output("art_plan", "Plan", &csv_plan(), 7)],
            vec![],
            vec![],
        ),
    );

    let applied = expand(&mut graph);
    assert_eq!(applied.len(), 1, "implement-task proposed for csv-writer");
    let implement = sole_work_of_kind(&graph, wf::KIND_IMPLEMENT);
    assert_eq!(
        graph
            .work(&implement)
            .expect("work")
            .effective_write_scopes(),
        vec![ScopePath::parse("src").expect("scope")]
    );
    start(&mut graph, "att_impl", implement.as_str());
    executor_delivers(
        &mut graph,
        receipt(
            "att_impl",
            implement.as_str(),
            vec![provider.output(
                "art_impl-notes",
                "Task notes",
                &wf::ResearchDoc {
                    question: "what changed?".to_string(),
                    findings: vec!["added src/csv/writer.rs".to_string()],
                    references: vec!["src/csv/writer.rs".to_string()],
                },
                8,
            )],
            vec![],
            vec![],
        ),
    );
    assert_eq!(status_of(&graph, implement.as_str()), WorkStatus::Completed);

    // With the task done, expansion proposes integration and the verifiers.
    expand(&mut graph);
    assert_eq!(works_of_kind(&graph, wf::KIND_INTEGRATE).len(), 1);
    assert_eq!(
        works_of_kind(&graph, wf::KIND_VERIFY).len(),
        2,
        "one verifier per criterion"
    );
    graph
}

// ---------------------------------------------------------------------------
// Scenario 1
// ---------------------------------------------------------------------------

#[test]
fn scenario_1_ambiguous_goal_creates_discovery_and_focused_clarification() {
    let mut graph = boot("make reporting less painful");

    let applied = expand(&mut graph);
    assert_eq!(
        applied.len(),
        3,
        "clarify work, its decision, discovery work"
    );
    let clarify = sole_work_of_kind(&graph, wf::KIND_CLARIFY);
    let discover = sole_work_of_kind(&graph, wf::KIND_DISCOVER);

    // The clarification is ready — for a human, not for a scheduler.
    let evaluation = relay_engine::evaluate(&graph);
    assert!(evaluation.ready.is_empty());
    assert_eq!(evaluation.awaiting_human.len(), 1);
    assert_eq!(evaluation.awaiting_human[0].work, clarify);
    assert!(evaluation.awaiting_human[0].question.contains("ambiguous"));

    // Discovery waits for the answer and says so.
    let explained = relay_engine::explain(&graph, &discover).expect("explainable");
    assert!(explained.detail.contains(clarify.as_str()));

    // The human answers; discovery runs and the brief becomes approvable.
    let fact = human_resolves(
        &graph,
        clarify.as_str(),
        "CSV export",
        "that is the outcome",
    );
    apply(&mut graph, &fact);
    let provider = FakeProvider::alpha();
    start(&mut graph, "att_discover", discover.as_str());
    executor_delivers(
        &mut graph,
        receipt(
            "att_discover",
            discover.as_str(),
            vec![provider.output(
                "art_brief",
                "Opportunity brief",
                &BriefDoc {
                    problem: "reports are stuck".to_string(),
                    audience: "analysts".to_string(),
                    outcome: "one-click CSV export".to_string(),
                    open_questions: vec![],
                },
                1,
            )],
            vec![],
            vec![],
        ),
    );
    expand(&mut graph);
    let approval = sole_work_of_kind(&graph, wf::KIND_APPROVE);
    let fact = human_resolves(&graph, approval.as_str(), "approve", "looks right");
    apply(&mut graph, &fact);
    assert_eq!(status_of(&graph, approval.as_str()), WorkStatus::Completed);
}

// ---------------------------------------------------------------------------
// Scenario 2
// ---------------------------------------------------------------------------

#[test]
fn scenario_2_approved_brief_expands_into_requirements_and_criteria_prd() {
    let graph = run_to_requirements(FakeProvider::alpha());

    // Addressable requirement and criterion artifacts exist with schemas.
    let requirement = RequirementDoc::from_content(&content_of(&graph, "art_req-csv"))
        .ok()
        .expect("requirement decodes");
    assert_eq!(requirement.slug, "csv-export");
    assert_eq!(requirement.brief, vref("art_brief", 1));
    let criterion = CriterionDoc::from_content(&content_of(&graph, "art_cri-valid"))
        .ok()
        .expect("criterion decodes");
    assert_eq!(criterion.requirement, vref("art_req-csv", 1));

    // The PRD view groups them coherently over exact versions, and renders
    // byte-identically for identical input.
    let criteria = [valid_criterion(1), headers_criterion(1)];
    let refs = (
        vref("art_brief", 1),
        vref("art_req-csv", 1),
        vref("art_cri-valid", 1),
        vref("art_cri-headers", 1),
    );
    let criterion_entries = [
        wf::CriterionEntry {
            reference: &refs.2,
            doc: &criteria[0],
            status: wf::CriterionStatus::Unevaluated,
            evidence: &[],
        },
        wf::CriterionEntry {
            reference: &refs.3,
            doc: &criteria[1],
            status: wf::CriterionStatus::Unevaluated,
            evidence: &[],
        },
    ];
    let requirement_entries = [wf::RequirementEntry {
        reference: &refs.1,
        doc: &requirement,
        criteria: &criterion_entries,
    }];
    let view = wf::PrdView {
        goal: "export team reports as CSV",
        brief: Some(&refs.0),
        requirements: &requirement_entries,
    };
    let prd = wf::render_prd(&view);
    assert!(prd.contains("# Product Requirements — export team reports as CSV"));
    assert!(prd.contains("Brief: art_brief@1"));
    assert!(prd.contains("## Requirement: csv-export — art_req-csv@1"));
    assert!(prd.contains("### Criterion: valid-rows — art_cri-valid@1 [unevaluated]"));
    assert!(prd.contains("### Criterion: headers-row — art_cri-headers@1 [unevaluated]"));
    assert_eq!(prd, wf::render_prd(&view), "PRD rendering is deterministic");
}

// ---------------------------------------------------------------------------
// Scenario 3
// ---------------------------------------------------------------------------

#[test]
fn scenario_3_changed_requirement_invalidates_only_actual_dependents() {
    let mut graph = run_to_implemented(FakeProvider::alpha());

    let design = sole_work_of_kind(&graph, wf::KIND_DESIGN);
    assert_eq!(status_of(&graph, design.as_str()), WorkStatus::Completed);
    let verifiers = works_of_kind(&graph, wf::KIND_VERIFY);
    assert_eq!(verifiers.len(), 2);

    // The user edits the PRD view; import parses the edit back into
    // structured content and an explicit revision work proposal.
    let requirement = RequirementDoc::from_content(&content_of(&graph, "art_req-csv"))
        .ok()
        .expect("requirement decodes");
    let criteria = [valid_criterion(1), headers_criterion(1)];
    let refs = (
        vref("art_brief", 1),
        vref("art_req-csv", 1),
        vref("art_cri-valid", 1),
        vref("art_cri-headers", 1),
    );
    let criterion_entries = [
        wf::CriterionEntry {
            reference: &refs.2,
            doc: &criteria[0],
            status: wf::CriterionStatus::Unevaluated,
            evidence: &[],
        },
        wf::CriterionEntry {
            reference: &refs.3,
            doc: &criteria[1],
            status: wf::CriterionStatus::Unevaluated,
            evidence: &[],
        },
    ];
    let requirement_entries = [wf::RequirementEntry {
        reference: &refs.1,
        doc: &requirement,
        criteria: &criterion_entries,
    }];
    let prd = wf::render_prd(&wf::PrdView {
        goal: "export team reports as CSV",
        brief: Some(&refs.0),
        requirements: &requirement_entries,
    });
    let edited = prd.replace(
        "Every team report exports as valid CSV.",
        "Every team report exports as valid CSV, with a stable schema.",
    );
    let import = wf::import_prd(&edited).expect("edited PRD imports");
    assert_eq!(
        import.requirements[0].statement,
        "Every team report exports as valid CSV, with a stable schema."
    );
    let revision = wf::prd_revision(&graph, &import, "work_revise-csv".parse().expect("id"))
        .ok()
        .expect("revision validates");
    let applied = relay_engine::validate_graph_change(&graph, &wf::revision_proposal(&revision))
        .ok()
        .expect("revision proposal accepted");
    for fact in &applied.facts {
        apply(&mut graph, fact);
    }

    // The revision work runs and publishes v2 of requirement and criteria.
    let revise = sole_work_of_kind(&graph, wf::KIND_REVISE);
    assert_eq!(status_of(&graph, revise.as_str()), WorkStatus::Ready);
    let outputs = revision
        .documents
        .iter()
        .map(|(artifact, document)| match document {
            wf::PrdDocument::Requirement(doc) => {
                FakeProvider::alpha().output(artifact.as_str(), "Revised requirement", doc, 9)
            }
            wf::PrdDocument::Criterion(doc) => {
                FakeProvider::alpha().output(artifact.as_str(), "Revised criterion", doc, 10)
            }
        })
        .collect();
    start(&mut graph, "att_revise", revise.as_str());
    executor_delivers(
        &mut graph,
        receipt("att_revise", revise.as_str(), outputs, vec![], vec![]),
    );
    assert_eq!(
        graph
            .current_version(&"art_req-csv".parse().expect("id"))
            .map(|version| version.get()),
        Some(2)
    );

    // Exactly the actual dependents of art_req-csv@1 are invalid — through
    // both direct pins and provenance: the design and implementation (which
    // pinned it), the plan (derived from it, so the integrate and revise
    // works reading it), and the two verifiers (whose criteria were
    // republished). Nothing else moved.
    let evaluation = relay_engine::evaluate(&graph);
    let invalid: Vec<&WorkItemId> = evaluation
        .invalidated
        .iter()
        .map(|entry| &entry.work)
        .collect();
    assert_eq!(
        invalid.len(),
        7,
        "every actual dependent, provenance included: {invalid:?}"
    );
    assert!(invalid.contains(&&design));
    for verifier in &verifiers {
        assert!(
            invalid.contains(&verifier),
            "{verifier} depended on the requirement"
        );
    }
    let explained = relay_engine::explain(&graph, &design).expect("explainable");
    assert!(
        explained.detail.contains("art_req-csv@1") && explained.detail.contains("art_req-csv@2"),
        "cause chain names the exact version change: {explained}"
    );
    // The old requirement version itself is just superseded history.
    assert!(matches!(
        relay_engine::status::version_status(&graph, &vref("art_req-csv", 1)),
        relay_engine::VersionStatus::Superseded { .. }
    ));
    // Research that was never pinned to the requirement stays untouched.
    let research = sole_work_of_kind(&graph, wf::KIND_RESEARCH);
    assert_eq!(status_of(&graph, research.as_str()), WorkStatus::Ready);
}

// ---------------------------------------------------------------------------
// Scenario 4
// ---------------------------------------------------------------------------

#[test]
fn scenario_4_readonly_work_becomes_ready_together_and_runs_concurrently() {
    let mut graph = run_to_requirements(FakeProvider::alpha());

    // The read-only research proposed at approval time and the model-driven
    // design proposed at requirements time are both ready together.
    expand(&mut graph);
    let research = sole_work_of_kind(&graph, wf::KIND_RESEARCH);
    let design = sole_work_of_kind(&graph, wf::KIND_DESIGN);

    let evaluation = relay_engine::evaluate(&graph);
    for work in [&research, &design] {
        assert!(
            evaluation.ready.iter().any(|entry| &entry.work == work),
            "{work} must be ready"
        );
    }
    let recommendation = relay_engine::recommend(
        &graph,
        &SchedulingPolicy {
            autonomy: Autonomy::Safe,
            max_concurrent_writers: 1,
        },
    );
    assert_eq!(
        recommendation.parallel.len(),
        2,
        "research + design parallelize"
    );
    assert!(recommendation.serialized.is_empty());

    // Both run concurrently; the later-completing one survives the other's
    // accepted facts because those changes are immaterial to it.
    start(&mut graph, "att_research", research.as_str());
    start(&mut graph, "att_design", design.as_str());
    assert_eq!(relay_engine::evaluate(&graph).active_attempts.len(), 2);

    executor_delivers(
        &mut graph,
        receipt(
            "att_research",
            research.as_str(),
            vec![FakeProvider::alpha().output(
                "art_research",
                "Findings",
                &wf::ResearchDoc {
                    question: "what does the repo look like?".to_string(),
                    findings: vec!["reports render through one service".to_string()],
                    references: vec!["src/reports".to_string()],
                },
                11,
            )],
            vec![],
            vec![],
        ),
    );
    // Facts landed while the design attempt was in flight.
    let design_attempt = graph
        .attempts()
        .find(|(_, node)| node.work == design)
        .map(|(_, node)| node.clone())
        .expect("attempt");
    assert!(graph.revision() > design_attempt.expected_revision);
    executor_delivers(
        &mut graph,
        receipt(
            "att_design",
            design.as_str(),
            vec![FakeProvider::alpha().output("art_design", "Design", &design_doc(), 12)],
            vec![],
            vec![],
        ),
    );
    assert_eq!(status_of(&graph, research.as_str()), WorkStatus::Completed);
    assert_eq!(status_of(&graph, design.as_str()), WorkStatus::Completed);
}

// ---------------------------------------------------------------------------
// Scenario 5
// ---------------------------------------------------------------------------

#[test]
fn scenario_5_overlapping_write_scopes_cannot_run_concurrently() {
    let mut graph = run_to_requirements(FakeProvider::alpha());

    // A plan with two tasks whose write scopes overlap (src and src/core).
    let overlapping_plan = PlanDoc {
        tasks: vec![
            PlanTask {
                slug: "writer".to_string(),
                kind: TaskKind::Implement,
                objective: "implement the writer".to_string(),
                requirement_refs: vec![vref("art_req-csv", 1)],
                write_scopes: vec!["src".to_string()],
            },
            PlanTask {
                slug: "renderer".to_string(),
                kind: TaskKind::Implement,
                objective: "implement the renderer".to_string(),
                requirement_refs: vec![vref("art_req-csv", 1)],
                write_scopes: vec!["src/core".to_string()],
            },
        ],
    };
    apply(
        &mut graph,
        &DomainFact::ArtifactVersionPublished {
            artifact: "art_plan".parse().expect("id"),
            version: relay_domain::ArtifactVersion::FIRST,
            title: Some("Plan".to_string()),
            content: FakeProvider::alpha().content(&overlapping_plan),
            digest: FakeProvider::alpha().digest(13),
            derived_from: vec![vref("art_req-csv", 1)],
            published_at_ms: 3_000,
        },
    );
    expand(&mut graph);
    let tasks = works_of_kind(&graph, wf::KIND_IMPLEMENT);
    assert_eq!(tasks.len(), 2, "one work item per plan task");

    // Scheduling never puts overlapping writers in one batch: with the
    // first-release cap of one writer, the second waits with a reason.
    let recommendation = relay_engine::recommend(
        &graph,
        &SchedulingPolicy {
            autonomy: Autonomy::Safe,
            max_concurrent_writers: 1,
        },
    );
    assert!(
        recommendation
            .parallel
            .iter()
            .all(|entry| entry.write_scopes.is_empty())
    );
    assert_eq!(recommendation.serialized.len(), 1);
    assert_eq!(recommendation.deferred.len(), 1);
    assert!(recommendation.deferred[0].reason.contains("serialize"));
}

// ---------------------------------------------------------------------------
// Scenario 6
// ---------------------------------------------------------------------------

#[test]
fn scenario_6_model_proposals_rejected_for_cycles_duplicates_capabilities_scope() {
    let mut graph = run_to_implemented(FakeProvider::alpha());
    let implement = sole_work_of_kind(&graph, wf::KIND_IMPLEMENT);

    // Cycles: a second task that the first requires cannot require it back.
    let second = WorkItem::new(
        "work_second".parse().expect("id"),
        relay_domain::WorkKind::parse(wf::KIND_IMPLEMENT).expect("kind"),
        relay_domain::ExecutorClass::Workspace,
        "second task",
        CompletionRule::Receipt,
    );
    apply(&mut graph, &DomainFact::WorkDeclared { work: second });
    apply(
        &mut graph,
        &DomainFact::DependencyAsserted {
            from: "work_second".parse().expect("id"),
            to: implement.clone(),
            kind: DependencyKind::Requires,
        },
    );
    let rejection = relay_engine::validate_graph_change(
        &graph,
        &GraphChangeProposal::DeclareDependency {
            edge: DependencyEdge::new(
                implement.clone(),
                "work_second".parse().expect("id"),
                DependencyKind::Requires,
            ),
        },
    )
    .err()
    .expect("cycle rejected");
    assert!(matches!(rejection, Rejected::Cycle { .. }));

    // Duplicates.
    let duplicate = relay_engine::validate_graph_change(
        &graph,
        &GraphChangeProposal::DeclareWork {
            work: graph.work(&implement).cloned().expect("work").work.clone(),
        },
    )
    .err()
    .expect("duplicate rejected");
    assert!(matches!(duplicate, Rejected::Duplicate { .. }));

    // Unauthorized scope expansion: the implement task's ceiling is its
    // declared write scope (src).
    let rejection = relay_engine::validate_graph_change(
        &graph,
        &GraphChangeProposal::ExpandScope {
            work: implement.clone(),
            added_write_scopes: vec![ScopePath::parse("docs").expect("scope")],
            added_operations: vec![],
            reason: "model wants to edit docs".to_string(),
        },
    )
    .err()
    .expect("scope expansion rejected");
    assert!(matches!(
        rejection,
        Rejected::UnauthorizedScopeExpansion { .. }
    ));

    // Excessive capability: an operation beyond the ceiling.
    let rejection = relay_engine::validate_graph_change(
        &graph,
        &GraphChangeProposal::ExpandScope {
            work: implement.clone(),
            added_write_scopes: vec![],
            added_operations: vec![relay_domain::Operation::Network],
            reason: "model wants network".to_string(),
        },
    )
    .err()
    .expect("operation rejected");
    assert!(matches!(
        rejection,
        Rejected::CapabilityCeilingExceeded { .. }
    ));

    // Unknown references.
    let rejection = relay_engine::validate_graph_change(
        &graph,
        &GraphChangeProposal::DeclareDependency {
            edge: DependencyEdge::new(
                "work_ghost".parse().expect("id"),
                implement.clone(),
                DependencyKind::Requires,
            ),
        },
    )
    .err()
    .expect("unknown endpoint rejected");
    assert!(matches!(rejection, Rejected::UnknownReference { .. }));
}

// ---------------------------------------------------------------------------
// Scenario 7
// ---------------------------------------------------------------------------

#[test]
fn scenario_7_attempt_cannot_operate_outside_its_bounded_grant() {
    let mut graph = run_to_implemented(FakeProvider::alpha());
    let research = sole_work_of_kind(&graph, wf::KIND_RESEARCH);

    // A grant that does not cover the work's read scope cannot start: the
    // fold rejects it before any execution exists.
    let fact = DomainFact::AttemptStarted {
        attempt: "att_short".parse().expect("id"),
        work: research.clone(),
        expected_revision: graph.revision(),
        grant: relay_domain::CapabilityGrant::read_only(
            "grant_short".parse().expect("id"),
            vec![ScopePath::parse("docs").expect("scope")],
        ),
        started_at_ms: 5_000,
    };
    let mut probe = graph.clone();
    let error = probe
        .apply(&fact)
        .err()
        .expect("insufficient grant rejected");
    assert!(matches!(
        error,
        relay_domain::FactError::CapabilityExceeded { .. }
    ));

    // A covering grant starts, and stays inside its boundary.
    start(&mut graph, "att_research", research.as_str());
    let attempt = graph
        .attempts()
        .find(|(_, node)| node.work == research)
        .map(|(_, node)| node.clone())
        .expect("attempt");
    assert!(
        attempt.grant.write_paths.is_empty(),
        "read-only work never carries write authority"
    );
    assert_eq!(
        attempt.grant.delivery,
        relay_domain::DeliveryAuthority::None
    );
}

// ---------------------------------------------------------------------------
// Scenario 8
// ---------------------------------------------------------------------------

#[test]
fn scenario_8_verification_failure_creates_corrective_work_then_ready_again() {
    let mut graph = run_to_implemented(FakeProvider::alpha());
    let verify = works_of_kind(&graph, wf::KIND_VERIFY)
        .into_iter()
        .find(|id| {
            let node = graph.work(id).cloned().expect("work");
            matches!(
                &node.work.completion,
                CompletionRule::CriterionSatisfied { criterion } if criterion.artifact.as_str() == "art_cri-valid"
            )
        })
        .expect("valid-rows verifier exists");

    // The first verification run completes as an account, but its evidence
    // refutes the criterion — so the work is NOT completed, and it proposes
    // targeted corrective work plus the dependency that gates re-runs.
    start(&mut graph, "att_verify1", verify.as_str());
    let corrective = WorkItem::new(
        "work_fix".parse().expect("id"),
        relay_domain::WorkKind::parse(wf::KIND_CORRECTIVE).expect("kind"),
        relay_domain::ExecutorClass::Workspace,
        "Fix the malformed-row export the failing run exposed",
        CompletionRule::Receipt,
    );
    executor_delivers(
        &mut graph,
        receipt(
            "att_verify1",
            verify.as_str(),
            vec![],
            vec![criterion_evidence(
                "ev_run1",
                ("art_cri-valid", 1),
                EvidenceOutcome::Fail,
                "2 of 5 report shapes export malformed rows",
            )],
            vec![
                GraphChangeProposal::DeclareWork { work: corrective },
                GraphChangeProposal::DeclareDependency {
                    edge: DependencyEdge::new(
                        verify.clone(),
                        "work_fix".parse().expect("id"),
                        DependencyKind::Requires,
                    ),
                },
            ],
        ),
    );
    assert_ne!(status_of(&graph, verify.as_str()), WorkStatus::Completed);
    let explained = relay_engine::explain(&graph, &verify)
        .ok()
        .expect("explainable");
    assert!(
        explained.detail.contains("work_fix"),
        "blocked on the corrective work: {explained}"
    );

    // The corrective work runs and completes.
    let fix = "work_fix".parse::<WorkItemId>().expect("id");
    start(&mut graph, "att_fix", fix.as_str());
    executor_delivers(
        &mut graph,
        receipt(
            "att_fix",
            fix.as_str(),
            vec![FakeProvider::alpha().output(
                "art_fix-notes",
                "Correction notes",
                &wf::ResearchDoc {
                    question: "what was fixed?".to_string(),
                    findings: vec!["quote escaping in nested values".to_string()],
                    references: vec!["src/csv/writer.rs".to_string()],
                },
                14,
            )],
            vec![],
            vec![],
        ),
    );

    // Verification is ready again, with the failing evidence on record.
    assert_eq!(status_of(&graph, verify.as_str()), WorkStatus::Ready);
    let explained = relay_engine::explain(&graph, &verify)
        .ok()
        .expect("explainable");
    assert!(explained.detail.contains("ev_run1"), "{explained}");

    // The stale failure is retired by an explicit, policy-validated
    // invalidation; a re-run passes and only then completes the work.
    let applied = relay_engine::validate_graph_change(
        &graph,
        &GraphChangeProposal::DeclareInvalidation {
            target: InvalidationTarget::Evidence("ev_run1".parse().expect("id")),
            cause: InvalidationCause::Declared {
                note: "observed against the implementation the corrective work replaced"
                    .to_string(),
            },
            changed_input: None,
        },
    )
    .ok()
    .expect("evidence retirement accepted");
    for fact in &applied.facts {
        apply(&mut graph, fact);
    }
    start(&mut graph, "att_verify2", verify.as_str());
    executor_delivers(
        &mut graph,
        receipt(
            "att_verify2",
            verify.as_str(),
            vec![],
            vec![criterion_evidence(
                "ev_run2",
                ("art_cri-valid", 1),
                EvidenceOutcome::Pass,
                "all 5 report shapes export valid rows",
            )],
            vec![],
        ),
    );
    assert_eq!(status_of(&graph, verify.as_str()), WorkStatus::Completed);
}

// ---------------------------------------------------------------------------
// Scenario 9
// ---------------------------------------------------------------------------

#[test]
fn scenario_9_crash_recovery_resumes_without_false_completion() {
    // Keep the authoritative fact history so prefixes can be re-folded.
    let mut facts: Vec<DomainFact> = vec![project("prj_crash"), goal("goal_crash", "export CSV")];
    let mut graph = fold(&facts);
    facts.extend(expand(&mut graph));
    graph = fold(&facts);
    let clarify = sole_work_of_kind(&graph, wf::KIND_CLARIFY);
    facts.push(human_resolves(
        &graph,
        clarify.as_str(),
        "CSV export",
        "the outcome",
    ));
    graph = fold(&facts);
    let discover = sole_work_of_kind(&graph, wf::KIND_DISCOVER);

    // Crash window 1: the attempt started (a durable fact) but no terminal
    // fact followed. Re-folding the surviving prefix shows an active
    // attempt — never a completion.
    facts.push(start(&mut graph, "att_discover", discover.as_str()));
    let graph = fold(&facts);
    let evaluation = relay_engine::evaluate(&graph);
    assert_eq!(evaluation.active_attempts.len(), 1);
    assert_eq!(
        status_of(&graph, discover.as_str()),
        WorkStatus::Running {
            attempt: "att_discover".parse().unwrap()
        }
    );
    assert!(!evaluation.completed.contains(&discover));

    // Reconciliation marks the orphaned attempt abandoned; the work goes
    // back to ready without pretending anything succeeded.
    facts.push(DomainFact::AttemptConcluded {
        attempt: "att_discover".parse().expect("id"),
        conclusion: relay_domain::AttemptConclusion::Abandoned {
            reason: "no terminal fact after restart".to_string(),
        },
    });
    let mut graph = fold(&facts);
    assert_eq!(status_of(&graph, discover.as_str()), WorkStatus::Ready);
    let explained = relay_engine::explain(&graph, &discover)
        .ok()
        .expect("explainable");
    assert!(explained.detail.contains("abandoned"), "{explained}");

    // Crash window 2: the receipt WAS accepted durably. Acknowledged
    // success survives (the post-acceptance fold reports the completion the
    // facts prove); the pre-acceptance prefix never claims it.
    let pre_run_prefix = facts.clone();
    facts.push(start(&mut graph, "att_discover2", discover.as_str()));
    facts.extend(executor_delivers(
        &mut graph,
        receipt(
            "att_discover2",
            discover.as_str(),
            vec![FakeProvider::alpha().output(
                "art_brief",
                "Opportunity brief",
                &BriefDoc {
                    problem: "reports are stuck".to_string(),
                    audience: "analysts".to_string(),
                    outcome: "one-click CSV export".to_string(),
                    open_questions: vec![],
                },
                15,
            )],
            vec![],
            vec![],
        ),
    ));
    let graph = fold(&facts);
    assert_eq!(status_of(&graph, discover.as_str()), WorkStatus::Completed);
    let prefix = ProjectGraph::from_facts(&pre_run_prefix).expect("prefix folds");
    assert_eq!(
        status_of(&prefix, discover.as_str()),
        WorkStatus::Ready,
        "the pre-acceptance prefix claims nothing"
    );
}

// ---------------------------------------------------------------------------
// Scenario 10
// ---------------------------------------------------------------------------

#[test]
fn scenario_10_relay_explains_every_work_item_and_every_rejection() {
    let mut graph = run_to_implemented(FakeProvider::alpha());

    // Every work item explains its current state.
    for (id, _) in graph.works() {
        let explanation = relay_engine::explain(&graph, id).expect("explainable");
        let rendered = explanation.to_string();
        assert!(
            rendered.starts_with(id.as_str()) && rendered.contains(" is "),
            "explanations are work-scoped: {rendered}"
        );
    }
    let verify = works_of_kind(&graph, wf::KIND_VERIFY)
        .into_iter()
        .find(|id| {
            let node = graph.work(id).cloned().expect("work");
            matches!(
                &node.work.completion,
                CompletionRule::CriterionSatisfied { criterion } if criterion.artifact.as_str() == "art_cri-valid"
            )
        })
        .expect("valid-rows verifier exists");
    let explained = relay_engine::explain(&graph, &verify).expect("explainable");
    assert!(
        explained.detail.contains("implement-task"),
        "verifier explanations name what they wait on: {explained}"
    );

    // After a failing run, the still-open state explains too.
    start(&mut graph, "att_verify1", verify.as_str());
    executor_delivers(
        &mut graph,
        receipt(
            "att_verify1",
            verify.as_str(),
            vec![],
            vec![criterion_evidence(
                "ev_bad",
                ("art_cri-valid", 1),
                EvidenceOutcome::Fail,
                "malformed rows",
            )],
            vec![],
        ),
    );
    for (id, _) in graph.works() {
        relay_engine::explain(&graph, id).expect("explainable after failure");
    }
    let explained = relay_engine::explain(&graph, &verify).expect("explainable");
    assert!(
        explained.detail.contains("ev_bad"),
        "the refuted criterion is named: {explained}"
    );

    // Unknown work and rejected proposals explain as well.
    let error = relay_engine::explain(&graph, &"work_ghost".parse().expect("id"))
        .err()
        .expect("unknown work errors");
    assert!(error.to_string().contains("work_ghost"));
    let rejection = relay_engine::validate_graph_change(
        &graph,
        &GraphChangeProposal::DeclareDependency {
            edge: DependencyEdge::new(verify.clone(), verify.clone(), DependencyKind::Requires),
        },
    )
    .err()
    .expect("self edge rejected");
    assert!(
        rejection.to_string().contains("itself"),
        "rejections carry their reason: {rejection}"
    );
}

// ---------------------------------------------------------------------------
// Scenario 11
// ---------------------------------------------------------------------------

#[test]
fn scenario_11_provider_swap_leaves_graph_and_policy_semantics_unchanged() {
    let alpha_graph = run_to_requirements(FakeProvider::alpha());
    let beta_graph = run_to_requirements(FakeProvider::beta());

    // The providers produced different bytes …
    let alpha_content = serde_json::to_string(&content_of(&alpha_graph, "art_brief")).unwrap();
    let beta_content = serde_json::to_string(&content_of(&beta_graph, "art_brief")).unwrap();
    assert_ne!(alpha_content, beta_content);

    // … but identical typed meaning and identical derived state.
    let alpha_requirement =
        RequirementDoc::from_content(&content_of(&alpha_graph, "art_req-csv")).expect("decodes");
    let beta_requirement =
        RequirementDoc::from_content(&content_of(&beta_graph, "art_req-csv")).expect("decodes");
    assert_eq!(alpha_requirement, beta_requirement);
    let alpha_eval = relay_engine::evaluate(&alpha_graph);
    let beta_eval = relay_engine::evaluate(&beta_graph);
    let summarize = |evaluation: &relay_engine::Evaluation| {
        evaluation
            .ready
            .iter()
            .map(|entry| (entry.work.clone(), entry.executor))
            .collect::<Vec<_>>()
    };
    assert_eq!(summarize(&alpha_eval), summarize(&beta_eval));
    assert_eq!(alpha_eval.awaiting_human, beta_eval.awaiting_human);
    assert_eq!(alpha_eval.completed, beta_eval.completed);

    // And the same next round of workflow proposals.
    let catalog = WorkflowCatalog::builtin();
    assert_eq!(
        wf::propose_next(&alpha_graph, &catalog).len(),
        wf::propose_next(&beta_graph, &catalog).len()
    );
}

// ---------------------------------------------------------------------------
// Scenario 12
// ---------------------------------------------------------------------------

#[test]
fn scenario_12_delivery_prepared_but_blocked_on_explicit_human_decision() {
    let mut graph = run_to_implemented(FakeProvider::alpha());

    // Integration completes; every criterion's verifier passes.
    let integrate = sole_work_of_kind(&graph, wf::KIND_INTEGRATE);
    start(&mut graph, "att_integrate", integrate.as_str());
    executor_delivers(
        &mut graph,
        receipt(
            "att_integrate",
            integrate.as_str(),
            vec![FakeProvider::alpha().output(
                "art_integration-notes",
                "Integration notes",
                &wf::ResearchDoc {
                    question: "what was integrated?".to_string(),
                    findings: vec!["writer wired into the report pipeline".to_string()],
                    references: vec!["src/reports".to_string()],
                },
                17,
            )],
            vec![],
            vec![],
        ),
    );
    for (index, verify) in works_of_kind(&graph, wf::KIND_VERIFY).iter().enumerate() {
        let node = graph.work(verify).cloned().expect("work");
        let criterion = match &node.work.completion {
            CompletionRule::CriterionSatisfied { criterion } => criterion.clone(),
            other => panic!("unexpected completion rule {other:?}"),
        };
        let attempt = format!("att_verify-pass{index}");
        start(&mut graph, &attempt, verify.as_str());
        executor_delivers(
            &mut graph,
            receipt(
                &attempt,
                verify.as_str(),
                vec![],
                vec![relay_domain::Evidence::new(
                    format!("ev_pass{index}")
                        .parse()
                        .expect("valid evidence id"),
                    relay_domain::EvidenceKind::TestRun,
                    relay_domain::EvidenceTarget::ArtifactVersion(criterion),
                    EvidenceOutcome::Pass,
                    "criterion met",
                    7_000,
                )],
                vec![],
            ),
        );
    }

    // All criteria satisfied: expansion proposes delivery-evidence
    // preparation, which the system executor performs deterministically.
    expand(&mut graph);
    let prep = sole_work_of_kind(&graph, wf::KIND_DELIVERY_PREP);
    start(&mut graph, "att_prep", prep.as_str());
    executor_delivers(
        &mut graph,
        receipt(
            "att_prep",
            prep.as_str(),
            vec![FakeProvider::alpha().output(
                "art_delivery",
                "Delivery report",
                &DeliveryDoc {
                    summary: "CSV export with verified criteria".to_string(),
                    includes: vec![vref("art_req-csv", 1)],
                    evidence: vec!["ev_pass0".parse().expect("id")],
                    checks: vec!["criteria evidence reviewed".to_string()],
                },
                16,
            )],
            vec![],
            vec![],
        ),
    );
    assert_eq!(status_of(&graph, prep.as_str()), WorkStatus::Completed);

    // The report renders with its authorization note.
    let delivery = DeliveryDoc::from_content(&content_of(&graph, "art_delivery")).expect("decodes");
    let report = wf::render_delivery_report(&wf::DeliveryReportView {
        reference: &vref("art_delivery", 1),
        doc: &delivery,
    });
    assert!(report.contains("# Delivery Report — art_delivery@1"));
    assert!(report.contains("Delivery requires an explicit human authorization decision."));

    // Expansion asks one human to authorize; nothing delivers before that.
    expand(&mut graph);
    let authorization = sole_work_of_kind(&graph, wf::KIND_DELIVERY_AUTH);
    let evaluation = relay_engine::evaluate(&graph);
    assert_eq!(evaluation.awaiting_human.len(), 1);
    assert_eq!(evaluation.awaiting_human[0].work, authorization);
    // No grant in the graph carries delivery authority of any kind.
    for (_, attempt) in graph.attempts() {
        assert_eq!(
            attempt.grant.delivery,
            relay_domain::DeliveryAuthority::None
        );
    }

    // The human holds: the package stays undelivered, and the project
    // still reaches its completion state (everything required is done).
    let fact = human_resolves(
        &graph,
        authorization.as_str(),
        "hold",
        "release window is next week",
    );
    apply(&mut graph, &fact);
    assert_eq!(
        status_of(&graph, authorization.as_str()),
        WorkStatus::Completed
    );
    let evaluation = relay_engine::evaluate(&graph);
    assert!(matches!(
        evaluation.completion,
        relay_engine::ProjectCompletion::Complete
    ));
}
