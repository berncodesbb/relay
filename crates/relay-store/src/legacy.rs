//! Read-only legacy import (`.relay/state.db` → canonical store) per
//! `docs/event-format.md` §10, plus import of a canonical JSONL directory
//! produced by `scripts/export-legacy-project.mjs`.
//!
//! The legacy TypeScript store keeps events in a SQLite table `events`
//! (`id, project_id, sequence, session_id, type, schema_version, timestamp,
//! correlation_id, causation_id, payload`) plus derived tables. Import is
//! direct and strictly read-only against the source:
//!
//! 1. Refuse when a Relay writer is active; hold a migration lock.
//! 2. `PRAGMA integrity_check` on the source; any failure aborts.
//! 3. Read rows ordered by `project_id, sequence`, validating each.
//! 4. Verify sequences are contiguous from 1.
//! 5. Stage the new store in `.relay-rust-migration-<projectId>/` with
//!    fresh hashes (legacy rows carry none).
//! 6. Copy artifacts byte-for-byte, recording SHA-256 digests.
//! 7. Replay the staged JSONL and compare against the legacy rows.
//! 8. Build SQLite projections in the staging store.
//! 9. Verify staged artifact digests, then atomically switch directories
//!    (old `.relay` → `.relay-backup-<UTC>/`, retained indefinitely).
//! 10. Any failure before the switch leaves the original `.relay/` in
//!     charge; the source database is never opened for writing.

use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use rusqlite::Connection;
use rusqlite::OpenFlags;
use sha2::{Digest, Sha256};

use crate::canonical::Json;
use crate::clock::Clock;
use crate::envelope::EnvelopeDraft;
use crate::envelope::EventEnvelope;
use crate::envelope::PayloadValidator;
use crate::envelope::RecordLocation;
use crate::error::Result;
use crate::error::StoreError;
use crate::fault::NoFaults;
use crate::lock;
use crate::log::Durability;
use crate::log::EventLog;
use crate::log::OpenMode;
use crate::paths::RelayPaths;
use crate::projection::SqliteProjection;

/// Migration behavior configuration.
#[derive(Clone)]
pub struct MigrationOptions {
    /// Migration-lock acquisition timeout.
    pub lock_timeout: Duration,
    /// Time source for stamps and manifest fields.
    pub clock: Arc<dyn Clock>,
    /// Optional strict payload validation (relay-core integration).
    pub payload_validator: Option<PayloadValidator>,
    /// Durability used while writing the staged store.
    pub durability: Durability,
    /// Optional probe invoked with the staging directory after projections
    /// are built and before the final artifact-digest verification. Test
    /// and diagnostic hook (e.g. to observe or disturb the staged bytes at
    /// the last moment before the atomic switch).
    pub staging_probe: Option<MigrationProbe>,
}

/// Probe invoked with the staging directory right before the final digest
/// verification and atomic switch.
pub type MigrationProbe = std::sync::Arc<dyn Fn(&Path) -> Result<()> + Send + Sync>;

impl Default for MigrationOptions {
    fn default() -> MigrationOptions {
        MigrationOptions {
            lock_timeout: crate::lock::DEFAULT_LOCK_TIMEOUT,
            clock: Arc::new(crate::clock::SystemClock),
            payload_validator: None,
            durability: Durability::Durable,
            staging_probe: None,
        }
    }
}

/// SHA-256 digest recorded for one migrated artifact file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactDigest {
    /// Path relative to the store's `artifacts/` directory.
    pub relative_path: String,
    /// Lowercase hex SHA-256 of the file bytes.
    pub sha256: String,
}

/// What a migration did.
#[derive(Debug, Clone)]
pub enum MigrationOutcome {
    /// The migration completed and the canonical store is now active.
    Migrated(Box<MigrationReport>),
    /// Nothing to do: the workspace already uses the canonical store.
    AlreadyMigrated {
        /// Explanation.
        detail: String,
    },
}

