//! The project goal: outcome, constraints, success definition, provenance.

use crate::ids::GoalId;

/// Who or what produced a piece of domain data.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum ProvenanceSource {
    /// Stated directly by the human user.
    User,
    /// Proposed by a model and accepted by policy or the user.
    Model,
    /// Carried over from a legacy Relay project during migration.
    Import,
}

/// Where a goal came from, in plain data.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Provenance {
    /// Who supplied the goal.
    pub source: ProvenanceSource,
    /// The original statement, when one exists.
    pub statement: Option<String>,
    /// Wall-clock milliseconds when the goal was expressed (caller-supplied).
    pub at_ms: i64,
}

/// The desired outcome of a project: what the project owes its user.
///
/// A project declares exactly one goal; later refinement arrives as new work,
/// decisions, and artifact versions rather than by mutating the goal.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Goal {
    /// Addressable goal identity.
    pub id: GoalId,
    /// The outcome wanted, as a plain statement.
    pub outcome: String,
    /// Constraints the outcome must respect (technical, product, or process).
    pub constraints: Vec<String>,
    /// How success will be recognized.
    pub success_definition: String,
    /// Where the goal came from.
    pub provenance: Provenance,
    /// Wall-clock milliseconds when the goal was declared (caller-supplied).
    pub declared_at_ms: i64,
}

impl Goal {
    /// Construct a goal from an outcome statement with empty constraints.
    pub fn new(
        id: GoalId,
        outcome: impl Into<String>,
        provenance: Provenance,
        declared_at_ms: i64,
    ) -> Self {
        Goal {
            id,
            outcome: outcome.into(),
            constraints: Vec::new(),
            success_definition: String::new(),
            provenance,
            declared_at_ms,
        }
    }
}
