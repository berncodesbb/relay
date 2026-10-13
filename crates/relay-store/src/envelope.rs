//! The event envelope: typed access, validation, and hash chaining
//! (`docs/event-format.md` §1 and §3).
//!
//! INTEGRATION SEAM: the envelope type, the event-type list, payload schema
//! versions, and payload validation belong to `relay-core`. Until relay-core
//! publishes `EventEnvelope` / `RelayEvent` and its schemas, this module
//! carries the store-local envelope with envelope-level validation (shape,
//! ids, versions, chain metadata) and an injectable
//! [`PayloadValidator`] hook that the relay-core integration will bind to
//! the real payload schemas. The type list below mirrors the 24 event types
//! named normatively by the spec.

use std::sync::Arc;

use sha2::{Digest, Sha256};

use crate::canonical::Json;
use crate::clock::parse_iso_to_epoch_millis;
use crate::error::Result;
use crate::error::StoreError;

/// Envelope container version understood (and written) by this build.
pub const ENVELOPE_VERSION: u64 = 1;

/// The 24 Relay event types (`src/core/events.ts` `RELAY_EVENT_TYPES`, as
/// referenced normatively by `docs/event-format.md` §1).
///
/// MIRRORED CONTRACT (relay-core): when relay-core publishes its event
/// module, delete this list and re-export from there.
pub const KNOWN_EVENT_TYPES: [&str; 24] = [
    "project.initialized",
    "session.started",
    "intent.proposed",
    "intent.applied",
    "mode.entered",
    "mode.transition.rejected",
    "autonomy.changed",
    "artifact.created",
    "artifact.revised",
    "artifact.approval.requested",
    "artifact.approved",
    "artifact.invalidated",
    "artifact.inspected",
    "transition.evaluated",
    "model.requested",
    "model.responded",
    "model.failed",
    "tool.executed",
    "tool.denied",
    "verification.completed",
    "workflow.paused",
    "workflow.resumed",
    "budget.exhausted",
    "run.interrupted",
];

/// Per-type payload schema versions (all `1` today). Mirrors
/// `EVENT_PAYLOAD_VERSIONS`; MIRRORED CONTRACT (relay-core).
pub fn payload_schema_version(event_type: &str) -> Option<u64> {
    KNOWN_EVENT_TYPES.contains(&event_type).then_some(1)
}

/// Id prefixes permitted in envelope id fields (`docs/event-format.md` §1).
pub const ID_PREFIXES: [&str; 6] = ["evt", "cor", "ses", "prj", "art", "run"];

/// A validated event envelope. Field names match the canonical JSON exactly.
#[derive(Debug, Clone, PartialEq)]
pub struct EventEnvelope {
    /// Envelope container version; always `1` in this format.
    pub envelope_version: u64,
    /// `<prefix>_<uuid-v4-lowercase>`; unique within the log.
    pub id: String,
    /// `prj_<uuid>`; identical for every event in one log.
    pub project_id: String,
    /// 1-based contiguous position in the project log.
    pub sequence: u64,
    /// Producing session, or `None`.
    pub session_id: Option<String>,
    /// One of [`KNOWN_EVENT_TYPES`].
    pub event_type: String,
    /// Per-type payload schema version.
    pub schema_version: u64,
    /// RFC 3339 UTC, millisecond precision, `Z` suffix.
    pub timestamp: String,
    /// Operation correlation id (`cor_<uuid>`).
    pub correlation_id: String,
    /// Directly causing event id, or `None`.
    pub causation_id: Option<String>,
    /// Type-specific payload (opaque canonical JSON here; typed by
    /// relay-core).
    pub payload: Json,
    /// Hash of the previous event; `None` exactly at sequence 1.
    pub previous_hash: Option<String>,
    /// Hex SHA-256 over the canonical envelope minus this field.
    pub hash: String,
}

/// Validates a payload for an event type. The store calls this during
/// append validation, replay, and legacy import ("same errors as replay
/// would give"). The relay-core integration supplies the real implementation
/// (strict per-type schemas); the default store configuration omits it and
/// performs envelope-level validation only.
pub type PayloadValidator =
    Arc<dyn Fn(&str, &Json) -> std::result::Result<(), String> + Send + Sync>;

