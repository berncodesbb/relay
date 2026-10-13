//! Controlled graph expansion: the built-in workflow's deterministic
//! "what should exist next?" brain.
//!
//! [`propose_next`] inspects the graph and returns the proposals the
//! opinionated workflow would make at this point — never more than one
//! round's worth, never applied here. Every proposal still passes
//! `relay_engine::validate_graph_change`; executors and this module share
//! the same policy gate. There is no mode progression: each rule fires on
//! what the graph currently proves (published versions, resolved
//! decisions, satisfied criteria), so re-running after any fact produces
//! the next coherent round.

use relay_domain::{
    ArtifactVersionRef, CompletionRule, Decision, DecisionId, DecisionSubject, EvidenceOutcome,
    EvidenceTarget, GraphChangeProposal, InputRef, ProjectGraph, ScopePath, WorkItem, WorkItemId,
};

use crate::catalog::{self, WorkflowCatalog};
use crate::schemas::{self, PlanDoc, StructuredDoc};

/// The id prefix minted for work items.
pub const WORK_ID_PREFIX: &str = "work_";
/// The id prefix minted for decisions.
pub const DECISION_ID_PREFIX: &str = "dec_";

/// Mint a deterministic id for a new instance of a kind: counts every
/// work item ever declared with that kind (terminal included, so ids are
/// never reused after cancellation or supersession).
fn mint_work_id(graph: &ProjectGraph, kind: &str) -> WorkItemId {
    let count = graph
        .works()
        .filter(|(_, node)| node.work.kind.as_str() == kind)
        .count();
    WorkItemId::parse(&format!("{WORK_ID_PREFIX}{kind}-{}", count + 1))
        .unwrap_or_else(|_| fallback_work_id())
}

// Unreachable while the embedded literals are valid ids; keeps id minting
// total without inventing a fallback state.
#[allow(clippy::expect_used, clippy::ok_expect)]
fn fallback_work_id() -> WorkItemId {
    // Unreachable while kind labels are valid slugs; keeps minting total.
    WorkItemId::parse("work_minted")
        .unwrap_or_else(|_| WorkItemId::parse("work_x").ok().expect("literal parses"))
}

// Same invariant as `fallback_work_id`.
#[allow(clippy::expect_used, clippy::ok_expect)]
fn mint_decision_id(graph: &ProjectGraph, kind: &str) -> DecisionId {
    let count = graph
        .decisions()
        .filter(|(id, _)| id.as_str().contains(kind))
        .count();
    DecisionId::parse(&format!("{DECISION_ID_PREFIX}{kind}-{}", count + 1)).unwrap_or_else(|_| {
        DecisionId::parse("dec_minted")
            .ok()
            .expect("literal parses")
    })
}

/// Current versions of every artifact whose current content carries the
/// given schema, in artifact-id order.
pub fn artifacts_with_schema(graph: &ProjectGraph, schema: &str) -> Vec<ArtifactVersionRef> {
    graph
        .artifacts()
        .filter_map(|(id, node)| {
            let version = node.current_version();
            let record = node.versions.get(&version)?;
            (schemas::content_schema(&record.content) == Some(schema))
                .then(|| ArtifactVersionRef::new(id.clone(), version))
        })
        .collect()
}

/// Whether an approval decision resolved a specific artifact version with
/// the given choice.
fn approved(graph: &ProjectGraph, reference: &ArtifactVersionRef, choice: &str) -> bool {
    graph.decisions().any(|(_, node)| {
        matches!(
            &node.decision.subject,
            DecisionSubject::ArtifactVersion(subject) if subject == reference
        ) && node
            .resolution
            .as_ref()
            .is_some_and(|resolution| resolution.choice == choice)
    })
}

/// Whether any work item exists with the given kind label.
fn any_work_of_kind(graph: &ProjectGraph, kind: &str) -> bool {
    graph
        .works()
        .any(|(_, node)| node.work.kind.as_str() == kind)
}

/// Whether a live (non-terminal) work item of the kind exists.
fn live_work_of_kind(graph: &ProjectGraph, kind: &str) -> bool {
    catalog::has_live_instance(graph, kind)
}

/// Whether an implement-task work instance exists for a plan-task slug
/// (matched on the expected-output label, which carries the slug).
fn task_work_exists(graph: &ProjectGraph, kind: &str, slug: &str) -> bool {
    graph.works().any(|(_, node)| {
        node.work.kind.as_str() == kind
            && node
                .work
                .expected_outputs
                .iter()
                .any(|output| output.label == slug)
    })
}

