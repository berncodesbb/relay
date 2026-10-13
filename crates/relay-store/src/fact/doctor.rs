//! Fact-store verification services (library-only `doctor` surface for the
//! intent-first store; the CLI is owned by the interface engineer).
//!
//! [`check`] is strictly read-only: it validates the manifest, the fact
//! stream (envelope shape, checksums or chain per the manifest's integrity
//! level, sequence contiguity, domain acceptance), segment tiling, orphan
//! artifact files, and projection consistency, and returns findings without
//! repairing anything. [`rebuild_projection`] quarantines the existing
//! projection database and rebuilds it purely from the JSONL fact log.

use std::path::Path;

use crate::error::Result;
use crate::error::StoreError;
use crate::fact::domain::DomainFactValidator;
use crate::fact::domain::from_record;
use crate::fact::graph::GraphFold;
use crate::fact::paths::FactPaths;
use crate::fact::store::FactRebuildReport;
use crate::fact::store::FactStore;
use crate::fact::store::FactStoreOptions;
use crate::log::OpenMode;

/// Severity of a finding (mirrors the legacy doctor's vocabulary).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FactSeverity {
    /// The check passed.
    Ok,
    /// Something worth reporting that does not block use.
    Warning,
    /// The store cannot be trusted until addressed.
    Error,
}

/// One fact-store doctor finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactFinding {
    /// Severity.
    pub severity: FactSeverity,
    /// Stable machine-readable code, e.g. `facts-ok`, `artifact-blob-missing`.
    pub code: String,
    /// Human-readable detail.
    pub detail: String,
}

impl FactFinding {
    fn ok(code: &str, detail: impl Into<String>) -> FactFinding {
        FactFinding {
            severity: FactSeverity::Ok,
            code: code.to_string(),
            detail: detail.into(),
        }
    }

    fn warning(code: &str, detail: impl Into<String>) -> FactFinding {
        FactFinding {
            severity: FactSeverity::Warning,
            code: code.to_string(),
            detail: detail.into(),
        }
    }

    fn error(code: &str, detail: impl Into<String>) -> FactFinding {
        FactFinding {
            severity: FactSeverity::Error,
            code: code.to_string(),
            detail: detail.into(),
        }
    }
}

/// Read-only validation of a workspace's fact store.
///
/// Checks: manifest readability and version, segment tiling, full stream
/// validation (checksums — and chain links when the stream records
/// `integrity: "chained"` — plus domain acceptance of every record),
/// presence of the artifact blobs referenced by published artifact-version
/// digests, projection checkpoint consistency, and recovery leftovers.
/// Never repairs anything.
pub fn check(workspace_root: &Path, options: &FactStoreOptions) -> Vec<FactFinding> {
    check_at(&FactPaths::new(workspace_root), options)
}

