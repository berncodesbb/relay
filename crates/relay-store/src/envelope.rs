//! The event envelope: parsing, validation, and hash chaining
//! (`docs/event-format.md` §1 and §3).
//!
//! INTEGRATED with `relay-core`: envelope shape rules, the 24 event types,
//! per-type payload schema versions, typed payload validation, and hash
//! computation are owned by [`relay_core::events`] (see
//! [`relay_core::events::validate_stored_event`] and
//! [`relay_core::events::compute_envelope_hash`]). This module keeps the
//! store's [`EventEnvelope`] wrapper — with the payload as the store's
//! [`Json`] value type and a non-optional `hash` — plus the two chain rules
//! core leaves to the log layer: `previousHash` is `null` exactly at
//! sequence 1, and the stored `hash` must equal the recomputed digest.

pub use relay_core::events::EVENT_ENVELOPE_VERSION;
pub use relay_core::events::EVENT_PAYLOAD_VERSIONS;
pub use relay_core::events::RELAY_EVENT_TYPES;
pub use relay_core::events::RelayEvent;
pub use relay_core::events::RelayEventType;

use relay_core::events::CoreError;

use crate::canonical::Json;
use crate::error::Result;
use crate::error::StoreError;

/// The store's validated event envelope: everything relay-core validated,
/// with the payload kept as the store's [`Json`] value and the chain hash
/// required (empty only between construction and hashing).
#[derive(Debug, Clone, PartialEq)]
pub struct EventEnvelope {
    /// Envelope container version; always `1` in this format.
    pub envelope_version: u32,
    /// `<prefix>_<uuid-v4-lowercase>`; unique within the log.
    pub id: String,
    /// `prj_<uuid>`; identical for every event in one log.
    pub project_id: String,
    /// 1-based contiguous position in the project log.
    pub sequence: u64,
    /// Producing session, or `None`.
    pub session_id: Option<String>,
    /// One of the 24 Relay event types.
    pub event_type: String,
    /// Per-type payload schema version.
    pub schema_version: u32,
    /// RFC 3339 UTC, millisecond precision, `Z` suffix.
    pub timestamp: String,
    /// Operation correlation id (`cor_<uuid>`).
    pub correlation_id: String,
    /// Directly causing event id, or `None`.
    pub causation_id: Option<String>,
    /// Type-specific payload (typed validation happens in relay-core).
    pub payload: Json,
    /// Hash of the previous event; `None` exactly at sequence 1.
    pub previous_hash: Option<String>,
    /// Hex SHA-256 over the canonical envelope minus this field.
    pub hash: String,
}

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
    /// Event type (one of the 24 Relay types).
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
    /// Build a fully hashed envelope from `draft`. Relay-core validates the
    /// envelope shape and the typed payload (so an unknown type or an
    /// invalid payload aborts before anything is written) and computes the
    /// chain hash.
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
        let known_type =
            RelayEventType::parse(&event_type).map_or(1u32, |known| known.payload_version());
        let core = relay_core::events::EventEnvelope {
            envelope_version: EVENT_ENVELOPE_VERSION,
            id,
            project_id,
            sequence,
            session_id,
            event_type,
            schema_version: known_type,
            timestamp,
            correlation_id,
            causation_id,
            payload: payload.to_serde(),
            previous_hash,
            hash: None,
        };
        relay_core::events::validate_stored_event(&core)
            .map_err(|error| core_error_to_store(&error, None))?;
        let hash = core.compute_hash();
        let envelope = EventEnvelope {
            envelope_version: core.envelope_version,
            id: core.id,
            project_id: core.project_id,
            sequence: core.sequence,
            session_id: core.session_id,
            event_type: core.event_type,
            schema_version: core.schema_version,
            timestamp: core.timestamp,
            correlation_id: core.correlation_id,
            causation_id: core.causation_id,
            payload: Json::from_serde(&core.payload)?,
            previous_hash: core.previous_hash,
            hash,
        };
        envelope.check_chain_metadata()?;
        Ok(envelope)
    }

    /// Parse one physical line (without the trailing `\n`) into a fully
    /// validated envelope. Validation and hashing are delegated to
    /// relay-core; readers never hash raw input bytes.
    pub fn parse(line: &str, location: RecordLocation<'_>) -> Result<EventEnvelope> {
        let value: serde_json::Value = serde_json::from_str(line).map_err(|error| {
            locate(
                location,
                StoreError::Canonical {
                    reason: format!("line is not valid JSON: {error}"),
                },
                None,
            )
        })?;
        let core: relay_core::events::EventEnvelope = serde_json::from_value(value.clone())
            .map_err(|error| {
                locate(
                    location,
                    StoreError::Canonical {
                        reason: format!("envelope fields are invalid: {error}"),
                    },
                    None,
                )
            })?;
        relay_core::events::validate_stored_event(&core)
            .map_err(|error| core_error_to_store(&error, Some(location)))?;
        let stored_hash = core
            .hash
            .clone()
            .filter(|hash| !hash.is_empty())
            .ok_or_else(|| {
                locate(
                    location,
                    StoreError::Canonical {
                        reason: "envelope field \"hash\" is missing".to_string(),
                    },
                    Some(core.sequence),
                )
            })?;
        validate_hex64(&stored_hash).map_err(|reason| {
            locate(
                location,
                StoreError::Canonical { reason },
                Some(core.sequence),
            )
        })?;
        let envelope = EventEnvelope {
            envelope_version: core.envelope_version,
            id: core.id,
            project_id: core.project_id,
            sequence: core.sequence,
            session_id: core.session_id,
            event_type: core.event_type,
            schema_version: core.schema_version,
            timestamp: core.timestamp,
            correlation_id: core.correlation_id,
            causation_id: core.causation_id,
            payload: Json::from_serde(&core.payload)?,
            previous_hash: core.previous_hash,
            hash: stored_hash,
        };
        envelope.check_chain_metadata()?;
        let expected = envelope.compute_hash();
        if expected != envelope.hash {
            return Err(StoreError::ChainBreak {
                segment: location.segment.to_string(),
                line: location.line,
                sequence: envelope.sequence,
                expected,
                actual: envelope.hash.clone(),
                hash_field: true,
            });
        }
        Ok(envelope)
    }

    /// The chain rules the log layer owns: `previousHash` is a lowercase
    /// hex digest, and is `null` exactly at sequence 1 (§1, §3).
    fn check_chain_metadata(&self) -> Result<()> {
        if let Some(previous) = &self.previous_hash {
            validate_hex64(previous).map_err(|reason| StoreError::Canonical { reason })?;
        }
        if (self.sequence == 1) != self.previous_hash.is_none() {
            return Err(StoreError::Canonical {
                reason: format!(
                    "previousHash must be null exactly when sequence is 1 (sequence {} has {})",
                    self.sequence,
                    if self.previous_hash.is_some() {
                        "a previousHash"
                    } else {
                        "null previousHash"
                    }
                ),
            });
        }
        Ok(())
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
            (
                "envelopeVersion",
                Json::UInt(u64::from(self.envelope_version)),
            ),
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
            ("schemaVersion", Json::UInt(u64::from(self.schema_version))),
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

    /// Recompute the chain hash through relay-core: SHA-256 over the
    /// canonical serialization of this envelope minus `hash` (§3).
    pub fn compute_hash(&self) -> String {
        let mut without_hash = self.to_json().to_serde();
        if let serde_json::Value::Object(map) = &mut without_hash {
            map.remove("hash");
        }
        relay_core::events::compute_envelope_hash(&without_hash)
    }

    /// The canonical physical line including the trailing `\n`.
    pub fn to_line(&self) -> Vec<u8> {
        let mut line = self.to_json().to_canonical_bytes();
        line.push(b'\n');
        line
    }

    /// Validate a built envelope exactly the way a later read would.
    pub fn validate(&self) -> Result<()> {
        let location = RecordLocation {
            segment: "(pending append)",
            line: 0,
        };
        let core = self.core_view();
        relay_core::events::validate_stored_event(&core)
            .map_err(|error| core_error_to_store(&error, Some(location)))?;
        self.check_chain_metadata()
            .map_err(|error| locate(location, error, Some(self.sequence)))?;
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

    /// The relay-core view of this envelope (payload as `serde_json`).
    fn core_view(&self) -> relay_core::events::EventEnvelope {
        relay_core::events::EventEnvelope {
            envelope_version: self.envelope_version,
            id: self.id.clone(),
            project_id: self.project_id.clone(),
            sequence: self.sequence,
            session_id: self.session_id.clone(),
            event_type: self.event_type.clone(),
            schema_version: self.schema_version,
            timestamp: self.timestamp.clone(),
            correlation_id: self.correlation_id.clone(),
            causation_id: self.causation_id.clone(),
            payload: self.payload.to_serde(),
            previous_hash: self.previous_hash.clone(),
            hash: Some(self.hash.clone()),
        }
    }
}

/// Map a relay-core validation error into a located store error, keeping
/// core's message text verbatim (the compatibility contract).
fn core_error_to_store(error: &CoreError, location: Option<RecordLocation<'_>>) -> StoreError {
    let reason = error.to_string();
    match location {
        Some(location) => StoreError::Record {
            segment: location.segment.to_string(),
            line: location.line,
            sequence: None,
            reason,
        },
        None => StoreError::Append { reason },
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

/// Convert a canonical/other error into a located [`StoreError::Record`].
fn locate(location: RecordLocation<'_>, error: StoreError, sequence: Option<u64>) -> StoreError {
    match error {
        StoreError::Canonical { reason } => StoreError::Record {
            segment: location.segment.to_string(),
            line: location.line,
            sequence,
            reason,
        },
        other => other,
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_draft() -> EnvelopeDraft {
        EnvelopeDraft {
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
        }
    }

    #[test]
    fn builds_and_recomputes_hash() -> Result<(), Box<dyn std::error::Error>> {
        let envelope = EventEnvelope::build(sample_draft())?;
        assert_eq!(envelope.compute_hash(), envelope.hash);
        // relay-core's direct hash computation agrees.
        let mut without_hash = envelope.to_json().to_serde();
        if let serde_json::Value::Object(map) = &mut without_hash {
            map.remove("hash");
        }
        assert_eq!(
            relay_core::events::compute_envelope_hash(&without_hash),
            envelope.hash
        );
        Ok(())
    }

    #[test]
    fn parses_its_own_line() -> Result<(), Box<dyn std::error::Error>> {
        let envelope = EventEnvelope::build(sample_draft())?;
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
        let mut envelope = EventEnvelope::build(sample_draft())?;
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
    fn rejects_unknown_type_and_bad_payload() -> Result<(), Box<dyn std::error::Error>> {
        // Unknown type: relay-core fails closed.
        let mut draft = sample_draft();
        draft.event_type = "made.up.event".to_string();
        let error = EventEnvelope::build(draft)
            .err()
            .ok_or("expected failure")?;
        assert!(error.to_string().contains("Unknown event type"), "{error}");
        // Invalid payload (empty goal): typed schema validation rejects it.
        let mut draft = sample_draft();
        draft.payload = Json::object([
            ("goal", Json::string("")),
            ("workspaceRoot", Json::string("/tmp/w")),
            ("initialAutonomy", Json::string("default")),
        ]);
        let error = EventEnvelope::build(draft)
            .err()
            .ok_or("expected failure")?;
        assert!(error.to_string().contains("invalid payload"), "{error}");
        Ok(())
    }

    #[test]
    fn rejects_newer_envelope_version() -> Result<(), Box<dyn std::error::Error>> {
        let mut envelope = EventEnvelope::build(sample_draft())?;
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
        assert!(error.to_string().contains("envelopeVersion"), "{error}");
        Ok(())
    }

    #[test]
    fn rejects_sequence_one_with_previous_hash() -> Result<(), Box<dyn std::error::Error>> {
        let mut envelope = EventEnvelope::build(sample_draft())?;
        envelope.previous_hash = Some("a".repeat(64));
        envelope.hash = envelope.compute_hash();
        let line = envelope.to_json().to_canonical_string();
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
    fn unknown_envelope_fields_are_accepted() -> Result<(), Box<dyn std::error::Error>> {
        // §1: additive envelope fields are permitted for forward
        // compatibility when the version is understood.
        let envelope = EventEnvelope::build(sample_draft())?;
        let mut value = envelope.to_json().to_serde();
        if let serde_json::Value::Object(map) = &mut value {
            map.insert(
                "futureField".to_string(),
                serde_json::Value::String("ignored".to_string()),
            );
        }
        let line = relay_core::canonical::canonical_json(&value);
        let location = RecordLocation {
            segment: "seg.jsonl",
            line: 1,
        };
        let parsed = EventEnvelope::parse(&line, location)?;
        // The stored hash still verifies: hashing ignores unknown fields.
        assert_eq!(parsed.id, envelope.id);
        Ok(())
    }
}
