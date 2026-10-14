//! Pure work-graph domain for Relay.
//!
//! This crate owns the addressable entities of the intent-first domain —
//! goals, work items, dependencies, decisions, artifacts, evidence,
//! attempts, capability grants, attempt receipts, and the minimal durable
//! fact set — plus their validation and the [`ProjectGraph`] fold. It is
//! the innermost crate: no IO, no clocks, no randomness, no HTTP, no SQL,
//! no terminal, no provider SDKs, no filesystem. Time, IDs, provider
//! output, and filesystem facts enter only through facts, commands, or
//! attempt receipts supplied by callers.
//!
//! The design rules that hold the kernel honest:
//!
//! - **Facts are the only history.** Everything operational (readiness,
//!   blockers, invalidation, completion) is derived by `relay-engine` from
//!   the folded graph; nothing derived is ever stored.
//! - **Exact versions carry dependency precision.** Work items pin exact
//!   artifact versions (or explicit selection rules), publications record
//!   the versions they were derived from, and invalidation follows those
//!   pins — never coarse phase order.
//! - **Executors propose; policy disposes.** Graph changes and receipts are
//!   data ([`GraphChangeProposal`], [`AttemptReceipt`]) validated elsewhere;
//!   nothing in this crate mutates the graph on an executor's behalf.
//! - **Humans are graph nodes.** Decisions are first-class work; there is no
//!   global mode or pause state anywhere in this crate.

pub mod artifact;
pub mod attempt;
pub mod conversation;
pub mod decision;
pub mod dependency;
pub mod error;
pub mod evidence;
pub mod facts;
pub mod goal;
pub mod grant;
pub mod graph;
pub mod ids;
pub mod proposal;
pub mod validation;
pub mod work;

pub use artifact::{ArtifactContent, ArtifactDigest, ArtifactVersionRef, DigestError};
pub use attempt::{AttemptConclusion, AttemptOutcome, AttemptReceipt, OutputProposal, Usage};
pub use conversation::{
    ConversationNode, MESSAGE_TEXT_MAX_CHARS, RecordedMessage, Speaker, TURN_RELATED_MAX,
    TranscriptEntry, TurnNode, TurnOutcome, TurnReferences,
};
pub use decision::{Authority, Decision, DecisionOption, DecisionResolution, DecisionSubject};
pub use dependency::{DependencyEdge, DependencyKind};
pub use error::FactError;
pub use evidence::{Evidence, EvidenceKind, EvidenceOutcome, EvidenceTarget};
pub use facts::{
    DomainFact, InvalidationCause, InvalidationTarget, KNOWN_FACT_KINDS, current_fact_version,
    fact_from_parts, fact_to_parts,
};
pub use goal::{Goal, Provenance, ProvenanceSource};
pub use grant::{
    Budget, CapabilityGrant, CommandPattern, DeliveryAuthority, ModelRoute, Operation,
};
pub use graph::{
    ArtifactNode, AttemptNode, DecisionNode, DeclaredInvalidation, DependencyNode, ProjectGraph,
    VersionRecord, WorkNode,
};
pub use ids::{
    ArtifactId, ArtifactVersion, AttemptId, ConversationId, DecisionId, EvidenceId, GoalId,
    GrantId, IdParseError, MessageId, ProjectId, ProjectRevision, TurnId, VersionError, WorkItemId,
};
pub use proposal::GraphChangeProposal;
pub use work::{
    CapabilityCeiling, CompletionRule, ExecutorClass, ExpectedOutput, InputRef, ScopeError,
    ScopePath, WorkItem, WorkKind, WorkKindError,
};