/// Report for a completed migration.
#[derive(Debug, Clone)]
pub struct MigrationReport {
    /// The migrated project.
    pub project_id: String,
    /// Events migrated.
    pub events_migrated: u64,
    /// Segment file names in the new store.
    pub segments: Vec<String>,
    /// Artifact digests recorded during the copy.
    pub artifacts: Vec<ArtifactDigest>,
    /// Where the previous `.relay/` was retained (legacy switch only).
    pub backup_dir: Option<PathBuf>,
}

/// Migrate a legacy `.relay/state.db` project in `workspace_root`.
pub fn migrate_legacy_state_db(
    workspace_root: &Path,
    options: &MigrationOptions,
) -> Result<MigrationOutcome> {
    let paths = RelayPaths::new(workspace_root);
    if let Preflight::AlreadyMigrated(detail) = preflight(&paths, true)? {
        return Ok(MigrationOutcome::AlreadyMigrated { detail });
    }

    // Refuse when a Relay writer is active (§10 step 1).
    refuse_if_writer_active(&paths)?;
    let _migration_guard = lock::acquire(
        &paths.migration_lock(),
        &paths.recovery_dir,
        options.lock_timeout,
        options.clock.as_ref(),
    )?;

    // Read-only source connection (§10 step 2). The source is never opened
    // for writing.
    let source =
        Connection::open_with_flags(&paths.legacy_state_db, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|error| StoreError::Legacy {
                reason: format!(
                    "cannot open the legacy database at {} read-only: {error}",
                    paths.legacy_state_db.display()
                ),
            })?;
    let integrity: String = source
        .query_row("PRAGMA integrity_check;", [], |row| row.get(0))
        .map_err(|error| StoreError::Legacy {
            reason: format!(
                "integrity check failed on {}: {error}",
                paths.legacy_state_db.display()
            ),
        })?;
    if integrity != "ok" {
        return Err(StoreError::Legacy {
            reason: format!(
                "the legacy database reports integrity \"{integrity}\" (expected \"ok\"); \
                 migration aborted and the original .relay/ is untouched"
            ),
        });
    }

    // Read and validate rows (§10 steps 3–4).
    let rows = read_legacy_rows(&source)?;
    if rows.is_empty() {
        return Err(StoreError::Legacy {
            reason: "the legacy database holds no events; nothing to migrate".to_string(),
        });
    }
    let project_id = rows
        .first()
        .map(|row| row.project_id.clone())
        .ok_or_else(|| StoreError::Legacy {
            reason: "legacy rows missing".to_string(),
        })?;
    let envelopes = build_envelopes(&rows, &project_id, options)?;

    // Stage (§10 step 5).
    let staging = workspace_root.join(format!(".relay-rust-migration-{project_id}"));
    reset_directory(&staging)?;
    let staging_paths = RelayPaths::from_relay_root(&staging);
    let mut log = EventLog::init(&staging_paths, &project_id, options.clock.as_ref())?;
    for chunk in envelopes.chunks(500) {
        log.append(chunk, options.durability, &NoFaults)?;
    }
    let segments = list_segments(&staging_paths)?;

    // Artifacts (§10 step 6): the legacy workspace must contain every
    // referenced artifact byte-for-byte.
    let referenced = referenced_artifact_paths(&envelopes);
    let artifacts = copy_artifacts(
        &paths.artifacts_dir,
        &staging_paths.artifacts_dir,
        &referenced,
        true,
    )?;

    // Replay comparison (§10 step 7).
    verify_staged_replay(&staging_paths, &rows)?;

    // Projections (§10 step 8).
    {
        let mut projection = SqliteProjection::open(
            &staging_paths.index_db,
            &staging_paths.recovery_dir,
            options.clock.as_ref(),
        )?;
        let records = log.read_all()?;
        projection.replay_all(&records, options.clock.as_ref(), &NoFaults)?;
    }

    // Digest verification just before the switch (§10 step 9 precondition),
    // after the optional probe (tests use it to disturb staged bytes and
    // prove the digest check catches them).
    if let Some(probe) = &options.staging_probe {
        probe(&staging)?;
    }
    verify_artifact_digests(&staging_paths.artifacts_dir, &artifacts)?;

    // Atomic switch (§10 step 9).
    let stamp = options.clock.now_iso();
    let backup = workspace_root.join(format!(
        ".relay-backup-{}",
        stamp
            .chars()
            .filter(|c| *c != '-' && *c != ':' && *c != '.')
            .collect::<String>()
    ));
    std::fs::rename(&paths.root, &backup).map_err(|error| StoreError::Legacy {
        reason: format!(
            "could not move the original .relay/ to {}: {error}",
            backup.display()
        ),
    })?;
    if let Err(error) = std::fs::rename(&staging, &paths.root) {
        // Put the original back before failing: the original stays in charge.
        let _ = std::fs::rename(&backup, &paths.root);
        return Err(StoreError::Legacy {
            reason: format!(
                "could not move the staged store into place at {}: {error} (the original \
                 .relay/ was restored)",
                paths.root.display()
            ),
        });
    }

    Ok(MigrationOutcome::Migrated(Box::new(MigrationReport {
        project_id,
        events_migrated: envelopes.len() as u64,
        segments,
        artifacts,
        backup_dir: Some(backup),
    })))
}

