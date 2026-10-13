//! `.relay` directory layout (`docs/event-format.md` §4).
//!
//! Mirrors `src/storage/paths.ts` for the legacy pieces (root, `state.db`,
//! `artifacts/`) and adds the Rust store layout (`manifest.json`, `events/`,
//! `index.sqlite`, `locks/`, `recovery/`). This module only computes paths;
//! callers own creation.

use std::path::Path;
use std::path::PathBuf;

/// Directory (relative to the workspace root) that holds all Relay state.
pub const RELAY_DIR: &str = ".relay";

/// Absolute paths of Relay state for a given workspace root.
#[derive(Debug, Clone)]
pub struct RelayPaths {
    /// `<workspace>/.relay`
    pub root: PathBuf,
    /// `<root>/manifest.json`
    pub manifest: PathBuf,
    /// `<root>/events`
    pub events_dir: PathBuf,
    /// `<root>/index.sqlite`
    pub index_db: PathBuf,
    /// `<root>/artifacts`
    pub artifacts_dir: PathBuf,
    /// `<root>/locks`
    pub locks_dir: PathBuf,
    /// `<root>/recovery`
    pub recovery_dir: PathBuf,
    /// `<root>/state.db` — the legacy TypeScript database, read-only input
    /// for migration.
    pub legacy_state_db: PathBuf,
}

impl RelayPaths {
    /// Compute the layout for `workspace_root` (state under
    /// `<workspace_root>/.relay`).
    pub fn new(workspace_root: &Path) -> RelayPaths {
        RelayPaths::from_relay_root(&workspace_root.join(RELAY_DIR))
    }

    /// Compute the layout for a directory that already **is** the `.relay`
    /// directory (used for migration staging, where the staging directory
    /// becomes the new store root directly).
    pub fn from_relay_root(relay_dir: &Path) -> RelayPaths {
        let root = relay_dir.to_path_buf();
        RelayPaths {
            manifest: root.join("manifest.json"),
            events_dir: root.join("events"),
            index_db: root.join("index.sqlite"),
            artifacts_dir: root.join("artifacts"),
            locks_dir: root.join("locks"),
            recovery_dir: root.join("recovery"),
            legacy_state_db: root.join("state.db"),
            root,
        }
    }

    /// The writer lock path (`<root>/locks/writer.lock`).
    pub fn writer_lock(&self) -> PathBuf {
        self.locks_dir.join("writer.lock")
    }

    /// The migration lock path. Kept *beside* `.relay`
    /// (`<workspace>/.relay-migration.lock`), not inside it: the §10
    /// atomic switch renames the whole `.relay` directory, and a lock left
    /// inside it would travel with the backup and block later runs.
    pub fn migration_lock(&self) -> PathBuf {
        self.root
            .parent()
            .map(|parent| parent.join(".relay-migration.lock"))
            .unwrap_or_else(|| self.locks_dir.join("migration.lock"))
    }

    /// Create the directory layout (idempotent). Does not create the legacy
    /// database and does not write the manifest.
    pub fn ensure_layout(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.events_dir)?;
        std::fs::create_dir_all(&self.artifacts_dir)?;
        std::fs::create_dir_all(&self.locks_dir)?;
        std::fs::create_dir_all(&self.recovery_dir)
    }
}
