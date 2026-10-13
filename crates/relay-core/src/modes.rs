//! Workflow mode contracts.
//!
//! Modes are the seven stages a product idea travels through. Each mode
//! declares the artifact it produces, the inputs it requires, the tool
//! capabilities it permits, and whether it may modify user files. These
//! contracts drive both the transition policy and the permission policy.
//! Ported verbatim from `src/core/modes.ts`.

use crate::artifacts::ArtifactKind;
use serde::{Deserialize, Serialize};

/// The seven workflow modes, in canonical order.
pub const WORKFLOW_MODES: [WorkflowMode; 7] = [
    WorkflowMode::Discovery,
    WorkflowMode::Specification,
    WorkflowMode::TechnicalDesign,
    WorkflowMode::Planning,
    WorkflowMode::Implementation,
    WorkflowMode::Verification,
    WorkflowMode::Delivery,
];

/// One workflow mode. Serialized in kebab-case (`technical-design`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WorkflowMode {
    /// Capture the problem, users, evidence, outcomes, assumptions, and open questions.
    Discovery,
    /// Turn the brief into testable requirements.
    Specification,
    /// Map requirements onto the target repository.
    TechnicalDesign,
    /// Produce an executable task graph.
    Planning,
    /// Execute approved, in-scope tasks.
    Implementation,
    /// Independently evaluate the resulting diff and evidence.
    Verification,
    /// Assemble the trace and prepare (but do not perform) delivery.
    Delivery,
}

impl WorkflowMode {
    /// The wire spelling of this mode.
    pub fn as_str(self) -> &'static str {
        match self {
            WorkflowMode::Discovery => "discovery",
            WorkflowMode::Specification => "specification",
            WorkflowMode::TechnicalDesign => "technical-design",
            WorkflowMode::Planning => "planning",
            WorkflowMode::Implementation => "implementation",
            WorkflowMode::Verification => "verification",
            WorkflowMode::Delivery => "delivery",
        }
    }

    /// Parse the wire spelling of a mode.
    pub fn parse(value: &str) -> Option<Self> {
        WORKFLOW_MODES
            .iter()
            .copied()
            .find(|mode| mode.as_str() == value)
    }

    /// The mode's contract (see [`MODE_CONTRACTS`]).
    pub fn contract(self) -> &'static ModeContract {
        MODE_CONTRACTS
            .iter()
            .find(|contract| contract.mode == self)
            .unwrap_or(&MODE_CONTRACTS[0])
    }

    /// The enum options as a Zod-style quoted list (`"a"|"b"`), used in
    /// validation messages to mirror the TypeScript diagnostics.
    pub fn zod_options() -> String {
        zod_option_list(WORKFLOW_MODES.iter().map(|mode| mode.as_str()))
    }
}

/// Format an iterator of options as Zod's `"a"|"b"` list for error messages.
pub(crate) fn zod_option_list<'a>(options: impl Iterator<Item = &'a str>) -> String {
    options
        .map(|option| format!("\"{option}\""))
        .collect::<Vec<_>>()
        .join("|")
}

/// Side-effect classes a tool can carry. Modes permit classes; policy gates execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ToolCapability {
    /// Read-only access to workspace files.
    Read,
    /// Create or modify workspace files.
    Write,
    /// Run bounded child processes.
    Execute,
}

impl ToolCapability {
    /// The wire spelling of this capability.
    pub fn as_str(self) -> &'static str {
        match self {
            ToolCapability::Read => "read",
            ToolCapability::Write => "write",
            ToolCapability::Execute => "execute",
        }
    }

    /// Parse the wire spelling of a capability.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "read" => Some(ToolCapability::Read),
            "write" => Some(ToolCapability::Write),
            "execute" => Some(ToolCapability::Execute),
            _ => None,
        }
    }
}

/// Canonical index of a mode in [`WORKFLOW_MODES`], used for ordering checks.
pub fn mode_index(mode: WorkflowMode) -> usize {
    WORKFLOW_MODES
        .iter()
        .position(|candidate| *candidate == mode)
        .unwrap_or(0)
}

/// The contract every workflow mode declares. Field-for-field port of the
/// TypeScript `ModeContract`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModeContract {
    /// The mode this contract describes.
    pub mode: WorkflowMode,
    /// Human-readable title.
    pub title: &'static str,
    /// Human-readable description.
    pub description: &'static str,
    /// Artifact this mode is expected to produce.
    pub produced_artifact: ArtifactKind,
    /// Kinds that must exist before this mode can run.
    pub required_inputs: &'static [ArtifactKind],
    /// Tool capabilities permitted while in this mode.
    pub permitted_tool_capabilities: &'static [ToolCapability],
    /// Whether work in this mode may modify files in the user's workspace.
    pub can_modify_user_files: bool,
    /// Human-readable description of the evidence required to complete the mode.
    pub completion_evidence: &'static str,
    /// The mode that precedes this one in canonical order, if any.
    pub previous_mode: Option<WorkflowMode>,
    /// The mode that follows this one in canonical order, if any.
    pub next_mode: Option<WorkflowMode>,
}