/// Import a canonical JSONL directory (the exporter output, which follows
/// `docs/event-format.md`) as the workspace's store.
///
/// The directory must contain an `events/` subdirectory with JSONL segment
/// files; a `manifest.json` and `artifacts/` are honored when present.
/// Envelopes are revalidated (chain included) before staging.
pub fn import_jsonl_directory(
    workspace_root: &Path,
    source_dir: &Path,
    options: &MigrationOptions,
) -> Result<MigrationOutcome> {
    let paths = RelayPaths::new(workspace_root);
    preflight(&paths, false)?;
    refuse_if_writer_active(&paths)?;
    let _migration_guard = lock::acquire(
        &paths.migration_lock(),
        &paths.recovery_dir,
        options.lock_timeout,
        options.clock.as_ref(),
    )?;

    // A manifest in the source, when present, is validated (fail closed on
    // newer store versions). For a single-file import the surrounding
    // directory provides manifest.json and artifacts/.
    let source_root: &Path = if source_dir.is_file() {
        source_dir.parent().ok_or_else(|| StoreError::Legacy {
            reason: format!(
                "import file {} has no parent directory",
                source_dir.display()
            ),
        })?
    } else {
        source_dir
    };
    let source_manifest = source_root.join("manifest.json");
    if source_manifest.exists() {
        crate::manifest::Manifest::read(&source_manifest).map_err(|error| StoreError::Legacy {
            reason: format!("the exporter manifest is invalid: {error}"),
        })?;
    }

    // Accepted layouts: a store directory (`events/*.jsonl`), the
    // exporter layout (`<projectId>.jsonl` files beside `export.json`), or
    // a single `.jsonl` file path.
    let mut files: Vec<PathBuf> = if source_dir.is_file() {
        vec![source_dir.to_path_buf()]
    } else {
        let source_events = source_dir.join("events");
        let scan_dir = if source_events.is_dir() {
            source_events
        } else {
            source_dir.to_path_buf()
        };
        let found: Vec<PathBuf> = std::fs::read_dir(&scan_dir)
            .map_err(|error| StoreError::Legacy {
                reason: format!("cannot read {}: {error}", scan_dir.display()),
            })?
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
            .collect();
        let mut sorted = found;
        sorted.sort();
        sorted
    };
    if files.is_empty() {
        return Err(StoreError::Legacy {
            reason: format!("no .jsonl files under {}", source_dir.display()),
        });
    }
    files.dedup();

    // Validate the whole chain before staging anything.
    let mut envelopes: Vec<EventEnvelope> = Vec::new();
    let mut expected_sequence: u64 = 1;
    let mut previous_hash: Option<String> = None;
    let mut project_id: Option<String> = None;
    for file in &files {
        let text = std::fs::read_to_string(file).map_err(|error| StoreError::Legacy {
            reason: format!("cannot read {}: {error}", file.display()),
        })?;
        let name = file
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        for (line_index, line) in text.lines().enumerate() {
            let line_number = line_index as u64 + 1;
            if line.is_empty() {
                continue;
            }
            let location = RecordLocation {
                segment: &name,
                line: line_number,
            };
            let envelope =
                EventEnvelope::parse(line, location).map_err(|error| StoreError::Legacy {
                    reason: format!(
                        "the exporter output failed validation ({}); migration aborted",
                        error
                    ),
                })?;
            if let Some(project) = &project_id {
                if *project != envelope.project_id {
                    return Err(StoreError::Legacy {
                        reason: "the exporter output mixes projects".to_string(),
                    });
                }
            } else {
                project_id = Some(envelope.project_id.clone());
            }
            if envelope.sequence != expected_sequence {
                return Err(StoreError::Legacy {
                    reason: format!(
                        "the exporter output is not contiguous: expected sequence \
                         {expected_sequence}, found {}",
                        envelope.sequence
                    ),
                });
            }
            if envelope.previous_hash != previous_hash {
                return Err(StoreError::Legacy {
                    reason: "the exporter output hash chain is broken".to_string(),
                });
            }
            expected_sequence += 1;
            previous_hash = Some(envelope.hash.clone());
            envelopes.push(envelope);
        }
    }
    let project_id = project_id.ok_or_else(|| StoreError::Legacy {
        reason: "the exporter output holds no events".to_string(),
    })?;

    let staging = workspace_root.join(format!(".relay-rust-migration-{project_id}"));
    reset_directory(&staging)?;
    let staging_paths = RelayPaths::from_relay_root(&staging);
    let mut log = EventLog::init(&staging_paths, &project_id, options.clock.as_ref())?;
    for chunk in envelopes.chunks(500) {
        log.append(chunk, options.durability, &NoFaults)?;
    }
    let segments = list_segments(&staging_paths)?;

    // Artifact bytes are copied when present; an events-only export keeps
    // its artifacts wherever they already live, so absence is not an error
    // on this path.
    let referenced = referenced_artifact_paths(&envelopes);
    let artifacts = copy_artifacts(
        &source_root.join("artifacts"),
        &staging_paths.artifacts_dir,
        &referenced,
        false,
    )?;
    verify_staged_replay_jsonl(&staging_paths, &envelopes)?;
    {
        let mut projection = SqliteProjection::open(
            &staging_paths.index_db,
            &staging_paths.recovery_dir,
            options.clock.as_ref(),
        )?;
        let records = log.read_all()?;
        projection.replay_all(&records, options.clock.as_ref(), &NoFaults)?;
    }
    if let Some(probe) = &options.staging_probe {
        probe(&staging)?;
    }
    verify_artifact_digests(&staging_paths.artifacts_dir, &artifacts)?;

    let had_relay = paths.root.exists();
    let mut backup = None;
    if had_relay {
        let stamp = options.clock.now_iso();
        let target = workspace_root.join(format!(
            ".relay-backup-{}",
            stamp
                .chars()
                .filter(|c| *c != '-' && *c != ':' && *c != '.')
                .collect::<String>()
        ));
        std::fs::rename(&paths.root, &target).map_err(|error| StoreError::Legacy {
            reason: format!(
                "could not move the existing .relay/ to {}: {error}",
                target.display()
            ),
        })?;
        backup = Some(target);
    }
    if let Err(error) = std::fs::rename(&staging, &paths.root) {
        if had_relay && let Some(backup) = &backup {
            let _ = std::fs::rename(backup, &paths.root);
        }
        return Err(StoreError::Legacy {
            reason: format!(
                "could not move the staged store into place at {}: {error}",
                paths.root.display()
            ),
        });
    }

    Ok(MigrationOutcome::Migrated(Box::new(MigrationReport {
        project_id,
        events_migrated: envelopes.len() as u64,
        segments,
        artifacts,
        backup_dir: backup,
    })))
}

