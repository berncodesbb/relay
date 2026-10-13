//! Durable domain facts.
//!
//! Facts are the sole authoritative record of project history: small,
//! immutable, serde-tagged statements that fold into a
//! [`crate::graph::ProjectGraph`]. Everything operational — readiness,
//! blockers, invalidation, completion — is derived, never stored. The wire
//! form is internally tagged (`{"type": "work_declared", …}`); sequencing,
//! checksums, and any envelope versioning are storage concerns that wrap
//! this typed form.

use crate::artifact::{ArtifactContent, ArtifactDigest, ArtifactVersionRef};
use crate::attempt::AttemptConclusion;
use crate::decision::{Decision, DecisionResolution};
use crate::error::FactError;
use crate::evidence::Evidence;
use crate::goal::Goal;
use crate::grant::{CapabilityGrant, Operation};
use crate::ids::{
    ArtifactId, ArtifactVersion, AttemptId, DecisionId, ProjectId, ProjectRevision, WorkItemId,
};
use crate::work::{ScopePath, WorkItem};

/// What an invalidation declaration targets.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum InvalidationTarget {
    /// A work item.
    #[serde(rename = "work")]
    Work(WorkItemId),
    /// An exact artifact version.
    #[serde(rename = "artifact_version")]
    ArtifactVersion(ArtifactVersionRef),
    /// A piece of evidence.
    #[serde(rename = "evidence")]
    Evidence(crate::ids::EvidenceId),
}

/// Why a node was declared invalid.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum InvalidationCause {
    /// An input version the node depended on was superseded by a newer
    /// publication.
    #[serde(rename = "superseded_input")]
    SupersededInput {
        /// The version the node depended on.
        previous: ArtifactVersionRef,
        /// The version that replaced it.
        current: ArtifactVersionRef,
    },
    /// A human or policy judged the node invalid.
    #[serde(rename = "declared")]
    Declared {
        /// The judgment, in one line.
        note: String,
    },
}

impl std::fmt::Display for InvalidationCause {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InvalidationCause::SupersededInput { previous, current } => {
                write!(f, "input {previous} was superseded by {current}")
            }
            InvalidationCause::Declared { note } => write!(f, "declared invalid: {note}"),
        }
    }
}

/// The minimal durable fact set for the work graph.
///
/// Facts are validated as they are folded: duplicates, unknown references,
/// cycles, capability violations, and sequence violations (version skips,
/// terminals before starts, resolving unresolved decisions) fail closed.
/// Facts that do not reference each other are order-independent.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DomainFact {
    /// The project exists.
    ProjectCreated {
        /// The project identity.
        project: ProjectId,
    },
    /// The project's single goal was declared.
    GoalDeclared {
        /// The goal.
        goal: Goal,
    },
    /// Work was declared with (possibly empty) initial dependencies.
    WorkDeclared {
        /// The declared work item.
        work: WorkItem,
    },
    /// Work was superseded by a replacement.
    WorkSuperseded {
        /// The replaced work item.
        work: WorkItemId,
        /// The replacement.
        superseded_by: WorkItemId,
        /// Why it was replaced.
        reason: String,
    },
    /// Work was canceled.
    WorkCanceled {
        /// The canceled work item.
        work: WorkItemId,
        /// Why it was canceled.
        reason: String,
    },
    /// A work item's scopes or operations grew within its ceiling.
    WorkScopeExpanded {
        /// The work item whose scopes grew.
        work: WorkItemId,
        /// Additional writable areas.
        added_write_scopes: Vec<ScopePath>,
        /// Additional operation classes.
        added_operations: Vec<Operation>,
        /// Why the expansion was needed.
        reason: String,
    },
    /// A dependency edge was asserted.
    DependencyAsserted {
        /// The dependent.
        from: WorkItemId,
        /// The prerequisite.
        to: WorkItemId,
        /// The typed relation.
        kind: crate::dependency::DependencyKind,
    },
    /// A dependency edge was retracted.
    DependencyRetracted {
        /// The dependent.
        from: WorkItemId,
        /// The prerequisite.
        to: WorkItemId,
        /// The typed relation.
        kind: crate::dependency::DependencyKind,
        /// Why it was retracted.
        reason: String,
    },
    /// A decision was opened.
    DecisionOpened {
        /// The decision.
        decision: Decision,
    },
    /// A decision was resolved with a choice.
    DecisionResolved {
        /// The resolved decision.
        decision: DecisionId,
        /// The resolution.
        resolution: DecisionResolution,
    },
    /// A decision was superseded by a reconsideration.
    DecisionSuperseded {
        /// The superseded decision.
        decision: DecisionId,
        /// The reconsidering decision.
        superseded_by: DecisionId,
    },
    /// A new artifact version was published with its content and digest
    /// commitment.
    ArtifactVersionPublished {
        /// The artifact identity.
        artifact: ArtifactId,
        /// The version (must be exactly current + 1).
        version: ArtifactVersion,
        /// A human-readable title for the version.
        title: Option<String>,
        /// The structured content.
        content: ArtifactContent,
        /// The digest commitment over the content.
        digest: ArtifactDigest,
        /// The exact input versions this content was derived from.
        derived_from: Vec<ArtifactVersionRef>,
        /// Wall-clock milliseconds when the version was published
        /// (caller-supplied).
        published_at_ms: i64,
    },
    /// Evidence was recorded.
    EvidenceRecorded {
        /// The evidence.
        evidence: Evidence,
    },
    /// An attempt started against an expected project revision under a
    /// capability grant.
    AttemptStarted {
        /// The attempt identity.
        attempt: AttemptId,
        /// The work item being attempted.
        work: WorkItemId,
        /// The project revision the attempt was computed against.
        expected_revision: ProjectRevision,
        /// The grant bound to this attempt.
        grant: CapabilityGrant,
        /// Wall-clock milliseconds when the attempt started
        /// (caller-supplied).
        started_at_ms: i64,
    },
    /// An attempt reached a terminal state.
    AttemptConcluded {
        /// The attempt identity.
        attempt: AttemptId,
        /// The terminal account.
        conclusion: AttemptConclusion,
    },
    /// A node was declared invalid, with cause and motivating input.
    InvalidationDeclared {
        /// What is invalid.
        target: InvalidationTarget,
        /// Why.
        cause: InvalidationCause,
        /// The input version change that motivated the invalidation.
        changed_input: Option<ArtifactVersionRef>,
    },
}