/// The fields of an envelope the store assigns before hashing, bundled for
/// [`EventEnvelope::build`].
#[derive(Debug, Clone)]
pub struct EnvelopeDraft {
    /// Project the event belongs to (`prj_<uuid>`).
    pub project_id: String,
    /// 1-based position in the project log.
    pub sequence: u64,
    /// Producing session, if any.
    pub session_id: Option<String>,
    /// Event type (one of [`KNOWN_EVENT_TYPES`]).
    pub event_type: String,
    /// Relay timestamp.
    pub timestamp: String,
    /// Correlation id (`cor_<uuid>`).
    pub correlation_id: String,
    /// Causing event id, if any.
    pub causation_id: Option<String>,
    /// Type-specific payload.
    pub payload: Json,
    /// Hash of the previous event; `None` exactly at sequence 1.
    pub previous_hash: Option<String>,
    /// Fresh event id (`evt_<uuid>`).
    pub id: String,
}

impl EventEnvelope {
    /// Build a fully hashed envelope from `draft`. The event type must be
    /// known; `previous_hash` must be `None` iff `sequence == 1`.
    pub fn build(draft: EnvelopeDraft) -> Result<EventEnvelope> {
        let EnvelopeDraft {
            project_id,
            sequence,
            session_id,
            event_type,
            timestamp,
            correlation_id,
            causation_id,
            payload,
            previous_hash,
            id,
        } = draft;
        let schema_version =
            payload_schema_version(&event_type).ok_or_else(|| StoreError::Append {
                reason: format!(
                    "unknown event type \"{event_type}\": no payload schema is registered \
                     for this event type"
                ),
            })?;
        let envelope = EventEnvelope {
            envelope_version: ENVELOPE_VERSION,
            id,
            project_id,
            sequence,
            session_id,
            event_type,
            schema_version,
            timestamp,
            correlation_id,
            causation_id,
            payload,
            previous_hash,
            hash: String::new(),
        };
        let mut with_hash = envelope.clone();
        with_hash.hash = with_hash.compute_hash();
        Ok(with_hash)
    }

    /// Parse one physical line (without the trailing `\n`) into a validated
    /// envelope, recomputing the hash — readers never trust raw bytes.
    ///
    /// `location` is used to build record diagnostics.
    pub fn parse(line: &str, location: RecordLocation<'_>) -> Result<EventEnvelope> {
        let value = Json::parse(line).map_err(|error| locate(location, error, None))?;
        let envelope = EventEnvelope::from_json(&value, location)?;
        let expected = envelope.compute_hash();
        if !constant_time_eq(expected.as_bytes(), envelope.hash.as_bytes()) {
            return Err(locate(
                location,
                StoreError::ChainBreak {
                    segment: location.segment.to_string(),
                    line: location.line,
                    sequence: envelope.sequence,
                    expected,
                    actual: envelope.hash.clone(),
                    hash_field: true,
                },
                Some(envelope.sequence),
            ));
        }
        Ok(envelope)
    }

