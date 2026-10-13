//! Deterministic Markdown views for published artifact versions.
//!
//! Structured content is authoritative; these renderings are what people
//! read. Grouped documents (brief, PRD, TRD, plan, verification report,
//! delivery report) reuse the byte-stable `relay-workflows` renderers,
//! assembling their view inputs from graph state; single-document schemas
//! without a grouped renderer (research findings) get a local deterministic
//! view. An edited view re-enters the graph only through the controlled
//! import (`relay import-prd`).

use relay_domain::{ArtifactVersionRef, EvidenceId, EvidenceOutcome, ProjectGraph};
use relay_workflows::{
    BRIEF_SCHEMA, BriefDoc, BriefView, CRITERION_SCHEMA, CriterionDoc, CriterionEntry,
    CriterionStatus, DELIVERY_SCHEMA, DESIGN_DECISION_SCHEMA, DESIGN_SCHEMA, DeliveryDoc,
    DeliveryReportView, DesignDecisionDoc, DesignDoc, PLAN_SCHEMA, PlanDoc, PlanView, PrdView,
    REQUIREMENT_SCHEMA, RESEARCH_SCHEMA, RequirementDoc, RequirementEntry, ResearchDoc,
    StructuredDoc, TrdView, VerificationReportView, render_brief, render_delivery_report,
    render_plan, render_prd, render_trd, render_verification_report,
};

/// Render the Markdown view of one published artifact version, when its
/// schema is one the built-in workflow knows. Returns `None` for unknown
/// schemas (the structured content stays the answer) or when the content
/// does not decode as its own schema claims.
pub fn render_artifact_view(
    graph: &ProjectGraph,
    reference: &ArtifactVersionRef,
) -> Option<String> {
    let record = graph.version_record(reference)?;
    let schema = relay_workflows::schemas::content_schema(&record.content)?;
    let goal = graph
        .goal()
        .map(|goal| goal.outcome.as_str())
        .unwrap_or_default();
    match schema {
        BRIEF_SCHEMA => {
            let doc = BriefDoc::from_content(&record.content).ok()?;
            Some(render_brief(&BriefView {
                goal,
                reference,
                doc: &doc,
            }))
        }
        REQUIREMENT_SCHEMA => {
            let doc = RequirementDoc::from_content(&record.content).ok()?;
            let owned = owned_criteria_for_requirement(graph, reference);
            let entries = criterion_entries(graph, &owned);
            let entry = RequirementEntry {
                reference,
                doc: &doc,
                criteria: &entries,
            };
            Some(render_prd(&PrdView {
                goal,
                brief: Some(&doc.brief),
                requirements: &[entry],
            }))
        }
        CRITERION_SCHEMA => {
            let doc = CriterionDoc::from_content(&record.content).ok()?;
            let owned = vec![OwnedCriterion {
                reference: reference.clone(),
                doc,
                evidence: standing_evidence(graph, reference),
            }];
            let entries = criterion_entries(graph, &owned);
            let overall = overall_of(&entries);
            Some(render_verification_report(&VerificationReportView {
                criteria: &entries,
                overall,
            }))
        }
        DESIGN_SCHEMA => {
            let doc = DesignDoc::from_content(&record.content).ok()?;
            let decisions = design_decisions(graph);
            let borrowed: Vec<(&ArtifactVersionRef, &DesignDecisionDoc)> = decisions
                .iter()
                .map(|(reference, doc)| (reference, doc))
                .collect();
            Some(render_trd(&TrdView {
                reference,
                doc: &doc,
                decisions: &borrowed,
            }))
        }
        DESIGN_DECISION_SCHEMA => {
            let doc = DesignDecisionDoc::from_content(&record.content).ok()?;
            let carrier = DesignDoc {
                approach: "(standalone recorded decision)".to_string(),
                components: Vec::new(),
                risks: Vec::new(),
            };
            Some(render_trd(&TrdView {
                reference,
                doc: &carrier,
                decisions: &[(reference, &doc)],
            }))
        }
        PLAN_SCHEMA => {
            let doc = PlanDoc::from_content(&record.content).ok()?;
            Some(render_plan(&PlanView {
                reference,
                doc: &doc,
            }))
        }
        RESEARCH_SCHEMA => {
            let doc = ResearchDoc::from_content(&record.content).ok()?;
            Some(render_research(reference, &doc))
        }
        DELIVERY_SCHEMA => {
            let doc = DeliveryDoc::from_content(&record.content).ok()?;
            Some(render_delivery_report(&DeliveryReportView {
                reference,
                doc: &doc,
            }))
        }
        _ => None,
    }
}

/// One criterion's owned rendering input: its identity, parsed document, and
/// standing evidence.
struct OwnedCriterion {
    reference: ArtifactVersionRef,
    doc: CriterionDoc,
    evidence: Vec<(EvidenceId, EvidenceOutcome)>,
}

