//! Staging, validation, atomic switch, and rollback for the semantic
//! migration.
//!
//! The migration never touches the legacy source. It stages a complete new
//! fact store in a sibling directory (`.relay-graph-migration-staging`),
//! writes the derived facts through the envelope-internal append path,
//! copies the preserved artifact bytes into the store's blob layout, and
//! only then — after the staged stream folds cleanly through the domain
//! graph AND the fact-doctor reports no errors — switches by two atomic
//! renames with the previous store (if any) retained as a backup. Any
//! failure before the switch leaves the workspace exactly as it was;
//! [`rollback`] swaps the backup back on request.

use std::path::Path;
use std::path::PathBuf;

use relay_domain::DomainFact;

use crate::error::Result;
use crate::error::StoreError;
use crate::fact::domain::DomainFactValidator;
use crate::fact::domain::to_input;
use crate::fact::graph::GraphFold;
use crate::fact::paths::FactPaths;
use crate::fact::store::FactStore;
use crate::fact::store::FactStoreOptions;
use crate::migrate::extract::ExtractionReport;
use crate::migrate::legacy_source::LegacyEvidence;

/// Where the staged store is built (a sibling of `.relay-graph`).
pub const STAGING_DIR: &str = ".relay-graph-migration-staging";

/// What the migration did.
#[derive(Debug, Clone)]
pub struct SemanticMigrationOutcome {
    /// The structured extraction report.
    pub report: ExtractionReport,
    /// The new store root (`<workspace>/.relay-graph`).
    pub store_root: PathBuf,
    /// Where the previous fact store was retained, when one existed.
    pub previous_store_backup: Option<PathBuf>,
    /// How many facts were staged.
    pub facts: u64,
}

/// Options for the semantic migration.
#[derive(Clone)]
pub struct SemanticMigrationOptions {
    /// Store options for the staged store (clock/fault injection).
    pub store: FactStoreOptions,
    /// Integrity level of the new fact stream (default: checksums).
    pub integrity: crate::fact::record::Integrity,
}

impl Default for SemanticMigrationOptions {
    fn default() -> Self {
        SemanticMigrationOptions {
            store: FactStoreOptions::default(),
            integrity: crate::fact::record::Integrity::Checksums,
        }
    }
}

/// Stage the extracted facts into `staging_root`, validate them, and return
/// the staged paths. Does not perform the switch.
pub(crate) fn stage(
    staging_root: &Path,
    evidence: &LegacyEvidence,
    facts: &[DomainFact],
    options: &SemanticMigrationOptions,
) -> Result<FactPaths> {
    if staging_root.exists() {
        return Err(StoreError::Legacy {
            reason: format!(
                "a previous migration staging directory exists at {}. Remove it (it is \
                 disposable staging output) and retry",
                staging_root.display()
            ),
        });
    }
    let paths = FactPaths::from_root(staging_root);
    let store = FactStore::create_at(
        &paths,
        &evidence.project_id,
        options.integrity,
        options.store.clone(),
    )?;
    // The import batch goes through the envelope-internal append path (the
    // fold gate is validated separately below); the whole batch is one
    // durable write.
    let inputs = facts.iter().map(to_input).collect::<Result<Vec<_>>>()?;
    let outcome = store.append_facts(&inputs, &DomainFactValidator, &GraphFold)?;
    if outcome.projection_lagging {
        return Err(StoreError::Legacy {
            reason: "the staged projection lagged behind the staged facts; the migration \
                     refuses to switch on an inconsistent projection"
                .to_string(),
        });
    }

    // Copy the preserved legacy Markdown bytes into the blob layout the
    // fact store and doctor expect.
    copy_preserved_bytes(&paths, evidence, facts)?;

    // VALIDATION GATE 1: the staged stream must fold through the domain
    // graph, read back through the typed face at the explicit staging root.
    drop(store);
    let (typed, _report) =
        crate::fact::domain::GraphFactStore::open_at(staging_root, options.store.clone())?;
    let _ = typed.project_graph()?;
    drop(typed);

    // VALIDATION GATE 2: the fact doctor must report no errors on the
    // staged store.
    let findings = crate::fact::doctor::check_at(&paths, &options.store);
    if findings
        .iter()
        .any(|finding| finding.severity == crate::fact::doctor::FactSeverity::Error)
    {
        return Err(StoreError::Legacy {
            reason: format!(
                "the staged store failed doctor validation: {}",
                findings
                    .iter()
                    .filter(|f| f.severity == crate::fact::doctor::FactSeverity::Error)
                    .map(|f| f.detail.clone())
                    .collect::<Vec<_>>()
                    .join("; ")
            ),
        });
    }
    Ok(paths)
}

