//! Deterministic grouped Markdown views.
//!
//! Each renderer is a pure function of its typed input: identical input
//! yields byte-identical Markdown (LF newlines, caller-ordered lists). The
//! structured artifact content stays authoritative; these views are what
//! people read, and the only way an edited view re-enters the graph is the
//! controlled import in [`crate::import`].

use relay_domain::{ArtifactVersionRef, DecisionId, EvidenceId, EvidenceOutcome};

use crate::schemas::{
    BriefDoc, CriterionDoc, DeliveryDoc, DesignDecisionDoc, DesignDoc, PlanDoc, PlanTask,
    RequirementDoc, VerificationMethod,
};

/// The derived state of one criterion in a report view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CriterionStatus {
    /// Standing evidence passes.
    Pass,
    /// Standing evidence fails.
    Fail,
    /// No standing evidence yet.
    Unevaluated,
}

impl CriterionStatus {
    fn tag(self) -> &'static str {
        match self {
            CriterionStatus::Pass => "pass",
            CriterionStatus::Fail => "fail",
            CriterionStatus::Unevaluated => "unevaluated",
        }
    }
}

fn method_name(method: VerificationMethod) -> &'static str {
    match method {
        VerificationMethod::AutomatedTest => "automated-test",
        VerificationMethod::ManualCheck => "manual-check",
        VerificationMethod::Inspection => "inspection",
    }
}

/// Input for the opportunity-brief view.
pub struct BriefView<'a> {
    /// The goal statement.
    pub goal: &'a str,
    /// The exact brief version being rendered.
    pub reference: &'a ArtifactVersionRef,
    /// The brief content.
    pub doc: &'a BriefDoc,
}

/// Render the opportunity brief.
pub fn render_brief(view: &BriefView) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Opportunity Brief — {}\n", view.reference));
    out.push('\n');
    out.push_str(&format!("Goal: {}\n", view.goal));
    out.push('\n');
    out.push_str(&format!("Problem: {}\n", view.doc.problem));
    out.push_str(&format!("Audience: {}\n", view.doc.audience));
    out.push_str(&format!("Outcome: {}\n", view.doc.outcome));
    if !view.doc.open_questions.is_empty() {
        out.push_str("\n## Open questions\n\n");
        for question in &view.doc.open_questions {
            out.push_str(&format!("- {question}\n"));
        }
    }
    out
}

/// One criterion with its derived status and standing evidence, for PRD and
/// verification views.
pub struct CriterionEntry<'a> {
    /// The exact criterion version.
    pub reference: &'a ArtifactVersionRef,
    /// The criterion content.
    pub doc: &'a CriterionDoc,
    /// Derived satisfaction state.
    pub status: CriterionStatus,
    /// Standing evidence (id + outcome), caller-ordered.
    pub evidence: &'a [(EvidenceId, EvidenceOutcome)],
}

/// One requirement with its criteria, for the PRD view.
pub struct RequirementEntry<'a> {
    /// The exact requirement version.
    pub reference: &'a ArtifactVersionRef,
    /// The requirement content.
    pub doc: &'a RequirementDoc,
    /// The requirement's criteria.
    pub criteria: &'a [CriterionEntry<'a>],
}

/// Input for the PRD view.
pub struct PrdView<'a> {
    /// The goal statement.
    pub goal: &'a str,
    /// The brief version the requirements trace to.
    pub brief: Option<&'a ArtifactVersionRef>,
    /// The requirements, caller-ordered.
    pub requirements: &'a [RequirementEntry<'a>],
}

/// Render the PRD: requirements and their acceptance criteria as one
/// coherent document over addressable versions.
pub fn render_prd(view: &PrdView) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Product Requirements — {}\n", view.goal));
    out.push('\n');
    if let Some(brief) = view.brief {
        out.push_str(&format!("Brief: {brief}\n\n"));
    }
    for requirement in view.requirements {
        out.push_str(&format!(
            "## Requirement: {} — {}\n\n",
            requirement.doc.slug, requirement.reference
        ));
        out.push_str(&format!("{}\n\n", requirement.doc.statement));
        out.push_str(&format!("Rationale: {}\n\n", requirement.doc.rationale));
        for criterion in requirement.criteria {
            out.push_str(&format!(
                "### Criterion: {} — {} [{}]\n\n",
                criterion.doc.slug,
                criterion.reference,
                criterion.status.tag()
            ));
            out.push_str(&format!("{}\n\n", criterion.doc.statement));
            out.push_str(&format!(
                "Method: {}\n\n",
                method_name(criterion.doc.method)
            ));
        }
    }
    out
}

/// Input for the TRD view.
pub struct TrdView<'a> {
    /// The exact design version.
    pub reference: &'a ArtifactVersionRef,
    /// The design content.
    pub doc: &'a DesignDoc,
    /// Recorded technical decisions, caller-ordered.
    pub decisions: &'a [(&'a ArtifactVersionRef, &'a DesignDecisionDoc)],
}

/// Render the TRD: design plus its recorded decisions.
pub fn render_trd(view: &TrdView) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Technical Design — {}\n\n", view.reference));
    out.push_str(&format!("{}\n\n", view.doc.approach));
    if !view.doc.components.is_empty() {
        out.push_str("## Components\n\n");
        for component in &view.doc.components {
            out.push_str(&format!("- {component}\n"));
        }
        out.push('\n');
    }
    if !view.doc.risks.is_empty() {
        out.push_str("## Risks\n\n");
        for risk in &view.doc.risks {
            out.push_str(&format!("- {risk}\n"));
        }
        out.push('\n');
    }
    if !view.decisions.is_empty() {
        out.push_str("## Decisions\n\n");
        for (reference, decision) in view.decisions {
            out.push_str(&format!(
                "### Decision: {} — {}\n\n",
                decision.slug, reference
            ));
            out.push_str(&format!("Question: {}\n", decision.question));
            out.push_str(&format!("Choice: {}\n", decision.choice));
            out.push_str(&format!("Rationale: {}\n", decision.rationale));
            out.push_str(&format!("Consequences: {}\n\n", decision.consequences));
        }
    }
    out
}

