//! Versioned Markdown artifact storage under `.relay/artifacts/`.
//!
//! Port of `ArtifactMarkdownStore` in `src/artifacts/markdown-store.ts`. Each
//! artifact version is an immutable file `<kind>/v<n>-<slug>.md` with YAML
//! frontmatter (id, kind, version, title, status, timestamps) followed by the
//! rendered body. Existing version files are never overwritten, and all reads
//! go through the filesystem so a fresh store instance on the same directory
//! sees everything a previous one wrote.
//!
//! Publication follows the prepare-and-publish protocol from the
//! architecture plan's "Artifact consistency" section: render deterministically,
//! write and sync a uniquely named temporary file inside the artifact
//! directory, then publish with a **hard-link + unlink** — an atomic
//! no-overwrite operation. `rename(2)` would silently replace an existing
//! target on Unix, while `link(2)` (and `CreateHardLinkW` on Windows) fails
//! with `EEXIST`/`AlreadyExists` when the destination name already exists, so
//! a file either becomes visible under its final name or the save fails
//! loudly. The temporary file is removed after the link succeeds; a crash
//! between link and unlink leaves a recoverable orphan `.tmp-*` file that
//! [`find_orphan_artifacts`] reports (never deletes).
//!
//! The TypeScript store generates `art_…` ids and timestamps internally; the
//! Rust port takes them as explicit inputs (deterministic, injected boundary
//! data — this crate has no clock or randomness).

use std::collections::HashSet;
use std::fs;
use std::io::Write as _;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use relay_core::{ARTIFACT_KINDS, ArtifactKind};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::drafts::{ParseArtifactDraftResult, parse_artifact_draft};
use crate::error::{
    ArtifactPathOutsideStoreError, ArtifactVersionExistsError, ArtifactsError,
    InvalidArtifactDraftError, InvalidArtifactFileError, InvalidArtifactOptionsError,
    InvalidArtifactsDirError,
};
use crate::frontmatter::{Frontmatter, split_frontmatter};
use crate::render::render_draft;
use crate::slug::slugify;

/// A stored artifact version descriptor (what `list`/`latest` return).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredArtifact {
    /// Artifact id (`art_…`).
    pub artifact_id: String,
    /// Artifact kind.
    pub kind: ArtifactKind,
    /// Draft title.
    pub title: String,
    /// Version number.
    pub version: u64,
    /// Path relative to the workspace root.
    pub path: String,
}

/// What [`ArtifactMarkdownStore::save`] reports: the stored descriptor plus
/// the publish-protocol extras (absolute path and SHA-256 content digest)
/// the referencing event must carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedArtifact {
    /// Artifact id (`art_…`).
    pub artifact_id: String,
    /// Artifact kind.
    pub kind: ArtifactKind,
    /// Draft title.
    pub title: String,
    /// Version number.
    pub version: u64,
    /// Path relative to the workspace root.
    pub path: String,
    /// Absolute path of the published file.
    pub absolute_path: PathBuf,
    /// Lowercase hex SHA-256 of the published file bytes.
    pub sha256: String,
}

/// Inputs to [`ArtifactMarkdownStore::save`]: the artifact id (the caller
/// generates fresh `art_…` ids — the TypeScript store did this internally),
/// the version being replaced when revising, and the injected timestamp for
/// `createdAt`/`updatedAt`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SaveArtifactOptions<'a> {
    /// Artifact id for the saved version.
    pub artifact_id: &'a str,
    /// Version being replaced; the saved version is `previous_version + 1`,
    /// or version 1 when `None`.
    pub previous_version: Option<u64>,
    /// RFC 3339 timestamp recorded as `createdAt` and `updatedAt`.
    pub timestamp: &'a str,
}

/// A stored artifact file: parsed frontmatter, body, and stored path.
#[derive(Debug, Clone, PartialEq)]
pub struct ArtifactFile {
    /// Parsed frontmatter mapping.
    pub frontmatter: Map<String, Value>,
    /// Rendered Markdown body.
    pub body: String,
    /// Path relative to the workspace root.
    pub path: String,
}

/// Only files we wrote are considered; they always carry a version prefix
/// (`/^v(\d+)-.*\.md$/`).
fn is_version_file(name: &str) -> bool {
    let Some(rest) = name.strip_prefix('v') else {
        return false;
    };
    let digits_len = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    if digits_len == 0 {
        return false;
    }
    rest[digits_len..].starts_with('-') && name.ends_with(".md")
}

