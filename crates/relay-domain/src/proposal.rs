//! Graph-change proposals: the vocabulary executors and models use to
//! propose (never install) changes to the work graph.

use crate::artifact::ArtifactVersionRef;
use crate::decision::Decision;
use crate::dependency::DependencyEdge;
use crate::facts::{InvalidationCause, InvalidationTarget};
use crate::grant::Operation;
use crate::ids::WorkItemId;
use crate::work::{ScopePath, WorkItem};

/// One proposed change to the work graph.
///
/// Proposals travel inside attempt receipts (and from any other proposer).
/// They become facts only after `relay_engine::validate_graph_change`
/// accepts them; executors never apply changes directly.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "change", rename_all = "snake_case")]
pub enum GraphChangeProposal {
    /// Declare new work. Initial dependencies ride along on the work item.
    DeclareWork {
        /// The work item to declare.
        work: WorkItem,
    },
    /// Assert a new dependency edge between existing work items.
    DeclareDependency {
        /// The edge to assert (`from` depends on `to`).
        edge: DependencyEdge,
    },
    /// Replace existing work with a new work item that supersedes it.
    SupersedeWork {
        /// The work item being replaced.
        supersede: WorkItemId,
        /// The replacement, with a fresh id.
        replacement: WorkItem,
        /// Why the replacement is warranted.
        reason: String,
    },
    /// Cancel work that should no longer run.
    CancelWork {
        /// The work item to cancel.
        work: WorkItemId,
        /// Why it is being canceled.
        reason: String,
    },
    /// Retract a previously asserted dependency edge.
    RetractDependency {
        /// The exact edge to retract.
        edge: DependencyEdge,
        /// Why it is being retracted.
        reason: String,
    },
    /// Open a decision for a human (or, where authorized, policy) to
    /// resolve.
    OpenDecision {
        /// The decision to open.
        decision: Decision,
    },
    /// Expand a work item's scopes or operations within its capability
    /// ceiling. Consequential (any write-scope growth) proposals are flagged
    /// as requiring a human decision by the engine.
    ExpandScope {
        /// The work item whose scopes grow.
        work: WorkItemId,
        /// Additional writable areas.
        added_write_scopes: Vec<ScopePath>,
        /// Additional operation classes.
        added_operations: Vec<Operation>,
        /// Why the expansion is needed.
        reason: String,
    },
    /// Declare that a node is invalid, with a cause and the input version
    /// change (if any) that motivates it.
    DeclareInvalidation {
        /// What is being invalidated.
        target: InvalidationTarget,
        /// Why it is invalid.
        cause: InvalidationCause,
        /// The input version whose change motivates the invalidation.
        changed_input: Option<ArtifactVersionRef>,
    },
}