/// Input for the plan view.
pub struct PlanView<'a> {
    /// The exact plan version.
    pub reference: &'a ArtifactVersionRef,
    /// The plan content.
    pub doc: &'a PlanDoc,
}

fn task_kind_name(task: &PlanTask) -> &'static str {
    match task.kind {
        crate::schemas::TaskKind::Implement => "implement",
        crate::schemas::TaskKind::Integrate => "integrate",
        crate::schemas::TaskKind::Correct => "correct",
    }
}

/// Render the execution plan.
pub fn render_plan(view: &PlanView) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Execution Plan — {}\n\n", view.reference));
    for task in &view.doc.tasks {
        out.push_str(&format!(
            "## Task: {} [{}]\n\n",
            task.slug,
            task_kind_name(task)
        ));
        out.push_str(&format!("{}\n\n", task.objective));
        out.push_str(&format!("Writes: {}\n", task.write_scopes.join(", ")));
        let refs: Vec<String> = task
            .requirement_refs
            .iter()
            .map(|r| r.to_string())
            .collect();
        out.push_str(&format!("Satisfies: {}\n\n", refs.join(", ")));
    }
    out
}

/// One human-gate decision as the log renders it.
pub struct DecisionLogEntry<'a> {
    /// The decision identity.
    pub id: &'a DecisionId,
    /// The question.
    pub question: &'a str,
    /// The selectable option labels.
    pub options: &'a [String],
    /// The chosen option or answer text, when resolved.
    pub resolution: Option<&'a str>,
    /// The superseding decision, when superseded.
    pub superseded_by: Option<&'a DecisionId>,
}

/// Input for the decision-log view.
pub struct DecisionLogView<'a> {
    /// The entries, caller-ordered.
    pub entries: &'a [DecisionLogEntry<'a>],
}

/// Render the decision log of human-gate decisions.
pub fn render_decision_log(view: &DecisionLogView) -> String {
    let mut out = String::new();
    out.push_str("# Decision Log\n\n");
    for entry in view.entries {
        out.push_str(&format!("## {}: {}\n\n", entry.id, entry.question));
        out.push_str(&format!("Options: {}\n", entry.options.join(", ")));
        match (entry.resolution, entry.superseded_by) {
            (Some(choice), _) => out.push_str(&format!("Resolution: {choice}\n")),
            (None, Some(by)) => out.push_str(&format!("Superseded by: {by}\n")),
            (None, None) => out.push_str("Resolution: open\n"),
        }
        out.push('\n');
    }
    out
}

/// Input for the verification report.
pub struct VerificationReportView<'a> {
    /// The criteria with derived status, caller-ordered.
    pub criteria: &'a [CriterionEntry<'a>],
    /// Derived overall outcome: `pass`, `fail`, or `incomplete`.
    pub overall: &'a str,
}

/// Render the verification report over acceptance-criterion evidence.
pub fn render_verification_report(view: &VerificationReportView) -> String {
    let mut out = String::new();
    out.push_str("# Verification Report\n\n");
    out.push_str(&format!("Overall: {}\n\n", view.overall));
    for criterion in view.criteria {
        out.push_str(&format!(
            "## Criterion: {} — {} [{}]\n\n",
            criterion.doc.slug,
            criterion.reference,
            criterion.status.tag()
        ));
        out.push_str(&format!("{}\n\n", criterion.doc.statement));
        out.push_str(&format!("Method: {}\n", method_name(criterion.doc.method)));
        if criterion.evidence.is_empty() {
            out.push_str("Evidence: none\n\n");
        } else {
            let rendered: Vec<String> = criterion
                .evidence
                .iter()
                .map(|(id, outcome)| {
                    let tag = match outcome {
                        EvidenceOutcome::Pass => "pass",
                        EvidenceOutcome::Fail => "fail",
                        EvidenceOutcome::Info => "info",
                    };
                    format!("{id} ({tag})")
                })
                .collect();
            out.push_str(&format!("Evidence: {}\n\n", rendered.join(", ")));
        }
    }
    out
}

/// Input for the delivery report.
pub struct DeliveryReportView<'a> {
    /// The exact delivery-report version.
    pub reference: &'a ArtifactVersionRef,
    /// The report content.
    pub doc: &'a DeliveryDoc,
}

/// Render the delivery report. The trailing authorization note is
/// deliberate: Relay prepares, only an explicit human decision delivers.
pub fn render_delivery_report(view: &DeliveryReportView) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Delivery Report — {}\n\n", view.reference));
    out.push_str(&format!("{}\n\n", view.doc.summary));
    out.push_str("## Includes\n\n");
    for reference in &view.doc.includes {
        out.push_str(&format!("- {reference}\n"));
    }
    out.push('\n');
    out.push_str("## Evidence\n\n");
    for evidence in &view.doc.evidence {
        out.push_str(&format!("- {evidence}\n"));
    }
    out.push('\n');
    if !view.doc.checks.is_empty() {
        out.push_str("## Checks\n\n");
        for check in &view.doc.checks {
            out.push_str(&format!("- {check}\n"));
        }
        out.push('\n');
    }
    out.push_str("Delivery requires an explicit human authorization decision.\n");
    out
}