/// Counter for unique temporary-file suffixes within this process.
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Lowercase hex SHA-256 digest of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

// -- lexical path helpers ----------------------------------------------------

/// Lexically resolve `.` and `..` components in an absolute path (the Node
/// `path.resolve` behavior for already-absolute inputs; no filesystem access).
fn lexical_clean(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => out.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(part) => out.push(part),
        }
    }
    out
}

/// Lexical `path.relative(base, target)` for two cleaned absolute paths.
/// Returns a relative path with leading `..` components when `target` is not
/// under `base`, or `target` itself when the two share no root/prefix.
fn lexical_relative(base: &Path, target: &Path) -> PathBuf {
    let base_parts: Vec<Component<'_>> = base.components().collect();
    let target_parts: Vec<Component<'_>> = target.components().collect();
    let mut common = 0;
    while common < base_parts.len()
        && common < target_parts.len()
        && base_parts[common] == target_parts[common]
    {
        common += 1;
    }
    if common == 0 {
        // Different roots (e.g. distinct Windows drive letters): Node's
        // path.relative returns the absolute target.
        return target.to_path_buf();
    }
    let mut relative = PathBuf::new();
    for _ in common..base_parts.len() {
        relative.push("..");
    }
    for part in &target_parts[common..] {
        relative.push(part.as_os_str());
    }
    relative
}

