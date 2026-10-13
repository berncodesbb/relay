//! Store manifest (`.relay/manifest.json`, `docs/event-format.md` §5).
//!
//! The manifest holds store metadata only — never workflow state. It is
//! rewritten atomically (write temp + rename) whenever the active segment
//! changes. Readers fail closed with upgrade guidance on a newer
//! `storeVersion`.

use std::path::Path;

use crate::canonical::Json;
use crate::clock::parse_iso_to_epoch_millis;
use crate::error::Result;
use crate::error::StoreError;
use crate::segments::SegmentName;

/// The JSONL/container format version this build writes and understands.
pub const STORE_VERSION: u64 = 1;

/// The minimum Relay version able to read a store written by this build.
pub const MIN_READER_VERSION: &str = "0.2.0";

/// Store metadata (§5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    /// Container format version (currently `1`).
    pub store_version: u64,
    /// `prj_<uuid>` of the project this log belongs to.
    pub project_id: String,
    /// Store creation timestamp.
    pub created_at: String,
    /// Active segment file name.
    pub active_segment: String,
    /// Minimum Relay version able to read this store.
    pub min_reader_version: String,
}

impl Manifest {
    /// Build a manifest for a fresh store.
    pub fn new(project_id: &str, created_at: String, active_segment: String) -> Manifest {
        Manifest {
            store_version: STORE_VERSION,
            project_id: project_id.to_string(),
            created_at,
            active_segment,
            min_reader_version: MIN_READER_VERSION.to_string(),
        }
    }

    /// Serialize to canonical JSON bytes (one line, no trailing newline).
    pub fn to_json(&self) -> Json {
        Json::object([
            ("storeVersion", Json::UInt(self.store_version)),
            ("projectId", Json::Str(self.project_id.clone())),
            ("createdAt", Json::Str(self.created_at.clone())),
            ("activeSegment", Json::Str(self.active_segment.clone())),
            (
                "minReaderVersion",
                Json::Str(self.min_reader_version.clone()),
            ),
        ])
    }

    /// Parse and validate a manifest.
    pub fn from_json(value: &Json) -> Result<Manifest> {
        let store_version = value
            .get("storeVersion")
            .and_then(Json::as_u64)
            .ok_or_else(|| StoreError::Canonical {
                reason: "manifest field \"storeVersion\" must be a non-negative integer"
                    .to_string(),
            })?;
        if store_version > STORE_VERSION {
            return Err(StoreError::UnsupportedStore {
                found: store_version,
                supported: STORE_VERSION,
                location: "manifest storeVersion".to_string(),
            });
        }
        if store_version == 0 {
            return Err(StoreError::Canonical {
                reason: "manifest \"storeVersion\" must be at least 1".to_string(),
            });
        }
        let project_id = value
            .get("projectId")
            .and_then(Json::as_str)
            .ok_or_else(|| StoreError::Canonical {
                reason: "manifest field \"projectId\" must be a string".to_string(),
            })?
            .to_string();
        relay_core::ids::ProjectId::parse(&project_id).map_err(|error| StoreError::Canonical {
            reason: format!("manifest \"projectId\": {error}"),
        })?;
        let created_at = value
            .get("createdAt")
            .and_then(Json::as_str)
            .ok_or_else(|| StoreError::Canonical {
                reason: "manifest field \"createdAt\" must be a string".to_string(),
            })?
            .to_string();
        if parse_iso_to_epoch_millis(&created_at).is_none() {
            return Err(StoreError::Canonical {
                reason: format!(
                    "manifest \"createdAt\" \"{created_at}\" must be \
                     YYYY-MM-DDTHH:MM:SS.sssZ"
                ),
            });
        }
        let active_segment = value
            .get("activeSegment")
            .and_then(Json::as_str)
            .ok_or_else(|| StoreError::Canonical {
                reason: "manifest field \"activeSegment\" must be a string".to_string(),
            })?
            .to_string();
        match SegmentName::parse(&active_segment) {
            Some(SegmentName::Open { .. }) => {}
            _ => {
                return Err(StoreError::Canonical {
                    reason: format!(
                        "manifest \"activeSegment\" \"{active_segment}\" must name the active \
                         (<first>-open.jsonl) segment"
                    ),
                });
            }
        }
        let min_reader_version = value
            .get("minReaderVersion")
            .and_then(Json::as_str)
            .ok_or_else(|| StoreError::Canonical {
                reason: "manifest field \"minReaderVersion\" must be a string".to_string(),
            })?
            .to_string();
        Ok(Manifest {
            store_version,
            project_id,
            created_at,
            active_segment,
            min_reader_version,
        })
    }

