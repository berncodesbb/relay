//! Versioned artifact content and exact version references.

use crate::ids::{ArtifactId, ArtifactVersion};

/// A reference to one exact published artifact version.
///
/// This is the currency of dependency precision: work items pin inputs as
/// exact version references, artifact versions record the references they
/// were derived from, and invalidation follows those pins instead of coarse
/// phase order.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct ArtifactVersionRef {
    /// The artifact identity.
    pub artifact: ArtifactId,
    /// The exact version.
    pub version: ArtifactVersion,
}

impl ArtifactVersionRef {
    /// Construct a reference from an artifact id and version.
    pub fn new(artifact: ArtifactId, version: ArtifactVersion) -> Self {
        ArtifactVersionRef { artifact, version }
    }
}

impl std::fmt::Display for ArtifactVersionRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}@{}", self.artifact, self.version)
    }
}

/// Structured artifact content. The authoritative meaning of an artifact
/// version lives here; deterministic Markdown views are derived from it by
/// the artifact/rendering layer, never the reverse.
///
/// Phase 1 keeps the payload as validated structured JSON; the built-in
/// workflow (Phase 2, `relay-workflows`) layers typed schemas for
/// requirements, criteria, decisions, and plans on top of this carrier.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct ArtifactContent(pub serde_json::Value);

impl ArtifactContent {
    /// Wrap structured JSON as artifact content.
    pub fn new(value: serde_json::Value) -> Self {
        ArtifactContent(value)
    }

    /// Content with no structured payload. Callers that cannot construct
    /// JSON values directly (and tests) use this; the digest remains the
    /// content commitment.
    pub fn empty() -> Self {
        ArtifactContent(serde_json::Value::Null)
    }
}

/// A caller-computed content digest commitment recorded with each published
/// artifact version. The kernel treats it as opaque data: hashing is a
/// storage concern, verification of digests against content happens at the
/// storage boundary.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ArtifactDigest(String);

/// Error raised for digests that are not lowercase hex of a sane length.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid artifact digest \"{0}\": expected 16-128 lowercase hex characters")]
pub struct DigestError(String);

impl ArtifactDigest {
    /// Validate a lowercase-hex digest string.
    pub fn parse(value: &str) -> Result<Self, DigestError> {
        let reject = || DigestError(value.to_string());
        if value.len() < 16 || value.len() > 128 {
            return Err(reject());
        }
        if !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(reject());
        }
        Ok(ArtifactDigest(value.to_string()))
    }

    /// The digest string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ArtifactDigest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::str::FromStr for ArtifactDigest {
    type Err = DigestError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        ArtifactDigest::parse(value)
    }
}

impl<'de> serde::Deserialize<'de> for ArtifactDigest {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        ArtifactDigest::parse(&value).map_err(serde::de::Error::custom)
    }
}

impl serde::Serialize for ArtifactDigest {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
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

    #[test]
    fn digests_validate_hex_shape() {
        assert!(ArtifactDigest::parse(&"a".repeat(16)).is_ok());
        assert!(ArtifactDigest::parse(&"a".repeat(64)).is_ok());
        assert!(ArtifactDigest::parse(&"a".repeat(15)).is_err());
        assert!(ArtifactDigest::parse(&"a".repeat(129)).is_err());
        assert!(ArtifactDigest::parse(&"A".repeat(64)).is_err());
        assert!(ArtifactDigest::parse(&"z".repeat(64)).is_err());
    }
}
