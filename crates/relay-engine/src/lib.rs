//! Deterministic work-graph evaluation for Relay.
//!
//! This crate evaluates a [`relay_domain::ProjectGraph`]: it derives ready
//! work, blockers with reasons, awaiting-human decisions, invalidations,
//! project completion, and scheduling recommendations from durable facts.
//! It validates graph-change proposals (cycles, duplicates, capability
//! ceilings, scope) and accepts attempt receipts against expected project
//! revisions. It is pure and deterministic — same facts in, same answers
//! out, on every platform — and never performs IO or consults a clock,
//! provider, or executor.
//!
//! # Entry points
//!
//! - [`evaluate()`]: the full projection (ready, blocked, awaiting-human,
//!   active attempts, invalidated, terminals, completion).
//! - [`explain()`]: why one work item is in its current state, in
//!   deterministic prose with typed cause chains.
//! - [`validate_graph_change`]: accept or reject an executor/model
//!   proposal; executors never apply changes directly.
//! - [`accept_receipt`]: validate a receipt and produce the facts that
//!   record its acceptance (including engine-assigned artifact versions).
//! - [`recommend`]: ordered scheduling advice under an autonomy policy,
//!   separating parallelizable read-only work from serialized writers.

pub mod evaluate;
pub mod explain;
pub mod proposals;
pub mod receipt;
pub mod schedule;
pub mod status;

pub use evaluate::{
    ActiveAttempt, AwaitingHumanEntry, BlockedEntry, Evaluation, InvalidatedEntry,
    ProjectCompletion, ReadyEntry,
};
pub use explain::{ExplainError, Explanation};
pub use proposals::{Applied, Rejected};
pub use receipt::{Acceptance, Rejection};
pub use schedule::{Autonomy, Deferred, Recommendation, Recommended, SchedulingPolicy};
pub use status::{
    BlockReason, EvidenceStatus, InvalidationChain, InvalidityStep, VersionStatus, WorkStatus,
    work_status, work_status_by_id,
};

/// Evaluate a project graph. See [`evaluate::evaluate`].
pub fn evaluate(graph: &relay_domain::ProjectGraph) -> Evaluation {
    evaluate::evaluate(graph)
}

/// Explain one work item. See [`explain::explain`].
pub fn explain(
    graph: &relay_domain::ProjectGraph,
    work: &relay_domain::WorkItemId,
) -> Result<Explanation, ExplainError> {
    explain::explain(graph, work)
}

/// Validate a graph-change proposal. See [`proposals::validate_graph_change`].
pub fn validate_graph_change(
    graph: &relay_domain::ProjectGraph,
    proposal: &relay_domain::GraphChangeProposal,
) -> Result<Applied, Rejected> {
    proposals::validate_graph_change(graph, proposal)
}

/// Accept an attempt receipt. See [`receipt::accept_receipt`].
pub fn accept_receipt(
    graph: &relay_domain::ProjectGraph,
    receipt: &relay_domain::AttemptReceipt,
) -> Result<Acceptance, Rejection> {
    receipt::accept_receipt(graph, receipt)
}

/// Compute a scheduling recommendation. See [`schedule::recommend`].
pub fn recommend(graph: &relay_domain::ProjectGraph, policy: &SchedulingPolicy) -> Recommendation {
    schedule::recommend(graph, policy)
}
