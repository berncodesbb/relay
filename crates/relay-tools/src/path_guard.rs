//! Workspace path boundary enforcement.
//!
//! Ported from `src/tools/path-guard.ts` (retired TypeScript tree, removed
//! from the repository 2026-10-14). Every tool resolves user/model
//! supplied paths through [`resolve_workspace_path`], which rejects traversal,
//! absolute paths, and home shortcuts, then resolves symlinks on the root and
//! on every existing ancestor of the target so a link inside the workspace
//! cannot escape it. Fail closed: anything that cannot be proven inside the
//! workspace is rejected.

use std::path::{Path, PathBuf};

/// A path argument tried to leave the authorized workspace.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("refusing workspace path \"{target}\": {reason}")]
pub struct PathEscapeError {
    /// The offending path as provided.
    pub target: String,
    /// Why it was refused.
    pub reason: &'static str,
}

impl PathEscapeError {
    fn new(target: impl Into<String>, reason: &'static str) -> Self {
        PathEscapeError {
            target: target.into(),
            reason,
        }
    }
}

fn check_relative_shape(relative_path: &str) -> Result<(), PathEscapeError> {
    if relative_path.is_empty() {
        return Err(PathEscapeError::new(relative_path, "path is empty"));
    }
    if relative_path.contains('\0') {
        return Err(PathEscapeError::new(
            relative_path,
            "path contains a NUL byte",
        ));
    }
    let looks_absolute = relative_path.starts_with('/')
        || relative_path.starts_with('\\')
        || relative_path
            .chars()
            .next()
            .is_some_and(|first| first.is_ascii_alphabetic())
            && relative_path
                .chars()
                .nth(1)
                .is_some_and(|second| second == ':');
    if looks_absolute {
        return Err(PathEscapeError::new(
            relative_path,
            "absolute paths are not allowed; paths must be workspace-relative",
        ));
    }
    if relative_path == "~" || relative_path.starts_with("~/") || relative_path.starts_with("~\\") {
        return Err(PathEscapeError::new(
            relative_path,
            "home-directory shortcuts (~) are not allowed",
        ));
    }
    if relative_path
        .split(['/', '\\'])
        .any(|segment| segment == "..")
    {
        return Err(PathEscapeError::new(
            relative_path,
            "parent traversal (\"..\") is not allowed",
        ));
    }
    Ok(())
}

/// True when `target` is `root` itself or lives underneath it. Both arguments
/// should already be fully resolved (realpath'd) paths; the comparison is
/// lexical over path components and never follows symlinks.
pub fn is_within_workspace(root: &Path, target: &Path) -> bool {
    // `Path::strip_prefix` compares whole components, so a sibling that
    // merely shares a leading string (/tmp/ws vs /tmp/ws2) never matches.
    if target.strip_prefix(root).is_ok() {
        return true;
    }
    // Paths that normalize upward (a resolved "..") escape the root: any
    // parent-component in the stripped remainder can only come from a root
    // that itself ends in `..`.
    false
}

/// Normalize a workspace-relative path to forward slashes.
pub fn normalize_workspace_path(relative_path: &str) -> String {
    relative_path.replace('\\', "/")
}

