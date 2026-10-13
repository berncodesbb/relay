//! Attempts and receipts: the executor boundary.

use crate::artifact::{ArtifactContent, ArtifactDigest};
use crate::evidence::Evidence;
use crate::ids::{AttemptId, WorkItemId};
use crate::proposal::GraphChangeProposal;

/// The terminal outcome an executor reports for an attempt.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum AttemptOutcome {
    /// The executor finished the work (whether the results pass their own
    /// criteria is carried by evidence, not by this flag).
    #[serde(rename = "completed")]
    Completed,
    /// The executor could not finish the work.
    #[serde(rename = "failed")]
    Failed {
        /// Why the attempt failed.
        reason: String,
    },
}

/// Resource usage reported by an attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct Usage {
    /// Input tokens consumed.
    pub input_tokens: u64,
    /// Output tokens consumed.
    pub output_tokens: u64,
    /// Tool invocations performed.
    pub tool_calls: u64,
    /// Wall-clock milliseconds the attempt took.
    pub duration_ms: i64,
}

/// A proposal to publish the next version of an artifact. The version
/// number is deliberately absent: acceptance assigns it deterministically
/// from graph state, so executors cannot skip or collide versions.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct OutputProposal {
    /// The artifact to publish (existing or new).
    pub artifact: crate::ids::ArtifactId,
    /// A human-readable title for the version.
    pub title: Option<String>,
    /// The structured content of the version.
    pub content: ArtifactContent,
    /// The executor's digest commitment over the content bytes; verified at
    /// the storage boundary.
    pub digest: ArtifactDigest,
}

/// The durable account of one attempt: what happened, what it produced, and
/// what it proposes. Bounded and immutable at the executor boundary.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AttemptReceipt {
    /// The attempt this account belongs to.
    pub attempt: AttemptId,
    /// The work item the attempt ran (must match the attempt's work).
    pub work: WorkItemId,
    /// The terminal outcome.
    pub outcome: AttemptOutcome,
    /// Wall-clock milliseconds when the attempt concluded (caller-supplied;
    /// must not precede the attempt's start).
    pub completed_at_ms: i64,
    /// Artifact versions to publish.
    pub outputs: Vec<OutputProposal>,
    /// Evidence to record.
    pub evidence: Vec<Evidence>,
    /// Graph changes to propose (validated separately from acceptance).
    pub graph_changes: Vec<GraphChangeProposal>,
    /// Resource usage.
    pub usage: Usage,
}

/// The terminal fact recorded for an attempt.
///
/// Only terminal facts are durable; progress is operational. `Accepted`
/// wraps a receipt whose mechanics survived
/// `relay_engine::accept_receipt`; the remaining variants are recorded by
/// the runtime when no acceptable receipt exists.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum AttemptConclusion {
    /// An accepted receipt accounts for the attempt.
    Accepted {
        /// The accepted receipt.
        receipt: AttemptReceipt,
    },
    /// The attempt failed without a usable receipt (crash, budget, error).
    Failed {
        /// Why it failed.
        reason: String,
    },
    /// A user or operator canceled the attempt.
    Canceled {
        /// Why it was canceled.
        reason: String,
    },
    /// Reconciliation after a crash marked the attempt abandoned: it
    /// started, no terminal fact followed, and the runtime will not
    /// pretend it succeeded.
    Abandoned {
        /// Reconciliation note.
        reason: String,
    },
}
