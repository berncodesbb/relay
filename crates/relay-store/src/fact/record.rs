//! The fact record: the durable unit of the fact format
//! (`docs/plans/fact-format-proposal.md`).
//!
//! A fact record is deliberately opaque to storage: the envelope carries a
//! sequence number, a fact type (an open string the domain owns), a per-type
//! fact version, a JSON payload, and the envelope timestamp of when the fact
//! was durably recorded. Storage validates the envelope shape, the sequence
//! contiguity, and the integrity digest(s); the domain supplies the
//! type/version acceptance gate through [`crate::fact::FactValidator`].
//!
//! There is no per-record envelope id and no container version embedded in
//! each line: the sequence number is the record identity within one stream,
//! and the manifest's `factStoreVersion` is the single format version for
//! both the container and the record line.

use sha2::Digest;
use sha2::Sha256;

use crate::canonical::Json;
use crate::clock::parse_iso_to_epoch_millis;
use crate::error::Result;
use crate::error::StoreError;
use crate::fact::canonical;

/// Integrity level of a fact stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Integrity {
    /// Per-record SHA-256 checksums only. The default: an edit to one record
    /// is detected by that record's own checksum.
    #[default]
    Checksums,
    /// Per-record checksums **plus** a per-stream chain: each record's
    /// `previous` field carries the checksum of the record at
    /// `sequence - 1`. Opt-in; detects reordering and truncation that
    /// preserves every surviving record's own checksum.
    Chained,
}

impl Integrity {
    /// The manifest token for this level.
    pub fn as_str(self) -> &'static str {
        match self {
            Integrity::Checksums => "checksums",
            Integrity::Chained => "chained",
        }
    }

    /// Parse the manifest token.
    pub fn parse(text: &str) -> Option<Integrity> {
        match text {
            "checksums" => Some(Integrity::Checksums),
            "chained" => Some(Integrity::Chained),
            _ => None,
        }
    }
}

/// A proposed fact. Storage assigns the sequence, the envelope timestamp
/// (when not supplied), the chain link, and the checksum.
#[derive(Debug, Clone)]
pub struct FactInput {
    /// Fact type (domain-owned open string, e.g. `goal.declared`).
    pub fact_type: String,
    /// Per-type fact version; the domain decides which versions it accepts.
    pub fact_version: u32,
    /// Fact payload.
    pub payload: Json,
    /// Explicit envelope timestamp; defaults to the injected clock.
    pub recorded_at: Option<String>,
}

impl FactInput {
    /// A minimal input: type + version + payload.
    pub fn new(fact_type: &str, fact_version: u32, payload: Json) -> FactInput {
        FactInput {
            fact_type: fact_type.to_string(),
            fact_version,
            payload,
            recorded_at: None,
        }
    }
}

/// A sealed fact record: everything that lands on one physical JSONL line.
#[derive(Debug, Clone, PartialEq)]
pub struct FactRecord {
    /// 1-based contiguous position in the stream.
    pub sequence: u64,
    /// Fact type (domain-owned open string).
    pub fact_type: String,
    /// Per-type fact version.
    pub fact_version: u32,
    /// Fact payload.
    pub payload: Json,
    /// RFC 3339 UTC, millisecond precision, `Z` suffix — when the fact was
    /// durably recorded. All domain semantics of *when something happened*
    /// belong in the payload, where the domain owns the schema.
    pub recorded_at: String,
    /// Checksum of the previous record; `Some` exactly when the stream is
    /// chained and `sequence > 1`.
    pub previous: Option<String>,
    /// Lowercase hex SHA-256 over the canonical serialization of this
    /// envelope minus the `checksum` field itself.
    pub checksum: String,
}

