//! Event contracts: the 24 event types, the untyped envelope, two-stage
//! decoding (envelope inspect → typed payload validate), and SHA-256 hash
//! chain helpers. Ported from `src/core/events.ts` plus the envelope rules in
//! `docs/event-format.md` §1 and §3.

use crate::canonical::canonical_json;
use crate::payloads::{EventPayload, decoder_for};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// Version of the event envelope itself.
pub const EVENT_ENVELOPE_VERSION: u32 = 1;

/// All 24 event types, in the canonical order of the TypeScript
/// `RELAY_EVENT_TYPES`.
pub const RELAY_EVENT_TYPES: [RelayEventType; 24] = [
    RelayEventType::ProjectInitialized,
    RelayEventType::SessionStarted,
    RelayEventType::IntentProposed,
    RelayEventType::IntentApplied,
    RelayEventType::ModeEntered,
    RelayEventType::ModeTransitionRejected,
    RelayEventType::AutonomyChanged,
    RelayEventType::ArtifactCreated,
    RelayEventType::ArtifactRevised,
    RelayEventType::ArtifactApprovalRequested,
    RelayEventType::ArtifactApproved,
    RelayEventType::ArtifactInvalidated,
    RelayEventType::ArtifactInspected,
    RelayEventType::TransitionEvaluated,
    RelayEventType::ModelRequested,
    RelayEventType::ModelResponded,
    RelayEventType::ModelFailed,
    RelayEventType::ToolExecuted,
    RelayEventType::ToolDenied,
    RelayEventType::VerificationCompleted,
    RelayEventType::WorkflowPaused,
    RelayEventType::WorkflowResumed,
    RelayEventType::BudgetExhausted,
    RelayEventType::RunInterrupted,
];

/// One event type. The wire spelling is the dotted event name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum RelayEventType {
    /// `project.initialized`
    #[serde(rename = "project.initialized")]
    ProjectInitialized,
    /// `session.started`
    #[serde(rename = "session.started")]
    SessionStarted,
    /// `intent.proposed`
    #[serde(rename = "intent.proposed")]
    IntentProposed,
    /// `intent.applied`
    #[serde(rename = "intent.applied")]
    IntentApplied,
    /// `mode.entered`
    #[serde(rename = "mode.entered")]
    ModeEntered,
    /// `mode.transition.rejected`
    #[serde(rename = "mode.transition.rejected")]
    ModeTransitionRejected,
    /// `autonomy.changed`
    #[serde(rename = "autonomy.changed")]
    AutonomyChanged,
    /// `artifact.created`
    #[serde(rename = "artifact.created")]
    ArtifactCreated,
    /// `artifact.revised`
    #[serde(rename = "artifact.revised")]
    ArtifactRevised,
    /// `artifact.approval.requested`
    #[serde(rename = "artifact.approval.requested")]
    ArtifactApprovalRequested,
    /// `artifact.approved`
    #[serde(rename = "artifact.approved")]
    ArtifactApproved,
    /// `artifact.invalidated`
    #[serde(rename = "artifact.invalidated")]
    ArtifactInvalidated,
    /// `artifact.inspected`
    #[serde(rename = "artifact.inspected")]
    ArtifactInspected,
    /// `transition.evaluated`
    #[serde(rename = "transition.evaluated")]
    TransitionEvaluated,
    /// `model.requested`
    #[serde(rename = "model.requested")]
    ModelRequested,
    /// `model.responded`
    #[serde(rename = "model.responded")]
    ModelResponded,
    /// `model.failed`
    #[serde(rename = "model.failed")]
    ModelFailed,
    /// `tool.executed`
    #[serde(rename = "tool.executed")]
    ToolExecuted,
    /// `tool.denied`
    #[serde(rename = "tool.denied")]
    ToolDenied,
    /// `verification.completed`
    #[serde(rename = "verification.completed")]
    VerificationCompleted,
    /// `workflow.paused`
    #[serde(rename = "workflow.paused")]
    WorkflowPaused,
    /// `workflow.resumed`
    #[serde(rename = "workflow.resumed")]
    WorkflowResumed,
    /// `budget.exhausted`
    #[serde(rename = "budget.exhausted")]
    BudgetExhausted,
    /// `run.interrupted`
    #[serde(rename = "run.interrupted")]
    RunInterrupted,
}