/// One legacy row, as read from the source database.
#[derive(Debug, Clone)]
struct LegacyRow {
    id: String,
    project_id: String,
    sequence: u64,
    session_id: Option<String>,
    event_type: String,
    schema_version: u64,
    timestamp: String,
    correlation_id: String,
    causation_id: Option<String>,
    payload: Json,
}

fn read_legacy_rows(source: &Connection) -> Result<Vec<LegacyRow>> {
    let mut statement = source
        .prepare(
            "SELECT id, project_id, sequence, session_id, type, schema_version, timestamp, \
             correlation_id, causation_id, payload FROM events ORDER BY project_id ASC, \
             sequence ASC;",
        )
        .map_err(|error| StoreError::Legacy {
            reason: format!("cannot read legacy events: {error} (is this a Relay state.db?)"),
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
    let mut parsed: Vec<LegacyRow> = Vec::new();
    for row in rows {
        let (
            id,
            project_id,
            sequence,
            session_id,
            event_type,
            schema_version,
            timestamp,
            correlation_id,
            causation_id,
            payload_text,
        ) = row.map_err(|error| StoreError::Legacy {
            reason: format!("cannot read a legacy row: {error}"),
        })?;
        let payload = Json::parse(&payload_text).map_err(|error| StoreError::Legacy {
            reason: format!("legacy event {id} carries a payload that is not valid JSON: {error}"),
        })?;
        parsed.push(LegacyRow {
            id,
            project_id,
            sequence: sequence.max(0) as u64,
            session_id,
            event_type,
            schema_version: schema_version.max(0) as u64,
            timestamp,
            correlation_id,
            causation_id,
            payload,
        });
    }
    Ok(parsed)
}

/// Validate legacy rows and build chained envelopes with fresh hashes
/// (§10 steps 3–5). Rows must belong to one project with contiguous
/// sequences from 1.
fn build_envelopes(
    rows: &[LegacyRow],
    project_id: &str,
    options: &MigrationOptions,
) -> Result<Vec<EventEnvelope>> {
    let distinct: Vec<&str> = {
        let mut seen: Vec<&str> = Vec::new();
        for row in rows {
            if row.project_id != project_id {
                seen.push(row.project_id.as_str());
            }
        }
        seen.sort();
        seen.dedup();
        seen
    };
    if !distinct.is_empty() {
        return Err(StoreError::Legacy {
            reason: format!(
                "the legacy database holds multiple projects ({project_id} and {}). The Rust \
                 store is one project per workspace; export each project into its own \
                 workspace with scripts/export-legacy-project.mjs and import them separately",
                distinct.join(", ")
            ),
        });
    }
    let mut previous_hash: Option<String> = None;
    let mut envelopes = Vec::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        let expected = index as u64 + 1;
        if row.sequence != expected {
            return Err(StoreError::Legacy {
                reason: format!(
                    "legacy sequences must be contiguous from 1: expected {expected}, found \
                     {} for event {}",
                    row.sequence, row.id
                ),
            });
        }
        crate::envelope::validate_prefixed_uuid(&row.id, "").map_err(|reason| {
            StoreError::Legacy {
                reason: format!("legacy event id {}: {reason}", row.id),
            }
        })?;
        crate::envelope::validate_prefixed_uuid(&row.correlation_id, "cor").map_err(|reason| {
            StoreError::Legacy {
                reason: format!("legacy event {} correlation id: {reason}", row.id),
            }
        })?;
        if let Some(session) = &row.session_id {
            crate::envelope::validate_prefixed_uuid(session, "ses").map_err(|reason| {
                StoreError::Legacy {
                    reason: format!("legacy event {} session id: {reason}", row.id),
                }
            })?;
        }
        let envelope = EventEnvelope::build(EnvelopeDraft {
            project_id: project_id.to_string(),
            sequence: row.sequence,
            session_id: row.session_id.clone(),
            event_type: row.event_type.clone(),
            timestamp: row.timestamp.clone(),
            correlation_id: row.correlation_id.clone(),
            causation_id: row.causation_id.clone(),
            payload: row.payload.clone(),
            previous_hash,
            id: row.id.clone(),
        })?;
        envelope.validate().map_err(|error| match error {
            StoreError::Record { reason, .. } => StoreError::Legacy {
                reason: format!(
                    "legacy event {} (sequence {}) failed validation: {reason}",
                    row.id, row.sequence
                ),
            },
            other => other,
        })?;
        envelope.validate_payload(&options.payload_validator)?;
        previous_hash = Some(envelope.hash.clone());
        envelopes.push(envelope);
    }
    Ok(envelopes)
}

/// Artifact paths referenced by artifact events, relative to the workspace
/// root.
fn referenced_artifact_paths(envelopes: &[EventEnvelope]) -> Vec<String> {
    let mut paths: Vec<String> = Vec::new();
    for envelope in envelopes {
        if (envelope.event_type == "artifact.created" || envelope.event_type == "artifact.revised")
            && let Some(path) = envelope.payload.get("path").and_then(Json::as_str)
        {
            paths.push(path.to_string());
        }
    }
    paths
}

/// Copy `source_dir` into `dest_dir` byte-for-byte, recording digests
/// (§10 step 6). Every referenced artifact path must exist under the
/// source's `artifacts/` tree; a missing file aborts the migration.
fn copy_artifacts(
    source_dir: &Path,
    dest_dir: &Path,
    referenced: &[String],
    require_referenced: bool,
) -> Result<Vec<ArtifactDigest>> {
    if require_referenced && !source_dir.exists() && !referenced.is_empty() {
        let first = referenced.first().cloned().unwrap_or_default();
        return Err(StoreError::Legacy {
            reason: format!(
                "legacy events reference artifacts (e.g. {first}) but {} does not exist",
                source_dir.display()
            ),
        });
    }
    let mut digests: Vec<ArtifactDigest> = Vec::new();
    if source_dir.exists() {
        copy_tree(source_dir, dest_dir, dest_dir, &mut digests)?;
    }
    if require_referenced {
        for referenced_path in referenced {
            // Legacy paths are workspace-relative and look like
            // `.relay/artifacts/<kind>/<file>`; exporter output may use
            // `artifacts/<kind>/<file>`.
            let artifacts_relative = referenced_path
                .strip_prefix(".relay/artifacts/")
                .or_else(|| referenced_path.strip_prefix("artifacts/"))
                .map(str::to_string);
            let present = match artifacts_relative {
                Some(relative) => dest_dir.join(relative).exists(),
                None => false,
            };
            if !present {
                return Err(StoreError::Legacy {
                    reason: format!(
                        "artifact file referenced by a legacy event is missing: {referenced_path}"
                    ),
                });
            }
        }
    }
    Ok(digests)
}

/// Recursively copy files, hashing each. Digest paths are recorded
/// relative to `root` (the destination `artifacts/` directory).
fn copy_tree(
    source: &Path,
    dest: &Path,
    root: &Path,
    digests: &mut Vec<ArtifactDigest>,
) -> Result<()> {
    let entries = std::fs::read_dir(source).map_err(|error| StoreError::Legacy {
        reason: format!("cannot read {}: {error}", source.display()),
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| StoreError::Legacy {
            reason: format!("cannot enumerate {}: {error}", source.display()),
        })?;
        let file_type = entry.file_type().map_err(|error| StoreError::Legacy {
            reason: format!("cannot inspect {}: {error}", entry.path().display()),
        })?;
        let target = dest.join(entry.file_name());
        if file_type.is_dir() {
            std::fs::create_dir_all(&target).map_err(|error| StoreError::Legacy {
                reason: format!("cannot create {}: {error}", target.display()),
            })?;
            copy_tree(&entry.path(), &target, root, digests)?;
        } else if file_type.is_file() {
            std::fs::create_dir_all(dest).map_err(|error| StoreError::Legacy {
                reason: format!("cannot create {}: {error}", dest.display()),
            })?;
            let bytes = std::fs::read(entry.path()).map_err(|error| StoreError::Legacy {
                reason: format!("cannot read {}: {error}", entry.path().display()),
            })?;
            let digest = hex::encode(Sha256::digest(&bytes));
            if target.exists() {
                return Err(StoreError::Legacy {
                    reason: format!(
                        "refusing to overwrite existing artifact {} during staging",
                        target.display()
                    ),
                });
            }
            crate::manifest::write_and_sync(&target, &bytes)?;
            let relative = target
                .strip_prefix(root)
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| entry.file_name().to_string_lossy().to_string());
            digests.push(ArtifactDigest {
                relative_path: relative,
                sha256: digest,
            });
        }
    }
    Ok(())
}

