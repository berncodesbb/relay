//! Pure domain core for Relay, ported from the TypeScript reference
//! implementation in `src/core` and `src/policies`.
//!
//! This crate contains the complete workflow domain — strongly typed ids,
//! workflow modes and autonomy bounds, the artifact dependency graph, intents
//! and slash commands, the 24 event payload types with two-stage decoding,
//! the deterministic event fold (`replay`), the transition policy, and the
//! permission, intervention, and policy-transition rules — with **no IO of any
//! kind**: no filesystem, no network, no database, no terminal, no clocks, no
//! randomness. Everything here is a pure function of its inputs so that
//! replaying the same event log always produces identical state.
//!
//! The TypeScript implementation is the compatibility oracle: error classes,
//! error messages (including exact English substrings), fold semantics, and
//! payload validation behavior mirror it byte-for-byte, and the fixture corpus
//! under `tests/fixtures/` pins the two implementations together.
//!
//! Canonical JSON serialization (`docs/event-format.md` §2) and SHA-256
//! envelope hashing (§3) also live here so the store and the compatibility
//! suites share one implementation.

pub mod artifacts;
pub mod canonical;
pub mod events;
pub mod fakes;
pub mod ids;
pub mod intents;
pub mod modes;
pub mod normalize;
pub mod outcomes;
pub mod payloads;
pub mod policies;
pub mod project_state;
pub mod reducer;
pub mod transitions;

pub use artifacts::{
    ARTIFACT_KINDS, ArtifactKind, CONTRACTUAL_ARTIFACT_KINDS, artifact_dependencies,
    downstream_artifact_kinds, is_contractual_artifact_kind,
};
pub use canonical::canonical_json;
pub use events::{
    CoreError, EVENT_ENVELOPE_VERSION, EVENT_PAYLOAD_VERSIONS, EventEnvelope, RELAY_EVENT_TYPES,
    RelayEvent, RelayEventType, compute_envelope_hash, verify_canonical_log,
};
pub use ids::{ArtifactId, CorrelationId, EventId, IdParseError, ProjectId, RunId, SessionId};
pub use intents::{
    CONSEQUENTIAL_INTENT_KINDS, INTENT_CONFIDENCE_FLOOR, INTENT_KINDS, IntentKind, IntentProposal,
    IntentSource, ParsedCommand, SLASH_COMMANDS, is_intent_ambiguous, parse_slash_command,
};
pub use modes::{
    AutonomyBound, MODE_CONTRACTS, ModeContract, NAMED_AUTONOMY_BOUNDS, ToolCapability,
    WORKFLOW_MODES, WorkflowMode, autonomy_target, is_autonomy_bound, is_workflow_mode, mode_index,
    parse_autonomy_bound,
};
pub use outcomes::{ControllerOutcome, OutcomeKind, outcome_kind};
pub use payloads::EventPayload;
pub use project_state::{
    ArtifactState, ArtifactStatus, OutcomeRecord, PauseState, ProjectState, TransitionRecord,
    VerificationSummary, create_empty_project_state,
};
pub use reducer::{apply_event, replay_project};
pub use transitions::{
    TransitionTrigger, can_enter_mode, earliest_affected_stage, evaluate_transition, kind_to_mode,
    should_auto_advance,
};

/// Version of the crate's shared domain contract (kept in lockstep with the
/// TypeScript `EVENT_ENVELOPE_VERSION` and fixture format).
pub const CORE_CONTRACT_VERSION: u32 = 1;