impl RelayEventType {
    /// The dotted wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            RelayEventType::ProjectInitialized => "project.initialized",
            RelayEventType::SessionStarted => "session.started",
            RelayEventType::IntentProposed => "intent.proposed",
            RelayEventType::IntentApplied => "intent.applied",
            RelayEventType::ModeEntered => "mode.entered",
            RelayEventType::ModeTransitionRejected => "mode.transition.rejected",
            RelayEventType::AutonomyChanged => "autonomy.changed",
            RelayEventType::ArtifactCreated => "artifact.created",
            RelayEventType::ArtifactRevised => "artifact.revised",
            RelayEventType::ArtifactApprovalRequested => "artifact.approval.requested",
            RelayEventType::ArtifactApproved => "artifact.approved",
            RelayEventType::ArtifactInvalidated => "artifact.invalidated",
            RelayEventType::ArtifactInspected => "artifact.inspected",
            RelayEventType::TransitionEvaluated => "transition.evaluated",
            RelayEventType::ModelRequested => "model.requested",
            RelayEventType::ModelResponded => "model.responded",
            RelayEventType::ModelFailed => "model.failed",
            RelayEventType::ToolExecuted => "tool.executed",
            RelayEventType::ToolDenied => "tool.denied",
            RelayEventType::VerificationCompleted => "verification.completed",
            RelayEventType::WorkflowPaused => "workflow.paused",
            RelayEventType::WorkflowResumed => "workflow.resumed",
            RelayEventType::BudgetExhausted => "budget.exhausted",
            RelayEventType::RunInterrupted => "run.interrupted",
        }
    }

    /// Parse the dotted wire spelling.
    pub fn parse(value: &str) -> Option<Self> {
        RELAY_EVENT_TYPES
            .iter()
            .copied()
            .find(|event_type| event_type.as_str() == value)
    }

    /// The payload schema version this build understands for the type.
    pub fn payload_version(self) -> u32 {
        EVENT_PAYLOAD_VERSIONS[RELAY_EVENT_TYPES
            .iter()
            .position(|candidate| *candidate == self)
            .unwrap_or(0)]
    }
}

/// Per-type payload schema versions. Bump when a payload shape changes
/// incompatibly; all are 1 today.
pub const EVENT_PAYLOAD_VERSIONS: [u32; 24] = [1; 24];

/// The untyped event envelope as persisted in a JSONL log. Field names are
/// camelCase per `docs/event-format.md` §1; unknown additional envelope fields
/// are ignored (readers must accept them when the envelope version is
/// understood), while payload schemas stay exact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventEnvelope {
    /// Envelope container version; readers reject newer versions fail-closed.
    #[serde(rename = "envelopeVersion", default)]
    pub envelope_version: u32,
    /// `evt_<uuid-v4>` event id, unique within the log.
    pub id: String,
    /// `prj_<uuid-v4>` project id; every event in one log shares it.
    #[serde(rename = "projectId")]
    pub project_id: String,
    /// 1-based, contiguous across the whole project log.
    pub sequence: u64,
    /// Producing session, or `null`.
    #[serde(rename = "sessionId")]
    pub session_id: Option<String>,
    /// The dotted event type name.
    #[serde(rename = "type")]
    pub event_type: String,
    /// Per-type payload schema version.
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    /// RFC 3339 UTC with millisecond precision and `Z` suffix.
    pub timestamp: String,
    /// `cor_<uuid>`; fresh when the event starts an operation.
    #[serde(rename = "correlationId")]
    pub correlation_id: String,
    /// Id of the event that directly caused this one, or `null`.
    #[serde(rename = "causationId")]
    pub causation_id: Option<String>,
    /// Type-specific payload (unvalidated at this stage).
    pub payload: Value,
    /// Hash of the previous event; `null` exactly at sequence 1.
    #[serde(rename = "previousHash")]
    pub previous_hash: Option<String>,
    /// Hex SHA-256 over the canonical envelope minus this field.
    pub hash: Option<String>,
}

