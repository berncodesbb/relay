//! The fold error: actionable failures from applying domain facts.

use crate::dependency::DependencyEdge;
use crate::ids::ProjectId;

/// Raised when a fact cannot be folded onto a project graph.
///
/// Every variant names the fact kind and enough context to act on the
/// failure. Folding is fail-closed: an unrecognized ordering or a tampered
/// record surfaces here instead of silently producing different state.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum FactError {
    /// The fact precedes `project_created`.
    #[error("fact {fact} cannot be applied before a project is created")]
    NotInitialized {
        /// The rejected fact kind.
        fact: &'static str,
    },
    /// The project was already created.
    #[error("fact {fact} cannot initialize the project twice (already {existing})")]
    AlreadyInitialized {
        /// The rejected fact kind.
        fact: &'static str,
        /// The existing project identity.
        existing: ProjectId,
    },
    /// The fact references an entity that does not exist.
    #[error("fact {fact} references unknown state: {detail}")]
    UnknownReference {
        /// The rejected fact kind.
        fact: &'static str,
        /// What was missing.
        detail: String,
    },
    /// The fact collides with existing state.
    #[error("fact {fact} duplicates existing state: {detail}")]
    Duplicate {
        /// The rejected fact kind.
        fact: &'static str,
        /// What collided.
        detail: String,
    },
    /// The fact violates a sequence rule (version monotonicity, start before
    /// terminal, current-revision start).
    #[error("fact {fact} violates ordering rules: {detail}")]
    SequenceViolation {
        /// The rejected fact kind.
        fact: &'static str,
        /// What rule was broken.
        detail: String,
    },
    /// The fact would create a dependency cycle.
    #[error("fact {fact} would create a dependency cycle: {path:?}")]
    DependencyCycle {
        /// The rejected fact kind.
        fact: &'static str,
        /// The edges forming the cycle.
        path: Vec<DependencyEdge>,
    },
    /// The fact exceeds a capability ceiling or grant.
    #[error("fact {fact} exceeds its capability: {detail}")]
    CapabilityExceeded {
        /// The rejected fact kind.
        fact: &'static str,
        /// What exceeded what.
        detail: String,
    },
    /// The fact is inconsistent with the current state in some other way.
    #[error("fact {fact} is invalid in the current state: {detail}")]
    InvalidState {
        /// The rejected fact kind.
        fact: &'static str,
        /// What is inconsistent.
        detail: String,
    },
}

impl FactError {
    /// The fact kind that was rejected.
    pub fn fact(&self) -> &'static str {
        match self {
            FactError::NotInitialized { fact }
            | FactError::AlreadyInitialized { fact, .. }
            | FactError::UnknownReference { fact, .. }
            | FactError::Duplicate { fact, .. }
            | FactError::SequenceViolation { fact, .. }
            | FactError::DependencyCycle { fact, .. }
            | FactError::CapabilityExceeded { fact, .. }
            | FactError::InvalidState { fact, .. } => fact,
        }
    }
}