impl FactRecord {
    /// Seal an input into a fully checksummed record at `sequence`,
    /// inheriting `previous` from the tail checksum when the stream is
    /// chained. The domain validation gate runs before anything is written.
    pub fn seal(
        input: &FactInput,
        sequence: u64,
        recorded_at: &str,
        previous: Option<String>,
    ) -> Result<FactRecord> {
        if sequence == 0 {
            return Err(StoreError::Append {
                reason: "fact sequence numbers are 1-based".to_string(),
            });
        }
        if input.fact_version == 0 {
            return Err(StoreError::Append {
                reason: format!(
                    "fact type \"{}\" must carry a positive factVersion",
                    input.fact_type
                ),
            });
        }
        validate_recorded_at(recorded_at)?;
        let previous = match previous {
            Some(checksum) => {
                if sequence == 1 {
                    return Err(StoreError::Append {
                        reason: "the first fact of a stream cannot chain onto a previous \
                                 checksum"
                            .to_string(),
                    });
                }
                Some(checksum)
            }
            None => None,
        };
        let record = FactRecord {
            sequence,
            fact_type: input.fact_type.clone(),
            fact_version: input.fact_version,
            payload: input.payload.clone(),
            recorded_at: recorded_at.to_string(),
            previous,
            checksum: String::new(),
        };
        let checksum = record.compute_checksum()?;
        Ok(FactRecord { checksum, ..record })
    }

    /// The canonical envelope object without `checksum` (the checksum input).
    fn hash_input(&self) -> Json {
        Json::object([
            ("factVersion", Json::UInt(u64::from(self.fact_version))),
            ("payload", self.payload.clone()),
            (
                "previous",
                match &self.previous {
                    Some(checksum) => Json::Str(checksum.clone()),
                    None => Json::Null,
                },
            ),
            ("recordedAt", Json::Str(self.recorded_at.clone())),
            ("sequence", Json::UInt(self.sequence)),
            ("type", Json::Str(self.fact_type.clone())),
        ])
    }

    /// The canonical envelope object including `checksum`.
    pub fn to_json(&self) -> Json {
        let mut object = self.hash_input();
        if let Json::Object(map) = &mut object {
            map.insert("checksum".to_string(), Json::Str(self.checksum.clone()));
        }
        object
    }

    /// Recompute the record checksum: SHA-256 over the canonical
    /// serialization of the envelope minus `checksum`.
    pub fn compute_checksum(&self) -> Result<String> {
        let bytes = canonical::serialize_bytes(&self.hash_input())?;
        Ok(hex::encode(Sha256::digest(bytes)))
    }

    /// The canonical physical line including the trailing `\n`.
    pub fn to_line(&self) -> Result<Vec<u8>> {
        let mut line = canonical::serialize_bytes(&self.to_json())?;
        line.push(b'\n');
        Ok(line)
    }

    /// Parse one physical line (without the trailing `\n`) into a fully
    /// validated record. Readers never checksum raw input bytes: the line is
    /// parsed, re-serialized canonically, and only that form is digested.
    /// Unknown fields are accepted (additive evolution) but dropped.
    pub fn parse(line: &str, segment: &str, line_number: u64) -> Result<FactRecord> {
        let located = |sequence: Option<u64>, reason: String| StoreError::FactRecord {
            segment: segment.to_string(),
            line: line_number,
            sequence,
            reason,
        };
        let value = canonical::parse(line)
            .map_err(|error| located(None, format!("line is not valid JSON: {error}")))?;
        if !matches!(value.get("payload"), Some(Json::Object(_))) {
            return Err(located(
                value.get("sequence").and_then(Json::as_u64),
                "field \"payload\" must be a JSON object".to_string(),
            ));
        }
        let sequence = value
            .get("sequence")
            .and_then(Json::as_u64)
            .ok_or_else(|| {
                located(
                    None,
                    "field \"sequence\" must be a non-negative integer".to_string(),
                )
            })?;
        if sequence == 0 {
            return Err(located(
                Some(sequence),
                "field \"sequence\" must be 1-based".to_string(),
            ));
        }
        let fact_type = value
            .get("type")
            .and_then(Json::as_str)
            .filter(|text| !text.is_empty())
            .ok_or_else(|| {
                located(
                    Some(sequence),
                    "field \"type\" must be a non-empty string".to_string(),
                )
            })?
            .to_string();
        let fact_version = value
            .get("factVersion")
            .and_then(Json::as_u64)
            .and_then(|version| u32::try_from(version).ok())
            .filter(|version| *version > 0)
            .ok_or_else(|| {
                located(
                    Some(sequence),
                    "field \"factVersion\" must be a positive integer".to_string(),
                )
            })?;
        let recorded_at = value
            .get("recordedAt")
            .and_then(Json::as_str)
            .ok_or_else(|| {
                located(
                    Some(sequence),
                    "field \"recordedAt\" must be a string".to_string(),
                )
            })?
            .to_string();
        validate_recorded_at(&recorded_at)
            .map_err(|error| located(Some(sequence), error.to_string()))?;
        let previous = match value.get("previous") {
            Some(Json::Null) | None => None,
            Some(Json::Str(checksum)) => {
                validate_hex64(checksum).map_err(|reason| {
                    located(Some(sequence), format!("field \"previous\": {reason}"))
                })?;
                Some(checksum.clone())
            }
            Some(_) => {
                return Err(located(
                    Some(sequence),
                    "field \"previous\" must be a checksum string or null".to_string(),
                ));
            }
        };
        let checksum = value
            .get("checksum")
            .and_then(Json::as_str)
            .ok_or_else(|| located(Some(sequence), "field \"checksum\" is missing".to_string()))?
            .to_string();
        validate_hex64(&checksum)
            .map_err(|reason| located(Some(sequence), format!("field \"checksum\": {reason}")))?;
        let record = FactRecord {
            sequence,
            fact_type,
            fact_version,
            payload: value
                .get("payload")
                .cloned()
                .unwrap_or(Json::Object(Default::default())),
            recorded_at,
            previous,
            checksum,
        };
        let expected = record.compute_checksum()?;
        if expected != record.checksum {
            return Err(located(
                Some(sequence),
                format!(
                    "checksum mismatch: the record contents hash to {expected}, but the \
                     stored checksum is {}",
                    record.checksum
                ),
            ));
        }
        Ok(record)
    }

