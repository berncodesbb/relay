//! Evidence: typed observations attached to graph nodes.

use crate::artifact::ArtifactVersionRef;
use crate::ids::{DecisionId, EvidenceId, WorkItemId};

/// What kind of observation a piece of evidence is.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    /// Output of an executed test suite.
    TestRun,
    /// Captured stdout/stderr of a command.
    CommandOutput,
    /// Static analysis or lint results.
    StaticAnalysis,
    /// Facts observed by reading the repository.
    Inspection,
    /// A review judgment.
    Review,
    /// A statement by the human user.
    HumanStatement,
}

/// What an observation establishes about its target.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceOutcome {
    /// The observation supports the target's claim.
    Pass,
    /// The observation refutes the target's claim.
    Fail,
    /// The observation carries no verdict (context, diagnostics, notes).
    Info,
}

/// What an observation is attached to. Claims and criteria are addressed as
/// artifact versions (Phase 2 gives claim schemas addressable identity);
/// decisions carry evidence so answerers can act with full context.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum EvidenceTarget {
    /// Evidence about a work item's outcome.
    #[serde(rename = "work_item")]
    WorkItem(WorkItemId),
    /// Evidence about an exact artifact version (criteria, claims).
    #[serde(rename = "artifact_version")]
    ArtifactVersion(ArtifactVersionRef),
    /// Evidence informing a decision.
    #[serde(rename = "decision")]
    Decision(DecisionId),
}

/// A typed observation: the unit of verification.
///
/// Evidence is immutable once recorded; corrections are new evidence. Whether
/// a criterion is satisfied is derived deterministically from the evidence
/// set attached to its exact version — never from a stored verdict.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Evidence {
    /// Addressable evidence identity.
    pub id: EvidenceId,
    /// The kind of observation.
    pub kind: EvidenceKind,
    /// What the evidence is about.
    pub target: EvidenceTarget,
    /// What the observation establishes.
    pub outcome: EvidenceOutcome,
    /// A one-line human-readable summary.
    pub summary: String,
    /// Structured observation payload (counts, failing cases, links). The
    /// payload is data for views and audits; policy branches only on
    /// `outcome`.
    pub payload: serde_json::Value,
    /// Wall-clock milliseconds when the observation was recorded
    /// (caller-supplied).
    pub recorded_at_ms: i64,
}

impl Evidence {
    /// Construct minimal evidence with an empty payload.
    pub fn new(
        id: EvidenceId,
        kind: EvidenceKind,
        target: EvidenceTarget,
        outcome: EvidenceOutcome,
        summary: impl Into<String>,
        recorded_at_ms: i64,
    ) -> Self {
        Evidence {
            id,
            kind,
            target,
            outcome,
            summary: summary.into(),
            payload: serde_json::Value::Null,
            recorded_at_ms,
        }
    }
}
