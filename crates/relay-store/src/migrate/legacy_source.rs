//! Reading legacy project state as migration evidence.
//!
//! Two source shapes, both read **strictly read-only**:
//!
//! - [`LegacySource::StateDb`]: a TypeScript `.relay/state.db`, whose
//!   `events` table is replayed as evidence. Each row is validated through
//!   `relay-core` (the frozen reference decoder for those bytes) before it
//!   is accepted; the legacy database is never opened for writing.
//! - [`LegacySource::JsonlStore`]: an old Rust JSONL event store
//!   (`docs/event-format.md`), whose segments are parsed and fully
//!   verified through `relay-core`'s reference decoder.
//!
//! The output is ordered evidence — `(sequence, timestamp, type, payload)`
//! plus the artifact files referenced by `artifact.created` events — with
//! no opinion about what any of it means. Meaning extraction lives in
//! [`mod@crate::migrate::extract`].

use std::path::Path;
use std::path::PathBuf;

use crate::canonical::Json;
use crate::error::Result;
use crate::error::StoreError;

/// One piece of validated legacy evidence.
#[derive(Debug, Clone)]
pub struct LegacyEvent {
    /// 1-based legacy sequence.
    pub sequence: u64,
    /// Legacy timestamp.
    pub timestamp: String,
    /// Legacy event type (e.g. `artifact.created`).
    pub event_type: String,
    /// The event payload.
    pub payload: Json,
}

/// Where the legacy project state comes from.
#[derive(Debug, Clone)]
pub enum LegacySource {
    /// A TypeScript `.relay/state.db`. `artifacts_dir` is the legacy
    /// `.relay/artifacts/` directory holding the Markdown files the events
    /// reference (usually the state db's sibling).
    StateDb {
        /// Path to `state.db`.
        db: PathBuf,
        /// Path to the legacy artifacts directory.
        artifacts_dir: PathBuf,
    },
    /// An old Rust JSONL event store: the directory holding
    /// `manifest.json` and `events/*.jsonl` (a `.relay` root or an exported
    /// fixture directory with `events.jsonl`).
    JsonlStore {
        /// Directory holding the event log.
        root: PathBuf,
        /// Path to the legacy artifacts directory.
        artifacts_dir: PathBuf,
    },
    /// A single exported `events.jsonl` (fixture form).
    EventsFile {
        /// Path to `events.jsonl`.
        file: PathBuf,
        /// Path to the legacy artifacts directory.
        artifacts_dir: PathBuf,
    },
}

/// The validated legacy evidence of one project.
#[derive(Debug, Clone)]
pub struct LegacyEvidence {
    /// The legacy project id (`prj_…`).
    pub project_id: String,
    /// Every event in sequence order, validated by the reference decoder.
    pub events: Vec<LegacyEvent>,
    /// Artifact Markdown files, keyed by their workspace-relative path as
    /// referenced by `artifact.created` (missing files are simply absent).
    pub artifacts: std::collections::BTreeMap<String, Vec<u8>>,
}

impl LegacySource {
    /// Read and validate the legacy evidence. The source is never written
    /// to, and any file the reader cannot parse fails the migration before
    /// anything is staged.
    pub fn read(&self) -> Result<LegacyEvidence> {
        match self {
            LegacySource::StateDb { db, artifacts_dir } => read_state_db(db, artifacts_dir),
            LegacySource::JsonlStore {
                root,
                artifacts_dir,
            } => read_jsonl_store(root, artifacts_dir),
            LegacySource::EventsFile {
                file,
                artifacts_dir,
            } => read_events_file(file, artifacts_dir),
        }
    }
}

/// Validate one legacy event through relay-core (the frozen reference
/// decoder) and return its evidence triple.
#[allow(clippy::too_many_arguments)]
fn validated_event(
    id: &str,
    sequence: u64,
    session_id: Option<String>,
    event_type: &str,
    timestamp: &str,
    correlation_id: &str,
    causation_id: Option<String>,
    payload: &Json,
    project_id: &str,
    previous_hash: Option<String>,
) -> Result<(LegacyEvent, String)> {
    let envelope = crate::envelope::EventEnvelope::build(crate::envelope::EnvelopeDraft {
        project_id: project_id.to_string(),
        sequence,
        session_id,
        event_type: event_type.to_string(),
        timestamp: timestamp.to_string(),
        correlation_id: correlation_id.to_string(),
        causation_id,
        payload: payload.clone(),
        previous_hash,
        id: id.to_string(),
    })
    .map_err(|error| StoreError::Legacy {
        reason: format!(
            "legacy event {id} (sequence {sequence}) failed reference validation: {error}"
        ),
    })?;
    envelope.validate().map_err(|error| StoreError::Legacy {
        reason: format!("legacy event {id} (sequence {sequence}) is invalid: {error}"),
    })?;
    Ok((
        LegacyEvent {
            sequence,
            timestamp: envelope.timestamp.clone(),
            event_type: envelope.event_type.clone(),
            payload: envelope.payload.clone(),
        },
        envelope.hash,
    ))
}