/// Work items of a kind whose expected-output label is the slug.
fn task_works(graph: &ProjectGraph, kind: &str, slug: &str) -> Vec<WorkItemId> {
    graph
        .works()
        .filter(|(_, node)| {
            node.work.kind.as_str() == kind
                && node
                    .work
                    .expected_outputs
                    .iter()
                    .any(|output| output.label == slug)
        })
        .map(|(id, _)| id.clone())
        .collect()
}

fn executor_work(
    catalog: &WorkflowCatalog,
    graph: &ProjectGraph,
    kind: &'static str,
    objective: impl Into<String>,
    inputs: Vec<InputRef>,
    write_scopes: Vec<ScopePath>,
    dependencies: Vec<relay_domain::DependencyEdge>,
) -> Option<WorkItem> {
    let definition = catalog.get(kind)?;
    let id = mint_work_id(graph, kind);
    catalog::instantiate_executor(
        definition,
        id,
        objective.into(),
        inputs,
        write_scopes,
        dependencies,
    )
    .ok()
}

/// Like [`executor_work`], but with a caller-minted id (used when a round
/// mints several instances of one kind).
fn executor_work_with_id(
    catalog: &WorkflowCatalog,
    id: WorkItemId,
    kind: &'static str,
    objective: impl Into<String>,
    inputs: Vec<InputRef>,
    write_scopes: Vec<ScopePath>,
    dependencies: Vec<relay_domain::DependencyEdge>,
) -> Option<WorkItem> {
    let definition = catalog.get(kind)?;
    catalog::instantiate_executor(
        definition,
        id,
        objective,
        inputs,
        write_scopes,
        dependencies,
    )
    .ok()
}

fn human_work_with_decision(
    catalog: &WorkflowCatalog,
    graph: &ProjectGraph,
    kind: &'static str,
    objective: impl Into<String>,
    inputs: Vec<InputRef>,
    decision: Decision,
) -> Option<(WorkItem, Decision)> {
    let definition = catalog.get(kind)?;
    let decision_id = decision.id.clone();
    let id = mint_work_id(graph, kind);
    let work = catalog::instantiate_human(definition, id, objective, decision_id, inputs).ok()?;
    Some((work, decision))
}