fn file_exists(path: &Path) -> bool {
    // Node's stat() follows symlinks; a dangling symlink does not "exist".
    fs::metadata(path).is_ok()
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

/// Versioned Markdown artifact store over one artifacts directory.
#[derive(Debug, Clone)]
pub struct ArtifactMarkdownStore {
    artifacts_dir: PathBuf,
    workspace_root: PathBuf,
}

impl ArtifactMarkdownStore {
    /// Create a store over `artifacts_dir` (must be absolute). Paths the
    /// store returns are workspace-relative for the standard
    /// `<workspace>/.relay/artifacts` layout; store-relative paths are also
    /// accepted on input.
    pub fn new(artifacts_dir: &Path) -> Result<Self, ArtifactsError> {
        if artifacts_dir.as_os_str().is_empty() || !artifacts_dir.is_absolute() {
            return Err(ArtifactsError::InvalidArtifactsDir(
                InvalidArtifactsDirError {
                    artifacts_dir: artifacts_dir.display().to_string(),
                    detail: "an absolute directory path is required".to_string(),
                },
            ));
        }
        let artifacts_dir = lexical_clean(artifacts_dir);
        let workspace_root = artifacts_dir
            .parent()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .unwrap_or_else(|| artifacts_dir.clone());
        Ok(Self {
            artifacts_dir,
            workspace_root,
        })
    }

    /// Validate and persist a draft as a new version (prepare-and-publish).
    /// Returns the stored descriptor plus the SHA-256 content digest of the
    /// published file; the layout is `<artifacts_dir>/<kind>/v<n>-<slug>.md`.
    pub fn save(
        &self,
        kind: ArtifactKind,
        draft: &Value,
        options: &SaveArtifactOptions<'_>,
    ) -> Result<SavedArtifact, ArtifactsError> {
        let parsed = match parse_artifact_draft(kind, draft) {
            ParseArtifactDraftResult::Ok(draft) => draft,
            ParseArtifactDraftResult::Err { kind, issues } => {
                return Err(ArtifactsError::InvalidArtifactDraft(
                    InvalidArtifactDraftError {
                        kind: kind.as_str().to_string(),
                        issues,
                    },
                ));
            }
        };
        let artifact_id = options.artifact_id;
        if artifact_id.is_empty()
            || artifact_id.chars().count() > 200
            || artifact_id.contains('/')
            || artifact_id.contains('\\')
            || artifact_id.contains("..")
        {
            return Err(ArtifactsError::InvalidArtifactOptions(
                InvalidArtifactOptionsError {
                    detail: format!(
                        "artifactId must be 1..200 characters without path separators or \"..\" (got \"{artifact_id}\")"
                    ),
                },
            ));
        }
        let previous_version = options.previous_version;
        if previous_version.is_some_and(|version| version < 1) {
            return Err(ArtifactsError::InvalidArtifactOptions(
                InvalidArtifactOptionsError {
                    detail: format!(
                        "previousVersion must be a positive integer (got {})",
                        previous_version.unwrap_or_default()
                    ),
                },
            ));
        }
        let version = previous_version.map_or(1, |previous| previous + 1);

        let kind_dir = self.artifacts_dir.join(kind.as_str());
        fs::create_dir_all(&kind_dir)?;
        let file_name = format!("v{}-{}.md", version, slugify(parsed.title()));
        let file = kind_dir.join(&file_name);

        // Version uniqueness is per artifact, not per filename: a retitle
        // must not smuggle in a second copy of an existing version.
        if let Some(conflict) = self.find_version_conflict(kind, artifact_id, version)? {
            return Err(ArtifactsError::ArtifactVersionExists(
                ArtifactVersionExistsError { path: conflict },
            ));
        }
        if file_exists(&file) {
            return Err(ArtifactsError::ArtifactVersionExists(
                ArtifactVersionExistsError {
                    path: self.to_stored_path(&file),
                },
            ));
        }

        let frontmatter = Frontmatter {
            id: artifact_id,
            kind,
            version,
            title: parsed.title(),
            status: "draft",
            created_at: options.timestamp,
            updated_at: options.timestamp,
        };
        let body = render_draft(&parsed);
        let content = crate::frontmatter::render_artifact_file(&frontmatter, &body);

        let published = publish_file(&kind_dir, &file_name, content.as_bytes()).map_err(
            |error| match error {
                // Re-map the race window onto the same stored-path error the
                // pre-check produces.
                ArtifactsError::ArtifactVersionExists(_) => {
                    ArtifactsError::ArtifactVersionExists(ArtifactVersionExistsError {
                        path: self.to_stored_path(&file),
                    })
                }
                other => other,
            },
        )?;

        Ok(SavedArtifact {
            artifact_id: artifact_id.to_string(),
            kind,
            title: parsed.title().to_string(),
            version,
            path: self.to_stored_path(&file),
            absolute_path: published.absolute_path,
            sha256: published.sha256,
        })
    }

    /// Read a stored artifact file by workspace-relative (or store-relative)
    /// path.
    pub fn read(&self, relative_path: &str) -> Result<ArtifactFile, ArtifactsError> {
        let absolute = self.resolve_artifact_path(relative_path).ok_or_else(|| {
            ArtifactsError::ArtifactPathOutsideStore(ArtifactPathOutsideStoreError {
                path: relative_path.to_string(),
            })
        })?;
        self.read_at(&absolute)
    }

    /// All stored versions of a kind, sorted by version (then path).
    pub fn list(&self, kind: ArtifactKind) -> Result<Vec<StoredArtifact>, ArtifactsError> {
        let names = self.version_file_names(kind)?;
        let mut stored: Vec<StoredArtifact> = Vec::with_capacity(names.len());
        for name in &names {
            let file = self.read_at(&self.artifacts_dir.join(kind.as_str()).join(name))?;
            stored.push(self.to_stored_artifact(kind, &file.frontmatter, &file.path)?);
        }
        stored.sort_by(|a, b| a.version.cmp(&b.version).then_with(|| a.path.cmp(&b.path)));
        Ok(stored)
    }

    /// Highest-version stored artifact of a kind, or `None` when none exist.
    pub fn latest(&self, kind: ArtifactKind) -> Result<Option<StoredArtifact>, ArtifactsError> {
        Ok(self.list(kind)?.pop())
    }

    /// Resolve an artifact by workspace-relative path or by artifact id
    /// (scanning kind directories for matching frontmatter). Returns `None`
    /// when not found.
    pub fn inspect(
        &self,
        artifact_id_or_path: &str,
    ) -> Result<Option<ArtifactFile>, ArtifactsError> {
        if let Some(absolute) = self.resolve_artifact_path(artifact_id_or_path)
            && file_exists(&absolute)
        {
            return Ok(Some(self.read_at(&absolute)?));
        }
        for kind in ARTIFACT_KINDS {
            let matches: Vec<StoredArtifact> = self
                .list(kind)?
                .into_iter()
                .filter(|item| item.artifact_id == artifact_id_or_path)
                .collect();
            if let Some(best) = matches.last()
                && let Some(target) = self.resolve_artifact_path(&best.path)
                && file_exists(&target)
            {
                return Ok(Some(self.read_at(&target)?));
            }
        }
        Ok(None)
    }

    /// The artifacts directory this store manages.
    pub fn artifacts_dir(&self) -> &Path {
        &self.artifacts_dir
    }

    // -- internals ------------------------------------------------------------

    fn version_file_names(&self, kind: ArtifactKind) -> Result<Vec<String>, ArtifactsError> {
        let kind_dir = self.artifacts_dir.join(kind.as_str());
        let entries = match fs::read_dir(&kind_dir) {
            Ok(entries) => entries,
            Err(_) => return Ok(Vec::new()),
        };
        let mut names: Vec<String> = entries
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| is_version_file(name))
            .collect();
        names.sort();
        Ok(names)
    }

    /// Stored path of any saved version of `artifact_id` at or above
    /// `version`, or `None`.
    fn find_version_conflict(
        &self,
        kind: ArtifactKind,
        artifact_id: &str,
        version: u64,
    ) -> Result<Option<String>, ArtifactsError> {
        let names = self.version_file_names(kind)?;
        for name in &names {
            let Ok(file) = self.read_at(&self.artifacts_dir.join(kind.as_str()).join(name)) else {
                // Unreadable/corrupt files cannot prove a conflict.
                continue;
            };
            let id = file.frontmatter.get("id");
            let saved_version = match file.frontmatter.get("version") {
                Some(Value::Number(number)) => number.as_u64(),
                _ => None,
            };
            if id == Some(&Value::String(artifact_id.to_string()))
                && saved_version.is_some_and(|saved| saved >= version)
            {
                return Ok(Some(file.path));
            }
        }
        Ok(None)
    }

    fn read_at(&self, absolute_path: &Path) -> Result<ArtifactFile, ArtifactsError> {
        let stored_path = self.to_stored_path(absolute_path);
        let content = fs::read_to_string(absolute_path).map_err(|error| {
            ArtifactsError::InvalidArtifactFile(InvalidArtifactFileError {
                path: stored_path.clone(),
                detail: error.to_string(),
            })
        })?;
        let parsed = split_frontmatter(&content, &stored_path)?;
        Ok(ArtifactFile {
            frontmatter: parsed.frontmatter,
            body: parsed.body,
            path: stored_path,
        })
    }

    fn to_stored_artifact(
        &self,
        kind: ArtifactKind,
        frontmatter: &Map<String, Value>,
        stored_path: &str,
    ) -> Result<StoredArtifact, ArtifactsError> {
        let invalid = |detail: String| {
            ArtifactsError::InvalidArtifactFile(InvalidArtifactFileError {
                path: stored_path.to_string(),
                detail,
            })
        };
        let artifact_id = match frontmatter.get("id") {
            Some(Value::String(id)) if !id.is_empty() => id.clone(),
            _ => {
                return Err(invalid(
                    "frontmatter \"id\" must be a non-empty string".to_string(),
                ));
            }
        };
        let title = match frontmatter.get("title") {
            Some(Value::String(title)) => title.clone(),
            _ => {
                return Err(invalid(
                    "frontmatter \"title\" must be a string".to_string(),
                ));
            }
        };
        let version = match frontmatter.get("version") {
            Some(Value::Number(number)) if number.is_u64() => number.as_u64(),
            _ => {
                return Err(invalid(
                    "frontmatter \"version\" must be a positive integer".to_string(),
                ));
            }
        };
        let version = version.filter(|value| *value >= 1).ok_or_else(|| {
            invalid("frontmatter \"version\" must be a positive integer".to_string())
        })?;
        Ok(StoredArtifact {
            artifact_id,
            kind,
            title,
            version,
            path: stored_path.to_string(),
        })
    }

    /// Resolve an input path (absolute, workspace-relative, or store-relative)
    /// to an absolute path inside the artifacts directory, or `None` when it
    /// escapes the store.
    fn resolve_artifact_path(&self, input: &str) -> Option<PathBuf> {
        let input_path = Path::new(input);
        let candidates: Vec<PathBuf> = if input_path.is_absolute() {
            vec![lexical_clean(input_path)]
        } else {
            vec![
                lexical_clean(&self.workspace_root.join(input_path)),
                lexical_clean(&self.artifacts_dir.join(input_path)),
            ]
        };
        for candidate in candidates {
            let relative = lexical_relative(&self.artifacts_dir, &candidate);
            let relative_str = relative.to_string_lossy();
            if !relative_str.is_empty()
                && !relative_str.starts_with("..")
                && !relative.is_absolute()
            {
                return Some(candidate);
            }
        }
        None
    }

    /// Workspace-relative path (forward slashes) for the standard layout.
    fn to_stored_path(&self, absolute_path: &Path) -> String {
        let relative = lexical_relative(&self.workspace_root, absolute_path);
        let relative_str = relative.to_string_lossy();
        if relative_str.starts_with("..") {
            lexical_relative(&self.artifacts_dir, absolute_path)
                .to_string_lossy()
                .replace('\\', "/")
        } else {
            relative_str.replace('\\', "/")
        }
    }
}

