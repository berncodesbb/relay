//! Store verification services (`doctor check` / `doctor rebuild-index`).
//!
//! Library-only: the CLI wires commands to these functions later (phase 2
//! item 8). `check` is strictly read-only: it validates the manifest, the
//! hash chain, segment tiling, artifact presence, and index consistency,
//! and returns findings without repairing anything. `rebuild_index`
//! quarantines the existing index and rebuilds it purely from JSONL.

use std::path::Path;

use crate::error::Result;
use crate::error::StoreError;
use crate::log::OpenMode;
use crate::paths::RelayPaths;
use crate::projection::SqliteProjection;
use crate::store::RebuildReport;
use crate::store::RelayStore;
use crate::store::StoreOptions;

/// Severity of a finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// The check passed.
    Ok,
    /// Something worth reporting that does not block use.
    Warning,
    /// The store cannot be trusted until addressed.
    Error,
}

/// One doctor finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorFinding {
    /// Severity.
    pub severity: Severity,
    /// Stable machine-readable code, e.g. `chain-ok`, `artifact-missing`.
    pub code: String,
    /// Human-readable detail.
    pub detail: String,
}

impl DoctorFinding {
    fn ok(code: &str, detail: impl Into<String>) -> DoctorFinding {
        DoctorFinding {
            severity: Severity::Ok,
            code: code.to_string(),
            detail: detail.into(),
        }
    }

    fn warning(code: &str, detail: impl Into<String>) -> DoctorFinding {
        DoctorFinding {
            severity: Severity::Warning,
            code: code.to_string(),
            detail: detail.into(),
        }
    }

    fn error(code: &str, detail: impl Into<String>) -> DoctorFinding {
        DoctorFinding {
            severity: Severity::Error,
            code: code.to_string(),
            detail: detail.into(),
        }
    }
}