/// Propose the next round of work for the built-in workflow.
///
/// Rules, each guarded so at most one round's worth of proposals returns:
///
/// 1. Fresh goal, no work: discovery + one focused clarification, discovery
///    requiring the answer.
/// 2. Published brief without an approval gate: one `approve-artifact`
///    human work + decision on that exact version.
/// 3. Approved brief, no requirements work: `define-requirements`.
/// 4. Approved brief, no research: one `inspect-repository` (read-only).
/// 5. Requirements exist, no design: `design-approach` (model, read-only —
///    becomes ready alongside any research; they never depend on each
///    other).
/// 6. Design exists, no plan: `decompose-plan`.
/// 7. Plan tasks without work instances: one `implement-task` per
///    unpublished task (write scopes from the task), plus
///    `integrate-changes` once at least one task work completed.
/// 8. Criteria with implement work underway: one `verify-criteria` per
///    criterion version, requiring the covering task works.
/// 9. All criteria satisfied: `prepare-delivery-evidence`.
/// 10. Delivery report published: `request-delivery-authorization` (human
///     decision — delivery itself is never grantable).
pub fn propose_next(graph: &ProjectGraph, catalog: &WorkflowCatalog) -> Vec<GraphChangeProposal> {
    let mut proposals: Vec<GraphChangeProposal> = Vec::new();

    // Rule 1: discovery + clarification for a fresh goal.
    if graph.goal().is_some() && graph.works().count() == 0 {
        if let Some(((clarify, decision), discover)) = human_work_with_decision(
            catalog,
            graph,
            crate::KIND_CLARIFY,
            "Answer the open question blocking discovery",
            vec![],
            clarification_decision(graph),
        )
        .zip(executor_work(
            catalog,
            graph,
            crate::KIND_DISCOVER,
            "Survey the problem space and draft an opportunity brief",
            vec![],
            vec![],
            vec![],
        )) {
            let clarify_id = clarify.id.clone();
            proposals.push(GraphChangeProposal::DeclareWork { work: clarify });
            proposals.push(GraphChangeProposal::OpenDecision { decision });
            let mut discover = discover;
            discover
                .dependencies
                .push(catalog::requires_edge(&discover.id, &clarify_id));
            proposals.push(GraphChangeProposal::DeclareWork { work: discover });
        }
        return proposals;
    }

    let briefs = artifacts_with_schema(graph, schemas::BRIEF_SCHEMA);
    let brief = briefs.first().cloned();

    // Rule 2: approval gate for a published brief.
    if let Some(brief_ref) = &brief
        && !approval_gate_live(graph, brief_ref)
    {
        let decision = catalog::approval_decision(
            mint_decision_id(graph, crate::KIND_APPROVE),
            brief_ref,
            format!("Approve opportunity brief {brief_ref} as the basis for requirements?"),
        );
        if let Some((work, decision)) = human_work_with_decision(
            catalog,
            graph,
            crate::KIND_APPROVE,
            format!("Approve (or send back) opportunity brief {brief_ref}"),
            vec![InputRef::Exact(brief_ref.clone())],
            decision,
        ) {
            proposals.push(GraphChangeProposal::DeclareWork { work });
            proposals.push(GraphChangeProposal::OpenDecision { decision });
        }
    }

    let brief_approved = brief
        .as_ref()
        .is_some_and(|reference| approved(graph, reference, "approve"));

    // Rule 3: requirements from an approved brief.
    let requirements = artifacts_with_schema(graph, schemas::REQUIREMENT_SCHEMA);
    if brief_approved
        && requirements.is_empty()
        && !live_work_of_kind(graph, crate::KIND_REQUIREMENTS)
        && let Some(brief_ref) = &brief
        && let Some(work) = executor_work(
            catalog,
            graph,
            crate::KIND_REQUIREMENTS,
            "Expand the approved brief into requirements and acceptance criteria",
            vec![InputRef::Exact(brief_ref.clone())],
            vec![],
            vec![],
        )
    {
        proposals.push(GraphChangeProposal::DeclareWork { work });
    }

    // Rule 4: read-only research alongside specification/design.
    if brief_approved
        && !live_work_of_kind(graph, crate::KIND_RESEARCH)
        && let Some(brief_ref) = &brief
        && let Some(mut work) = executor_work(
            catalog,
            graph,
            crate::KIND_RESEARCH,
            "Inspect the repository and prior art for the delivery ahead",
            vec![InputRef::Exact(brief_ref.clone())],
            vec![],
            vec![],
        )
    {
        work.read_scopes = vec![ScopePath::root()];
        proposals.push(GraphChangeProposal::DeclareWork { work });
    }

    // Rule 5: design once requirements exist.
    let designs = artifacts_with_schema(graph, schemas::DESIGN_SCHEMA);
    if !requirements.is_empty()
        && designs.is_empty()
        && !live_work_of_kind(graph, crate::KIND_DESIGN)
        && let Some(work) = executor_work(
            catalog,
            graph,
            crate::KIND_DESIGN,
            "Design the technical approach for the requirements",
            requirements.iter().cloned().map(InputRef::Exact).collect(),
            vec![],
            vec![],
        )
    {
        proposals.push(GraphChangeProposal::DeclareWork { work });
    }

    // Rule 6: plan once a design exists.
    let plans = artifacts_with_schema(graph, schemas::PLAN_SCHEMA);
    if !designs.is_empty()
        && plans.is_empty()
        && !live_work_of_kind(graph, crate::KIND_PLAN)
        && let Some(work) = executor_work(
            catalog,
            graph,
            crate::KIND_PLAN,
            "Decompose the design into bounded tasks",
            designs
                .iter()
                .chain(requirements.iter())
                .cloned()
                .map(InputRef::Exact)
                .collect(),
            vec![],
            vec![],
        )
    {
        proposals.push(GraphChangeProposal::DeclareWork { work });
    }

    // Rules 7–8 run off the current plan.
    if let Some(plan_ref) = plans.first() {
        let Some(record) = graph.version_record(plan_ref) else {
            return proposals;
        };
        let Ok(plan) = PlanDoc::from_content(&record.content) else {
            return proposals;
        };

        // Rule 7a: one implement-task per unpublished plan task. Ids stay
        // unique within the round by offsetting the minted ordinal.
        let implement_base = graph
            .works()
            .filter(|(_, node)| node.work.kind.as_str() == crate::KIND_IMPLEMENT)
            .count();
        let mut round_offset = 0usize;
        for task in &plan.tasks {
            if task_work_exists(graph, crate::KIND_IMPLEMENT, &task.slug) {
                continue;
            }
            round_offset += 1;
            let mut inputs = vec![InputRef::Exact(plan_ref.clone())];
            inputs.extend(task.requirement_refs.iter().cloned().map(InputRef::Exact));
            let write_scopes = task
                .write_scopes
                .iter()
                .filter_map(|scope| ScopePath::parse(scope).ok())
                .collect::<Vec<_>>();
            let id = WorkItemId::parse(&format!(
                "{WORK_ID_PREFIX}{}-{}",
                crate::KIND_IMPLEMENT,
                implement_base + round_offset
            ))
            .unwrap_or_else(|_| fallback_work_id());
            if let Some(mut work) = executor_work_with_id(
                catalog,
                id,
                crate::KIND_IMPLEMENT,
                format!("Implement plan task {}", task.slug),
                inputs,
                write_scopes,
                vec![],
            ) {
                // The expected-output label carries the task slug so
                // expansion can match instances to plan tasks.
                work.expected_outputs = vec![relay_domain::ExpectedOutput {
                    label: task.slug.clone(),
                    description: task.objective.clone(),
                }];
                proposals.push(GraphChangeProposal::DeclareWork { work });
            }
        }

        // Rule 7b: integration once a task completed.
        let any_task_completed = plan.tasks.iter().any(|task| {
            task_works(graph, crate::KIND_IMPLEMENT, &task.slug)
                .iter()
                .any(|id| {
                    graph.work(id).is_some_and(|node| {
                        matches!(
                            relay_engine::work_status(graph, node),
                            relay_engine::WorkStatus::Completed
                        )
                    })
                })
        });
        if any_task_completed
            && !live_work_of_kind(graph, crate::KIND_INTEGRATE)
            && !any_work_of_kind(graph, crate::KIND_INTEGRATE)
        {
            let write_scopes: Vec<ScopePath> = plan
                .tasks
                .iter()
                .flat_map(|task| task.write_scopes.iter())
                .filter_map(|scope| ScopePath::parse(scope).ok())
                .collect();
            if let Some(work) = executor_work(
                catalog,
                graph,
                crate::KIND_INTEGRATE,
                "Integrate completed tasks into one coherent change set",
                vec![InputRef::Exact(plan_ref.clone())],
                write_scopes,
                vec![],
            ) {
                proposals.push(GraphChangeProposal::DeclareWork { work });
            }
        }

        // Rule 8: verify-criteria per criterion whose requirement is under
        // implementation.
        let criteria = artifacts_with_schema(graph, schemas::CRITERION_SCHEMA);
        for criterion_ref in criteria {
            if verification_live_for(graph, &criterion_ref) {
                continue;
            }
            let Some(record) = graph.version_record(&criterion_ref) else {
                continue;
            };
            let Ok(criterion) = schemas::CriterionDoc::from_content(&record.content) else {
                continue;
            };
            // Covering tasks: implement works whose task references this
            // criterion's requirement.
            let mut covering: Vec<WorkItemId> = Vec::new();
            for task in &plan.tasks {
                if task.requirement_refs.contains(&criterion.requirement) {
                    covering.extend(task_works(graph, crate::KIND_IMPLEMENT, &task.slug));
                }
            }
            if covering.is_empty() {
                continue;
            }
            let dependencies = covering
                .iter()
                .map(|id| catalog::requires_edge(&verification_id_for(graph, &criterion_ref), id))
                .collect::<Vec<_>>();
            if let Some(definition) = catalog.get(crate::KIND_VERIFY) {
                let id = verification_id_for(graph, &criterion_ref);
                if let Ok(work) = catalog::instantiate_verification(
                    definition,
                    id,
                    format!("Verify criterion {}", criterion.slug),
                    criterion_ref.clone(),
                    dependencies,
                ) {
                    proposals.push(GraphChangeProposal::DeclareWork { work });
                }
            }
        }
    }

    // Rule 9: delivery evidence once every criterion stands satisfied.
    let criteria = artifacts_with_schema(graph, schemas::CRITERION_SCHEMA);
    let all_satisfied = !criteria.is_empty()
        && criteria
            .iter()
            .all(|reference| relay_engine::status::criterion_satisfied(graph, reference));
    if all_satisfied
        && !live_work_of_kind(graph, crate::KIND_DELIVERY_PREP)
        && !any_work_of_kind(graph, crate::KIND_DELIVERY_PREP)
        && let Some(work) = executor_work(
            catalog,
            graph,
            crate::KIND_DELIVERY_PREP,
            "Assemble the delivery package and its evidence",
            criteria.iter().cloned().map(InputRef::Exact).collect(),
            vec![],
            vec![],
        )
    {
        proposals.push(GraphChangeProposal::DeclareWork { work });
    }

    // Rule 10: delivery authorization for a prepared package.
    let deliveries = artifacts_with_schema(graph, schemas::DELIVERY_SCHEMA);
    if let Some(delivery_ref) = deliveries.first()
        && !live_work_of_kind(graph, crate::KIND_DELIVERY_AUTH)
        && !any_work_of_kind(graph, crate::KIND_DELIVERY_AUTH)
    {
        let decision = catalog::approval_decision(
            mint_decision_id(graph, crate::KIND_DELIVERY_AUTH),
            delivery_ref,
            format!("Authorize delivery of the package at {delivery_ref}?"),
        );
        // Re-shape the approval options to delivery semantics.
        let mut decision = decision;
        decision.options = vec![
            relay_domain::DecisionOption {
                label: "authorize".to_string(),
                description: "deliver the prepared package".to_string(),
            },
            relay_domain::DecisionOption {
                label: "hold".to_string(),
                description: "keep the package undelivered".to_string(),
            },
        ];
        if let Some((work, decision)) = human_work_with_decision(
            catalog,
            graph,
            crate::KIND_DELIVERY_AUTH,
            format!("Authorize (or hold) delivery of {delivery_ref}"),
            vec![InputRef::Exact(delivery_ref.clone())],
            decision,
        ) {
            proposals.push(GraphChangeProposal::DeclareWork { work });
            proposals.push(GraphChangeProposal::OpenDecision { decision });
        }
    }

    proposals
}