    fn from_json(value: &Json, location: RecordLocation<'_>) -> Result<EventEnvelope> {
        let field = |name: &str| -> Result<&Json> {
            value.get(name).ok_or_else(|| {
                locate(
                    location,
                    StoreError::Canonical {
                        reason: format!("envelope field \"{name}\" is missing"),
                    },
                    None,
                )
            })
        };
        let envelope_version = require_u64(field("envelopeVersion")?, location, "envelopeVersion")?;
        match envelope_version.cmp(&ENVELOPE_VERSION) {
            std::cmp::Ordering::Equal => {}
            std::cmp::Ordering::Greater => {
                return Err(locate(
                    location,
                    StoreError::UnsupportedStore {
                        found: envelope_version,
                        supported: ENVELOPE_VERSION,
                        location: "envelope field \"envelopeVersion\"".to_string(),
                    },
                    None,
                ));
            }
            std::cmp::Ordering::Less => {
                return Err(locate(
                    location,
                    StoreError::Canonical {
                        reason: format!("envelopeVersion must be {ENVELOPE_VERSION}"),
                    },
                    None,
                ));
            }
        }

        let id = require_prefixed_uuid(field("id")?, location, "id")?;
        let project_id = require_exact_prefix(field("projectId")?, location, "projectId", "prj")?;
        let sequence = require_u64(field("sequence")?, location, "sequence")?;
        if sequence == 0 {
            return Err(locate(
                location,
                StoreError::Canonical {
                    reason: "sequence is 1-based and must be at least 1".to_string(),
                },
                Some(sequence),
            ));
        }
        let session_id = match value.get("sessionId") {
            None | Some(Json::Null) => None,
            Some(Json::Str(text)) => {
                validate_prefixed_uuid(text, "ses").map_err(|reason| {
                    locate(location, StoreError::Canonical { reason }, Some(sequence))
                })?;
                Some(text.clone())
            }
            Some(_) => {
                return Err(locate(
                    location,
                    StoreError::Canonical {
                        reason: "sessionId must be a string or null".to_string(),
                    },
                    Some(sequence),
                ));
            }
        };
        let event_type = match field("type")?.as_str() {
            Some(text) => text.to_string(),
            None => {
                return Err(locate(
                    location,
                    StoreError::Canonical {
                        reason: "type must be a string".to_string(),
                    },
                    Some(sequence),
                ));
            }
        };
        if !KNOWN_EVENT_TYPES.iter().any(|known| *known == event_type) {
            return Err(locate(
                location,
                StoreError::Canonical {
                    reason: format!(
                        "unknown event type \"{event_type}\". The event log may come from a \
                         different tool or a newer Relay version"
                    ),
                },
                Some(sequence),
            ));
        }
        let schema_version = require_u64(field("schemaVersion")?, location, "schemaVersion")?;
        let expected_version = payload_schema_version(&event_type).unwrap_or(1);
        if schema_version > expected_version {
            return Err(locate(
                location,
                StoreError::Canonical {
                    reason: format!(
                        "event \"{event_type}\" has schema version {schema_version}, but this \
                         Relay build understands version {expected_version}. The project was \
                         likely written by a newer Relay. Upgrade Relay or archive this project \
                         directory."
                    ),
                },
                Some(sequence),
            ));
        }
        if schema_version != expected_version {
            return Err(locate(
                location,
                StoreError::Canonical {
                    reason: format!(
                        "event \"{event_type}\" has schema version {schema_version}, expected \
                         {expected_version}"
                    ),
                },
                Some(sequence),
            ));
        }
        let timestamp = match field("timestamp")?.as_str() {
            Some(text) => text.to_string(),
            None => {
                return Err(locate(
                    location,
                    StoreError::Canonical {
                        reason: "timestamp must be a string".to_string(),
                    },
                    Some(sequence),
                ));
            }
        };
        if parse_iso_to_epoch_millis(&timestamp).is_none() {
            return Err(locate(
                location,
                StoreError::Canonical {
                    reason: format!(
                        "timestamp \"{timestamp}\" must be RFC 3339 UTC with millisecond \
                         precision and a Z suffix (YYYY-MM-DDTHH:MM:SS.sssZ)"
                    ),
                },
                Some(sequence),
            ));
        }
        let correlation_id =
            require_exact_prefix(field("correlationId")?, location, "correlationId", "cor")?;
        let causation_id = match value.get("causationId") {
            None | Some(Json::Null) => None,
            Some(Json::Str(text)) => {
                validate_prefixed_uuid(text, "").map_err(|reason| {
                    locate(location, StoreError::Canonical { reason }, Some(sequence))
                })?;
                Some(text.clone())
            }
            Some(_) => {
                return Err(locate(
                    location,
                    StoreError::Canonical {
                        reason: "causationId must be a string or null".to_string(),
                    },
                    Some(sequence),
                ));
            }
        };
        let payload = field("payload")?.clone();
        if !matches!(payload, Json::Object(_)) {
            return Err(locate(
                location,
                StoreError::Canonical {
                    reason: "payload must be a JSON object".to_string(),
                },
                Some(sequence),
            ));
        }
        let previous_hash = match value.get("previousHash") {
            None | Some(Json::Null) => None,
            Some(Json::Str(text)) => {
                validate_hex64(text).map_err(|reason| {
                    locate(location, StoreError::Canonical { reason }, Some(sequence))
                })?;
                Some(text.clone())
            }
            Some(_) => {
                return Err(locate(
                    location,
                    StoreError::Canonical {
                        reason: "previousHash must be a string or null".to_string(),
                    },
                    Some(sequence),
                ));
            }
        };
        if (sequence == 1) != previous_hash.is_none() {
            return Err(locate(
                location,
                StoreError::Canonical {
                    reason: format!(
                        "previousHash must be null exactly when sequence is 1 (sequence \
                         {sequence} has {})",
                        if previous_hash.is_some() {
                            "a previousHash"
                        } else {
                            "null previousHash"
                        }
                    ),
                },
                Some(sequence),
            ));
        }
        let hash = match field("hash")?.as_str() {
            Some(text) => text.to_string(),
            None => {
                return Err(locate(
                    location,
                    StoreError::Canonical {
                        reason: "hash must be a string".to_string(),
                    },
                    Some(sequence),
                ));
            }
        };
        validate_hex64(&hash)
            .map_err(|reason| locate(location, StoreError::Canonical { reason }, Some(sequence)))?;
        Ok(EventEnvelope {
            envelope_version,
            id,
            project_id,
            sequence,
            session_id,
            event_type,
            schema_version,
            timestamp,
            correlation_id,
            causation_id,
            payload,
            previous_hash,
            hash,
        })
    }

