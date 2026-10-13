//! Strongly typed identifiers, versions, and revisions.
//!
//! Every addressable node in the work graph carries a typed id of the form
//! `<prefix>_<slug>`, where the slug is 1–64 characters of lowercase ASCII
//! letters, digits, or `-` and does not start with `-`. Identifiers are
//! minted by callers (the kernel has no randomness) and validated by
//! `parse`/`FromStr`, so an id newtype is never constructible
//! in an invalid state.
//!
//! Prefixes: [`ProjectId`] `prj_`, [`GoalId`] `goal_`, [`WorkItemId`] `work_`,
//! [`ArtifactId`] `art_`, [`DecisionId`] `dec_`, [`EvidenceId`] `ev_`,
//! [`AttemptId`] `att_`, [`GrantId`] `grant_`.

use std::fmt;

/// Error raised when a string is not a valid prefixed domain identifier.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "invalid identifier \"{value}\": expected \"{prefix}<slug>\" where <slug> is 1-64 chars of [a-z0-9-] not starting with '-'"
)]
pub struct IdParseError {
    /// The expected prefix, including its trailing underscore.
    pub prefix: &'static str,
    /// The offending string.
    pub value: String,
}

/// Validate the `<prefix>_<slug>` shape and return the slug.
fn split_prefixed<'a>(prefix: &'static str, value: &'a str) -> Result<&'a str, IdParseError> {
    let reject = |value: &str| IdParseError {
        prefix: prefix.strip_suffix('_').unwrap_or(prefix),
        value: value.to_string(),
    };
    let Some(tail) = value.strip_prefix(prefix) else {
        return Err(reject(value));
    };
    let bytes = tail.as_bytes();
    if bytes.is_empty() || bytes.len() > 64 || bytes[0] == b'-' {
        return Err(reject(value));
    }
    if !bytes
        .iter()
        .all(|&b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err(reject(value));
    }
    Ok(tail)
}

macro_rules! domain_id {
    ($name:ident, $prefix:literal) => {
        #[doc = concat!("A strongly typed identifier with the `", $prefix, "…` prefix. See the [module](self) docs for the format; construct with [`parse`](Self::parse) or `FromStr`.")]
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);

        impl $name {
            /// Validate `value` as an identifier of this kind and return it.
            pub fn parse(value: &str) -> Result<Self, IdParseError> {
                split_prefixed($prefix, value).map(|_| Self(value.to_string()))
            }

            /// The identifier string, including its prefix.
            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// Consume the id and return the underlying string.
            pub fn into_string(self) -> String {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl std::str::FromStr for $name {
            type Err = IdParseError;
            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::parse(value)
            }
        }

        impl serde::Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(&self.0)
            }
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let value = String::deserialize(deserializer)?;
                Self::parse(&value).map_err(serde::de::Error::custom)
            }
        }
    };
}

domain_id!(ProjectId, "prj_");
domain_id!(GoalId, "goal_");
domain_id!(WorkItemId, "work_");
domain_id!(ArtifactId, "art_");
domain_id!(DecisionId, "dec_");
domain_id!(EvidenceId, "ev_");
domain_id!(AttemptId, "att_");
domain_id!(GrantId, "grant_");

/// Error raised for artifact versions below 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid artifact version {0}: versions are monotonic integers starting at 1")]
pub struct VersionError(pub u32);

/// A monotonic artifact version. Versions start at 1 and increase by exactly
/// one per publication, enforced by the [`crate::graph`] fold.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(transparent)]
pub struct ArtifactVersion(u32);

impl ArtifactVersion {
    /// The first version of any artifact.
    pub const FIRST: ArtifactVersion = ArtifactVersion(1);

    /// Validate `n` as a version number (must be ≥ 1).
    pub fn new(n: u32) -> Result<Self, VersionError> {
        if n == 0 {
            return Err(VersionError(n));
        }
        Ok(ArtifactVersion(n))
    }

    /// The version number.
    pub fn get(self) -> u32 {
        self.0
    }
}

impl fmt::Display for ArtifactVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// The number of domain facts applied to a project. Attempts pin the revision
/// they were started against so acceptance can detect material changes; the
/// count-based definition makes the revision of a fact set independent of the
/// order in which independent facts were folded.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Default,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(transparent)]
pub struct ProjectRevision(u64);

impl ProjectRevision {
    /// Construct a revision from a count of applied facts.
    pub const fn from_count(count: u64) -> Self {
        ProjectRevision(count)
    }

    /// The revision number.
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for ProjectRevision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
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
    fn parses_valid_prefixed_ids() {
        assert!(GoalId::parse("goal_1").is_ok());
        assert!(WorkItemId::parse("work_discovery-2").is_ok());
        assert!(ArtifactId::parse("art_brief").is_ok());
        assert!(DecisionId::parse("dec_q").is_ok());
        assert!(EvidenceId::parse("ev_9").is_ok());
        assert!(AttemptId::parse("att_1").is_ok());
        assert!(GrantId::parse("grant_model").is_ok());
        assert!(ProjectId::parse("prj_alpha").is_ok());
    }

    #[test]
    fn rejects_wrong_prefix_shape_and_case() {
        assert!(GoalId::parse("work_1").is_err());
        assert!(GoalId::parse("goal_").is_err());
        assert!(GoalId::parse("goal_-x").is_err());
        assert!(GoalId::parse("goal_UpPER").is_err());
        assert!(GoalId::parse("goal_a_b").is_err());
        assert!(GoalId::parse(&format!("goal_{}", "a".repeat(65))).is_err());
        assert!(GoalId::parse(&format!("goal_{}", "a".repeat(64))).is_ok());
    }

    #[test]
    fn versions_start_at_one() {
        assert!(ArtifactVersion::new(0).is_err());
        assert_eq!(
            ArtifactVersion::new(1).ok().map(ArtifactVersion::get),
            Some(1)
        );
        assert!(ArtifactVersion::FIRST < ArtifactVersion::new(2).ok().unwrap());
    }

    #[test]
    fn ids_round_trip_through_serde() {
        let id = WorkItemId::parse("work_spec").ok().unwrap();
        let json = serde_json::to_string(&id).ok().unwrap();
        assert_eq!(json, "\"work_spec\"");
        let back: WorkItemId = serde_json::from_str(&json).ok().unwrap();
        assert_eq!(back, id);
        assert!(serde_json::from_str::<WorkItemId>("\"typo_1\"").is_err());
    }

    #[test]
    fn revisions_compare_and_display() {
        assert!(ProjectRevision::from_count(3) > ProjectRevision::default());
        assert_eq!(ProjectRevision::from_count(12).to_string(), "12");
    }
}
