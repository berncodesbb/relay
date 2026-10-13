//! Strongly typed identifier newtypes.
//!
//! Relay identifiers are `<prefix>_<uuid-v4-lowercase>` strings (see
//! `docs/event-format.md` §1). The prefixes in use are `evt`, `cor`, `ses`,
//! `prj`, `art`, and `run`. One historical exception exists: the TypeScript
//! reference implementation generates session ids with the longer `session_`
//! prefix (`newId('session')` in `src/core/ids.ts`), so
//! [`SessionId::parse`] accepts both spellings; `ses_` is the spelling the
//! format specification prescribes for new data.
//!
//! Construction goes through [`parse`] (or `FromStr`), which validates the
//! prefix and the uuid-v4 shape; ids with invariants are never constructible
//! in an invalid state.

use std::fmt;

/// A `<prefix>_<uuid>` identifier kind, used for validation and placeholders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum IdKind {
    /// Event ids (`evt_…`).
    Event,
    /// Correlation ids (`cor_…`).
    Correlation,
    /// Session ids (`ses_…` or the legacy `session_…`).
    Session,
    /// Project ids (`prj_…`).
    Project,
    /// Artifact ids (`art_…`).
    Artifact,
    /// Run ids (`run_…`).
    Run,
}

impl fmt::Display for IdKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.prefix())
    }
}

impl IdKind {
    /// The canonical prefix for this kind (the spec spelling for sessions).
    pub fn prefix(self) -> &'static str {
        match self {
            IdKind::Event => "evt",
            IdKind::Correlation => "cor",
            IdKind::Session => "ses",
            IdKind::Project => "prj",
            IdKind::Artifact => "art",
            IdKind::Run => "run",
        }
    }
}

/// Error raised when a string is not a valid prefixed identifier.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid {kind} id \"{value}\": expected \"{prefix}_<uuid-v4-lowercase>\"")]
pub struct IdParseError {
    /// The identifier kind that was being parsed.
    pub kind: IdKind,
    /// The offending string.
    pub value: String,
    /// The expected prefix.
    pub prefix: String,
}

/// Validate the uuid-v4-lowercase tail: 8-4-4-4-12 hex digits with version
/// nibble `4` and variant nibble in `89ab`.
fn is_uuid_v4_tail(tail: &str) -> bool {
    let bytes = tail.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    let dash_at = |i: usize| bytes[i] == b'-';
    if !(dash_at(8) && dash_at(13) && dash_at(18) && dash_at(23)) {
        return false;
    }
    let hex_at = |i: usize| bytes[i].is_ascii_hexdigit() && !bytes[i].is_ascii_uppercase();
    for i in 0..36 {
        if dash_at(i) {
            continue;
        }
        if !hex_at(i) {
            return false;
        }
    }
    // Version nibble (first hex of the third group) must be 4; variant nibble
    // (first hex of the fourth group) must be 8, 9, a, or b.
    bytes[14] == b'4' && matches!(bytes[19], b'8' | b'9' | b'a' | b'b')
}

fn parse_prefixed(kind: IdKind, prefixes: &[&str], value: &str) -> Result<String, IdParseError> {
    for prefix in prefixes {
        let head = format!("{prefix}_");
        if let Some(tail) = value.strip_prefix(&head)
            && is_uuid_v4_tail(tail)
        {
            return Ok((*prefix).to_string());
        }
    }
    Err(IdParseError {
        kind,
        value: value.to_string(),
        prefix: prefixes[0].to_string(),
    })
}

macro_rules! id_newtype {
    ($name:ident, $kind:expr, [$($prefix:literal),+]) => {
        /// A strongly typed Relay identifier. See the [module](self) docs for
        /// the format; use [`parse`](Self::parse) or `FromStr` to construct.
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);

        impl $name {
            /// Validate `value` as an identifier of this kind and return it.
            pub fn parse(value: &str) -> Result<Self, IdParseError> {
                parse_prefixed($kind, &[$($prefix),+], value).map(|_| Self(value.to_string()))
            }

            /// The identifier string.
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

id_newtype!(EventId, IdKind::Event, ["evt"]);
id_newtype!(CorrelationId, IdKind::Correlation, ["cor"]);
// `ses_` is the specified prefix; `session_` is the TypeScript-era spelling
// present in existing logs and must keep validating.
id_newtype!(SessionId, IdKind::Session, ["ses", "session"]);
id_newtype!(ProjectId, IdKind::Project, ["prj"]);
id_newtype!(ArtifactId, IdKind::Artifact, ["art"]);
id_newtype!(RunId, IdKind::Run, ["run"]);

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_uuid_shaped_ids() {
        // Valid v4 shape: version nibble 4, variant nibble in 89ab.
        assert!(EventId::parse("evt_01234567-89ab-4def-8123-456789abcdef").is_ok());
        assert!(EventId::parse("evt_01234567-89ab-4def-9123-456789abcdef").is_ok());
        assert!(EventId::parse("evt_01234567-89ab-4def-a123-456789abcdef").is_ok());
        assert!(EventId::parse("evt_01234567-89ab-4def-b123-456789abcdef").is_ok());
        // Wrong version nibble, wrong variant nibble, non-v4 shape, bad
        // prefix, bad shape, and uppercase hex all fail.
        assert!(EventId::parse("evt_01234567-89ab-1def-8123-456789abcdef").is_err());
        assert!(EventId::parse("evt_01234567-89ab-4def-c123-456789abcdef").is_err());
        assert!(EventId::parse("evt_01234567-89ab-cdef-0123-456789abcdef").is_err());
        assert!(EventId::parse("evt_not-a-uuid").is_err());
        assert!(EventId::parse("cor_01234567-89ab-4def-8123-456789abcdef").is_err());
        assert!(EventId::parse("evt_01234567-89AB-4DEF-8123-456789ABCDEF").is_err());
    }

    #[test]
    fn accepts_both_session_prefixes() {
        assert!(SessionId::parse("ses_01234567-89ab-4def-8123-456789abcdef").is_ok());
        assert!(SessionId::parse("session_01234567-89ab-4def-8123-456789abcdef").is_ok());
        assert!(SessionId::parse("run_01234567-89ab-4def-8123-456789abcdef").is_err());
    }

    #[test]
    fn error_names_the_kind_and_prefix() {
        let err = ProjectId::parse("evt_01234567-89ab-4def-8123-456789abcdef")
            .expect_err("wrong prefix must fail");
        assert_eq!(err.kind, IdKind::Project);
        assert_eq!(err.prefix, "prj");
        assert!(err.to_string().contains("invalid prj id"));
    }
}