    /// The canonical envelope object including `hash`.
    pub fn to_json(&self) -> Json {
        let mut object = self.hash_input();
        if let Json::Object(map) = &mut object {
            map.insert("hash".to_string(), Json::Str(self.hash.clone()));
        }
        object
    }

    /// The canonical envelope object without `hash` (the hash input).
    pub fn hash_input(&self) -> Json {
        Json::object([
            ("envelopeVersion", Json::UInt(self.envelope_version)),
            ("id", Json::Str(self.id.clone())),
            ("projectId", Json::Str(self.project_id.clone())),
            ("sequence", Json::UInt(self.sequence)),
            (
                "sessionId",
                match &self.session_id {
                    Some(id) => Json::Str(id.clone()),
                    None => Json::Null,
                },
            ),
            ("type", Json::Str(self.event_type.clone())),
            ("schemaVersion", Json::UInt(self.schema_version)),
            ("timestamp", Json::Str(self.timestamp.clone())),
            ("correlationId", Json::Str(self.correlation_id.clone())),
            (
                "causationId",
                match &self.causation_id {
                    Some(id) => Json::Str(id.clone()),
                    None => Json::Null,
                },
            ),
            ("payload", self.payload.clone()),
            (
                "previousHash",
                match &self.previous_hash {
                    Some(hash) => Json::Str(hash.clone()),
                    None => Json::Null,
                },
            ),
        ])
    }

    /// Recompute the chain hash: SHA-256 over the canonical serialization of
    /// this envelope minus `hash` (§3).
    pub fn compute_hash(&self) -> String {
        let bytes = self.hash_input().to_canonical_bytes();
        let digest = Sha256::digest(&bytes);
        hex::encode(digest)
    }

    /// The canonical physical line including the trailing `\n`.
    pub fn to_line(&self) -> Vec<u8> {
        let mut line = self.to_json().to_canonical_bytes();
        line.push(b'\n');
        line
    }

    /// Run the payload validator hook, if configured.
    pub fn validate_payload(&self, validator: &Option<PayloadValidator>) -> Result<()> {
        if let Some(validate) = validator {
            validate(&self.event_type, &self.payload).map_err(|reason| StoreError::Append {
                reason: format!(
                    "event \"{}\" (sequence {}) has an invalid payload: {}",
                    self.event_type, self.sequence, reason
                ),
            })?;
        }
        Ok(())
    }

    /// Validate a freshly built envelope the same way a later read would:
    /// full envelope-shape checks plus hash self-consistency. Used before
    /// any bytes are written (§6 step 4).
    pub fn validate(&self) -> Result<()> {
        let location = RecordLocation {
            segment: "(pending append)",
            line: 0,
        };
        let json = self.to_json();
        EventEnvelope::from_json(&json, location)?;
        if self.compute_hash() != self.hash {
            return Err(StoreError::Append {
                reason: format!(
                    "internal error: envelope {} hash does not match its contents",
                    self.id
                ),
            });
        }
        Ok(())
    }
}

/// Where a record came from, for diagnostics.
#[derive(Debug, Clone, Copy)]
pub struct RecordLocation<'a> {
    /// Segment file name.
    pub segment: &'a str,
    /// 1-based line number.
    pub line: u64,
}

/// Convert a canonical/other error into a located [`StoreError::Record`]
/// when it is not already a located variant.
fn locate(location: RecordLocation<'_>, error: StoreError, sequence: Option<u64>) -> StoreError {
    match error {
        StoreError::Canonical { reason } => StoreError::Record {
            segment: location.segment.to_string(),
            line: location.line,
            sequence,
            reason,
        },
        StoreError::UnsupportedStore {
            found,
            supported,
            location: where_,
        } => StoreError::Record {
            segment: location.segment.to_string(),
            line: location.line,
            sequence,
            reason: format!(
                "version {found} (this build understands up to {supported}) at {where_}: the \
                 project was likely written by a newer Relay; upgrade Relay or archive this \
                 project directory"
            ),
        },
        other => other,
    }
}

