//! Fact-store manifest (`<root>/manifest.json`,
//! `docs/plans/fact-format-proposal.md`).
//!
//! Store metadata only — never workflow state. The single format version
//! (`factStoreVersion`) governs both the container (segmentation, manifest
//! fields) and the record line format. Readers fail closed with upgrade
//! guidance on a newer version. The manifest is rewritten atomically
//! (write temp + fsync + rename) whenever the active segment changes.

use std::path::Path;

use crate::canonical::Json;
use crate::clock::parse_iso_to_epoch_millis;
use crate::error::Result;
use crate::error::StoreError;
use crate::fact::canonical;
use crate::fact::record::Integrity;
use crate::manifest::fsync_dir_best_effort;
use crate::manifest::write_and_sync;
use crate::segments::SegmentName;

/// The fact-format version this build writes and understands.
pub const FACT_STORE_VERSION: u64 = 1;

/// The minimum Relay version able to read a fact store written by this build.
pub const FACT_MIN_READER_VERSION: &str = "0.2.0";

/// Fact-store metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactManifest {
    /// Container + record format version (currently `1`).
    pub store_version: u64,
    /// Opaque domain identifier of the stream this log records (e.g. the
    /// project id). Storage never parses it; it must be a non-empty string
    /// without control characters.
    pub stream_id: String,
    /// Integrity level the stream was created with.
    pub integrity: Integrity,
    /// Store creation timestamp.
    pub created_at: String,
    /// Active segment file name.
    pub active_segment: String,
    /// Minimum Relay version able to read this store.
    pub min_reader_version: String,
}

impl FactManifest {
    /// Build a manifest for a fresh fact store.
    pub fn new(
        stream_id: &str,
        integrity: Integrity,
        created_at: String,
        active_segment: String,
    ) -> Result<FactManifest> {
        validate_stream_id(stream_id)?;
        validate_timestamp("createdAt", &created_at)?;
        Ok(FactManifest {
            store_version: FACT_STORE_VERSION,
            stream_id: stream_id.to_string(),
            integrity,
            created_at,
            active_segment,
            min_reader_version: FACT_MIN_READER_VERSION.to_string(),
        })
    }

    /// Serialize to canonical fact JSON.
    pub fn to_json(&self) -> Result<Json> {
        Ok(Json::object([
            ("activeSegment", Json::Str(self.active_segment.clone())),
            ("createdAt", Json::Str(self.created_at.clone())),
            ("integrity", Json::Str(self.integrity.as_str().to_string())),
            (
                "minReaderVersion",
                Json::Str(self.min_reader_version.clone()),
            ),
            ("streamId", Json::Str(self.stream_id.clone())),
            ("storeVersion", Json::UInt(self.store_version)),
        ]))
    }