/// All seven mode contracts, in canonical mode order.
pub static MODE_CONTRACTS: [ModeContract; 7] = [
    ModeContract {
        mode: WorkflowMode::Discovery,
        title: "Discovery",
        description: "Capture the problem, users, evidence, outcomes, assumptions, and open questions.",
        produced_artifact: ArtifactKind::OpportunityBrief,
        required_inputs: &[],
        permitted_tool_capabilities: &[ToolCapability::Read],
        can_modify_user_files: false,
        completion_evidence: "An opportunity brief covering problem, users, evidence, outcomes, assumptions, constraints, and open questions.",
        previous_mode: None,
        next_mode: Some(WorkflowMode::Specification),
    },
    ModeContract {
        mode: WorkflowMode::Specification,
        title: "Specification",
        description: "Turn the brief into testable requirements with explicit exclusions and acceptance criteria.",
        produced_artifact: ArtifactKind::Prd,
        required_inputs: &[ArtifactKind::OpportunityBrief],
        permitted_tool_capabilities: &[ToolCapability::Read],
        can_modify_user_files: false,
        completion_evidence: "A PRD with testable requirements, exclusions, and acceptance criteria, ready for approval as a contract.",
        previous_mode: Some(WorkflowMode::Discovery),
        next_mode: Some(WorkflowMode::TechnicalDesign),
    },
    ModeContract {
        mode: WorkflowMode::TechnicalDesign,
        title: "Technical design",
        description: "Map requirements onto the target repository: architecture, interfaces, migrations, risks, rollout, rollback.",
        produced_artifact: ArtifactKind::Trd,
        required_inputs: &[ArtifactKind::Prd],
        permitted_tool_capabilities: &[ToolCapability::Read, ToolCapability::Execute],
        can_modify_user_files: false,
        completion_evidence: "A TRD grounded in repository facts with architecture, interfaces, migration notes, risks, and rollout/rollback.",
        previous_mode: Some(WorkflowMode::Specification),
        next_mode: Some(WorkflowMode::Planning),
    },
    ModeContract {
        mode: WorkflowMode::Planning,
        title: "Planning",
        description: "Produce an executable task graph with dependencies, affected files, checks, and integration points.",
        produced_artifact: ArtifactKind::ExecutionPlan,
        required_inputs: &[ArtifactKind::Prd, ArtifactKind::Trd],
        permitted_tool_capabilities: &[ToolCapability::Read, ToolCapability::Execute],
        can_modify_user_files: false,
        completion_evidence: "An execution plan whose tasks have dependencies, affected files, verification commands, and integration points.",
        previous_mode: Some(WorkflowMode::TechnicalDesign),
        next_mode: Some(WorkflowMode::Implementation),
    },
    ModeContract {
        mode: WorkflowMode::Implementation,
        title: "Implementation",
        description: "Execute approved, in-scope tasks while preserving user changes and recording evidence.",
        produced_artifact: ArtifactKind::DecisionLog,
        required_inputs: &[ArtifactKind::ExecutionPlan],
        permitted_tool_capabilities: &[
            ToolCapability::Read,
            ToolCapability::Write,
            ToolCapability::Execute,
        ],
        can_modify_user_files: true,
        completion_evidence: "All in-scope plan tasks executed with recorded file changes and check results, leaving user changes intact.",
        previous_mode: Some(WorkflowMode::Planning),
        next_mode: Some(WorkflowMode::Verification),
    },
    ModeContract {
        mode: WorkflowMode::Verification,
        title: "Verification",
        description: "Independently evaluate the resulting diff and evidence against every approved acceptance criterion.",
        produced_artifact: ArtifactKind::VerificationReport,
        required_inputs: &[ArtifactKind::ExecutionPlan],
        permitted_tool_capabilities: &[ToolCapability::Read, ToolCapability::Execute],
        can_modify_user_files: false,
        completion_evidence: "A verification report that scores every approved acceptance criterion pass/fail with evidence from the diff and test runs.",
        previous_mode: Some(WorkflowMode::Implementation),
        next_mode: Some(WorkflowMode::Delivery),
    },
    ModeContract {
        mode: WorkflowMode::Delivery,
        title: "Delivery",
        description: "Assemble the trace from problem to requirement to change to evidence, and prepare (but do not perform) delivery.",
        produced_artifact: ArtifactKind::DeliveryReport,
        required_inputs: &[
            ArtifactKind::ExecutionPlan,
            ArtifactKind::VerificationReport,
        ],
        permitted_tool_capabilities: &[ToolCapability::Read],
        can_modify_user_files: false,
        completion_evidence: "A delivery report linking requirements to changes and verification evidence, held before any external delivery action.",
        previous_mode: Some(WorkflowMode::Verification),
        next_mode: None,
    },
];

