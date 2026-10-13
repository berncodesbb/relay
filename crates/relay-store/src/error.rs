//! Error type for the Relay store.
//!
//! Every variant renders an actionable, self-describing message. Diagnostics
//! that point at a specific record always carry the segment name and the
//! 1-based line number so users can find the offending bytes, matching
//! `docs/event-format.md` §3 and §7.

use std::path::PathBuf;

/// Result alias used across the store crate.
pub type Result<T, E = StoreError> = std::result::Result<T, E>;

/// Everything that can go wrong while opening, appending to, verifying, or
/// migrating a Relay store.
#[derive(Debug)]
pub enum StoreError {
    /// An underlying filesystem operation failed. `context` says what the
    /// store was doing.
    Io {
        /// Operation description, e.g. `"append to active segment"`.
        context: String,
        /// The file or directory involved.
        path: PathBuf,
        /// The original error.
        source: std::io::Error,
    },
    /// JSON could not be parsed or contains a number this reader cannot
    /// represent canonically.
    Canonical {
        /// What was wrong.
        reason: String,
    },
    /// The store manifest is missing, unreadable, or invalid.
    Manifest {
        /// Manifest path.
        path: PathBuf,
        /// What was wrong.
        reason: String,
    },
    /// A store written by a newer Relay (higher `storeVersion`) — readers
    /// fail closed with upgrade guidance instead of guessing.
    UnsupportedStore {
        /// Which version was found.
        found: u64,
        /// The highest version this build understands.
        supported: u64,
        /// Where the version came from (manifest or envelope).
        location: String,
    },
    /// A single event record failed validation. `segment` and `line` locate
    /// the record; `sequence` is included when the record parsed far enough
    /// to carry one.
    Record {
        /// Segment file name, e.g. `0000000000000501-open.jsonl`.
        segment: String,
        /// 1-based physical line number within the segment.
        line: u64,
        /// Event sequence number when known.
        sequence: Option<u64>,
        /// Human-readable reason.
        reason: String,
    },
    /// The hash chain is broken at a specific record.
    ChainBreak {
        /// Segment file name.
        segment: String,
        /// 1-based line number.
        line: u64,
        /// Sequence of the offending event.
        sequence: u64,
        /// The digest the chain required.
        expected: String,
        /// The digest actually present (or recomputed, for hash mismatches).
        actual: String,
        /// Whether the mismatch is the stored `hash` field (true) or the
        /// `previousHash` link (false).
        hash_field: bool,
    },
    /// Segment discovery or tiling is invalid (gap, overlap, duplicate
    /// active segment, bad name).
    Segments {
        /// Human-readable reason.
        reason: String,
    },
    /// The writer lock could not be acquired, or its state is unusable.
    Lock {
        /// Lock file path.
        path: PathBuf,
        /// Human-readable diagnostic (holder pid and age when known).
        detail: String,
    },
    /// A SQLite projection error (open, query, transaction).
    Projection {
        /// Human-readable context.
        reason: String,
        /// The underlying rusqlite error, when present.
        source: Option<rusqlite::Error>,
    },
    /// An append was rejected: invalid batch member, sequence/hash conflict,
    /// or stale expected revision.
    Append {
        /// Human-readable, actionable reason.
        reason: String,
    },
    /// Legacy import failure (source unreadable, validation failed, staging
    /// problem, atomic switch failed).
    Legacy {
        /// Human-readable reason.
        reason: String,
    },
    /// A fault-injection hook asked the store to simulate a crash at this
    /// point. Storage layers above must treat it as a process death; the
    /// durable JSONL state must remain recoverable.
    SimulatedCrash {
        /// The crash point identifier.
        point: String,
    },
    /// Invalid argument or misuse of the store API.
    Invalid {
        /// Human-readable reason.
        reason: String,
    },
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Io {
                context,
                path,
                source,
            } => write!(
                f,
                "storage I/O error while {context} at {}: {source}",
                path.display()
            ),
            StoreError::Canonical { reason } => write!(f, "canonical JSON error: {reason}"),
            StoreError::Manifest { path, reason } => {
                write!(f, "invalid store manifest at {}: {reason}", path.display())
            }
            StoreError::UnsupportedStore {
                found,
                supported,
                location,
            } => write!(
                f,
                "this Relay project was written by a newer Relay ({location} version {found}, \
                 but this build understands up to {supported}). Upgrade Relay — or open the \
                 project with the newer install — before using this store again."
            ),
            StoreError::Record {
                segment,
                line,
                sequence,
                reason,
            } => match sequence {
                Some(sequence) => write!(
                    f,
                    "invalid event record at {segment} line {line} (sequence {sequence}): {reason}"
                ),
                None => write!(f, "invalid event record at {segment} line {line}: {reason}"),
            },
            StoreError::ChainBreak {
                segment,
                line,
                sequence,
                expected,
                actual,
                hash_field,
            } => {
                let field = if *hash_field { "hash" } else { "previousHash" };
                write!(
                    f,
                    "broken hash chain at {segment} line {line} (sequence {sequence}): \
                     {field} expected {expected}, found {actual}. The event log was truncated, \
                     reordered, or edited; run `relay doctor check` to inspect the store."
                )
            }
            StoreError::Segments { reason } => write!(f, "invalid event segments: {reason}"),
            StoreError::Lock { path, detail } => write!(
                f,
                "could not acquire the Relay writer lock at {}: {detail}",
                path.display()
            ),
            StoreError::Projection { reason, source } => match source {
                Some(source) => write!(f, "SQLite projection error while {reason}: {source}"),
                None => write!(f, "SQLite projection error: {reason}"),
            },
            StoreError::Append { reason } => write!(f, "append rejected: {reason}"),
            StoreError::Legacy { reason } => write!(f, "legacy migration error: {reason}"),
            StoreError::SimulatedCrash { point } => {
                write!(f, "simulated crash at storage point `{point}`")
            }
            StoreError::Invalid { reason } => write!(f, "invalid store usage: {reason}"),
        }
    }
}

impl std::error::Error for StoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            StoreError::Io { source, .. } => Some(source),
            StoreError::Projection {
                source: Some(source),
                ..
            } => Some(source),
            _ => None,
        }
    }
}

impl From<rusqlite::Error> for StoreError {
    fn from(error: rusqlite::Error) -> Self {
        StoreError::Projection {
            reason: "executing a projection statement".to_string(),
            source: Some(error),
        }
    }
}
