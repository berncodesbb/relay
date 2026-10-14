//! Error types mirroring the TypeScript artifact error classes.
//!
//! Each error carries the same public fields as its TypeScript counterpart and
//! renders the same message text, so diagnostics fed back to models or users
//! stay byte-compatible with the retired TypeScript implementation
//! (`src/artifacts/*.ts`, removed from the tree 2026-10-14).

use std::fmt;

/// Unknown artifact kind (`UnknownArtifactKindError`): `Unknown artifact kind
/// "{kind}": {detail}`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub struct UnknownArtifactKindError {
    /// The rejected kind spelling.
    pub kind: String,
    /// Explanation (usually the expected kind list).
    pub detail: String,
}

impl fmt::Display for UnknownArtifactKindError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Unknown artifact kind \"{}\": {}",
            self.kind, self.detail
        )
    }
}

/// Draft failed schema validation (`InvalidArtifactDraftError`):
/// `Invalid {kind} draft: {issues joined by "; "}`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub struct InvalidArtifactDraftError {
    /// The artifact kind whose schema rejected the draft.
    pub kind: String,
    /// Human-readable `path: message` issue strings, in schema order.
    pub issues: Vec<String>,
}

impl fmt::Display for InvalidArtifactDraftError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Invalid {} draft: {}", self.kind, self.issues.join("; "))
    }
}

/// Invalid artifacts directory (`InvalidArtifactsDirError`):
/// `Invalid artifacts directory "{artifacts_dir}": {detail}`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub struct InvalidArtifactsDirError {
    /// The rejected directory path.
    pub artifacts_dir: String,
    /// Explanation.
    pub detail: String,
}

impl fmt::Display for InvalidArtifactsDirError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Invalid artifacts directory \"{}\": {}",
            self.artifacts_dir, self.detail
        )
    }
}

/// Invalid save options (`InvalidArtifactOptionsError`):
/// `Invalid artifact save options: {detail}`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub struct InvalidArtifactOptionsError {
    /// Explanation.
    pub detail: String,
}

impl fmt::Display for InvalidArtifactOptionsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Invalid artifact save options: {}", self.detail)
    }
}

/// Target version file already exists (`ArtifactVersionExistsError`): version
/// files are immutable and are never overwritten.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub struct ArtifactVersionExistsError {
    /// Stored (workspace-relative) path of the conflicting file.
    pub path: String,
}

impl fmt::Display for ArtifactVersionExistsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Artifact version file already exists and will not be overwritten: {}",
            self.path
        )
    }
}

/// Path escapes the artifacts directory (`ArtifactPathOutsideStoreError`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub struct ArtifactPathOutsideStoreError {
    /// The rejected input path.
    pub path: String,
}

impl fmt::Display for ArtifactPathOutsideStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Artifact path \"{}\" resolves outside the artifacts directory",
            self.path
        )
    }
}

/// Artifact file exists but cannot be parsed (`InvalidArtifactFileError`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub struct InvalidArtifactFileError {
    /// Stored (workspace-relative) path of the offending file.
    pub path: String,
    /// Explanation (missing delimiters, invalid YAML, invalid frontmatter).
    pub detail: String,
}

impl fmt::Display for InvalidArtifactFileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Invalid artifact file \"{}\": {}",
            self.path, self.detail
        )
    }
}

/// Every error the artifacts crate can produce, wrapping the typed error
/// structs above plus filesystem failures.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ArtifactsError {
    /// Unknown artifact kind.
    #[error(transparent)]
    UnknownArtifactKind(#[from] UnknownArtifactKindError),

    /// Draft failed schema validation.
    #[error(transparent)]
    InvalidArtifactDraft(#[from] InvalidArtifactDraftError),

    /// Invalid artifacts directory.
    #[error(transparent)]
    InvalidArtifactsDir(#[from] InvalidArtifactsDirError),

    /// Invalid artifact save options.
    #[error(transparent)]
    InvalidArtifactOptions(#[from] InvalidArtifactOptionsError),

    /// Artifact version file already exists.
    #[error(transparent)]
    ArtifactVersionExists(#[from] ArtifactVersionExistsError),

    /// Artifact path resolves outside the store.
    #[error(transparent)]
    ArtifactPathOutsideStore(#[from] ArtifactPathOutsideStoreError),

    /// Artifact file could not be parsed.
    #[error(transparent)]
    InvalidArtifactFile(#[from] InvalidArtifactFileError),

    /// Filesystem failure during a store operation.
    #[error("artifact store io error: {0}")]
    Io(#[from] std::io::Error),
}