/// Re-open the staged store and compare field-exactly with the legacy rows
/// (§10 step 7). The full normalized-state comparison joins the relay-core
/// reducer at integration time; event-level equivalence is the storage
/// contract here.
fn verify_staged_replay(staging: &RelayPaths, rows: &[LegacyRow]) -> Result<()> {
    let (log, _recoveries) = EventLog::open(staging, OpenMode::Reader, &crate::clock::SystemClock)?;
    let records = log.read_all()?;
    if records.len() != rows.len() {
        return Err(StoreError::Legacy {
            reason: format!(
                "staged replay produced {} events, the legacy database holds {}",
                records.len(),
                rows.len()
            ),
        });
    }
    for (record, row) in records.iter().zip(rows.iter()) {
        let envelope = &record.envelope;
        if envelope.id != row.id
            || envelope.sequence != row.sequence
            || envelope.session_id != row.session_id
            || envelope.event_type != row.event_type
            || envelope.schema_version != row.schema_version
            || envelope.timestamp != row.timestamp
            || envelope.correlation_id != row.correlation_id
            || envelope.causation_id != row.causation_id
            || envelope.payload != row.payload
        {
            return Err(StoreError::Legacy {
                reason: format!(
                    "staged event {} differs from its legacy row; migration aborted",
                    row.id
                ),
            });
        }
    }
    Ok(())
}