// ---------------------------------------------------------------------------
// Prepare-and-publish
// ---------------------------------------------------------------------------

/// A successfully published artifact file: its absolute path and the SHA-256
/// digest of its bytes (to be recorded in the referencing event).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedFile {
    /// Absolute path of the published file.
    pub absolute_path: PathBuf,
    /// Lowercase hex SHA-256 of the file bytes.
    pub sha256: String,
}

/// Publish `content` as `directory/file_name` using the prepare-and-publish
/// protocol: write a uniquely named temporary file in `directory`, sync it,
/// then atomically link it to its final name — failing loudly if that name
/// already exists (never silently overwriting). The temporary file is removed
/// once the link is in place; a crash before that leaves only the orphan
/// temporary file for [`find_orphan_artifacts`] to report.
pub fn publish_file(
    directory: &Path,
    file_name: &str,
    content: &[u8],
) -> Result<PublishedFile, ArtifactsError> {
    let suffix = format!(
        "{}-{}",
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    let temp_path = directory.join(format!("{file_name}.tmp-{suffix}"));
    let target = directory.join(file_name);
    let outcome = write_sync_and_link(&temp_path, &target, content);
    // Both paths remove the temporary file: after a successful link it is
    // just an extra name for the published content.
    let _ = fs::remove_file(&temp_path);
    outcome?;
    // Best-effort durability of the new directory entry.
    if let Ok(dir_handle) = fs::File::open(directory) {
        let _ = dir_handle.sync_all();
    }
    Ok(PublishedFile {
        absolute_path: target,
        sha256: sha256_hex(content),
    })
}

fn write_sync_and_link(
    temp_path: &Path,
    target: &Path,
    content: &[u8],
) -> Result<(), ArtifactsError> {
    let mut file = fs::File::create(temp_path)?;
    file.write_all(content)?;
    file.sync_all()?;
    drop(file);
    // Atomic no-overwrite publish: the hard link either creates the final
    // name or fails with AlreadyExists; an existing file is never replaced.
    match fs::hard_link(temp_path, target) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Err(
            ArtifactsError::ArtifactVersionExists(ArtifactVersionExistsError {
                path: target.display().to_string(),
            }),
        ),
        Err(error) => Err(ArtifactsError::Io(error)),
    }
}