/// Read-only validation of a fact store at an explicit root (staging
/// layouts); see [`check`].
pub fn check_at(paths: &FactPaths, options: &FactStoreOptions) -> Vec<FactFinding> {
    // Blob references resolve against the store root itself, so staging
    // layouts check the same way as workspaces.
    let workspace_root = paths.root.clone();
    let workspace_root = workspace_root.as_path();
    let mut findings: Vec<FactFinding> = Vec::new();

    // Manifest.
    let manifest = match crate::fact::manifest::FactManifest::read(&paths.manifest) {
        Ok(manifest) => {
            findings.push(FactFinding::ok(
                "manifest-ok",
                format!(
                    "fact store version {}, stream {}, integrity {}, active segment {}",
                    manifest.store_version,
                    manifest.stream_id,
                    manifest.integrity.as_str(),
                    manifest.active_segment
                ),
            ));
            manifest
        }
        Err(error) => {
            findings.push(FactFinding::error("manifest-invalid", error.to_string()));
            return findings;
        }
    };

    // Fact stream (read-only open validates envelopes, checksums/chain,
    // contiguity, and domain acceptance).
    let entries = match crate::fact::log::FactLog::open(
        paths,
        OpenMode::Reader,
        options.clock.as_ref(),
        &DomainFactValidator,
    ) {
        Ok((log, _)) => {
            for unknown in log.unknown_files() {
                findings.push(FactFinding::warning(
                    "facts-unknown-file",
                    format!("unexpected file in facts/: {unknown} (reported, not blocking)"),
                ));
            }
            match log.read_all(&DomainFactValidator) {
                Ok(entries) => {
                    let integrity = if manifest.integrity == crate::fact::record::Integrity::Chained
                    {
                        "checksums + chain"
                    } else {
                        "checksums"
                    };
                    findings.push(FactFinding::ok(
                        "facts-ok",
                        format!(
                            "{} facts validate ({integrity}); tail sequence {}",
                            entries.len(),
                            log.tail().last_sequence
                        ),
                    ));
                    entries
                }
                Err(error) => {
                    findings.push(FactFinding::error("facts-invalid", error.to_string()));
                    Vec::new()
                }
            }
        }
        Err(error) => {
            findings.push(FactFinding::error("facts-invalid", error.to_string()));
            Vec::new()
        }
    };

    // Orphan artifacts: published artifact-version digests reference blob
    // files under <root>/artifacts/<artifact>/<version>-<digest prefix>.
    // Doctor checks that referenced blobs exist and report files that no
    // published version references.
    let mut referenced: Vec<String> = Vec::new();
    for entry in &entries {
        if let Ok(relay_domain::DomainFact::ArtifactVersionPublished {
            artifact,
            version,
            digest,
            ..
        }) = from_record(&entry.record)
            && let Some(blob) = artifact_blob_path(
                workspace_root,
                artifact.as_str(),
                version.get(),
                digest.as_str(),
            )
        {
            referenced.push(blob);
        }
    }
    let mut missing = 0usize;
    for relative in &referenced {
        if !workspace_root.join(relative).exists() {
            missing += 1;
            findings.push(FactFinding::error(
                "artifact-blob-missing",
                format!("published artifact blob does not exist: {relative}"),
            ));
        }
    }
    if !referenced.is_empty() && missing == 0 {
        findings.push(FactFinding::ok(
            "artifact-blobs-ok",
            format!("all {} referenced artifact blobs exist", referenced.len()),
        ));
    }
    if let Some(orphans) = orphan_artifact_blobs(workspace_root, &referenced)
        && !orphans.is_empty()
    {
        findings.push(FactFinding::warning(
            "artifact-blobs-orphan",
            format!(
                "{} artifact blob(s) no published version references: {}. Orphans are \
                 reported, never deleted",
                orphans.len(),
                orphans.join(", ")
            ),
        ));
    }

    // Projection consistency (read-only probe; a WAL database can need
    // recovery a read-only probe cannot perform — reported as a warning).
    let tail = entries
        .last()
        .map(|entry| (entry.record.sequence, entry.record.checksum.clone()));
    if paths.projection_db.exists() {
        match read_projection_checkpoint(&paths.projection_db) {
            Ok(Some(checkpoint)) => match tail {
                Some((sequence, checksum)) if checkpoint.last_sequence == sequence => {
                    if checkpoint.last_checksum == checksum {
                        findings.push(FactFinding::ok(
                            "projection-ok",
                            format!(
                                "projection checkpoint matches the fact tail at sequence {sequence}"
                            ),
                        ));
                    } else {
                        findings.push(FactFinding::error(
                            "projection-divergent",
                            format!(
                                "projection checkpoint checksum {} differs from the fact tail {} \
                                 at sequence {sequence}; reopening replays or rebuilds the \
                                 projection",
                                checkpoint.last_checksum, checksum
                            ),
                        ));
                    }
                }
                Some((sequence, _)) => findings.push(FactFinding::error(
                    "projection-divergent",
                    format!(
                        "projection checkpoint is at sequence {}, the fact tail is at {sequence}; \
                         reopening replays the suffix or rebuilds",
                        checkpoint.last_sequence
                    ),
                )),
                None => findings.push(FactFinding::warning(
                    "projection-empty-log",
                    "the projection holds a checkpoint but the fact log is empty".to_string(),
                )),
            },
            Ok(None) => findings.push(FactFinding::warning(
                "projection-no-checkpoint",
                "the projection exists but has no checkpoint row; it will be rebuilt on next open",
            )),
            Err(error) => findings.push(FactFinding::warning(
                "projection-unreadable",
                format!(
                    "the projection could not be read read-only ({error}); it will be \
                     quarantined and rebuilt on next open"
                ),
            )),
        }
    } else {
        findings.push(FactFinding::warning(
            "projection-missing",
            "projection.sqlite does not exist; it will be rebuilt from the fact log on next open",
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
            findings.push(FactFinding::warning(
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

/// Quarantine the projection and rebuild it purely from the fact log.
/// Never touches the JSONL log or artifact blobs.
pub fn rebuild_projection(
    workspace_root: &Path,
    options: FactStoreOptions,
) -> Result<FactRebuildReport> {
    let (store, _report) =
        FactStore::open(workspace_root, options, &DomainFactValidator, &GraphFold)?;
    store.rebuild_projection(&DomainFactValidator, &GraphFold)
}

/// The blob path a published version is expected to occupy. Storage owns
/// the artifact blob layout (the domain owns content schemas):
/// `<workspace>/.relay-graph/artifacts/<artifact>/<version>-<digest16>.blob`
/// — the no-overwrite publication primitive in `relay-artifacts::publish_file`
/// writes exactly there. Returns `None` for digests too short to prefix
/// (defensive; the domain already validates 16+ hex characters).
fn artifact_blob_path(
    _store_root: &Path,
    artifact: &str,
    version: u32,
    digest: &str,
) -> Option<String> {
    let prefix: String = digest.chars().take(16).collect();
    if prefix.is_empty() {
        return None;
    }
    Some(format!("artifacts/{artifact}/{version}-{prefix}.blob"))
}

/// Blob files present under the artifact store that nothing references.
fn orphan_artifact_blobs(store_root: &Path, referenced: &[String]) -> Option<Vec<String>> {
    let artifacts_dir = store_root.join("artifacts");
    let mut files: Vec<String> = Vec::new();
    let mut stack = vec![artifacts_dir];
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.filter_map(|entry| entry.ok()) {
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                stack.push(path);
            } else if file_type.is_file() {
                let stripped = path
                    .strip_prefix(store_root)
                    .map(|relative| relative.to_string_lossy().replace('\\', "/"))
                    .unwrap_or_default();
                files.push(stripped);
            }
        }
    }
    files.sort();
    Some(
        files
            .into_iter()
            .filter(|file| !referenced.contains(file))
            .collect(),
    )
}

/// Read the projection checkpoint read-only, without modifying anything.
fn read_projection_checkpoint(
    path: &Path,
) -> std::result::Result<Option<crate::fact::projection::FactCheckpoint>, StoreError> {
    use rusqlite::OptionalExtension;
    let conn =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let exists: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = \
             'fact_checkpoint';",
            [],
            |row| row.get::<_, i64>(0).map(|count| count > 0),
        )
        .unwrap_or(false);
    if !exists {
        return Ok(None);
    }
    conn.query_row(
        "SELECT last_sequence, last_checksum FROM fact_checkpoint WHERE id = 1;",
        [],
        |row| {
            Ok(crate::fact::projection::FactCheckpoint {
                last_sequence: row.get::<_, i64>(0)? as u64,
                last_checksum: row.get(1)?,
            })
        },
    )
    .optional()
    .map_err(|error| StoreError::Projection {
        reason: "reading the fact projection checkpoint read-only".to_string(),
        source: Some(error),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use relay_domain::ArtifactContent;
    use relay_domain::ArtifactDigest;
    use relay_domain::ArtifactId;
    use relay_domain::ArtifactVersion;
    use relay_domain::DomainFact;
    use relay_domain::ProjectId;

    const CLOCK_MS: i64 = 1_791_878_400_000;

    fn invalid(reason: String) -> StoreError {
        StoreError::Invalid { reason }
    }

    fn published(artifact: &ArtifactId, version: u32) -> Result<DomainFact> {
        Ok(DomainFact::ArtifactVersionPublished {
            artifact: artifact.clone(),
            version: ArtifactVersion::new(version).map_err(|e| invalid(e.to_string()))?,
            title: Some("Brief".to_string()),
            content: ArtifactContent::empty(),
            digest: ArtifactDigest::parse(&"ab".repeat(32)).map_err(|e| invalid(e.to_string()))?,
            derived_from: Vec::new(),
            published_at_ms: CLOCK_MS,
        })
    }

    #[test]
    fn check_reports_healthy_store_and_missing_blobs() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let workspace = dir.path();
        let project = ProjectId::parse("prj_demo").map_err(|e| invalid(e.to_string()))?;
        let artifact = ArtifactId::parse("art_brief").map_err(|e| invalid(e.to_string()))?;

        let store = crate::fact::domain::GraphFactStore::create(
            workspace,
            &project,
            crate::fact::record::Integrity::Checksums,
            FactStoreOptions::deterministic(CLOCK_MS),
        )?;
        store.append(&[
            DomainFact::ProjectCreated {
                project: project.clone(),
            },
            published(&artifact, 1)?,
        ])?;
        drop(store);

        // No blob on disk yet: doctor flags the missing reference.
        let findings = check(workspace, &FactStoreOptions::deterministic(CLOCK_MS));
        assert!(
            findings.iter().any(|f| f.code == "manifest-ok"),
            "{findings:?}"
        );
        assert!(
            findings.iter().any(|f| f.code == "facts-ok"),
            "{findings:?}"
        );
        assert!(
            findings.iter().any(|f| f.code == "artifact-blob-missing"),
            "{findings:?}"
        );

        // Write the expected blob; the finding clears.
        let blob_dir = workspace
            .join(".relay-graph")
            .join("artifacts")
            .join("art_brief");
        std::fs::create_dir_all(&blob_dir)?;
        std::fs::write(blob_dir.join("1-abababababababab.blob"), b"content")?;
        let findings = check(workspace, &FactStoreOptions::deterministic(CLOCK_MS));
        assert!(
            findings.iter().any(|f| f.code == "artifact-blobs-ok"),
            "{findings:?}"
        );
        assert!(
            !findings.iter().any(|f| f.code == "artifact-blob-missing"),
            "{findings:?}"
        );

        // An unreferenced blob is reported as an orphan, never deleted.
        std::fs::write(blob_dir.join("9-deadbeefdeadbeef.blob"), b"orphan")?;
        let findings = check(workspace, &FactStoreOptions::deterministic(CLOCK_MS));
        let orphan = findings
            .iter()
            .find(|f| f.code == "artifact-blobs-orphan")
            .ok_or("expected orphan finding")?;
        assert!(orphan.detail.contains("9-deadbeef"), "{}", orphan.detail);
        assert!(blob_dir.join("9-deadbeefdeadbeef.blob").exists());
        Ok(())
    }

    #[test]
    fn check_flags_corrupt_stream() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let workspace = dir.path();
        let project = ProjectId::parse("prj_demo").map_err(|e| invalid(e.to_string()))?;
        let store = crate::fact::domain::GraphFactStore::create(
            workspace,
            &project,
            crate::fact::record::Integrity::Checksums,
            FactStoreOptions::deterministic(CLOCK_MS),
        )?;
        store.append(&[DomainFact::ProjectCreated { project }])?;
        drop(store);

        // Tamper with the record bytes.
        let segment = workspace
            .join(".relay-graph")
            .join("facts")
            .join("0000000000000001-open.jsonl");
        let text = std::fs::read_to_string(&segment)?;
        std::fs::write(&segment, text.replace("prj_demo", "prj_evil"))?;

        let findings = check(workspace, &FactStoreOptions::deterministic(CLOCK_MS));
        assert!(
            findings.iter().any(|f| f.code == "facts-invalid"),
            "{findings:?}"
        );
        Ok(())
    }

    #[test]
    fn rebuild_projection_service_round_trips() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let workspace = dir.path();
        let project = ProjectId::parse("prj_demo").map_err(|e| invalid(e.to_string()))?;
        let artifact = ArtifactId::parse("art_brief").map_err(|e| invalid(e.to_string()))?;
        let store = crate::fact::domain::GraphFactStore::create(
            workspace,
            &project,
            crate::fact::record::Integrity::Checksums,
            FactStoreOptions::deterministic(CLOCK_MS),
        )?;
        store.append(&[
            DomainFact::ProjectCreated {
                project: project.clone(),
            },
            published(&artifact, 1)?,
        ])?;
        drop(store);

        let report = rebuild_projection(workspace, FactStoreOptions::deterministic(CLOCK_MS))?;
        assert_eq!(report.facts_indexed, 2);
        assert!(report.quarantine.is_some());
        let (reopened, _) = crate::fact::domain::GraphFactStore::open(
            workspace,
            FactStoreOptions::deterministic(CLOCK_MS),
        )?;
        let versions = reopened
            .projection(|conn| crate::fact::graph::artifact_versions(conn, Some(&artifact)))?;
        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].version, 1);
        Ok(())
    }
}