impl EventEnvelope {
    /// Compute this envelope's chain hash: the lowercase hex SHA-256 over the
    /// canonical serialization of the envelope without its `hash` field.
    pub fn compute_hash(&self) -> String {
        let mut without_hash = serde_json::to_value(self).unwrap_or(Value::Null);
        if let Value::Object(map) = &mut without_hash {
            map.remove("hash");
        }
        compute_envelope_hash(&without_hash)
    }
}

/// A fully validated event: known type, compatible version, valid payload.
#[derive(Debug, Clone, PartialEq)]
pub struct RelayEvent {
    /// `evt_…` event id.
    pub id: String,
    /// `prj_…` project id.
    pub project_id: String,
    /// 1-based sequence number.
    pub sequence: u64,
    /// Producing session, or `None`.
    pub session_id: Option<String>,
    /// The event type.
    pub event_type: RelayEventType,
    /// Payload schema version (always the version this build understands).
    pub schema_version: u32,
    /// RFC 3339 UTC timestamp with milliseconds and `Z`.
    pub timestamp: String,
    /// `cor_…` correlation id.
    pub correlation_id: String,
    /// Causing event id, or `None`.
    pub causation_id: Option<String>,
    /// The typed, validated payload.
    pub payload: EventPayload,
}

/// Domain error. The variants and messages mirror the TypeScript error
/// classes exactly — the message strings are part of the compatibility
/// contract.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum CoreError {
    /// Unknown event type (`UnknownEventTypeError`).
    #[error("Unknown event type \"{event_type}\": {detail}")]
    UnknownEventType {
        /// The unrecognized event type name.
        event_type: String,
        /// Actionable diagnostic.
        detail: String,
    },
    /// Payload schema version this build cannot read (`IncompatibleEventVersionError`).
    #[error(
        "Event \"{event_type}\" has schema version {found}, but this Relay build understands version {expected}. \
         The project was likely written by a newer Relay. Upgrade Relay or archive this project directory."
    )]
    IncompatibleEventVersion {
        /// The event type whose version is incompatible.
        event_type: String,
        /// The version found in the record.
        found: u32,
        /// The version this build understands.
        expected: u32,
    },
    /// Payload failed schema validation (`InvalidEventPayloadError`).
    #[error("Event \"{event_type}\" (sequence {sequence}) has an invalid payload: {detail}")]
    InvalidEventPayload {
        /// The event type whose payload is invalid.
        event_type: String,
        /// The sequence of the offending event.
        sequence: u64,
        /// Zod-style field-level detail.
        detail: String,
    },
    /// The event log cannot be folded into consistent state (`EventFoldError`).
    #[error("{detail}")]
    EventFold {
        /// Actionable diagnostic.
        detail: String,
    },
    /// The envelope itself is unreadable (spec-level check the TypeScript
    /// store does not perform; see `docs/event-format.md` §1).
    #[error("event envelope is invalid: {detail}")]
    InvalidEventEnvelope {
        /// Actionable diagnostic.
        detail: String,
    },
}

impl CoreError {
    /// The TypeScript error class name this error corresponds to.
    pub fn error_name(&self) -> &'static str {
        match self {
            CoreError::UnknownEventType { .. } => "UnknownEventTypeError",
            CoreError::IncompatibleEventVersion { .. } => "IncompatibleEventVersionError",
            CoreError::InvalidEventPayload { .. } => "InvalidEventPayloadError",
            CoreError::EventFold { .. } => "EventFoldError",
            CoreError::InvalidEventEnvelope { .. } => "InvalidEventEnvelopeError",
        }
    }
}