/// Same comparison for JSONL imports (source of truth is the envelope list).
fn verify_staged_replay_jsonl(staging: &RelayPaths, envelopes: &[EventEnvelope]) -> Result<()> {
    let (log, _recoveries) = EventLog::open(staging, OpenMode::Reader, &crate::clock::SystemClock)?;
    let records = log.read_all()?;
    if records.len() != envelopes.len() {
        return Err(StoreError::Legacy {
            reason: format!(
                "staged replay produced {} events, the import source holds {}",
                records.len(),
                envelopes.len()
            ),
        });
    }
    for (record, envelope) in records.iter().zip(envelopes.iter()) {
        if record.envelope != *envelope {
            return Err(StoreError::Legacy {
                reason: format!(
                    "staged event {} differs from the imported envelope; migration aborted",
                    envelope.id
                ),
            });
        }
    }
    Ok(())
}

/// Re-verify staged artifact bytes against the recorded digests right
/// before the atomic switch; any mismatch aborts with the original
/// untouched (§10).
fn verify_artifact_digests(artifacts_dir: &Path, digests: &[ArtifactDigest]) -> Result<()> {
    for digest in digests {
        // Digests are recorded relative to the artifacts root.
        let path = artifacts_dir.join(&digest.relative_path);
        let bytes = std::fs::read(&path).map_err(|error| StoreError::Legacy {
            reason: format!(
                "staged artifact {} disappeared before the switch: {error}",
                path.display()
            ),
        })?;
        let actual = hex::encode(Sha256::digest(&bytes));
        if actual != digest.sha256 {
            return Err(StoreError::Legacy {
                reason: format!(
                    "staged artifact {} changed after copying (expected sha256 {}, found \
                     {}); migration aborted and the original .relay/ is untouched",
                    digest.relative_path, digest.sha256, actual
                ),
            });
        }
    }
    Ok(())
}