    /// Parse and validate a manifest. Unknown additive fields are ignored.
    pub fn from_json(value: &Json) -> Result<FactManifest> {
        let store_version = value
            .get("storeVersion")
            .and_then(Json::as_u64)
            .ok_or_else(|| StoreError::Manifest {
                path: Default::default(),
                reason: "field \"storeVersion\" must be a non-negative integer".to_string(),
            })?;
        if store_version > FACT_STORE_VERSION {
            return Err(StoreError::UnsupportedStore {
                found: store_version,
                supported: FACT_STORE_VERSION,
                location: "fact manifest storeVersion".to_string(),
            });
        }
        if store_version == 0 {
            return Err(StoreError::Manifest {
                path: Default::default(),
                reason: "field \"storeVersion\" must be at least 1".to_string(),
            });
        }
        let stream_id = value
            .get("streamId")
            .and_then(Json::as_str)
            .ok_or_else(|| StoreError::Manifest {
                path: Default::default(),
                reason: "field \"streamId\" must be a string".to_string(),
            })?
            .to_string();
        validate_stream_id(&stream_id).map_err(|error| match error {
            StoreError::Invalid { reason } => StoreError::Manifest {
                path: Default::default(),
                reason,
            },
            other => other,
        })?;
        let integrity = value
            .get("integrity")
            .and_then(Json::as_str)
            .and_then(Integrity::parse)
            .ok_or_else(|| StoreError::Manifest {
                path: Default::default(),
                reason: "field \"integrity\" must be \"checksums\" or \"chained\"".to_string(),
            })?;
        let created_at = value
            .get("createdAt")
            .and_then(Json::as_str)
            .ok_or_else(|| StoreError::Manifest {
                path: Default::default(),
                reason: "field \"createdAt\" must be a string".to_string(),
            })?
            .to_string();
        validate_timestamp("createdAt", &created_at)?;
        let active_segment = value
            .get("activeSegment")
            .and_then(Json::as_str)
            .ok_or_else(|| StoreError::Manifest {
                path: Default::default(),
                reason: "field \"activeSegment\" must be a string".to_string(),
            })?
            .to_string();
        match SegmentName::parse(&active_segment) {
            Some(SegmentName::Open { .. }) => {}
            _ => {
                return Err(StoreError::Manifest {
                    path: Default::default(),
                    reason: format!(
                        "field \"activeSegment\" \"{active_segment}\" must name the active \
                         (<first>-open.jsonl) segment"
                    ),
                });
            }
        }
        let min_reader_version = value
            .get("minReaderVersion")
            .and_then(Json::as_str)
            .ok_or_else(|| StoreError::Manifest {
                path: Default::default(),
                reason: "field \"minReaderVersion\" must be a string".to_string(),
            })?
            .to_string();
        Ok(FactManifest {
            store_version,
            stream_id,
            integrity,
            created_at,
            active_segment,
            min_reader_version,
        })
    }

    /// Read and validate the manifest at `path`.
    pub fn read(path: &Path) -> Result<FactManifest> {
        let text = std::fs::read_to_string(path).map_err(|error| StoreError::Manifest {
            path: path.to_path_buf(),
            reason: if error.kind() == std::io::ErrorKind::NotFound {
                "fact manifest not found: either this workspace has no fact store (initialize \
                     the project) or the store directory is incomplete"
                    .to_string()
            } else {
                error.to_string()
            },
        })?;
        let value = canonical::parse(&text).map_err(|error| StoreError::Manifest {
            path: path.to_path_buf(),
            reason: error.to_string(),
        })?;
        // `from_json` reports validation problems with an empty path; attach
        // the real one here.
        FactManifest::from_json(&value).map_err(|error| match error {
            StoreError::Manifest { path, reason } if path.as_os_str().is_empty() => {
                StoreError::Manifest {
                    path: path.to_path_buf(),
                    reason,
                }
            }
            other => other,
        })
    }

    /// Write the manifest atomically: temp file in the same directory,
    /// fsync, rename over the destination, best-effort directory fsync.
    pub fn write_atomic(&self, path: &Path) -> Result<()> {
        let bytes = canonical::serialize_bytes(&self.to_json()?)?;
        let parent = path.parent().ok_or_else(|| StoreError::Invalid {
            reason: format!("manifest path {} has no parent directory", path.display()),
        })?;
        std::fs::create_dir_all(parent).map_err(|error| StoreError::Io {
            context: "creating the fact manifest directory".to_string(),
            path: parent.to_path_buf(),
            source: error,
        })?;
        let temp = parent.join(".manifest.json.tmp");
        write_and_sync(&temp, &bytes)?;
        std::fs::rename(&temp, path).map_err(|error| StoreError::Io {
            context: "renaming the fact manifest into place".to_string(),
            path: path.to_path_buf(),
            source: error,
        })?;
        fsync_dir_best_effort(parent);
        Ok(())
    }
}

