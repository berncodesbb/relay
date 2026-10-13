//! Directory layout of the fact store
//! (`docs/plans/fact-format-proposal.md`).
//!
//! The fact store keeps its own root so it can coexist with the frozen
//! legacy `.relay` event store in the same workspace during the migration
//! window: the default root is `<workspace>/.relay-graph`. Migration staging
//! constructs a layout over any directory with [`FactPaths::from_root`].
//! This module only computes paths; callers own creation.

use std::path::Path;
use std::path::PathBuf;

/// Directory (relative to the workspace root) holding the intent-first fact
/// store. Distinct from the legacy `.relay` root so neither store can
/// misinterpret the other's segments, manifest, or projection.
pub const FACT_STORE_DIR: &str = ".relay-graph";

/// Absolute paths of the fact store for a given root directory.
#[derive(Debug, Clone)]
pub struct FactPaths {
    /// The fact-store root.
    pub root: PathBuf,
    /// `<root>/manifest.json`
    pub manifest: PathBuf,
    /// `<root>/facts` — the segmented JSONL fact log.
    pub facts_dir: PathBuf,
    /// `<root>/projection.sqlite` — disposable, rebuildable.
    pub projection_db: PathBuf,
    /// `<root>/locks`
    pub locks_dir: PathBuf,
    /// `<root>/recovery`
    pub recovery_dir: PathBuf,
}

impl FactPaths {
    /// Compute the layout for `workspace_root` (state under
    /// `<workspace_root>/.relay-graph`).
    pub fn new(workspace_root: &Path) -> FactPaths {
        FactPaths::from_root(&workspace_root.join(FACT_STORE_DIR))
    }

    /// Compute the layout for a directory that already **is** the fact-store
    /// root (used for migration staging).
    pub fn from_root(root: &Path) -> FactPaths {
        let root = root.to_path_buf();
        FactPaths {
            manifest: root.join("manifest.json"),
            facts_dir: root.join("facts"),
            projection_db: root.join("projection.sqlite"),
            locks_dir: root.join("locks"),
            recovery_dir: root.join("recovery"),
            root,
        }
    }

    /// The writer lock path (`<root>/locks/writer.lock`).
    pub fn writer_lock(&self) -> PathBuf {
        self.locks_dir.join("writer.lock")
    }

    /// Create the directory layout (idempotent). Does not write the
    /// manifest.
    pub fn ensure_layout(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.facts_dir)?;
        std::fs::create_dir_all(&self.locks_dir)?;
        std::fs::create_dir_all(&self.recovery_dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_is_stable_and_distinct_from_legacy() {
        let paths = FactPaths::new(Path::new("/tmp/workspace"));
        assert_eq!(paths.root, Path::new("/tmp/workspace/.relay-graph"));
        assert_eq!(paths.manifest, paths.root.join("manifest.json"));
        assert_eq!(paths.facts_dir, paths.root.join("facts"));
        assert_eq!(paths.projection_db, paths.root.join("projection.sqlite"));
        assert_eq!(paths.writer_lock(), paths.locks_dir.join("writer.lock"));
        assert!(!paths.root.to_string_lossy().contains(".relay/events"));
    }

    #[test]
    fn staging_layout_points_at_the_given_root() {
        let paths = FactPaths::from_root(Path::new("/tmp/staging"));
        assert_eq!(paths.root, Path::new("/tmp/staging"));
        assert_eq!(paths.facts_dir, Path::new("/tmp/staging/facts"));
    }
}