fn clarification_decision(graph: &ProjectGraph) -> Decision {
    let id = mint_decision_id(graph, crate::KIND_CLARIFY);
    let question = graph
        .goal()
        .map(|goal| {
            format!(
                "The goal is ambiguous: \"{}\" — what does success mean concretely?",
                goal.outcome
            )
        })
        .unwrap_or_else(|| "What does success mean concretely?".to_string());
    Decision::new(
        id,
        DecisionSubject::Goal(goal_id(graph)),
        question,
        vec![],
        0,
    )
}

// Same infallible-literal invariant as the id fallbacks above.
#[allow(clippy::expect_used, clippy::ok_expect)]
fn goal_id(graph: &ProjectGraph) -> relay_domain::GoalId {
    graph
        .goal()
        .map(|goal| goal.id.clone())
        .unwrap_or_else(|| relay_domain::GoalId::parse("goal_unnamed").expect("literal parses"))
}

/// Whether an approval decision (open, resolved, or superseded) or a live
/// approval work already targets this exact version — one gate per version,
/// so a resolved approval never re-proposes.
fn approval_gate_live(graph: &ProjectGraph, reference: &ArtifactVersionRef) -> bool {
    graph.decisions().any(|(_, decision)| {
        matches!(
            &decision.decision.subject,
            DecisionSubject::ArtifactVersion(subject) if subject == reference
        )
    }) || graph.works().any(|(_, node)| {
        node.work.kind.as_str() == crate::KIND_APPROVE
            && !node.is_declared_terminal()
            && node.work.inputs.iter().any(
                |input| matches!(input, InputRef::Exact(reference_) if reference_ == reference),
            )
    })
}

