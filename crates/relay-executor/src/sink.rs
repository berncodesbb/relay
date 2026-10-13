//! The fact sink port: fact append and graph reads without a store
//! dependency.
//!
//! The orchestrator speaks only to [`FactSink`]. The application
//! implements it over `relay-store` in Phase 6 — each `append` takes the
//! storage writer lock for that append alone, so no lock is ever held
//! across executor work. [`InMemoryFactSink`] validates and folds facts in
//! memory for deterministic tests and embeddings.

use std::sync::Mutex;

use relay_domain::{DomainFact, ProjectGraph};

/// Why a fact-sink operation failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FactSinkError {
    /// The fact failed validation against the folded history; nothing was
    /// appended.
    #[error("fact rejected: {0}")]
    Rejected(String),
    /// The sink itself failed (IO, lock poisoning, storage health).
    #[error("fact sink failed: {0}")]
    Failed(String),
}

/// Durable fact storage as seen by the orchestrator.
pub trait FactSink: Send + Sync {
    /// Validate and append one fact. Implementations hold the storage
    /// writer lock for this append alone and return only after the fact is
    /// durable.
    fn append(&self, fact: DomainFact) -> Result<(), FactSinkError>;

    /// The full durable fact history, oldest first.
    fn facts(&self) -> Result<Vec<DomainFact>, FactSinkError>;

    /// The graph folded from the durable history.
    fn graph(&self) -> Result<ProjectGraph, FactSinkError> {
        let facts = self.facts()?;
        ProjectGraph::from_facts(facts.iter())
            .map_err(|error| FactSinkError::Failed(format!("history no longer folds: {error}")))
    }
}

/// An in-memory, validating fact sink for tests and deterministic
/// embeddings. Every append is validated by folding the whole history, so
/// invalid facts are rejected exactly as a real store would reject them.
#[derive(Debug, Default)]
pub struct InMemoryFactSink {
    facts: Mutex<Vec<DomainFact>>,
}

impl InMemoryFactSink {
    /// An empty sink.
    pub fn new() -> Self {
        InMemoryFactSink::default()
    }

    /// An empty sink seeded with a starting history (typically
    /// `ProjectCreated` plus declarations).
    pub fn seeded(facts: Vec<DomainFact>) -> Result<Self, FactSinkError> {
        let sink = InMemoryFactSink::new();
        for fact in facts {
            sink.append(fact)?;
        }
        Ok(sink)
    }
}

impl FactSink for InMemoryFactSink {
    fn append(&self, fact: DomainFact) -> Result<(), FactSinkError> {
        let mut facts = self
            .facts
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        // Validate against the folded history before accepting: the
        // appendix order and all domain rules must hold.
        let mut candidate = facts.clone();
        candidate.push(fact.clone());
        ProjectGraph::from_facts(candidate.iter())
            .map_err(|error| FactSinkError::Rejected(error.to_string()))?;
        facts.push(fact);
        Ok(())
    }

    fn facts(&self) -> Result<Vec<DomainFact>, FactSinkError> {
        Ok(self
            .facts
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone())
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use relay_domain::{DomainFact, Goal, GoalId, ProjectId, Provenance, ProvenanceSource};

    fn project() -> DomainFact {
        DomainFact::ProjectCreated {
            project: ProjectId::parse("prj_1").unwrap(),
        }
    }

    fn goal() -> DomainFact {
        DomainFact::GoalDeclared {
            goal: Goal {
                id: GoalId::parse("goal_1").unwrap(),
                outcome: "ship it".into(),
                constraints: Vec::new(),
                success_definition: "tests pass".into(),
                provenance: Provenance {
                    source: ProvenanceSource::User,
                    statement: None,
                    at_ms: 0,
                },
                declared_at_ms: 0,
            },
        }
    }

    #[test]
    fn appends_validate_against_history() {
        let sink = InMemoryFactSink::new();
        sink.append(project()).expect("first fact");
        sink.append(goal()).expect("second fact");
        assert_eq!(sink.facts().expect("facts").len(), 2);
        assert_eq!(sink.graph().expect("graph").revision().get(), 2);
    }

    #[test]
    fn invalid_facts_are_rejected_without_appending() {
        let sink = InMemoryFactSink::new();
        // A goal before a project cannot fold.
        let rejected = sink.append(goal());
        assert!(matches!(rejected, Err(FactSinkError::Rejected(_))));
        assert!(sink.facts().expect("facts").is_empty());
    }

    #[test]
    fn duplicate_facts_are_rejected() {
        let sink = InMemoryFactSink::new();
        sink.append(project()).expect("first");
        assert!(matches!(
            sink.append(project()),
            Err(FactSinkError::Rejected(_))
        ));
    }
}