/// Autonomy bound: how far Relay may run without asking the user.
///
/// `off` pauses at every checkpoint; `next-checkpoint` runs until the next
/// artifact approval or intervention; `until:<mode>` runs until the workflow
/// enters that mode; `workflow` runs the whole workflow but still pauses for
/// consequential choices, approvals, destructive operations, and delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AutonomyBound {
    /// Pause at every checkpoint.
    Off,
    /// Run until the next artifact approval or intervention.
    NextCheckpoint,
    /// Run the whole workflow.
    Workflow,
    /// Run until the workflow enters the target mode.
    UntilMode(WorkflowMode),
}

/// The three named (non-`until:`) autonomy bounds.
pub const NAMED_AUTONOMY_BOUNDS: [&str; 3] = ["off", "next-checkpoint", "workflow"];

impl AutonomyBound {
    /// The wire spelling (`off`, `next-checkpoint`, `workflow`, `until:<mode>`).
    pub fn as_str(self) -> String {
        match self {
            AutonomyBound::Off => "off".to_string(),
            AutonomyBound::NextCheckpoint => "next-checkpoint".to_string(),
            AutonomyBound::Workflow => "workflow".to_string(),
            AutonomyBound::UntilMode(mode) => format!("until:{}", mode.as_str()),
        }
    }
}

impl Serialize for AutonomyBound {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.as_str())
    }
}

impl<'de> Deserialize<'de> for AutonomyBound {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        parse_autonomy_bound(&value).ok_or_else(|| {
            serde::de::Error::custom(
                "autonomy must be off, next-checkpoint, workflow, or until:<mode>",
            )
        })
    }
}

impl std::fmt::Display for AutonomyBound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.as_str())
    }
}

/// Parse an autonomy bound; returns `None` for anything that is not `off`,
/// `next-checkpoint`, `workflow`, or `until:<mode>` with a known mode.
pub fn parse_autonomy_bound(value: &str) -> Option<AutonomyBound> {
    match value {
        "off" => Some(AutonomyBound::Off),
        "next-checkpoint" => Some(AutonomyBound::NextCheckpoint),
        "workflow" => Some(AutonomyBound::Workflow),
        other => other
            .strip_prefix("until:")
            .and_then(WorkflowMode::parse)
            .map(AutonomyBound::UntilMode),
    }
}

/// Whether `value` parses as an autonomy bound.
pub fn is_autonomy_bound(value: &str) -> bool {
    parse_autonomy_bound(value).is_some()
}

/// Whether `value` is one of the seven workflow modes.
pub fn is_workflow_mode(value: &str) -> bool {
    WorkflowMode::parse(value).is_some()
}

/// Target mode for `until:` bounds, or `None` for named bounds.
pub fn autonomy_target(bound: AutonomyBound) -> Option<WorkflowMode> {
    match bound {
        AutonomyBound::UntilMode(mode) => Some(mode),
        _ => None,
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn orders_the_seven_workflow_modes() {
        let names: Vec<&str> = WORKFLOW_MODES.iter().map(|mode| mode.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "discovery",
                "specification",
                "technical-design",
                "planning",
                "implementation",
                "verification",
                "delivery",
            ]
        );
        assert_eq!(mode_index(WorkflowMode::Delivery), 6);
    }

    #[test]
    fn parses_autonomy_bounds_and_rejects_garbage() {
        assert_eq!(parse_autonomy_bound("off"), Some(AutonomyBound::Off));
        assert_eq!(
            parse_autonomy_bound("next-checkpoint"),
            Some(AutonomyBound::NextCheckpoint)
        );
        assert_eq!(
            parse_autonomy_bound("workflow"),
            Some(AutonomyBound::Workflow)
        );
        assert_eq!(
            parse_autonomy_bound("until:delivery"),
            Some(AutonomyBound::UntilMode(WorkflowMode::Delivery))
        );
        assert_eq!(parse_autonomy_bound("until:nowhere"), None);
        assert_eq!(parse_autonomy_bound(""), None);
        assert!(!is_autonomy_bound("yolo"));
    }

    #[test]
    fn mode_contracts_expose_artifacts_and_neighbors() {
        assert_eq!(
            WorkflowMode::Discovery.contract().produced_artifact,
            ArtifactKind::OpportunityBrief
        );
        assert_eq!(WorkflowMode::Delivery.contract().next_mode, None);
        assert_eq!(
            WorkflowMode::Delivery.contract().previous_mode,
            Some(WorkflowMode::Verification)
        );
        assert!(
            WorkflowMode::Implementation
                .contract()
                .can_modify_user_files
        );
        assert!(!WorkflowMode::Verification.contract().can_modify_user_files);
    }
}