/// Copy the preserved legacy artifact bytes into
/// `<root>/artifacts/<artifact>/<version>-<digest16>.blob`, matching the
/// digests the staged `ArtifactVersionPublished` facts recorded.
fn copy_preserved_bytes(
    paths: &FactPaths,
    evidence: &LegacyEvidence,
    facts: &[DomainFact],
) -> Result<()> {
    use relay_domain::ArtifactVersionRef;
    // Every published version commits bytes to the blob store: preserved
    // legacy documents commit their original Markdown; derived documents
    // commit their canonical structured content.
    let published: Vec<(ArtifactVersionRef, String, Vec<u8>)> = facts
        .iter()
        .filter_map(|fact| match fact {
            DomainFact::ArtifactVersionPublished {
                artifact,
                version,
                digest,
                content,
                ..
            } => {
                let reference = ArtifactVersionRef {
                    artifact: artifact.clone(),
                    version: *version,
                };
                let digest = digest.as_str().to_string();
                let legacy = evidence
                    .artifacts
                    .values()
                    .find(|bytes| crate::migrate::extract::sha256_hex(bytes) == digest);
                let bytes: Option<Vec<u8>> = match legacy {
                    Some(bytes) => Some(bytes.clone()),
                    None => crate::canonical::Json::from_serde(&content.0)
                        .ok()
                        .and_then(|json| crate::fact::canonical::serialize_bytes(&json).ok()),
                };
                bytes.map(|bytes| (reference, digest, bytes))
            }
            _ => None,
        })
        .collect();
    for (reference, digest, bytes) in published {
        let prefix: String = digest.chars().take(16).collect();
        let dir = paths
            .root
            .join("artifacts")
            .join(reference.artifact.as_str());
        std::fs::create_dir_all(&dir).map_err(|error| StoreError::Io {
            context: "creating the staged artifact directory".to_string(),
            path: dir.clone(),
            source: error,
        })?;
        let blob = dir.join(format!("{}-{prefix}.blob", reference.version.get()));
        if blob.exists() {
            continue;
        }
        crate::manifest::write_and_sync(&blob, &bytes)?;
    }
    Ok(())
}

/// Perform the atomic switch: back up any existing `.relay-graph`, then
/// move the staged store into place. On failure, restores.
pub(crate) fn switch(
    workspace_root: &Path,
    staging_root: &Path,
    stamp: &str,
) -> Result<Option<PathBuf>> {
    let target = workspace_root.join(crate::fact::paths::FACT_STORE_DIR);
    let mut backup: Option<PathBuf> = None;
    if target.exists() {
        let backup_path = workspace_root.join(format!(
            "{}-pre-migration-{stamp}",
            crate::fact::paths::FACT_STORE_DIR
        ));
        std::fs::rename(&target, &backup_path).map_err(|error| StoreError::Legacy {
            reason: format!(
                "could not move the existing fact store aside at {}: {error}",
                target.display()
            ),
        })?;
        backup = Some(backup_path);
    }
    if let Err(error) = std::fs::rename(staging_root, &target) {
        if let Some(backup) = &backup
            && let Err(restore_error) = std::fs::rename(backup, &target)
        {
            return Err(StoreError::Legacy {
                reason: format!(
                    "could not move the staged store into place at {} ({error}), and the \
                     backup could not be restored ({restore_error}); the workspace needs \
                     manual attention",
                    target.display()
                ),
            });
        }
        return Err(StoreError::Legacy {
            reason: format!(
                "could not move the staged store into place at {}: {error}",
                target.display()
            ),
        });
    }
    Ok(backup)
}

/// Roll a completed migration back: move the current `.relay-graph` aside
/// (retained, never deleted) and restore the pre-migration backup.
pub fn rollback(workspace_root: &Path, backup_dir: &Path) -> Result<PathBuf> {
    if !backup_dir.is_dir() {
        return Err(StoreError::Legacy {
            reason: format!(
                "the migration backup directory {} does not exist",
                backup_dir.display()
            ),
        });
    }
    let current = workspace_root.join(crate::fact::paths::FACT_STORE_DIR);
    let stamp: String = crate::clock::Clock::now_iso(&crate::clock::SystemClock);
    let rolled_back = workspace_root.join(format!(
        "{}-rolled-back-{}",
        crate::fact::paths::FACT_STORE_DIR,
        stamp
            .chars()
            .filter(|c| *c != '-' && *c != ':' && *c != '.')
            .collect::<String>()
    ));
    if current.exists() {
        std::fs::rename(&current, &rolled_back).map_err(|error| StoreError::Legacy {
            reason: format!(
                "could not move the migrated store aside at {}: {error}",
                current.display()
            ),
        })?;
    }
    std::fs::rename(backup_dir, &current).map_err(|error| StoreError::Legacy {
        reason: format!(
            "could not restore the backup from {}: {error}",
            backup_dir.display()
        ),
    })?;
    Ok(rolled_back)
}
