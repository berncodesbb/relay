//! Relay's built-in software-delivery workflow.
//!
//! The one opinionated workflow compiled into Rust (no public DSL, no
//! plugin system): work definitions for discovery through delivery
//! authorization, structured addressable requirement/criterion/decision/
//! task/evidence schemas, deterministic grouped Markdown views (opportunity
//! brief, PRD, TRD, plan, decision log, verification report, delivery
//! report), per-kind completion and invalidation rules, and controlled
//! graph-expansion proposals. Discovery, specification, design, planning,
//! implementation, verification, and delivery remain recognizable as
//! grouped views over work — never as a global workflow-mode state machine.
//!
//! This crate is pure: renderers return [`String`]s, publication is an
//! app-layer concern, and every graph change still passes
//! `relay_engine::validate_graph_change`.

pub mod catalog;
pub mod expansion;
pub mod import;
pub mod render;
pub mod schemas;

pub use catalog::{
    ApprovalRule, CeilingTemplate, CompletionTemplate, InputPolicy, InstantiateError, Multiplicity,
    Phase, WorkDefinition, WorkflowCatalog, approval_decision, has_live_instance, human_decision,
    instantiate_executor, instantiate_human, instantiate_verification, live_instance_count,
    requires_edge, validates_edge,
};
pub use expansion::{artifacts_with_schema, criterion_failing, propose_next};
pub use import::{
    ImportError, ImportedCriterion, ImportedRequirement, PrdDocument, PrdImport, PrdRevision,
    import_prd, prd_revision, revision_proposal,
};
pub use render::{
    BriefView, CriterionEntry, CriterionStatus, DecisionLogEntry, DecisionLogView,
    DeliveryReportView, PlanView, PrdView, RequirementEntry, TrdView, VerificationReportView,
    render_brief, render_decision_log, render_delivery_report, render_plan, render_prd, render_trd,
    render_verification_report,
};
pub use schemas::{
    BRIEF_SCHEMA, BriefDoc, CRITERION_SCHEMA, CriterionDoc, DELIVERY_SCHEMA,
    DESIGN_DECISION_SCHEMA, DESIGN_SCHEMA, DeliveryDoc, DesignDecisionDoc, DesignDoc, PLAN_SCHEMA,
    PlanDoc, PlanTask, REQUIREMENT_SCHEMA, RESEARCH_SCHEMA, RequirementDoc, ResearchDoc,
    SchemaError, StructuredDoc, TaskKind, VerificationMethod,
};

/// Kind label: survey the problem and draft the opportunity brief.
pub const KIND_DISCOVER: &str = "discover-opportunity";
/// Kind label: one focused human clarification.
pub const KIND_CLARIFY: &str = "clarify-unknowns";
/// Kind label: expand an approved brief into requirements and criteria.
pub const KIND_REQUIREMENTS: &str = "define-requirements";
/// Kind label: read-only repository and prior-art inspection.
pub const KIND_RESEARCH: &str = "inspect-repository";
/// Kind label: design the technical approach.
pub const KIND_DESIGN: &str = "design-approach";
/// Kind label: decompose the design into bounded tasks.
pub const KIND_PLAN: &str = "decompose-plan";
/// Kind label: implement one bounded task.
pub const KIND_IMPLEMENT: &str = "implement-task";
/// Kind label: integrate completed tasks.
pub const KIND_INTEGRATE: &str = "integrate-changes";
/// Kind label: collect evidence against one criterion version.
pub const KIND_VERIFY: &str = "verify-criteria";
/// Kind label: fix what failing evidence showed is broken.
pub const KIND_CORRECTIVE: &str = "create-corrective-work";
/// Kind label: assemble the delivery package and evidence.
pub const KIND_DELIVERY_PREP: &str = "prepare-delivery-evidence";
/// Kind label: ask one human to authorize (or hold) delivery.
pub const KIND_DELIVERY_AUTH: &str = "request-delivery-authorization";
/// Kind label: approve (or send back) one exact artifact version.
pub const KIND_APPROVE: &str = "approve-artifact";
/// Kind label: turn an edited Markdown view into a new artifact version.
pub const KIND_REVISE: &str = "revise-artifact";

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::ok_expect,
    clippy::err_expect
)] // test-only conveniences, house style
mod tests {
    use super::*;
    use relay_domain::WorkKind;

    #[test]
    fn catalog_labels_parse() {
        let catalog = WorkflowCatalog::builtin();
        for label in [
            KIND_DISCOVER,
            KIND_CLARIFY,
            KIND_REQUIREMENTS,
            KIND_RESEARCH,
            KIND_DESIGN,
            KIND_PLAN,
            KIND_IMPLEMENT,
            KIND_INTEGRATE,
            KIND_VERIFY,
            KIND_CORRECTIVE,
            KIND_DELIVERY_PREP,
            KIND_DELIVERY_AUTH,
            KIND_APPROVE,
            KIND_REVISE,
        ] {
            assert!(
                WorkKind::parse(label).is_ok(),
                "{label} must be a valid kind label"
            );
            assert!(
                catalog.get(label).is_some(),
                "{label} must be in the catalog"
            );
        }
    }
}