    /// Validate a sealed record exactly the way a later read would (used
    /// before an append is written).
    pub fn validate(&self) -> Result<()> {
        if self.compute_checksum()? != self.checksum {
            return Err(StoreError::Append {
                reason: format!(
                    "internal error: fact {} at sequence {} has a checksum that does not \
                     match its contents",
                    self.fact_type, self.sequence
                ),
            });
        }
        Ok(())
    }
}

/// Where a fact record came from, for diagnostics.
#[derive(Debug, Clone)]
pub struct FactEntry {
    /// The validated record.
    pub record: FactRecord,
    /// Segment file name.
    pub segment: String,
    /// 1-based line number within the segment.
    pub line: u64,
}

/// Validate an envelope timestamp (`YYYY-MM-DDTHH:MM:SS.sssZ`).
fn validate_recorded_at(text: &str) -> Result<()> {
    if parse_iso_to_epoch_millis(text).is_none() {
        return Err(StoreError::Canonical {
            reason: format!(
                "recordedAt \"{text}\" must be YYYY-MM-DDTHH:MM:SS.sssZ (RFC 3339 UTC, \
                 millisecond precision)"
            ),
        });
    }
    Ok(())
}

/// Validate a 64-character lowercase hex string.
fn validate_hex64(text: &str) -> std::result::Result<(), String> {
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
        return Err("checksums must be lowercase hex".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const RECORDED_AT: &str = "2026-10-13T08:00:00.000Z";

    fn sample_input() -> FactInput {
        FactInput::new(
            "goal.declared",
            1,
            Json::object([
                ("goal", Json::string("Ship the gate")),
                ("scope", Json::Array(vec![Json::string("src/cli")])),
            ]),
        )
    }

    #[test]
    fn seals_parses_and_round_trips() -> Result<(), Box<dyn std::error::Error>> {
        let record = FactRecord::seal(&sample_input(), 7, RECORDED_AT, None)?;
        assert_eq!(record.sequence, 7);
        assert_eq!(record.previous, None);
        let line = String::from_utf8(record.to_line()?)
            .map_err(|e| e.to_string())?
            .trim_end()
            .to_string();
        let parsed = FactRecord::parse(&line, "seg.jsonl", 3)?;
        assert_eq!(parsed, record);
        Ok(())
    }

    #[test]
    fn checksum_covers_every_envelope_field() -> Result<(), Box<dyn std::error::Error>> {
        let record = FactRecord::seal(&sample_input(), 1, RECORDED_AT, None)?;
        let mut tampered = record.clone();
        tampered.payload =
            Json::object([("goal", Json::string("Changed without fixing the checksum"))]);
        let line = String::from_utf8(tampered.to_line()?)
            .map_err(|e| e.to_string())?
            .trim_end()
            .to_string();
        let error = FactRecord::parse(&line, "seg.jsonl", 1)
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(error.contains("checksum mismatch"), "{error}");
        assert!(error.contains("seg.jsonl line 1"), "{error}");

        // The timestamp and type are covered too.
        for tamper in [
            FactRecord {
                recorded_at: "2026-10-13T09:00:00.000Z".to_string(),
                ..record.clone()
            },
            FactRecord {
                fact_type: "goal.renamed".to_string(),
                ..record.clone()
            },
        ] {
            let line = String::from_utf8(tamper.to_line()?)
                .map_err(|e| e.to_string())?
                .trim_end()
                .to_string();
            assert!(
                FactRecord::parse(&line, "seg.jsonl", 1).is_err(),
                "tampered record must not verify"
            );
        }
        Ok(())
    }

    #[test]
    fn chain_link_participates_in_the_checksum() -> Result<(), Box<dyn std::error::Error>> {
        let plain = FactRecord::seal(&sample_input(), 2, RECORDED_AT, None)?;
        let chained = FactRecord::seal(&sample_input(), 2, RECORDED_AT, Some("a".repeat(64)))?;
        assert_ne!(plain.checksum, chained.checksum);
        assert_eq!(chained.previous, Some("a".repeat(64)));
        Ok(())
    }

    #[test]
    fn rejects_malformed_envelopes() -> Result<(), Box<dyn std::error::Error>> {
        let record = FactRecord::seal(&sample_input(), 1, RECORDED_AT, None)?;
        let base = String::from_utf8(record.to_line()?)
            .map_err(|e| e.to_string())?
            .trim_end()
            .to_string();

        // Not JSON.
        assert!(FactRecord::parse("{oops", "s.jsonl", 1).is_err());
        // Missing / wrong-typed fields.
        for mutate in [
            |v: &mut Json| {
                if let Json::Object(map) = v {
                    map.remove("type");
                }
            },
            |v: &mut Json| {
                if let Json::Object(map) = v {
                    map.insert("sequence".into(), Json::Int(-3));
                }
            },
            |v: &mut Json| {
                if let Json::Object(map) = v {
                    map.insert("factVersion".into(), Json::UInt(0));
                }
            },
            |v: &mut Json| {
                if let Json::Object(map) = v {
                    map.insert("recordedAt".into(), Json::string("yesterday"));
                }
            },
            |v: &mut Json| {
                if let Json::Object(map) = v {
                    map.insert("checksum".into(), Json::string("nothex"));
                }
            },
            |v: &mut Json| {
                if let Json::Object(map) = v {
                    map.insert("payload".into(), Json::Array(vec![]));
                }
            },
        ] {
            let mut value = canonical::parse(&base)?;
            mutate(&mut value);
            let text = canonical::serialize(&value)?;
            assert!(
                FactRecord::parse(&text, "s.jsonl", 1).is_err(),
                "mutated record must be rejected: {text}"
            );
        }

        // Additive unknown fields are accepted (forward compatibility).
        let mut value = canonical::parse(&base)?;
        if let Json::Object(map) = &mut value {
            map.insert("futureField".to_string(), Json::string("ignored on decode"));
        }
        // The checksum still verifies because hashing uses the known fields.
        let text = canonical::serialize(&value)?;
        assert!(FactRecord::parse(&text, "s.jsonl", 1).is_ok());
        Ok(())
    }

    #[test]
    fn seal_rejects_bad_inputs() -> Result<()> {
        assert!(FactRecord::seal(&sample_input(), 0, RECORDED_AT, None).is_err());
        let zero_version = FactInput::new("goal.declared", 0, Json::Object(Default::default()));
        assert!(FactRecord::seal(&zero_version, 1, RECORDED_AT, None).is_err());
        assert!(FactRecord::seal(&sample_input(), 1, "not-a-time", None).is_err());
        assert!(FactRecord::seal(&sample_input(), 1, RECORDED_AT, Some("x".repeat(64))).is_err());
        Ok(())
    }

    #[test]
    fn integrity_tokens_round_trip() {
        assert_eq!(Integrity::Checksums.as_str(), "checksums");
        assert_eq!(Integrity::Chained.as_str(), "chained");
        assert_eq!(Integrity::parse("checksums"), Some(Integrity::Checksums));
        assert_eq!(Integrity::parse("chained"), Some(Integrity::Chained));
        assert_eq!(Integrity::parse("signed"), None);
    }
}
