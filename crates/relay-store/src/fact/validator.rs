//! Domain-supplied acceptance gate for fact records.
//!
//! Storage validates the envelope shape, sequence contiguity, and digests.
//! Which fact **types** and **versions** exist is domain knowledge: the
//! domain crate plugs its catalog in here so unknown types and newer
//! versions fail closed (with upgrade guidance the domain words) instead of
//! being silently accepted. The gate runs on every read and before every
//! append, so a record is never written that a later replay would reject.

use crate::error::Result;
use crate::error::StoreError;
use crate::fact::record::FactInput;
use crate::fact::record::FactRecord;

/// Accepts or rejects a fact record on domain grounds.
pub trait FactValidator: Send + Sync {
    /// Validate a sealed record. `Err` fails closed with an actionable
    /// reason; the storage layer attaches the segment/line location.
    fn validate(&self, record: &FactRecord) -> std::result::Result<(), String>;

    /// Validate a proposed input before it is sealed (same rules, no
    /// sequence or checksum yet). The default implementation seals nothing
    /// and accepts, so implementors override it to reject early.
    fn validate_input(&self, input: &FactInput) -> std::result::Result<(), String> {
        let _ = input;
        Ok(())
    }
}

/// A validator that accepts every well-formed record. Storage alone cannot
/// know the domain fact catalog; production callers must inject their own
/// gate so unknown types fail closed.
#[derive(Debug, Default, Clone, Copy)]
pub struct AcceptAllFacts;

impl FactValidator for AcceptAllFacts {
    fn validate(&self, _record: &FactRecord) -> std::result::Result<(), String> {
        Ok(())
    }
}

/// Accepts only the given `(type, version)` pairs: a small deterministic
/// catalog for tests and for callers that know their schema up front.
#[derive(Debug, Clone)]
pub struct KnownFactVersions {
    entries: Vec<(String, u32)>,
}

impl KnownFactVersions {
    /// Build a catalog from `(type, version)` pairs.
    pub fn new<I: IntoIterator<Item = (String, u32)>>(entries: I) -> KnownFactVersions {
        KnownFactVersions {
            entries: entries.into_iter().collect(),
        }
    }
}

impl FactValidator for KnownFactVersions {
    fn validate(&self, record: &FactRecord) -> std::result::Result<(), String> {
        validate_catalog(&self.entries, &record.fact_type, record.fact_version)
    }

    fn validate_input(&self, input: &FactInput) -> std::result::Result<(), String> {
        validate_catalog(&self.entries, &input.fact_type, input.fact_version)
    }
}

fn validate_catalog(
    entries: &[(String, u32)],
    fact_type: &str,
    fact_version: u32,
) -> std::result::Result<(), String> {
    for (known_type, known_version) in entries {
        if known_type == fact_type {
            if known_version == &fact_version {
                return Ok(());
            }
            return Err(format!(
                "fact type \"{fact_type}\" has version {fact_version}, but this build \
                 understands version {known_version}. Upgrade Relay — or open the project \
                 with the newer install — before using this store again."
            ));
        }
    }
    let catalog = entries
        .iter()
        .map(|(known, _)| known.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    Err(format!(
        "unknown fact type \"{fact_type}\" (known types: {catalog}). The fact was written by \
         a newer Relay or by a workflow this build does not carry; upgrade Relay before \
         using this store again."
    ))
}

/// Adapt a domain rejection into a located store error.
pub(crate) fn located_rejection(
    segment: &str,
    line: u64,
    sequence: Option<u64>,
    reason: String,
) -> StoreError {
    StoreError::FactRecord {
        segment: segment.to_string(),
        line,
        sequence,
        reason,
    }
}

/// Run the gate over a sealed record for the append path.
pub(crate) fn check_append(validator: &dyn FactValidator, record: &FactRecord) -> Result<()> {
    validator
        .validate(record)
        .map_err(|reason| StoreError::Append { reason })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canonical::Json;

    fn input(kind: &str, version: u32) -> FactInput {
        FactInput::new(kind, version, Json::Object(Default::default()))
    }

    #[test]
    fn known_catalog_accepts_and_fails_closed() {
        let validator = KnownFactVersions::new([("goal.declared".to_string(), 1)]);
        assert!(validator.validate_input(&input("goal.declared", 1)).is_ok());
        let newer = validator
            .validate_input(&input("goal.declared", 2))
            .err()
            .unwrap_or_default();
        assert!(newer.contains("version 2"), "{newer}");
        assert!(newer.contains("Upgrade Relay"), "{newer}");
        let unknown = validator
            .validate_input(&input("goal.renamed", 1))
            .err()
            .unwrap_or_default();
        assert!(unknown.contains("unknown fact type"), "{unknown}");
    }

    #[test]
    fn accept_all_never_rejects() {
        let validator = AcceptAllFacts;
        assert!(
            validator
                .validate_input(&input("anything.at.all", 9))
                .is_ok()
        );
    }
}