fn require_u64(value: &Json, location: RecordLocation<'_>, name: &str) -> Result<u64> {
    value.as_u64().ok_or_else(|| {
        locate(
            location,
            StoreError::Canonical {
                reason: format!("envelope field \"{name}\" must be a non-negative integer"),
            },
            None,
        )
    })
}

fn require_prefixed_uuid(value: &Json, location: RecordLocation<'_>, name: &str) -> Result<String> {
    match value.as_str() {
        Some(text) => {
            validate_prefixed_uuid(text, "").map_err(|reason| {
                locate(
                    location,
                    StoreError::Canonical {
                        reason: format!("envelope field \"{name}\": {reason}"),
                    },
                    None,
                )
            })?;
            Ok(text.to_string())
        }
        None => Err(locate(
            location,
            StoreError::Canonical {
                reason: format!("envelope field \"{name}\" must be a string"),
            },
            None,
        )),
    }
}

fn require_exact_prefix(
    value: &Json,
    location: RecordLocation<'_>,
    name: &str,
    prefix: &str,
) -> Result<String> {
    match value.as_str() {
        Some(text) => {
            validate_prefixed_uuid(text, prefix).map_err(|reason| {
                locate(
                    location,
                    StoreError::Canonical {
                        reason: format!("envelope field \"{name}\": {reason}"),
                    },
                    None,
                )
            })?;
            Ok(text.to_string())
        }
        None => Err(locate(
            location,
            StoreError::Canonical {
                reason: format!("envelope field \"{name}\" must be a string"),
            },
            None,
        )),
    }
}

/// Validate `<prefix>_<uuid-v4-lowercase>`; an empty `required_prefix`
/// accepts any of [`ID_PREFIXES`].
pub fn validate_prefixed_uuid(
    text: &str,
    required_prefix: &str,
) -> std::result::Result<(), String> {
    let (prefix, uuid) = text
        .split_once('_')
        .ok_or_else(|| format!("\"{text}\" must look like <prefix>_<uuid-v4-lowercase>"))?;
    let allowed: Vec<&str> = if required_prefix.is_empty() {
        ID_PREFIXES.to_vec()
    } else {
        vec![required_prefix]
    };
    if !allowed.contains(&prefix) {
        return Err(format!(
            "id prefix \"{prefix}\" is not one of {}",
            allowed.join(", ")
        ));
    }
    validate_uuid_v4(uuid)
}

/// Validate a lowercase hyphenated UUID v4.
pub fn validate_uuid_v4(uuid: &str) -> std::result::Result<(), String> {
    let bytes = uuid.as_bytes();
    if bytes.len() != 36
        || bytes[8] != b'-'
        || bytes[13] != b'-'
        || bytes[18] != b'-'
        || bytes[23] != b'-'
    {
        return Err(format!("\"{uuid}\" is not a hyphenated uuid"));
    }
    for (index, &b) in bytes.iter().enumerate() {
        if index == 8 || index == 13 || index == 18 || index == 23 {
            continue;
        }
        if !b.is_ascii_hexdigit() || b.is_ascii_uppercase() {
            return Err(format!("\"{uuid}\" must use lowercase hex digits"));
        }
    }
    let version = bytes
        .get(14)
        .and_then(|b| (*b as char).to_digit(16))
        .unwrap_or(0);
    if version != 4 {
        return Err(format!("\"{uuid}\" is not a version-4 uuid"));
    }
    let variant = bytes
        .get(19)
        .and_then(|b| (*b as char).to_digit(16))
        .unwrap_or(0);
    if !(8..=11).contains(&variant) {
        return Err(format!(
            "\"{uuid}\" does not carry the RFC 4122 variant bits"
        ));
    }
    Ok(())
}

/// Validate a 64-character lowercase hex string.
pub fn validate_hex64(text: &str) -> std::result::Result<(), String> {
    let bytes = text.as_bytes();
    if bytes.len() != 64 {
        return Err(format!(
            "expected 64 hex characters, found {} in \"{text}\"",
            bytes.len()
        ));
    }
    if bytes
        .iter()
        .any(|b| !matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Err("hashes must be lowercase hex".to_string());
    }
    Ok(())
}