/// Resolve a workspace-relative path to an absolute path that is guaranteed
/// to stay inside `workspace_root` after symlink resolution.
///
/// Non-existent targets are handled by realpath-ing the deepest existing
/// ancestor and appending the remaining segments, then re-checking
/// containment. Returns [`PathEscapeError`] for any path that could leave the
/// workspace.
pub fn resolve_workspace_path(
    workspace_root: &Path,
    relative_path: &str,
) -> Result<PathBuf, PathEscapeError> {
    check_relative_shape(relative_path)?;

    let root_real = std::fs::canonicalize(workspace_root).map_err(|_| {
        PathEscapeError::new(
            workspace_root.to_string_lossy().into_owned(),
            "workspace root does not exist or cannot be resolved",
        )
    })?;

    let segments: Vec<&str> = relative_path
        .split(['/', '\\'])
        .filter(|segment| !segment.is_empty() && *segment != ".")
        .collect();

    let mut tail: Vec<&str> = Vec::new();
    let mut existing = root_real.clone();
    for depth in (0..segments.len()).rev() {
        let mut candidate = root_real.clone();
        for segment in &segments[..=depth] {
            candidate.push(segment);
        }
        // `stat` follows symlinks like the reference implementation, so a
        // dangling link counts as non-existent.
        if std::fs::metadata(&candidate).is_ok() {
            existing = candidate;
            break;
        }
        tail.insert(0, segments[depth]);
    }

    let existing_real = std::fs::canonicalize(&existing).map_err(|_| {
        PathEscapeError::new(
            relative_path,
            "resolved location leaves the workspace via a symlink",
        )
    })?;
    if !is_within_workspace(&root_real, &existing_real) {
        return Err(PathEscapeError::new(
            relative_path,
            "resolved location leaves the workspace via a symlink",
        ));
    }

    let mut target = existing_real;
    for segment in &tail {
        target.push(segment);
    }
    if !is_within_workspace(&root_real, &target) {
        return Err(PathEscapeError::new(
            relative_path,
            "resolved path leaves the workspace",
        ));
    }
    Ok(target)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    fn temp_dir(prefix: &str) -> PathBuf {
        tempfile::Builder::new()
            .prefix(prefix)
            .tempdir()
            .expect("temp dir")
            .keep()
    }

    #[test]
    fn rejects_absolute_paths() {
        for path in ["/etc/passwd", "\\Windows\\system32", "C:/Users/x"] {
            let error = resolve_workspace_path(Path::new("/definitely/not/real"), path)
                .expect_err("absolute rejected");
            assert_eq!(
                error.reason,
                "absolute paths are not allowed; paths must be workspace-relative"
            );
        }
    }

    #[test]
    fn rejects_parent_traversal() {
        for path in ["../outside.txt", "a/../../outside.txt", "a\\..\\..\\b.txt"] {
            assert!(resolve_workspace_path(Path::new("/definitely/not/real"), path).is_err());
        }
    }

    #[test]
    fn rejects_home_shortcuts_and_empty_paths() {
        for path in ["~", "~/.ssh/id_rsa", "~\\home", "", "\0nul"] {
            let error = resolve_workspace_path(Path::new("/definitely/not/real"), path)
                .expect_err("rejected");
            assert!(
                error.reason.contains("not allowed")
                    || error.reason.contains("empty")
                    || error.reason.contains("NUL"),
                "{}",
                error.reason
            );
        }
    }

    #[test]
    fn rejects_a_symlink_inside_the_workspace_that_points_outside() {
        let ws = temp_dir("relay-pathguard-");
        let outside = temp_dir("relay-outside-");
        std::fs::write(outside.join("secret.txt"), "secret").expect("write");
        std::os::unix::fs::symlink(&outside, ws.join("escape")).expect("symlink");

        let error = resolve_workspace_path(&ws, "escape/secret.txt").expect_err("escape rejected");
        assert_eq!(
            error.reason,
            "resolved location leaves the workspace via a symlink"
        );
    }

    #[test]
    fn rejects_deep_symlink_chains_that_leave_the_workspace() {
        let ws = temp_dir("relay-pathguard-");
        let outside = temp_dir("relay-outside-");
        std::os::unix::fs::symlink(&outside, ws.join("hop-two")).expect("symlink");
        std::os::unix::fs::symlink(ws.join("hop-two"), ws.join("hop-one")).expect("symlink");

        assert!(resolve_workspace_path(&ws, "hop-one/file.txt").is_err());
    }

    #[test]
    fn allows_symlinks_that_stay_inside_the_workspace() {
        let ws = temp_dir("relay-pathguard-");
        let real_dir = ws.join("real");
        std::fs::create_dir(&real_dir).expect("mkdir");
        std::os::unix::fs::symlink(&real_dir, ws.join("alias")).expect("symlink");

        let resolved = resolve_workspace_path(&ws, "alias/file.txt").expect("inside workspace");
        let real = std::fs::canonicalize(&real_dir).expect("canonical");
        assert_eq!(resolved, real.join("file.txt"));
        assert!(is_within_workspace(&real, &resolved));
    }

    #[test]
    fn resolves_nonexistent_nested_paths_inside_the_workspace() {
        let ws = temp_dir("relay-pathguard-");
        let resolved = resolve_workspace_path(&ws, "a/b/c.txt").expect("inside workspace");
        let real = std::fs::canonicalize(&ws).expect("canonical");
        assert_eq!(resolved, real.join("a").join("b").join("c.txt"));
        assert!(is_within_workspace(&real, &resolved));
    }

    #[test]
    fn normalizes_dot_and_trailing_separators() {
        let ws = temp_dir("relay-pathguard-");
        let resolved = resolve_workspace_path(&ws, "./src/").expect("inside workspace");
        let real = std::fs::canonicalize(&ws).expect("canonical");
        assert_eq!(resolved, real.join("src"));
    }

    #[test]
    fn works_when_the_workspace_root_itself_is_behind_a_symlink() {
        let parent = temp_dir("relay-pathguard-");
        let real_root = parent.join("real-root");
        std::fs::create_dir(&real_root).expect("mkdir");
        let linked_root = parent.join("link-root");
        std::os::unix::fs::symlink(&real_root, &linked_root).expect("symlink");
        let real_resolved = std::fs::canonicalize(&real_root).expect("canonical");

        assert_eq!(
            resolve_workspace_path(&linked_root, "notes.txt").expect("inside"),
            real_resolved.join("notes.txt")
        );
        assert_eq!(
            resolve_workspace_path(&linked_root, "deep/nested/notes.txt").expect("inside"),
            real_resolved.join("deep").join("nested").join("notes.txt")
        );
    }

    #[test]
    fn throws_when_the_workspace_root_does_not_exist() {
        let error = resolve_workspace_path(
            &std::env::temp_dir().join("relay-missing-root-xyz"),
            "a.txt",
        )
        .expect_err("missing root rejected");
        assert_eq!(
            error.reason,
            "workspace root does not exist or cannot be resolved"
        );
    }

    #[test]
    fn is_within_workspace_accepts_root_and_children_only() {
        let root = Path::new("/tmp/ws");
        assert!(is_within_workspace(root, Path::new("/tmp/ws")));
        assert!(is_within_workspace(root, Path::new("/tmp/ws/a/b.txt")));
        assert!(is_within_workspace(root, Path::new("/tmp/ws/..hidden")));
        assert!(!is_within_workspace(root, Path::new("/tmp/ws2/file.txt")));
        assert!(!is_within_workspace(root, Path::new("/tmp")));
        assert!(!is_within_workspace(root, Path::new("/etc/passwd")));
    }
}