/// A deterministic id for the verification instance of one criterion
/// version: `work_verify-criteria-<artifact>-v<version>` — one live
/// verifier per exact version.
fn verification_id_for(graph: &ProjectGraph, reference: &ArtifactVersionRef) -> WorkItemId {
    let _ = graph;
    WorkItemId::parse(&format!(
        "{WORK_ID_PREFIX}{}-{}-v{}",
        crate::KIND_VERIFY,
        id_slug(reference.artifact.as_str()),
        reference.version.get()
    ))
    .unwrap_or_else(|_| fallback_work_id())
}

/// Whether any verification instance (live or terminal) is bound to this
/// exact criterion version — one verifier per version; a satisfied criterion
/// never re-proposes, and a republished criterion gets a fresh version and
/// therefore a fresh verifier.
fn verification_live_for(graph: &ProjectGraph, reference: &ArtifactVersionRef) -> bool {
    graph.works().any(|(_, node)| {
        node.work.kind.as_str() == crate::KIND_VERIFY
            && matches!(
                &node.work.completion,
                CompletionRule::CriterionSatisfied { criterion } if criterion == reference
            )
    })
}

/// Make an artifact id safe for embedding in a work-id slug.
fn id_slug(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// Whether any evidence outcome for the criterion version is failing
/// (exposed for the app's "needs corrective work" hint).
pub fn criterion_failing(graph: &ProjectGraph, reference: &ArtifactVersionRef) -> bool {
    graph
        .evidence_for(&EvidenceTarget::ArtifactVersion(reference.clone()))
        .iter()
        .any(|e| e.outcome == EvidenceOutcome::Fail)
}
