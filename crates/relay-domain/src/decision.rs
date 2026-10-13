//! Human decisions as first-class graph nodes.

use crate::artifact::ArtifactVersionRef;
use crate::ids::{DecisionId, GoalId, WorkItemId};

/// What a decision is about.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum DecisionSubject {
    /// A decision about the goal itself.
    #[serde(rename = "goal")]
    Goal(GoalId),
    /// A decision attached to a work item (clarifications, scope
    /// authorizations).
    #[serde(rename = "work_item")]
    WorkItem(WorkItemId),
    /// A decision about an exact artifact version (approvals).
    #[serde(rename = "artifact_version")]
    ArtifactVersion(ArtifactVersionRef),
}

/// One selectable answer to a decision.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DecisionOption {
    /// A short stable label identifying the option (e.g. `approve`).
    pub label: String,
    /// What choosing this option means.
    pub description: String,
}

/// Who is entitled to resolve a decision. Human authority is the default;
/// system authority covers policy-resolvable decisions under explicit
/// autonomy settings.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Authority {
    /// Only a human may resolve.
    Human,
    /// Deterministic policy may resolve without a human.
    System,
}

/// An unresolved or resolved human choice.
///
/// Decisions are work: they gate readiness of dependent work through
/// [`crate::work::CompletionRule::Decision`], and they record options, a
/// recommendation, supporting evidence, and consequences so the question can
/// be answered with full context. Resolution is terminal; reconsideration
/// happens by superseding the decision with a new one.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Decision {
    /// Addressable decision identity.
    pub id: DecisionId,
    /// What the decision is about.
    pub subject: DecisionSubject,
    /// The question, phrased to be answerable as one focused choice.
    pub question: String,
    /// The selectable answers. Empty for free-form clarifications, where the
    /// resolution is the answer text itself.
    pub options: Vec<DecisionOption>,
    /// The recommended option label, when policy or a model dares to have
    /// one.
    pub recommendation: Option<String>,
    /// Artifact versions the answerer should read first.
    pub evidence: Vec<ArtifactVersionRef>,
    /// What hinges on this decision.
    pub consequences: String,
    /// Who may resolve the decision.
    pub authority: Authority,
    /// Wall-clock milliseconds when the decision was opened
    /// (caller-supplied).
    pub opened_at_ms: i64,
}

impl Decision {
    /// Construct a decision with a single recommended option and no
    /// supporting evidence.
    pub fn new(
        id: DecisionId,
        subject: DecisionSubject,
        question: impl Into<String>,
        options: Vec<DecisionOption>,
        opened_at_ms: i64,
    ) -> Self {
        Decision {
            id,
            subject,
            question: question.into(),
            options,
            recommendation: None,
            evidence: Vec::new(),
            consequences: String::new(),
            authority: Authority::Human,
            opened_at_ms,
        }
    }

    /// Whether `choice` is an acceptable resolution: it must name a declared
    /// option, or be free text when no options are declared.
    pub fn accepts_choice(&self, choice: &str) -> bool {
        if self.options.is_empty() {
            return !choice.trim().is_empty();
        }
        self.options.iter().any(|option| option.label == choice)
    }
}

/// The durable record of how a decision was answered.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DecisionResolution {
    /// The chosen option label, or the free-form answer text when the
    /// decision declared no options.
    pub choice: String,
    /// Why this choice was made.
    pub rationale: String,
    /// Wall-clock milliseconds when the decision was resolved
    /// (caller-supplied).
    pub resolved_at_ms: i64,
}