/// Whether a timestamp matches RFC 3339 UTC with millisecond precision and a
/// `Z` suffix (`YYYY-MM-DDTHH:MM:SS.sssZ`, exactly like `Date.toISOString()`).
pub fn is_iso_timestamp_millis_z(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 24 {
        return false;
    }
    let digit = |i: usize| bytes[i].is_ascii_digit();
    let all_digits = |range: std::ops::Range<usize>| range.clone().all(|i| digit(i));
    if !(all_digits(0..4)
        && bytes[4] == b'-'
        && all_digits(5..7)
        && bytes[7] == b'-'
        && all_digits(8..10)
        && bytes[10] == b'T'
        && all_digits(11..13)
        && bytes[13] == b':'
        && all_digits(14..16)
        && bytes[16] == b':'
        && all_digits(17..19)
        && bytes[19] == b'.'
        && all_digits(20..23)
        && bytes[23] == b'Z')
    {
        return false;
    }
    let month = value[5..7].parse::<u8>().unwrap_or(0);
    let day = value[8..10].parse::<u8>().unwrap_or(0);
    let hour = value[11..13].parse::<u8>().unwrap_or(99);
    let minute = value[14..16].parse::<u8>().unwrap_or(99);
    let second = value[17..19].parse::<u8>().unwrap_or(99);
    (1..=12).contains(&month) && (1..=31).contains(&day) && hour < 24 && minute < 60 && second < 60
}

/// Validate the envelope-level rules that precede payload decoding: envelope
/// version, identifier shapes, sequence, and timestamp format.
fn validate_envelope_shape(envelope: &EventEnvelope) -> Result<(), CoreError> {
    if envelope.envelope_version != EVENT_ENVELOPE_VERSION {
        return Err(CoreError::InvalidEventEnvelope {
            detail: format!(
                "unsupported envelopeVersion {} (this Relay build understands {}); \
                 the project was likely written by a newer Relay. Upgrade Relay or archive this project directory",
                envelope.envelope_version, EVENT_ENVELOPE_VERSION
            ),
        });
    }
    if envelope.sequence == 0 {
        return Err(CoreError::InvalidEventEnvelope {
            detail: format!(
                "sequence must be a 1-based positive integer (found {})",
                envelope.sequence
            ),
        });
    }
    if let Err(error) = crate::ids::EventId::parse(&envelope.id) {
        return Err(CoreError::InvalidEventEnvelope {
            detail: format!("id {error}"),
        });
    }
    if let Err(error) = crate::ids::ProjectId::parse(&envelope.project_id) {
        return Err(CoreError::InvalidEventEnvelope {
            detail: format!("projectId {error}"),
        });
    }
    if let Some(session_id) = &envelope.session_id {
        if let Err(error) = crate::ids::SessionId::parse(session_id) {
            return Err(CoreError::InvalidEventEnvelope {
                detail: format!("sessionId {error}"),
            });
        }
    }
    if let Err(error) = crate::ids::CorrelationId::parse(&envelope.correlation_id) {
        return Err(CoreError::InvalidEventEnvelope {
            detail: format!("correlationId {error}"),
        });
    }
    if let Some(causation_id) = &envelope.causation_id {
        if let Err(error) = crate::ids::EventId::parse(causation_id) {
            return Err(CoreError::InvalidEventEnvelope {
                detail: format!("causationId {error}"),
            });
        }
    }
    if !is_iso_timestamp_millis_z(&envelope.timestamp) {
        return Err(CoreError::InvalidEventEnvelope {
            detail: format!(
                "timestamp \"{}\" must be RFC 3339 UTC with millisecond precision and a Z suffix",
                envelope.timestamp
            ),
        });
    }
    Ok(())
}