// ---------------------------------------------------------------------------
// Orphan detection
// ---------------------------------------------------------------------------

/// An artifact file no event references, reported (never deleted) by
/// [`find_orphan_artifacts`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrphanArtifact {
    /// Path relative to the artifacts directory (`<kind>/<file>`).
    pub path: String,
    /// Lowercase hex SHA-256 of the file bytes (for doctor reporting).
    pub sha256: String,
}

/// The store-relative path (`<kind>/<file>`) of a workspace-relative artifact
/// path (`.relay/artifacts/<kind>/<file>`); unchanged when the prefix is
/// absent. Public so the runtime normalizes event paths the same way.
pub fn store_relative_path(workspace_relative: &str) -> &str {
    workspace_relative
        .strip_prefix(".relay/artifacts/")
        .unwrap_or(workspace_relative)
}

/// Scan `artifacts_dir` for files that no event references. `referenced`
/// entries may be workspace-relative (`.relay/artifacts/<kind>/<file>`) or
/// store-relative (`<kind>/<file>`). Orphans are returned sorted by path;
/// nothing is modified or deleted — quarantine is a doctor decision.
pub fn find_orphan_artifacts(
    artifacts_dir: &Path,
    referenced: &[String],
) -> Result<Vec<OrphanArtifact>, ArtifactsError> {
    let referenced_set: HashSet<String> = referenced
        .iter()
        .map(|path| store_relative_path(path).replace('\\', "/"))
        .collect();
    let mut files: Vec<(String, PathBuf)> = Vec::new();
    let mut stack: Vec<PathBuf> = vec![artifacts_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match fs::read_dir(&dir) {
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
                let relative = lexical_relative(artifacts_dir, &path)
                    .to_string_lossy()
                    .replace('\\', "/");
                files.push((relative, path));
            }
        }
    }
    files.sort();
    let mut orphans: Vec<OrphanArtifact> = Vec::new();
    for (relative, path) in files {
        if referenced_set.contains(&relative) {
            continue;
        }
        let bytes = fs::read(&path)?;
        orphans.push(OrphanArtifact {
            path: relative,
            sha256: sha256_hex(&bytes),
        });
    }
    Ok(orphans)
}