fn list_segments(paths: &RelayPaths) -> Result<Vec<String>> {
    let listing = crate::segments::discover(&paths.events_dir)?;
    let mut names: Vec<String> = listing
        .closed
        .iter()
        .map(|segment| segment.file_name())
        .collect();
    if let Some(active) = listing.active {
        names.push(active.file_name());
    }
    Ok(names)
}

/// What preflight decided about the workspace.
enum Preflight {
    /// Continue with the migration.
    Proceed,
    /// The workspace already uses the canonical store; nothing to do.
    AlreadyMigrated(String),
}

/// Check the workspace state before migrating: already-migrated detection
/// and interrupted-switch completion (crash between the two renames of
/// step 9 — the staging store is complete and the original already moved).
///
/// `from_legacy` distinguishes the state.db path (a manifest without a
/// legacy database means already migrated) from the JSONL-import path (any
/// existing manifest is refused).
fn preflight(paths: &RelayPaths, from_legacy: bool) -> Result<Preflight> {
    if paths.root.exists() && paths.manifest.exists() {
        if from_legacy && !paths.legacy_state_db.exists() {
            return Ok(Preflight::AlreadyMigrated(format!(
                "this workspace already uses the canonical store at {}",
                paths.root.display()
            )));
        }
        return Err(StoreError::Legacy {
            reason: format!(
                "the workspace already contains a canonical store ({}). {}",
                paths.manifest.display(),
                if from_legacy {
                    "The project looks already migrated; remove or archive the leftover legacy \
                     database before migrating again"
                } else {
                    "Importing over an existing store is refused; roll back or move it first"
                }
            ),
        });
    }
    if !paths.root.exists() {
        // Maybe an interrupted switch: staging complete + backup present.
        if let Some(parent) = paths.root.parent() {
            let mut staging: Option<PathBuf> = None;
            let mut backups: Vec<PathBuf> = Vec::new();
            if let Ok(entries) = std::fs::read_dir(parent) {
                for entry in entries.filter_map(|e| e.ok()) {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if name.starts_with(".relay-rust-migration-") {
                        staging = Some(entry.path());
                    } else if name.starts_with(".relay-backup-") {
                        backups.push(entry.path());
                    }
                }
            }
            if let Some(staging) = staging
                && staging.join("manifest.json").exists()
            {
                if backups.is_empty() {
                    return Err(StoreError::Legacy {
                        reason: format!(
                            "an interrupted migration left {} but no .relay/ and no \
                                 backup; inspect the staging directory manually before \
                                 retrying",
                            staging.display()
                        ),
                    });
                }
                // Complete the interrupted switch: the original was
                // already moved to the backup by the previous run.
                let backup = backups.first().cloned().unwrap_or_default();
                std::fs::rename(&staging, &paths.root).map_err(|error| StoreError::Legacy {
                    reason: format!(
                        "completing an interrupted migration failed ({error}); the \
                                 backup is retained at {}",
                        backup.display()
                    ),
                })?;
                return Ok(Preflight::AlreadyMigrated(format!(
                    "an interrupted migration was completed: the staged store is now \
                         active at {} and the backup is retained at {}",
                    paths.root.display(),
                    backup.display()
                )));
            }
        }
    }
    Ok(Preflight::Proceed)
}

/// Refuse when a live Relay writer holds the lock (§10 step 1).
fn refuse_if_writer_active(paths: &RelayPaths) -> Result<()> {
    let lock_path = paths.writer_lock();
    if !lock_path.exists() {
        return Ok(());
    }
    if let Some(info) = lock::read_holder(&lock_path)
        && lock::process_alive(info.pid)
    {
        return Err(StoreError::Legacy {
            reason: format!(
                "a Relay writer (pid {}) is active in this workspace; stop it before \
                     migrating",
                info.pid
            ),
        });
    }
    Ok(())
}

/// Remove a previous staging directory (migration scratch, not user data)
/// so a retried migration starts clean.
fn reset_directory(staging: &Path) -> Result<()> {
    if staging.exists() {
        std::fs::remove_dir_all(staging).map_err(|error| StoreError::Legacy {
            reason: format!(
                "cannot remove the previous staging directory {}: {error}",
                staging.display()
            ),
        })?;
    }
    Ok(())
}