/// Read-only validation of a workspace's store (§7: `doctor check`
/// re-validates everything read-only and reports findings).
pub fn check(workspace_root: &Path, options: &StoreOptions) -> Vec<DoctorFinding> {
    let mut findings: Vec<DoctorFinding> = Vec::new();
    let paths = RelayPaths::new(workspace_root);

    // Manifest.
    match crate::manifest::Manifest::read(&paths.manifest) {
        Ok(manifest) => findings.push(DoctorFinding::ok(
            "manifest-ok",
            format!(
                "store version {}, project {}, active segment {}",
                manifest.store_version, manifest.project_id, manifest.active_segment
            ),
        )),
        Err(error) => {
            findings.push(DoctorFinding::error("manifest-invalid", error.to_string()));
            return findings;
        }
    }

    // Segments and chain.
    let records = match crate::log::EventLog::open(&paths, OpenMode::Reader, options.clock.as_ref())
    {
        Ok((log, _)) => {
            let read = log.read_all();
            for unknown in log.unknown_files() {
                findings.push(DoctorFinding::warning(
                    "events-unknown-file",
                    format!("unexpected file in events/: {unknown} (reported, not blocking)"),
                ));
            }
            match read {
                Ok(records) => {
                    findings.push(DoctorFinding::ok(
                        "chain-ok",
                        format!(
                            "{} events validate; active segment {}; tail sequence {}",
                            records.len(),
                            log.manifest().active_segment,
                            log.tail().last_sequence
                        ),
                    ));
                    records
                }
                Err(error) => {
                    findings.push(DoctorFinding::error("chain-invalid", error.to_string()));
                    Vec::new()
                }
            }
        }
        Err(error) => {
            findings.push(DoctorFinding::error("chain-invalid", error.to_string()));
            Vec::new()
        }
    };

    // Artifact presence (referenced by events).
    let mut referenced: Vec<String> = Vec::new();
    for record in &records {
        let payload = &record.envelope.payload;
        if (record.envelope.event_type == "artifact.created"
            || record.envelope.event_type == "artifact.revised")
            && let Some(path) = payload.get("path").and_then(crate::canonical::Json::as_str)
        {
            referenced.push(path.to_string());
        }
    }
    let mut missing = 0usize;
    for relative in &referenced {
        let absolute = workspace_root.join(relative);
        if !absolute.exists() {
            missing += 1;
            findings.push(DoctorFinding::error(
                "artifact-missing",
                format!("artifact file referenced by an event does not exist: {relative}"),
            ));
        }
    }
    if !referenced.is_empty() && missing == 0 {
        findings.push(DoctorFinding::ok(
            "artifacts-ok",
            format!("all {} referenced artifact files exist", referenced.len()),
        ));
    }

    // Index consistency (read-only open; a WAL index can need recovery,
    // which a read-only probe cannot perform — reported as a warning).
    let tail = records
        .last()
        .map(|record| (record.envelope.sequence, record.envelope.hash.clone()));
    if paths.index_db.exists() {
        match read_index_checkpoint(&paths.index_db) {
            Ok(Some(checkpoint)) => match tail {
                Some((sequence, hash)) if checkpoint.last_sequence == sequence => {
                    if checkpoint.last_hash == hash {
                        findings.push(DoctorFinding::ok(
                            "index-ok",
                            format!(
                                "index checkpoint matches the JSONL tail at sequence {sequence}"
                            ),
                        ));
                    } else {
                        findings.push(DoctorFinding::error(
                            "index-divergent",
                            format!(
                                "index checkpoint hash {} differs from the JSONL tail {} at \
                                 sequence {sequence}; run `relay doctor rebuild-index`",
                                checkpoint.last_hash, hash
                            ),
                        ));
                    }
                }
                Some((sequence, _)) => findings.push(DoctorFinding::error(
                    "index-divergent",
                    format!(
                        "index checkpoint is at sequence {}, the JSONL tail is at {sequence}; \
                         reopening replays or rebuilds the suffix",
                        checkpoint.last_sequence
                    ),
                )),
                None => findings.push(DoctorFinding::warning(
                    "index-empty-log",
                    "the index holds a checkpoint but the JSONL log is empty".to_string(),
                )),
            },
            Ok(None) => findings.push(DoctorFinding::warning(
                "index-no-checkpoint",
                "the index exists but has no checkpoint row; it will be rebuilt on next open",
            )),
            Err(error) => findings.push(DoctorFinding::warning(
                "index-unreadable",
                format!(
                    "the index could not be read read-only ({error}); it will be quarantined \
                     and rebuilt on next open"
                ),
            )),
        }
    } else {
        findings.push(DoctorFinding::warning(
            "index-missing",
            "index.sqlite does not exist; it will be rebuilt from JSONL on next open",
        ));
    }

    // Recovery leftovers.
    if paths.recovery_dir.exists() {
        let quarantined: Vec<String> = std::fs::read_dir(&paths.recovery_dir)
            .map(|entries| {
                entries
                    .filter_map(|entry| entry.ok())
                    .map(|entry| entry.file_name().to_string_lossy().to_string())
                    .collect()
            })
            .unwrap_or_default();
        if !quarantined.is_empty() {
            findings.push(DoctorFinding::warning(
                "recovery-present",
                format!(
                    "recovery/ holds quarantined data: {}. Quarantine files are retained for \
                     manual review",
                    quarantined.join(", ")
                ),
            ));
        }
    }
    findings
}

/// Read the checkpoint of an index database read-only, without modifying it.
fn read_index_checkpoint(path: &Path) -> Result<Option<crate::projection::Checkpoint>, StoreError> {
    use rusqlite::OptionalExtension;
    let conn =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let exists: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'checkpoint';",
            [],
            |row| row.get::<_, i64>(0).map(|count| count > 0),
        )
        .unwrap_or(false);
    if !exists {
        return Ok(None);
    }
    conn.query_row(
        "SELECT last_sequence, last_hash FROM checkpoint WHERE id = 1;",
        [],
        |row| {
            Ok(crate::projection::Checkpoint {
                last_sequence: row.get::<_, i64>(0)? as u64,
                last_hash: row.get(1)?,
            })
        },
    )
    .optional()
    .map_err(|error| StoreError::Projection {
        reason: "reading the index checkpoint read-only".to_string(),
        source: Some(error),
    })
}

/// Quarantine the index and rebuild it purely from JSONL (§7: `doctor
/// rebuild-index`). Never touches JSONL or artifacts.
pub fn rebuild_index(workspace_root: &Path, options: StoreOptions) -> Result<RebuildReport> {
    let (store, _report) = RelayStore::open(workspace_root, options)?;
    store.rebuild_index()
}

/// Convenience: open the projection of an existing store for inspection.
pub fn open_projection(paths: &RelayPaths, options: &StoreOptions) -> Result<SqliteProjection> {
    SqliteProjection::open(&paths.index_db, &paths.recovery_dir, options.clock.as_ref())
}