fn read_state_db(db: &Path, artifacts_dir: &Path) -> Result<LegacyEvidence> {
    if !db.is_file() {
        return Err(StoreError::Legacy {
            reason: format!("legacy state database not found at {}", db.display()),
        });
    }
    let connection =
        rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|error| StoreError::Legacy {
                reason: format!(
                    "cannot open legacy state.db at {} read-only: {error}",
                    db.display()
                ),
            })?;
    let mut statement = connection
        .prepare(
            "SELECT id, project_id, sequence, session_id, type, schema_version, timestamp, \
             correlation_id, causation_id, payload FROM events ORDER BY project_id ASC, \
             sequence ASC;",
        )
        .map_err(|error| StoreError::Legacy {
            reason: format!("cannot read legacy events (is this a Relay state.db?): {error}"),
        })?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, Option<String>>(8)?,
                row.get::<_, String>(9)?,
            ))
        })
        .map_err(|error| StoreError::Legacy {
            reason: format!("cannot iterate legacy events: {error}"),
        })?;
    let mut project_id: Option<String> = None;
    let mut events: Vec<LegacyEvent> = Vec::new();
    let mut previous_hash: Option<String> = None;
    for row in rows {
        let (
            id,
            row_project,
            sequence,
            session_id,
            event_type,
            _schema,
            timestamp,
            correlation,
            causation,
            payload_text,
        ) = row.map_err(|error| StoreError::Legacy {
            reason: format!("cannot read a legacy row: {error}"),
        })?;
        let project = project_id.get_or_insert(row_project.clone()).clone();
        if row_project != project {
            return Err(StoreError::Legacy {
                reason: format!(
                    "the legacy database holds multiple projects ({project} and {row_project}). \
                     Migrate each project in its own workspace"
                ),
            });
        }
        let payload = Json::parse(&payload_text).map_err(|error| StoreError::Legacy {
            reason: format!("legacy event {id} payload is not valid JSON: {error}"),
        })?;
        let (event, hash) = validated_event(
            &id,
            sequence.max(0) as u64,
            session_id,
            &event_type,
            &timestamp,
            &correlation,
            causation,
            &payload,
            &project,
            previous_hash,
        )?;
        // The legacy rows carry no chain; chain them as they are read so
        // the reference decoder accepts them (same technique as the legacy
        // store importer).
        previous_hash = Some(hash);
        events.push(event);
    }
    let project_id = project_id.ok_or_else(|| StoreError::Legacy {
        reason: "the legacy database holds no events; nothing to migrate".to_string(),
    })?;
    let artifacts = collect_artifacts(&events, artifacts_dir);
    Ok(LegacyEvidence {
        project_id,
        events,
        artifacts,
    })
}