fn validate_stream_id(stream_id: &str) -> Result<()> {
    let char_count = stream_id.chars().count();
    if stream_id.is_empty() || char_count > 200 {
        return Err(StoreError::Invalid {
            reason: format!("streamId must be 1..200 characters (got {char_count})"),
        });
    }
    if stream_id
        .chars()
        .any(|c| c.is_control() || c == '\n' || c == '\r')
    {
        return Err(StoreError::Invalid {
            reason: "streamId must not contain control characters".to_string(),
        });
    }
    Ok(())
}

fn validate_timestamp(field: &str, text: &str) -> Result<()> {
    if parse_iso_to_epoch_millis(text).is_none() {
        return Err(StoreError::Invalid {
            reason: format!("{field} \"{text}\" must be YYYY-MM-DDTHH:MM:SS.sssZ"),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Result<FactManifest> {
        FactManifest::new(
            "prj_00000000-0000-4000-8000-000000000001",
            Integrity::Checksums,
            "2026-10-13T08:00:00.000Z".to_string(),
            "0000000000000001-open.jsonl".to_string(),
        )
    }

    #[test]
    fn round_trips_and_defaults_to_checksums() -> Result<()> {
        let manifest = sample()?;
        assert_eq!(manifest.integrity, Integrity::Checksums);
        let parsed = FactManifest::from_json(&manifest.to_json()?)?;
        assert_eq!(parsed, manifest);
        Ok(())
    }

    #[test]
    fn chained_round_trips() -> Result<()> {
        let mut manifest = sample()?;
        manifest.integrity = Integrity::Chained;
        let parsed = FactManifest::from_json(&manifest.to_json()?)?;
        assert_eq!(parsed, manifest);
        Ok(())
    }

    #[test]
    fn newer_version_fails_closed_with_upgrade_guidance() -> Result<()> {
        let mut manifest = sample()?;
        manifest.store_version = FACT_STORE_VERSION + 1;
        let error = FactManifest::from_json(&manifest.to_json()?)
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(error.contains("Upgrade Relay"), "{error}");
        Ok(())
    }

    #[test]
    fn unknown_integrity_token_fails() -> Result<()> {
        let mut manifest = sample()?;
        manifest.active_segment = "0000000000000002-open.jsonl".to_string();
        let mut value = manifest.to_json()?;
        if let Json::Object(map) = &mut value {
            map.insert("integrity".to_string(), Json::string("md5"));
        }
        assert!(FactManifest::from_json(&value).is_err());
        Ok(())
    }

    #[test]
    fn rejects_bad_stream_ids_and_segments() -> Result<()> {
        assert!(
            FactManifest::new(
                "",
                Integrity::Checksums,
                "2026-10-13T08:00:00.000Z".to_string(),
                "0000000000000001-open.jsonl".to_string()
            )
            .is_err()
        );
        assert!(
            FactManifest::new(
                &"x".repeat(201),
                Integrity::Checksums,
                "2026-10-13T08:00:00.000Z".to_string(),
                "0000000000000001-open.jsonl".to_string()
            )
            .is_err()
        );
        let mut manifest = sample()?;
        manifest.active_segment = "0000000000000001-0000000000000009.jsonl".to_string();
        assert!(FactManifest::from_json(&manifest.to_json()?).is_err());
        Ok(())
    }

    #[test]
    fn additive_fields_are_ignored() -> Result<()> {
        let manifest = sample()?;
        let mut value = manifest.to_json()?;
        if let Json::Object(map) = &mut value {
            map.insert("futureField".to_string(), Json::string("ignored"));
        }
        assert_eq!(FactManifest::from_json(&value)?, manifest);
        Ok(())
    }

    #[test]
    fn atomic_write_replaces_content() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("manifest.json");
        let manifest = sample()?;
        manifest.write_atomic(&path)?;
        manifest.write_atomic(&path)?;
        assert_eq!(FactManifest::read(&path)?, manifest);
        let leftovers: Vec<String> = std::fs::read_dir(dir.path())?
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(leftovers, vec!["manifest.json".to_string()]);
        Ok(())
    }
}