/// Two-stage validation of a stored envelope into a typed event: envelope
/// shape/version inspection first, then the typed payload schema. Error
/// classes, messages, and their order match `validateStoredEvent` in the
/// TypeScript core (with envelope checks the spec adds on top).
pub fn validate_stored_event(envelope: &EventEnvelope) -> Result<RelayEvent, CoreError> {
    validate_envelope_shape(envelope)?;
    let Some(event_type) = RelayEventType::parse(&envelope.event_type) else {
        return Err(CoreError::UnknownEventType {
            event_type: envelope.event_type.clone(),
            detail: format!(
                "at sequence {}. The event log may come from a different tool or a newer Relay version.",
                envelope.sequence
            ),
        });
    };
    let expected = event_type.payload_version();
    if envelope.schema_version != expected {
        return Err(CoreError::IncompatibleEventVersion {
            event_type: envelope.event_type.clone(),
            found: envelope.schema_version,
            expected,
        });
    }
    let payload = crate::payloads::decode_payload(&envelope.payload, decoder_for(&event_type))
        .map_err(|detail| CoreError::InvalidEventPayload {
            event_type: envelope.event_type.clone(),
            sequence: envelope.sequence,
            detail,
        })?;
    Ok(RelayEvent {
        id: envelope.id.clone(),
        project_id: envelope.project_id.clone(),
        sequence: envelope.sequence,
        session_id: envelope.session_id.clone(),
        event_type,
        schema_version: envelope.schema_version,
        timestamp: envelope.timestamp.clone(),
        correlation_id: envelope.correlation_id.clone(),
        causation_id: envelope.causation_id.clone(),
        payload,
    })
}

/// Hex SHA-256 over the canonical serialization of `envelope` minus its
/// `hash` field (§3). `envelope` may carry the `hash` field; it is removed
/// before hashing.
pub fn compute_envelope_hash(envelope: &Value) -> String {
    let mut without_hash = envelope.clone();
    if let Value::Object(map) = &mut without_hash {
        map.remove("hash");
    }
    let canonical = canonical_json(&without_hash);
    let digest = Sha256::digest(canonical.as_bytes());
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
}

/// One violation found while verifying a canonical JSONL event log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainViolation {
    /// 1-based line number the violation was found on.
    pub line: usize,
    /// Human-readable description of the violation.
    pub message: String,
}