/// Every criterion version whose requirement pin names `requirement`, in
/// artifact-id order.
fn owned_criteria_for_requirement(
    graph: &ProjectGraph,
    requirement: &ArtifactVersionRef,
) -> Vec<OwnedCriterion> {
    let mut owned = Vec::new();
    for (artifact, node) in graph.artifacts() {
        let version = node.current_version();
        let reference = ArtifactVersionRef::new(artifact.clone(), version);
        let Some(record) = node.versions.get(&version) else {
            continue;
        };
        if relay_workflows::schemas::content_schema(&record.content) != Some(CRITERION_SCHEMA) {
            continue;
        }
        let Ok(doc) = CriterionDoc::from_content(&record.content) else {
            continue;
        };
        if &doc.requirement != requirement {
            continue;
        }
        owned.push(OwnedCriterion {
            evidence: standing_evidence(graph, &reference),
            reference,
            doc,
        });
    }
    owned
}

/// Borrow the renderer's view inputs from the owned data. The engine
/// derives each status; this function only assembles.
fn criterion_entries<'a>(
    graph: &'a ProjectGraph,
    owned: &'a [OwnedCriterion],
) -> Vec<CriterionEntry<'a>> {
    owned
        .iter()
        .map(|item| CriterionEntry {
            reference: &item.reference,
            doc: &item.doc,
            status: criterion_status(graph, item),
            evidence: &item.evidence,
        })
        .collect()
}

/// Derive one criterion's status: satisfied stands as `pass`, standing
/// failing evidence as `fail`, anything else `unevaluated`.
fn criterion_status(graph: &ProjectGraph, item: &OwnedCriterion) -> CriterionStatus {
    if relay_engine::status::criterion_satisfied(graph, &item.reference) {
        CriterionStatus::Pass
    } else if item
        .evidence
        .iter()
        .any(|(_, outcome)| *outcome == EvidenceOutcome::Fail)
    {
        CriterionStatus::Fail
    } else {
        CriterionStatus::Unevaluated
    }
}

/// The verification report's overall outcome over its entries.
fn overall_of(entries: &[CriterionEntry<'_>]) -> &'static str {
    if entries
        .iter()
        .all(|entry| entry.status == CriterionStatus::Pass)
    {
        "pass"
    } else if entries
        .iter()
        .any(|entry| entry.status == CriterionStatus::Fail)
    {
        "fail"
    } else {
        "incomplete"
    }
}

/// Standing (valid) evidence for one criterion version, in evidence-id
/// order.
fn standing_evidence(
    graph: &ProjectGraph,
    reference: &ArtifactVersionRef,
) -> Vec<(EvidenceId, EvidenceOutcome)> {
    let mut evidence: Vec<(EvidenceId, EvidenceOutcome)> = graph
        .evidence_for(&relay_domain::EvidenceTarget::ArtifactVersion(
            reference.clone(),
        ))
        .into_iter()
        .filter(|item| {
            matches!(
                relay_engine::status::evidence_status(graph, item),
                relay_engine::status::EvidenceStatus::Valid
            )
        })
        .map(|item| (item.id.clone(), item.outcome))
        .collect();
    evidence.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
    evidence
}

/// Every recorded design-decision version, in artifact-id order.
fn design_decisions(graph: &ProjectGraph) -> Vec<(ArtifactVersionRef, DesignDecisionDoc)> {
    let mut decisions = Vec::new();
    for (artifact, node) in graph.artifacts() {
        let version = node.current_version();
        let reference = ArtifactVersionRef::new(artifact.clone(), version);
        let Some(record) = node.versions.get(&version) else {
            continue;
        };
        if relay_workflows::schemas::content_schema(&record.content) != Some(DESIGN_DECISION_SCHEMA)
        {
            continue;
        }
        if let Ok(doc) = DesignDecisionDoc::from_content(&record.content) {
            decisions.push((reference, doc));
        }
    }
    decisions
}

/// A deterministic local view for research findings: the research schema is
/// single-document, so the built-in workflow carries no grouped renderer
/// for it.
fn render_research(reference: &ArtifactVersionRef, doc: &ResearchDoc) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Research — {reference}\n\n"));
    out.push_str(&format!("Question: {}\n\n", doc.question));
    if !doc.findings.is_empty() {
        out.push_str("## Findings\n\n");
        for finding in &doc.findings {
            out.push_str(&format!("- {finding}\n"));
        }
        out.push('\n');
    }
    if !doc.references.is_empty() {
        out.push_str("## References\n\n");
        for source in &doc.references {
            out.push_str(&format!("- {source}\n"));
        }
        out.push('\n');
    }
    out
}
