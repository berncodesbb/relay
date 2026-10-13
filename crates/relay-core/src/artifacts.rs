//! Core artifact contracts: kinds, the dependency graph between them, and
//! deterministic downstream invalidation. Ported from `src/core/artifacts.ts`.

use serde::{Deserialize, Serialize};

/// All artifact kinds, in canonical order.
pub const ARTIFACT_KINDS: [ArtifactKind; 7] = [
    ArtifactKind::OpportunityBrief,
    ArtifactKind::DecisionLog,
    ArtifactKind::Prd,
    ArtifactKind::Trd,
    ArtifactKind::ExecutionPlan,
    ArtifactKind::VerificationReport,
    ArtifactKind::DeliveryReport,
];

/// One artifact kind. Serialized in kebab-case (`opportunity-brief`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ArtifactKind {
    /// Discovery output: problem, users, evidence, outcomes, assumptions, open questions.
    OpportunityBrief,
    /// Implementation output: the record of decisions and evidence.
    DecisionLog,
    /// Specification output: testable requirements.
    Prd,
    /// Technical-design output: architecture and interfaces.
    Trd,
    /// Planning output: the executable task graph.
    ExecutionPlan,
    /// Verification output: criterion-by-criterion evaluation.
    VerificationReport,
    /// Delivery output: the requirement-to-evidence trace.
    DeliveryReport,
}

impl ArtifactKind {
    /// The wire spelling of this kind.
    pub fn as_str(self) -> &'static str {
        match self {
            ArtifactKind::OpportunityBrief => "opportunity-brief",
            ArtifactKind::DecisionLog => "decision-log",
            ArtifactKind::Prd => "prd",
            ArtifactKind::Trd => "trd",
            ArtifactKind::ExecutionPlan => "execution-plan",
            ArtifactKind::VerificationReport => "verification-report",
            ArtifactKind::DeliveryReport => "delivery-report",
        }
    }

    /// Parse the wire spelling of a kind.
    pub fn parse(value: &str) -> Option<Self> {
        ARTIFACT_KINDS
            .iter()
            .copied()
            .find(|kind| kind.as_str() == value)
    }

    /// The enum options as a Zod-style quoted list for validation messages.
    pub fn zod_options() -> String {
        crate::modes::zod_option_list(ARTIFACT_KINDS.iter().map(|kind| kind.as_str()))
    }
}

/// Upstream artifacts that must exist (and normally be approved) before an
/// artifact of a given kind is created. Used for deterministic downstream
/// invalidation: revising an approved upstream artifact marks every transitive
/// dependent stale until it is regenerated and re-approved.
pub fn artifact_dependencies(kind: ArtifactKind) -> &'static [ArtifactKind] {
    match kind {
        ArtifactKind::OpportunityBrief => &[],
        ArtifactKind::DecisionLog => &[ArtifactKind::OpportunityBrief],
        ArtifactKind::Prd => &[ArtifactKind::OpportunityBrief],
        ArtifactKind::Trd => &[ArtifactKind::Prd],
        ArtifactKind::ExecutionPlan => &[ArtifactKind::Prd, ArtifactKind::Trd],
        ArtifactKind::VerificationReport => &[ArtifactKind::ExecutionPlan],
        ArtifactKind::DeliveryReport => &[
            ArtifactKind::Prd,
            ArtifactKind::Trd,
            ArtifactKind::ExecutionPlan,
            ArtifactKind::VerificationReport,
        ],
    }
}

/// Artifact kinds that act as contractual checkpoints requiring explicit approval.
pub const CONTRACTUAL_ARTIFACT_KINDS: [ArtifactKind; 5] = [
    ArtifactKind::OpportunityBrief,
    ArtifactKind::Prd,
    ArtifactKind::Trd,
    ArtifactKind::ExecutionPlan,
    ArtifactKind::VerificationReport,
];

/// Whether `kind` is a contractual checkpoint requiring explicit approval.
pub fn is_contractual_artifact_kind(kind: ArtifactKind) -> bool {
    CONTRACTUAL_ARTIFACT_KINDS.contains(&kind)
}

/// All kinds transitively downstream of `kind` (excluding itself), in
/// canonical kind order.
pub fn downstream_artifact_kinds(kind: ArtifactKind) -> Vec<ArtifactKind> {
    // The graph is acyclic and tiny; the TypeScript version collects a set in
    // canonical order and so does this one.
    let mut result: Vec<ArtifactKind> = Vec::new();
    let mut changed = true;
    while changed {
        changed = false;
        for candidate in ARTIFACT_KINDS {
            if result.contains(&candidate) || candidate == kind {
                continue;
            }
            if artifact_dependencies(candidate).contains(&kind)
                || artifact_dependencies(candidate)
                    .iter()
                    .any(|upstream| result.contains(upstream))
            {
                result.push(candidate);
                changed = true;
            }
        }
    }
    result
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    fn names(kinds: &[ArtifactKind]) -> Vec<&'static str> {
        kinds.iter().map(|kind| kind.as_str()).collect()
    }

    #[test]
    fn derives_the_dependency_graph_and_downstream_sets() {
        assert_eq!(
            names(artifact_dependencies(ArtifactKind::ExecutionPlan)),
            vec!["prd", "trd"]
        );
        let mut prd_downstream = names(&downstream_artifact_kinds(ArtifactKind::Prd));
        prd_downstream.sort_unstable();
        assert_eq!(
            prd_downstream,
            vec![
                "delivery-report",
                "execution-plan",
                "trd",
                "verification-report"
            ]
        );
        let mut brief_downstream =
            names(&downstream_artifact_kinds(ArtifactKind::OpportunityBrief));
        brief_downstream.sort_unstable();
        assert_eq!(
            brief_downstream,
            vec![
                "decision-log",
                "delivery-report",
                "execution-plan",
                "prd",
                "trd",
                "verification-report"
            ]
        );
        assert!(downstream_artifact_kinds(ArtifactKind::DeliveryReport).is_empty());
    }

    #[test]
    fn classifies_contractual_artifact_kinds() {
        assert!(is_contractual_artifact_kind(ArtifactKind::Prd));
        assert!(!is_contractual_artifact_kind(ArtifactKind::DecisionLog));
        assert!(!is_contractual_artifact_kind(ArtifactKind::DeliveryReport));
        assert_eq!(CONTRACTUAL_ARTIFACT_KINDS.len(), 5);
    }
}