    /// Read and validate the manifest at `path`.
    pub fn read(path: &Path) -> Result<Manifest> {
        let text = std::fs::read_to_string(path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                StoreError::Manifest {
                    path: path.to_path_buf(),
                    reason: "manifest not found. Either this workspace has no Relay project (run \
                         the project initialization, or migrate a legacy .relay/state.db), or \
                         the .relay directory is incomplete."
                        .to_string(),
                }
            } else {
                StoreError::Manifest {
                    path: path.to_path_buf(),
                    reason: error.to_string(),
                }
            }
        })?;
        let value = Json::parse(&text).map_err(|error| StoreError::Manifest {
            path: path.to_path_buf(),
            reason: error.to_string(),
        })?;
        Manifest::from_json(&value).map_err(|error| StoreError::Manifest {
            path: path.to_path_buf(),
            reason: error.to_string(),
        })
    }

    /// Write the manifest atomically: a temp file in the same directory,
    /// fsync, rename over the destination, best-effort directory fsync
    /// (§5).
    pub fn write_atomic(&self, path: &Path) -> Result<()> {
        let bytes = self.to_json().to_canonical_bytes();
        let parent = path.parent().ok_or_else(|| StoreError::Invalid {
            reason: format!("manifest path {} has no parent directory", path.display()),
        })?;
        std::fs::create_dir_all(parent).map_err(|error| StoreError::Io {
            context: "creating manifest directory".to_string(),
            path: parent.to_path_buf(),
            source: error,
        })?;
        let temp = parent.join(".manifest.json.tmp");
        write_and_sync(&temp, &bytes)?;
        std::fs::rename(&temp, path).map_err(|error| StoreError::Io {
            context: "renaming manifest into place".to_string(),
            path: path.to_path_buf(),
            source: error,
        })?;
        fsync_dir_best_effort(parent);
        Ok(())
    }
}

/// Write `bytes` to `path` (truncating), flush, and fsync. Used for
/// manifests, locks, and recovery artifacts that must survive a crash.
pub(crate) fn write_and_sync(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut file = std::fs::File::create(path).map_err(|error| StoreError::Io {
        context: "creating file".to_string(),
        path: path.to_path_buf(),
        source: error,
    })?;
    file.write_all(bytes).map_err(|error| StoreError::Io {
        context: "writing file".to_string(),
        path: path.to_path_buf(),
        source: error,
    })?;
    file.flush().map_err(|error| StoreError::Io {
        context: "flushing file".to_string(),
        path: path.to_path_buf(),
        source: error,
    })?;
    file.sync_all().map_err(|error| StoreError::Io {
        context: "fsyncing file".to_string(),
        path: path.to_path_buf(),
        source: error,
    })?;
    Ok(())
}

/// Best-effort fsync of a directory (§6 step 7): a crash-durability aid on
/// Unix; a no-op elsewhere. Failures are ignored by design.
pub(crate) fn fsync_dir_best_effort(dir: &Path) {
    #[cfg(unix)]
    {
        if let Ok(handle) = std::fs::File::open(dir) {
            let _ = handle.sync_all();
        }
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_roundtrip_and_validation() {
        let manifest = Manifest::new(
            "prj_00000000-0000-4000-8000-000000000001",
            "2026-10-13T08:00:00.000Z".to_string(),
            "0000000000000001-open.jsonl".to_string(),
        );
        let json = manifest.to_json();
        let parsed = Manifest::from_json(&json).ok();
        assert_eq!(parsed, Some(manifest.clone()));

        // Newer storeVersion fails closed with upgrade guidance.
        let mut newer = manifest.clone();
        newer.store_version = 2;
        let error = Manifest::from_json(&newer.to_json()).err();
        assert!(
            error
                .as_ref()
                .is_some_and(|e| e.to_string().contains("Upgrade Relay")),
            "{error:?}"
        );

        // Non-open active segment names are rejected.
        let mut closed = manifest.clone();
        closed.active_segment = "0000000000000001-0000000000000002.jsonl".to_string();
        assert!(Manifest::from_json(&closed.to_json()).is_err());
    }

    #[test]
    fn atomic_write_replaces_content() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("manifest.json");
        let manifest = Manifest::new(
            "prj_00000000-0000-4000-8000-000000000001",
            "2026-10-13T08:00:00.000Z".to_string(),
            "0000000000000001-open.jsonl".to_string(),
        );
        manifest.write_atomic(&path)?;
        manifest.write_atomic(&path)?;
        let read_back = Manifest::read(&path)?;
        assert_eq!(read_back, manifest);
        // No temp file is left behind.
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())?
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(leftovers, vec!["manifest.json".to_string()]);
        Ok(())
    }
}