/// Verify a canonical JSONL event log: every line canonically serialized,
/// every hash correct, and the previous-hash chain contiguous (`null` at
/// sequence 1). Returns all violations found; an empty list means the log is
/// well-formed.
pub fn verify_canonical_log(text: &str) -> Vec<ChainViolation> {
    let mut violations = Vec::new();
    let ends_with_newline = text.is_empty() || text.ends_with('\n');
    if !ends_with_newline {
        violations.push(ChainViolation {
            line: 0,
            message: "log does not end with a newline".to_string(),
        });
    }
    let body: Vec<&str> = text.split('\n').filter(|line| !line.is_empty()).collect();
    let mut expected_previous_hash: Option<String> = None;
    for (index, line) in body.iter().enumerate() {
        let line_number = index + 1;
        let parsed: Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(error) => {
                violations.push(ChainViolation {
                    line: line_number,
                    message: format!("not valid JSON ({error})"),
                });
                return violations;
            }
        };
        if canonical_json(&parsed) != *line {
            violations.push(ChainViolation {
                line: line_number,
                message: "not canonically serialized".to_string(),
            });
        }
        let Value::Object(map) = &parsed else {
            violations.push(ChainViolation {
                line: line_number,
                message: "not a JSON object".to_string(),
            });
            return violations;
        };
        if map.get("envelopeVersion").and_then(Value::as_u64)
            != Some(u64::from(EVENT_ENVELOPE_VERSION))
        {
            violations.push(ChainViolation {
                line: line_number,
                message: format!(
                    "envelopeVersion {:?} is not {}",
                    map.get("envelopeVersion"),
                    EVENT_ENVELOPE_VERSION
                ),
            });
        }
        let expected_hash = compute_envelope_hash(&parsed);
        let actual_hash = map.get("hash").and_then(Value::as_str);
        if actual_hash != Some(expected_hash.as_str()) {
            violations.push(ChainViolation {
                line: line_number,
                message: format!("hash mismatch (expected {expected_hash}, found {actual_hash:?})"),
            });
        }
        let actual_previous = map.get("previousHash").and_then(Value::as_str);
        if actual_previous != expected_previous_hash.as_deref() {
            violations.push(ChainViolation {
                line: line_number,
                message: format!(
                    "previousHash mismatch (expected {:?}, found {:?})",
                    expected_previous_hash, actual_previous
                ),
            });
        }
        expected_previous_hash = Some(expected_hash);
    }
    violations
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::payloads::EventPayload;
    use serde_json::json;

    fn envelope(payload: Value) -> EventEnvelope {
        EventEnvelope {
            envelope_version: 1,
            id: "evt_00000000-0000-4000-8000-000000000001".to_string(),
            project_id: "prj_00000000-0000-4000-8000-000000000001".to_string(),
            sequence: 1,
            session_id: None,
            event_type: "project.initialized".to_string(),
            schema_version: 1,
            timestamp: "2026-10-13T00:00:00.000Z".to_string(),
            correlation_id: "cor_00000000-0000-4000-8000-000000000001".to_string(),
            causation_id: None,
            payload,
            previous_hash: None,
            hash: None,
        }
    }

    #[test]
    fn validates_a_stored_event() {
        let event = envelope(json!({
            "goal": "g",
            "workspaceRoot": "/tmp",
            "initialAutonomy": "workflow"
        }));
        let validated = validate_stored_event(&event);
        assert!(validated.is_ok());
        assert_eq!(
            validated.unwrap().event_type,
            RelayEventType::ProjectInitialized
        );
    }

    #[test]
    fn rejects_unknown_event_types_with_actionable_diagnostics() {
        let mut event = envelope(json!({}));
        event.event_type = "magic.happened".to_string();
        let error = validate_stored_event(&event).expect_err("unknown type must fail");
        assert_eq!(error.error_name(), "UnknownEventTypeError");
        assert!(
            error
                .to_string()
                .contains("Unknown event type \"magic.happened\"")
        );
        assert!(error.to_string().contains("at sequence 1"));
    }

    #[test]
    fn rejects_incompatible_event_versions() {
        let mut event = envelope(json!({
            "goal": "g",
            "workspaceRoot": "/tmp",
            "initialAutonomy": "workflow"
        }));
        event.schema_version = 7;
        let error = validate_stored_event(&event).expect_err("newer version must fail");
        assert_eq!(error.error_name(), "IncompatibleEventVersionError");
        assert!(error.to_string().contains("schema version 7"));
        assert!(
            error
                .to_string()
                .contains("Upgrade Relay or archive this project directory")
        );
    }

    #[test]
    fn rejects_invalid_payloads_with_field_level_detail() {
        let mut event = envelope(json!({ "goal": "", "workspaceRoot": "/tmp" }));
        event.sequence = 4;
        let error = validate_stored_event(&event).expect_err("invalid payload must fail");
        assert_eq!(error.error_name(), "InvalidEventPayloadError");
        let message = error.to_string();
        assert!(message.contains("sequence 4"), "{message}");
        assert!(
            message.contains("goal: Too small: expected string to have >=1 characters"),
            "{message}"
        );
    }

    #[test]
    fn rejects_newer_envelope_versions_fail_closed() {
        let mut event = envelope(json!({
            "goal": "g",
            "workspaceRoot": "/tmp",
            "initialAutonomy": "workflow"
        }));
        event.envelope_version = 2;
        let error = validate_stored_event(&event).expect_err("newer envelope must fail");
        assert_eq!(error.error_name(), "InvalidEventEnvelopeError");
        assert!(error.to_string().contains("envelopeVersion"), "{}", error);
    }

    #[test]
    fn accepts_model_events_with_and_without_the_optional_role_field() {
        let mut base = envelope(json!({}));
        base.event_type = "model.requested".to_string();
        base.sequence = 1;
        base.payload = json!({
            "provider": "anthropic",
            "model": "claude-sonnet-5-5",
            "purpose": "mode:discovery",
            "mode": "discovery",
            "attempt": 1
        });
        let legacy = validate_stored_event(&base).expect("pre-role payload must validate");
        match legacy.payload {
            EventPayload::ModelRequested(payload) => assert!(payload.role.is_none()),
            other => panic!("unexpected payload {other:?}"),
        }
        let mut roled = base.clone();
        roled.sequence = 2;
        roled.payload["role"] = json!("reasoning");
        let event = validate_stored_event(&roled).expect("role payload must validate");
        match event.payload {
            EventPayload::ModelRequested(payload) => {
                assert_eq!(payload.role.as_deref(), Some("reasoning"))
            }
            other => panic!("unexpected payload {other:?}"),
        }
        let mut empty_role = base;
        empty_role.sequence = 3;
        empty_role.payload["role"] = json!("");
        let error = validate_stored_event(&empty_role).expect_err("empty role must fail");
        assert!(error.to_string().contains("role"), "{}", error);
    }

    #[test]
    fn keeps_model_event_payload_schema_versions_at_one() {
        for event_type in [
            RelayEventType::ModelRequested,
            RelayEventType::ModelResponded,
            RelayEventType::ModelFailed,
        ] {
            assert_eq!(event_type.payload_version(), 1);
        }
        assert_eq!(RelayEventType::ProjectInitialized.payload_version(), 1);
    }

    #[test]
    fn validates_timestamp_shape() {
        assert!(is_iso_timestamp_millis_z("2026-10-13T08:00:00.123Z"));
        assert!(!is_iso_timestamp_millis_z("2026-10-13T08:00:00Z"));
        assert!(!is_iso_timestamp_millis_z("2026-10-13T08:00:00.123+00:00"));
        assert!(!is_iso_timestamp_millis_z("2026-13-13T08:00:00.123Z"));
        assert!(!is_iso_timestamp_millis_z("2026-10-13T24:00:00.123Z"));
    }

    #[test]
    fn computes_and_verifies_the_hash_chain() {
        let mut event = envelope(json!({
            "goal": "g",
            "workspaceRoot": "/tmp",
            "initialAutonomy": "workflow"
        }));
        event.hash = Some(event.compute_hash());
        let first = serde_json::to_value(&event).unwrap();
        let mut second = envelope(json!({
            "sessionId": "ses_00000000-0000-4000-8000-000000000002",
            "interactive": true
        }));
        second.event_type = "session.started".to_string();
        second.sequence = 2;
        second.id = "evt_00000000-0000-4000-8000-000000000002".to_string();
        second.correlation_id = "cor_00000000-0000-4000-8000-000000000002".to_string();
        second.timestamp = "2026-10-13T00:00:01.000Z".to_string();
        second.previous_hash = event.hash.clone();
        second.hash = Some(second.compute_hash());
        let second_value = serde_json::to_value(&second).unwrap();

        let text = format!(
            "{}\n{}\n",
            canonical_json(&first),
            canonical_json(&second_value)
        );
        assert!(verify_canonical_log(&text).is_empty(), "{text}");

        // Tamper with the second payload: the hash check must catch it.
        let mut tampered = second_value.clone();
        tampered["payload"]["interactive"] = json!(false);
        let tampered_text = format!(
            "{}\n{}\n",
            canonical_json(&first),
            canonical_json(&tampered)
        );
        let violations = verify_canonical_log(&tampered_text);
        assert!(
            violations
                .iter()
                .any(|violation| violation.message.contains("hash mismatch"))
        );
    }

    #[test]
    fn round_trips_payload_serialization() {
        let mut event = envelope(json!({
            "provider": "anthropic",
            "model": "claude-sonnet-5-5",
            "purpose": "mode:discovery",
            "mode": "discovery",
            "attempt": 1,
            "role": "reasoning"
        }));
        event.event_type = "model.requested".to_string();
        let validated = validate_stored_event(&event).expect("valid model event");
        let wire = serde_json::to_value(&validated.payload).unwrap();
        assert_eq!(
            wire,
            json!({
                "provider": "anthropic",
                "model": "claude-sonnet-5-5",
                "purpose": "mode:discovery",
                "mode": "discovery",
                "attempt": 1,
                "role": "reasoning"
            })
        );
        // Legacy event without role: the field stays absent on the wire.
        event.payload["role"].take();
        let legacy = validate_stored_event(&event).expect("valid legacy model event");
        let legacy_wire = serde_json::to_value(&legacy.payload).unwrap();
        assert!(legacy_wire.get("role").is_none());
    }
}