/// Length-only comparison that also avoids short-circuiting (hashes are not
/// secrets, but constant behavior keeps timing tidy).
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::iso_from_epoch_millis;

    fn sample_envelope() -> Result<EventEnvelope, Box<dyn std::error::Error>> {
        Ok(EventEnvelope::build(EnvelopeDraft {
            project_id: "prj_00000000-0000-4000-8000-000000000001".to_string(),
            sequence: 1,
            session_id: Some("ses_00000000-0000-4000-8000-00000000000a".to_string()),
            event_type: "project.initialized".to_string(),
            timestamp: "2026-10-13T08:00:00.000Z".to_string(),
            correlation_id: "cor_00000000-0000-4000-8000-000000000002".to_string(),
            causation_id: None,
            payload: Json::object([
                ("goal", Json::string("Ship it")),
                ("workspaceRoot", Json::string("/tmp/w")),
                ("initialAutonomy", Json::string("default")),
            ]),
            previous_hash: None,
            id: "evt_00000000-0000-4000-8000-000000000003".to_string(),
        })?)
    }

    #[test]
    fn builds_and_recomputes_hash() -> Result<(), Box<dyn std::error::Error>> {
        let envelope = sample_envelope()?;
        assert_eq!(envelope.compute_hash(), envelope.hash);
        Ok(())
    }

    #[test]
    fn parses_its_own_line() -> Result<(), Box<dyn std::error::Error>> {
        let envelope = sample_envelope()?;
        let line = String::from_utf8(envelope.to_line())?
            .trim_end()
            .to_string();
        let location = RecordLocation {
            segment: "0000000000000001-open.jsonl",
            line: 1,
        };
        let parsed = EventEnvelope::parse(&line, location)?;
        assert_eq!(parsed, envelope);
        Ok(())
    }

    #[test]
    fn detects_hash_tampering() -> Result<(), Box<dyn std::error::Error>> {
        let mut envelope = sample_envelope()?;
        envelope.payload = Json::object([
            ("goal", Json::string("Changed")),
            ("workspaceRoot", Json::string("/tmp/w")),
            ("initialAutonomy", Json::string("default")),
        ]);
        let line = String::from_utf8(envelope.to_line())?
            .trim_end()
            .to_string();
        let location = RecordLocation {
            segment: "seg.jsonl",
            line: 2,
        };
        let error = EventEnvelope::parse(&line, location)
            .err()
            .ok_or("expected failure")?;
        assert!(error.to_string().contains("broken hash chain"), "{error}");
        assert!(error.to_string().contains("seg.jsonl line 2"), "{error}");
        Ok(())
    }

    #[test]
    fn rejects_unknown_type_and_bad_shapes() -> Result<(), Box<dyn std::error::Error>> {
        let mut envelope = sample_envelope()?;
        envelope.event_type = "made.up.event".to_string();
        envelope.hash = envelope.compute_hash();
        let line = String::from_utf8(envelope.to_line())?
            .trim_end()
            .to_string();
        let location = RecordLocation {
            segment: "seg.jsonl",
            line: 3,
        };
        let error = EventEnvelope::parse(&line, location)
            .err()
            .ok_or("expected failure")?;
        assert!(error.to_string().contains("unknown event type"), "{error}");
        Ok(())
    }

    #[test]
    fn rejects_newer_envelope_version() -> Result<(), Box<dyn std::error::Error>> {
        let mut envelope = sample_envelope()?;
        envelope.envelope_version = 2;
        envelope.hash = envelope.compute_hash();
        let line = String::from_utf8(envelope.to_line())?
            .trim_end()
            .to_string();
        let location = RecordLocation {
            segment: "seg.jsonl",
            line: 1,
        };
        let error = EventEnvelope::parse(&line, location)
            .err()
            .ok_or("expected failure")?;
        assert!(error.to_string().contains("newer Relay"), "{error}");
        Ok(())
    }

    #[test]
    fn rejects_sequence_one_with_previous_hash() -> Result<(), Box<dyn std::error::Error>> {
        let envelope = sample_envelope()?;
        let mut json = envelope.to_json();
        if let Json::Object(map) = &mut json {
            map.insert("previousHash".to_string(), Json::Str("a".repeat(64)));
        }
        let line = json.to_canonical_string();
        let location = RecordLocation {
            segment: "seg.jsonl",
            line: 1,
        };
        let error = EventEnvelope::parse(&line, location)
            .err()
            .ok_or("expected failure")?;
        assert!(
            error.to_string().contains("previousHash must be null"),
            "{error}"
        );
        Ok(())
    }

    #[test]
    fn iso_helpers_agree() {
        let text = iso_from_epoch_millis(0);
        assert_eq!(text, "1970-01-01T00:00:00.000Z");
    }
}