fn read_jsonl_store(root: &Path, artifacts_dir: &Path) -> Result<LegacyEvidence> {
    let events_dir = root.join("events");
    if !events_dir.is_dir() {
        // Fall back to a single events.jsonl inside the root.
        let file = root.join("events.jsonl");
        if file.is_file() {
            return read_events_file(&file, artifacts_dir);
        }
        return Err(StoreError::Legacy {
            reason: format!(
                "no legacy event log found under {} (expected events/ or events.jsonl)",
                root.display()
            ),
        });
    }
    let listing = crate::segments::discover(&events_dir)?;
    crate::segments::validate_tiling(&listing)?;
    let mut events: Vec<LegacyEvent> = Vec::new();
    let mut project_id: Option<String> = None;
    for segment in listing.closed.iter().chain(listing.active.iter()) {
        let path = events_dir.join(segment.file_name());
        let text = std::fs::read_to_string(&path).map_err(|error| StoreError::Legacy {
            reason: format!("cannot read legacy segment {}: {error}", path.display()),
        })?;
        for (index, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let name = segment.file_name();
            // The frozen reference decoder: envelope rules, typed payload
            // validation, and hash verification all come from relay-core.
            let envelope = crate::envelope::EventEnvelope::parse(
                line,
                crate::envelope::RecordLocation {
                    segment: &name,
                    line: (index + 1) as u64,
                },
            )
            .map_err(|error| StoreError::Legacy {
                reason: format!("legacy segment {name} is invalid: {error}"),
            })?;
            let project = project_id
                .get_or_insert(envelope.project_id.clone())
                .clone();
            if envelope.project_id != project {
                return Err(StoreError::Legacy {
                    reason: format!(
                        "the legacy log holds multiple projects ({project} and {})",
                        envelope.project_id
                    ),
                });
            }
            events.push(LegacyEvent {
                sequence: envelope.sequence,
                timestamp: envelope.timestamp.clone(),
                event_type: envelope.event_type.clone(),
                payload: envelope.payload.clone(),
            });
        }
    }
    let project_id = project_id.ok_or_else(|| StoreError::Legacy {
        reason: "the legacy event log is empty; nothing to migrate".to_string(),
    })?;
    let artifacts = collect_artifacts(&events, artifacts_dir);
    Ok(LegacyEvidence {
        project_id,
        events,
        artifacts,
    })
}

fn read_events_file(file: &Path, artifacts_dir: &Path) -> Result<LegacyEvidence> {
    let text = std::fs::read_to_string(file).map_err(|error| StoreError::Legacy {
        reason: format!("cannot read legacy events file {}: {error}", file.display()),
    })?;
    let mut events: Vec<LegacyEvent> = Vec::new();
    let mut project_id: Option<String> = None;
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let envelope = crate::envelope::EventEnvelope::parse(
            line,
            crate::envelope::RecordLocation {
                segment: "events.jsonl",
                line: (index + 1) as u64,
            },
        )
        .map_err(|error| StoreError::Legacy {
            reason: format!(
                "legacy events file {} is invalid at line {}: {error}",
                file.display(),
                index + 1
            ),
        })?;
        let project = project_id
            .get_or_insert(envelope.project_id.clone())
            .clone();
        if envelope.project_id != project {
            return Err(StoreError::Legacy {
                reason: format!(
                    "the legacy log holds multiple projects ({project} and {})",
                    envelope.project_id
                ),
            });
        }
        events.push(LegacyEvent {
            sequence: envelope.sequence,
            timestamp: envelope.timestamp.clone(),
            event_type: envelope.event_type.clone(),
            payload: envelope.payload.clone(),
        });
    }
    let project_id = project_id.ok_or_else(|| StoreError::Legacy {
        reason: "the legacy events file is empty; nothing to migrate".to_string(),
    })?;
    let artifacts = collect_artifacts(&events, artifacts_dir);
    Ok(LegacyEvidence {
        project_id,
        events,
        artifacts,
    })
}

/// Load the Markdown files referenced by `artifact.created` events. Paths
/// are legacy workspace-relative; missing files are skipped (the extractor
/// records the absence).
fn collect_artifacts(
    events: &[LegacyEvent],
    artifacts_dir: &Path,
) -> std::collections::BTreeMap<String, Vec<u8>> {
    let mut map = std::collections::BTreeMap::new();
    for event in events {
        if (event.event_type == "artifact.created" || event.event_type == "artifact.revised")
            && let Some(path) = event.payload.get("path").and_then(Json::as_str)
            && let Some(bytes) = read_artifact_file(artifacts_dir, path)
        {
            map.insert(path.to_string(), bytes);
        }
    }
    map
}

/// Resolve a legacy workspace-relative artifact path against the legacy
/// artifacts directory, rejecting anything that escapes it.
fn read_artifact_file(artifacts_dir: &Path, workspace_relative: &str) -> Option<Vec<u8>> {
    let relative = workspace_relative
        .strip_prefix(".relay/artifacts/")
        .unwrap_or(workspace_relative);
    let mut candidate = artifacts_dir.to_path_buf();
    for segment in relative.split('/') {
        if segment == ".." || segment.is_empty() {
            return None;
        }
        candidate.push(segment);
    }
    std::fs::read(&candidate).ok()
}