impl DomainFact {
    /// The stable wire name of this fact kind, for actionable errors.
    pub fn kind(&self) -> &'static str {
        match self {
            DomainFact::ProjectCreated { .. } => "project_created",
            DomainFact::GoalDeclared { .. } => "goal_declared",
            DomainFact::WorkDeclared { .. } => "work_declared",
            DomainFact::WorkSuperseded { .. } => "work_superseded",
            DomainFact::WorkCanceled { .. } => "work_canceled",
            DomainFact::WorkScopeExpanded { .. } => "work_scope_expanded",
            DomainFact::DependencyAsserted { .. } => "dependency_asserted",
            DomainFact::DependencyRetracted { .. } => "dependency_retracted",
            DomainFact::DecisionOpened { .. } => "decision_opened",
            DomainFact::DecisionResolved { .. } => "decision_resolved",
            DomainFact::DecisionSuperseded { .. } => "decision_superseded",
            DomainFact::ArtifactVersionPublished { .. } => "artifact_version_published",
            DomainFact::EvidenceRecorded { .. } => "evidence_recorded",
            DomainFact::AttemptStarted { .. } => "attempt_started",
            DomainFact::AttemptConcluded { .. } => "attempt_concluded",
            DomainFact::InvalidationDeclared { .. } => "invalidation_declared",
        }
    }
}

/// The complete catalog of fact kinds this crate can fold. Storage uses it
/// to fail closed on records from a different tool or a newer Relay build.
pub const KNOWN_FACT_KINDS: &[&str] = &[
    "project_created",
    "goal_declared",
    "work_declared",
    "work_superseded",
    "work_canceled",
    "work_scope_expanded",
    "dependency_asserted",
    "dependency_retracted",
    "decision_opened",
    "decision_resolved",
    "decision_superseded",
    "artifact_version_published",
    "evidence_recorded",
    "attempt_started",
    "attempt_concluded",
    "invalidation_declared",
];

/// The payload version of a fact kind. All kinds are at version 1; a newer
/// answer than the caller understands must be treated as incompatible
/// (fail closed with upgrade guidance), and additive payload fields do not
/// bump it.
pub fn current_fact_version(kind: &str) -> Option<u32> {
    KNOWN_FACT_KINDS.contains(&kind).then_some(1)
}

/// Split a fact into its wire name and its payload (the fact's serde form
/// with the `type` tag removed). The pair is what storage persists;
/// `fact_from_parts` reassembles it. Round-trips exactly for every fact
/// this crate can express; unknown extra payload fields are preserved as
/// data but ignored when decoding (additive fields must not invalidate
/// legacy records).
pub fn fact_to_parts(fact: &DomainFact) -> (&'static str, serde_json::Value) {
    // Serialization of these purely-data facts cannot fail; Null is the
    // defensive stand-in if that ever changes.
    let mut value = serde_json::to_value(fact).unwrap_or_default();
    if let serde_json::Value::Object(map) = &mut value {
        map.remove("type");
    }
    (fact.kind(), value)
}

/// Reassemble a fact from its wire name and payload. Fails closed when the
/// kind is not in [`KNOWN_FACT_KINDS`] or the payload does not decode.
pub fn fact_from_parts(kind: &str, payload: &serde_json::Value) -> Result<DomainFact, FactError> {
    let reject = |detail: String| FactError::InvalidState {
        fact: "fact_from_parts",
        detail,
    };
    if !KNOWN_FACT_KINDS.contains(&kind) {
        return Err(reject(format!(
            "unknown fact kind {kind:?}; this build knows {} kinds — the record may come from a different tool or a newer Relay build",
            KNOWN_FACT_KINDS.len()
        )));
    }
    let mut full = payload.clone();
    let serde_json::Value::Object(map) = &mut full else {
        return Err(reject(format!(
            "payload for {kind:?} must be a JSON object"
        )));
    };
    map.insert(
        "type".to_string(),
        serde_json::Value::String(kind.to_string()),
    );
    serde_json::from_value::<DomainFact>(full)
        .map_err(|error| reject(format!("payload for {kind:?} does not decode: {error}")))
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::ok_expect,
    clippy::err_expect
)] // test-only conveniences, house style
mod tests {
    use super::*;

    fn sample_fact() -> DomainFact {
        DomainFact::ProjectCreated {
            project: ProjectId::parse("prj_demo").ok().unwrap(),
        }
    }

    fn sample_concluded() -> DomainFact {
        DomainFact::AttemptConcluded {
            attempt: AttemptId::parse("att_1").ok().unwrap(),
            conclusion: AttemptConclusion::Failed {
                reason: "provider unavailable".to_string(),
            },
        }
    }

    #[test]
    fn facts_round_trip_through_tagged_json() -> serde_json::Result<()> {
        for fact in [sample_fact(), sample_concluded()] {
            let json = serde_json::to_string(&fact)?;
            assert!(json.starts_with("{\"type\":\""));
            let back: DomainFact = serde_json::from_str(&json)?;
            assert_eq!(back, fact);
        }
        Ok(())
    }

    #[test]
    fn kind_names_match_wire_tags() {
        assert_eq!(sample_fact().kind(), "project_created");
        assert_eq!(sample_concluded().kind(), "attempt_concluded");
    }

    #[test]
    fn catalog_covers_every_foldable_kind() {
        assert_eq!(KNOWN_FACT_KINDS.len(), 16);
        assert_eq!(current_fact_version("work_declared"), Some(1));
        assert_eq!(current_fact_version("attempt_concluded"), Some(1));
        assert_eq!(current_fact_version("mode_entered"), None);
        assert_eq!(current_fact_version(""), None);
    }

    #[test]
    fn parts_round_trip_every_sample_fact() {
        for fact in [sample_fact(), sample_concluded()] {
            let (kind, payload) = fact_to_parts(&fact);
            assert!(
                payload.get("type").is_none(),
                "payload must not carry the tag"
            );
            let back = fact_from_parts(kind, &payload).ok().expect("reassembles");
            assert_eq!(back, fact);
        }
    }

    #[test]
    fn fact_from_parts_fails_closed_on_unknown_kind_and_bad_payload() {
        assert!(fact_from_parts("workflow_paused", &serde_json::json!({})).is_err());
        let error = fact_from_parts("work_declared", &serde_json::json!({"unrelated": true}))
            .err()
            .expect("malformed payload must fail");
        assert!(matches!(error, FactError::InvalidState { .. }));
        assert!(fact_from_parts("work_declared", &serde_json::Value::Null).is_err());
    }
}
