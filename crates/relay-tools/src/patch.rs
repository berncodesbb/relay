//! Unified-diff patch application tool.
//!
//! Ported from `src/tools/patch.ts` (retired TypeScript tree, removed from
//! the repository 2026-10-14). The entire patch is parsed and applied
//! in memory first: every context line must match the current file exactly
//! or nothing is written (atomic across all files). File deletion, traversal,
//! and absolute-path headers are rejected, and a re-stat guard aborts when a
//! target changed while the patch was being prepared, so unrelated user
//! edits are never clobbered.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::LazyLock;
use std::time::SystemTime;

use crate::capability::ToolCapability;
use regex::Regex;
use serde_json::json;

use crate::path_guard::{normalize_workspace_path, resolve_workspace_path};
use crate::redact::{bound_output, redact_secrets};
use crate::registry::{
    ToolArgsSchema, ToolContext, ToolDefinition, ToolRegistrationError, ToolRegistry, ToolResult,
};
use crate::schemas::{RELATIVE_PATH_MESSAGE, is_safe_relative_path, object_arg, string_arg};

const PATCH_TIMEOUT_MS: u64 = 30_000;
const MAX_OUTPUT_CHARS: usize = 30_000;

/// A parsed patch is malformed or unsupported.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct PatchFormatError(pub String);

/// One body line's leading character in a hunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchBodyOp {
    /// ` ` — context line that must match the current file exactly.
    Context,
    /// `+` — line added by the patch.
    Addition,
    /// `-` — line removed by the patch.
    Removal,
    /// `\\` — "\\ No newline at end of file" marker.
    Marker,
}

/// One line of a hunk body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchBodyLine {
    /// What the leading character was.
    pub op: PatchBodyOp,
    /// The line text after the leading character.
    pub text: String,
}

/// One `@@ ... @@` hunk of a file block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedHunk {
    /// Patch line number of the `@@` header (1-based, for messages).
    pub line_no: usize,
    /// Old-file line number the hunk starts at (1-based).
    pub old_start: usize,
    /// Old-line count declared by the header, when present.
    pub old_count: Option<usize>,
    /// Context/add/remove/marker lines in order.
    pub body: Vec<PatchBodyLine>,
}

/// One `--- a/<path>` / `+++ b/<path>` file block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedFile {
    /// Workspace-relative target path.
    pub rel_path: String,
    /// Whether the block creates the file (`--- /dev/null` header).
    pub is_new: bool,
    /// Hunks in patch order.
    pub hunks: Vec<ParsedHunk>,
    /// Patch line number of the `---` header (1-based, for messages).
    pub header_line_no: usize,
}

/// Hunk-header shape. Compiled once; if the constant ever fails to compile,
/// every hunk header is reported as malformed (fail closed).
static HUNK_HEADER: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@").ok());

/// Render a short JSON-quoted excerpt for error messages.
fn quote(text: &str) -> String {
    let shortened: String = if text.chars().count() > 120 {
        let mut shortened: String = text.chars().take(117).collect();
        shortened.push_str("...");
        shortened
    } else {
        text.to_owned()
    };
    serde_json::to_string(&shortened).unwrap_or_else(|_| format!("{shortened:?}"))
}

/// Strip timestamps and quotes from a diff file token.
fn parse_file_token(raw: &str) -> String {
    let mut token = raw;
    if let Some(tab) = token.find('\t') {
        token = &token[..tab];
    }
    if token.len() >= 2 && token.starts_with('"') && token.ends_with('"') {
        token[1..token.len() - 1].to_owned()
    } else {
        token.to_owned()
    }
}

/// Drop a leading `a/` or `b/` prefix from a file token.
fn strip_diff_prefix(token: &str, prefix: char) -> &str {
    let second = match token.as_bytes().first() {
        Some(&first) if first == prefix as u8 => token
            .as_bytes()
            .get(1)
            .map(|second| *second as char)
            .unwrap_or('\0'),
        _ => return token,
    };
    match second {
        '/' => &token[2..],
        _ => token,
    }
}

/// Parse a unified diff into file blocks, rejecting deletions, renames,
/// duplicate blocks, unsafe target paths, and malformed hunks.
pub fn parse_patch(patch: &str) -> Result<Vec<ParsedFile>, PatchFormatError> {
    let mut raw_lines: Vec<&str> = patch.split('\n').collect();
    if raw_lines.last() == Some(&"") {
        raw_lines.pop();
    }
    let mut files: Vec<ParsedFile> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut index = 0usize;

    const SKIP_PREFIXES: [&str; 9] = [
        "diff ",
        "index ",
        "new file mode ",
        "old mode ",
        "new mode ",
        "similarity index ",
        "rename ",
        "copy ",
        "Binary files ",
    ];

    while index < raw_lines.len() {
        let line = raw_lines[index];
        if line.is_empty() {
            index += 1;
            continue;
        }
        if line.starts_with("deleted file mode ") {
            return Err(PatchFormatError(
                "patch deletes a file, which is not supported (refusing)".into(),
            ));
        }
        if SKIP_PREFIXES.iter().any(|prefix| line.starts_with(prefix)) {
            index += 1;
            continue;
        }
        if !line.starts_with("--- ") {
            return Err(PatchFormatError(format!(
                "patch line {}: unexpected {}; expected a \"--- a/<path>\" file header",
                index + 1,
                quote(line)
            )));
        }

        let header_line_no = index + 1;
        let old_token = parse_file_token(&line[4..]);
        let plus_line = raw_lines.get(index + 1).copied();
        let Some(plus_line) = plus_line else {
            return Err(PatchFormatError(format!(
                "patch line {}: expected \"+++ b/<path>\" after the \"--- \" header, found {}",
                header_line_no + 1,
                quote("<end of patch>")
            )));
        };
        if !plus_line.starts_with("+++ ") {
            return Err(PatchFormatError(format!(
                "patch line {}: expected \"+++ b/<path>\" after the \"--- \" header, found {}",
                header_line_no + 1,
                quote(plus_line)
            )));
        }
        let new_token = parse_file_token(&plus_line[4..]);

        if new_token == "/dev/null" {
            return Err(PatchFormatError(format!(
                "patch line {}: file deletion is not supported (\"+++ /dev/null\" for {})",
                header_line_no + 1,
                quote(&old_token)
            )));
        }
        let rel_path = normalize_workspace_path(strip_diff_prefix(&new_token, 'b'));
        let old_path = if old_token == "/dev/null" {
            None
        } else {
            Some(normalize_workspace_path(strip_diff_prefix(&old_token, 'a')))
        };
        if rel_path.is_empty() {
            return Err(PatchFormatError(format!(
                "patch line {header_line_no}: empty target path"
            )));
        }
        let path_issue: Option<String> = if rel_path.chars().count() > 1000 {
            Some("Too big: expected string to have <=1000 characters".to_owned())
        } else if !is_safe_relative_path(&rel_path) {
            Some(RELATIVE_PATH_MESSAGE.to_owned())
        } else {
            None
        };
        if let Some(message) = path_issue {
            return Err(PatchFormatError(format!(
                "patch line {header_line_no}: target path {} is not a safe workspace-relative path ({message})",
                quote(&rel_path)
            )));
        }
        if old_path.is_some_and(|old_path| old_path != rel_path) {
            return Err(PatchFormatError(format!(
                "patch line {header_line_no}: rename patches are not supported ({} -> {})",
                quote(&old_token),
                quote(&new_token)
            )));
        }
        if seen.contains(&rel_path) {
            return Err(PatchFormatError(format!(
                "patch contains more than one block for {}; merge them into one",
                quote(&rel_path)
            )));
        }
        seen.insert(rel_path.clone());

        index += 2;
        let mut hunks: Vec<ParsedHunk> = Vec::new();
        while index < raw_lines.len() {
            let candidate = raw_lines[index];
            if !candidate.starts_with("@@") {
                break;
            }
            let hunk_line_no = index + 1;
            let Some(captures) = HUNK_HEADER
                .as_ref()
                .and_then(|regex| regex.captures(candidate))
            else {
                return Err(PatchFormatError(format!(
                    "patch line {hunk_line_no}: malformed hunk header {}",
                    quote(candidate)
                )));
            };
            let old_start: usize = captures
                .get(1)
                .and_then(|value| value.as_str().parse().ok())
                .unwrap_or(0);
            let old_count: Option<usize> = captures
                .get(2)
                .and_then(|value| value.as_str().parse().ok());
            let new_count: Option<usize> = captures
                .get(4)
                .and_then(|value| value.as_str().parse().ok());
            let expected_old = old_count.unwrap_or(1);
            let expected_new = new_count.unwrap_or(1);
            index += 1;
            let mut body: Vec<PatchBodyLine> = Vec::new();
            let mut need_old = expected_old;
            let mut need_new = expected_new;
            while index < raw_lines.len() {
                let body_line = raw_lines[index];
                let Some(op) = body_line.chars().next() else {
                    break;
                };
                if op == '\\' {
                    // "\ No newline at end of file" markers attach to the
                    // previous line.
                    body.push(PatchBodyLine {
                        op: PatchBodyOp::Marker,
                        text: body_line[1..].to_owned(),
                    });
                    index += 1;
                    continue;
                }
                if need_old == 0 && need_new == 0 {
                    break;
                }
                match op {
                    ' ' => {
                        if need_old == 0 || need_new == 0 {
                            return Err(PatchFormatError(format!(
                                "patch line {}: unexpected context line in hunk for {} (header declares {expected_old} old and {expected_new} new line(s))",
                                index + 1,
                                quote(&rel_path)
                            )));
                        }
                        body.push(PatchBodyLine {
                            op: PatchBodyOp::Context,
                            text: body_line[1..].to_owned(),
                        });
                        need_old -= 1;
                        need_new -= 1;
                    }
                    '-' => {
                        if need_old == 0 {
                            return Err(PatchFormatError(format!(
                                "patch line {}: more removed lines than the hunk header for {} declares ({expected_old})",
                                index + 1,
                                quote(&rel_path)
                            )));
                        }
                        body.push(PatchBodyLine {
                            op: PatchBodyOp::Removal,
                            text: body_line[1..].to_owned(),
                        });
                        need_old -= 1;
                    }
                    '+' => {
                        if need_new == 0 {
                            return Err(PatchFormatError(format!(
                                "patch line {}: more added lines than the hunk header for {} declares ({expected_new})",
                                index + 1,
                                quote(&rel_path)
                            )));
                        }
                        body.push(PatchBodyLine {
                            op: PatchBodyOp::Addition,
                            text: body_line[1..].to_owned(),
                        });
                        need_new -= 1;
                    }
                    _ => break,
                }
                index += 1;
            }
            if need_old > 0 || need_new > 0 {
                return Err(PatchFormatError(format!(
                    "patch line {hunk_line_no}: hunk for {} declares {expected_old} old and {expected_new} new line(s) but its body is incomplete",
                    quote(&rel_path)
                )));
            }
            if body.is_empty() {
                return Err(PatchFormatError(format!(
                    "patch line {hunk_line_no}: hunk for {} has no body lines",
                    quote(&rel_path)
                )));
            }
            hunks.push(ParsedHunk {
                line_no: hunk_line_no,
                old_start,
                old_count,
                body,
            });
        }

        if hunks.is_empty() {
            return Err(PatchFormatError(format!(
                "patch line {header_line_no}: file block for {} has no hunks (mode-only changes are not supported)",
                quote(&rel_path)
            )));
        }
        files.push(ParsedFile {
            rel_path,
            is_new: old_token == "/dev/null",
            hunks,
            header_line_no,
        });
    }

    if files.is_empty() {
        return Err(PatchFormatError(
            "patch contains no file blocks (expected \"--- a/<path>\" / \"+++ b/<path>\" headers)"
                .into(),
        ));
    }
    Ok(files)
}

/// A file's observed facts used by the concurrency guard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StatSnapshot {
    modified: SystemTime,
    size: u64,
}

/// Where a target's current facts come from. Production reads the
/// filesystem; tests substitute observations that change between the
/// patch's read and the pre-write re-check, exercising the concurrency
/// guard deterministically.
type StatFn<'a> = &'a dyn Fn(&Path) -> Option<StatSnapshot>;

fn real_stat(path: &Path) -> Option<StatSnapshot> {
    let metadata = std::fs::metadata(path).ok()?;
    Some(StatSnapshot {
        modified: metadata.modified().ok()?,
        size: metadata.len(),
    })
}

/// One file's prepared new content plus what is needed to publish or roll
/// it back.
struct PreparedFile {
    rel_path: String,
    abs_path: PathBuf,
    is_new: bool,
    added: usize,
    removed: usize,
    new_content: String,
    /// Content before the patch (absent for new files); used to roll back a
    /// failed multi-file commit.
    original_content: Option<String>,
    /// Facts observed when the patch read the file (absent for new files).
    stat_before: Option<StatSnapshot>,
}

/// Monotonic suffix for staged temp files (unique within a process, no
/// randomness so runs stay reproducible).
static TEMP_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Write content to a temp file next to the target so every file can be
/// published only after all of them staged cleanly.
fn stage_write(abs_path: &Path, content: &str) -> std::io::Result<PathBuf> {
    let parent = abs_path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let n = TEMP_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let temp = parent.join(format!(".relay-patch-{}-{n}", std::process::id()));
    std::fs::write(&temp, content.as_bytes())?;
    Ok(temp)
}

/// Publish one staged temp file (create-only hard link for new files).
fn commit_staged(abs_path: &Path, temp: &Path, is_new: bool) -> std::io::Result<()> {
    if is_new {
        std::fs::hard_link(temp, abs_path)?;
        std::fs::remove_file(temp)?;
    } else {
        std::fs::rename(temp, abs_path)?;
    }
    Ok(())
}

/// Best-effort removal of staged temp files that were never published.
fn cleanup_staged(staged: &[Option<PathBuf>]) {
    for temp in staged.iter().flatten() {
        // The file may already have been published or removed.
        let _ = std::fs::remove_file(temp);
    }
}

/// Best-effort rollback of files already published by a failed commit loop.
fn rollback_published(prepared: &[PreparedFile], published: &[usize]) {
    for position in published.iter().rev() {
        let file = &prepared[*position];
        let result = if file.is_new {
            std::fs::remove_file(&file.abs_path)
        } else {
            match &file.original_content {
                Some(original) => stage_write(&file.abs_path, original)
                    .and_then(|restore| commit_staged(&file.abs_path, &restore, false)),
                None => Ok(()),
            }
        };
        // Rollback is best-effort; the failure is reported with the tool
        // result.
        let _ = result;
    }
}

/// Apply every parsed file block: verify contexts in memory (refusing on
/// any conflict so nothing is written), re-stat every target, then stage
/// and publish all files, rolling back already-published ones on an I/O
/// failure so a half-applied multi-file patch cannot remain.
fn apply_files(files: &[ParsedFile], context: &ToolContext, stat_file: StatFn<'_>) -> ToolResult {
    let mut prepared: Vec<PreparedFile> = Vec::new();
    for file in files {
        let abs_path = match resolve_workspace_path(&context.workspace_root, &file.rel_path) {
            Ok(abs_path) => abs_path,
            Err(error) => return ToolResult::fail(error.to_string()),
        };
        let (original, stat_before) = if file.is_new {
            if stat_file(&abs_path).is_some() {
                return ToolResult::fail(format!(
                    "cannot create \"{}\" with this patch: the file already exists (patch line {})",
                    file.rel_path, file.header_line_no
                ));
            }
            (String::new(), None)
        } else {
            let Some(stat_before) = stat_file(&abs_path) else {
                return ToolResult::fail(format!(
                    "conflict: patch modifies \"{}\" but that file does not exist (patch line {}); use \"--- /dev/null\" to create it",
                    file.rel_path, file.header_line_no
                ));
            };
            let original = match std::fs::read_to_string(&abs_path) {
                Ok(original) => original,
                Err(error) => return ToolResult::fail(format!("apply-patch failed: {error}")),
            };
            (original, Some(stat_before))
        };

        let normalized = original.replace("\r\n", "\n");
        let ended_with_newline =
            file.is_new || (!normalized.is_empty() && normalized.ends_with('\n'));
        let mut lines: Vec<String> = if normalized.is_empty() {
            Vec::new()
        } else {
            let body = if ended_with_newline {
                &normalized[..normalized.len() - 1]
            } else {
                &normalized[..]
            };
            body.split('\n').map(str::to_owned).collect()
        };
        let mut delta: i64 = 0;
        let mut added = 0usize;
        let mut removed = 0usize;
        let mut no_newline_at_end = false;

        for (hunk_index, hunk) in file.hunks.iter().enumerate() {
            let start = (((hunk.old_start as i64) - 1) + delta).max(0) as usize;
            let mut emitted: Vec<String> = Vec::new();
            let mut consumed = 0usize;
            let mut conflict: Option<(String, Option<String>, usize)> = None;

            for body_line in &hunk.body {
                match body_line.op {
                    PatchBodyOp::Marker => {
                        no_newline_at_end = true;
                    }
                    PatchBodyOp::Addition => {
                        emitted.push(body_line.text.clone());
                        added += 1;
                    }
                    PatchBodyOp::Context | PatchBodyOp::Removal => {
                        let actual = lines.get(start + consumed).cloned();
                        if actual.as_deref() != Some(body_line.text.as_str()) {
                            conflict = Some((body_line.text.clone(), actual, start + consumed + 1));
                            break;
                        }
                        if body_line.op == PatchBodyOp::Context {
                            emitted.push(body_line.text.clone());
                        } else {
                            removed += 1;
                        }
                        consumed += 1;
                    }
                }
            }

            if let Some((expected, found, at_line)) = conflict {
                return ToolResult::fail(format!(
                    "conflict in \"{}\" hunk {} (patch line {}) at file line {}: patch expected {} but found {}. Nothing was written; re-read the file and regenerate the patch against current content.",
                    file.rel_path,
                    hunk_index + 1,
                    hunk.line_no,
                    at_line,
                    quote(&expected),
                    match &found {
                        Some(found) => quote(found),
                        None => "<end of file>".to_owned(),
                    }
                ));
            }

            // `Array.prototype.splice` clamps its start index to the end of
            // the array; `consumed` is zero whenever clamping happened
            // because any context/removal line beyond the end conflicts.
            let at = start.min(lines.len());
            let emitted_count = emitted.len();
            lines.splice(at..at + consumed, emitted);
            delta += emitted_count as i64 - consumed as i64;
        }

        let new_content = if lines.is_empty() {
            String::new()
        } else {
            let mut new_content = lines.join("\n");
            if !no_newline_at_end && ended_with_newline {
                new_content.push('\n');
            }
            new_content
        };
        prepared.push(PreparedFile {
            rel_path: file.rel_path.clone(),
            abs_path,
            is_new: file.is_new,
            added,
            removed,
            new_content,
            original_content: if file.is_new { None } else { Some(original) },
            stat_before,
        });
    }

    // Concurrency guard: refuse to write if any target changed since read.
    for file in &prepared {
        let Some(stat_before) = file.stat_before else {
            continue;
        };
        if stat_file(&file.abs_path) != Some(stat_before) {
            return ToolResult::fail(format!(
                "file changed while patch was prepared: \"{}\" (size or modification time differs from when the patch read it); re-read the file and re-apply",
                file.rel_path
            ));
        }
    }
    if context.canceled() {
        return ToolResult::fail("apply-patch canceled before any file was written");
    }

    // Phase 1: stage every file's new content next to its target.
    let mut staged: Vec<Option<PathBuf>> = Vec::with_capacity(prepared.len());
    for file in &prepared {
        match stage_write(&file.abs_path, &file.new_content) {
            Ok(temp) => staged.push(Some(temp)),
            Err(error) => {
                cleanup_staged(&staged);
                return ToolResult::fail(format!("apply-patch failed: {error}"));
            }
        }
    }
    // Phase 2: publish. A failure rolls back what was already published.
    let mut published: Vec<usize> = Vec::new();
    let mut failure: Option<std::io::Error> = None;
    for (position, file) in prepared.iter().enumerate() {
        let Some(temp) = staged[position].as_deref() else {
            continue;
        };
        if let Err(error) = commit_staged(&file.abs_path, temp, file.is_new) {
            failure = Some(error);
            break;
        }
        published.push(position);
    }
    if let Some(error) = failure {
        rollback_published(&prepared, &published);
        cleanup_staged(&staged);
        return ToolResult::fail(format!("apply-patch failed: {error}"));
    }

    let total_added: usize = prepared.iter().map(|file| file.added).sum();
    let total_removed: usize = prepared.iter().map(|file| file.removed).sum();
    let details = prepared
        .iter()
        .map(|file| {
            if file.is_new {
                format!("  {}: created (+{} lines)", file.rel_path, file.added)
            } else {
                format!(
                    "  {}: +{} -{} lines",
                    file.rel_path, file.added, file.removed
                )
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let plural = if prepared.len() == 1 { "" } else { "s" };
    let output = format!(
        "applied patch to {} file{plural}:\n{details}\ntotal: +{total_added} -{total_removed} lines",
        prepared.len()
    );
    context.log(&format!("apply-patch changed {} file(s)", prepared.len()));
    let output = match bound_output(&redact_secrets(&output), MAX_OUTPUT_CHARS) {
        Ok(output) => output,
        Err(error) => return ToolResult::fail(error.to_string()),
    };
    ToolResult {
        ok: true,
        outcome: crate::registry::ToolOutcome::Ok,
        output,
        data: Some(json!({
            "files": prepared
                .iter()
                .map(|file| json!({
                    "path": file.rel_path,
                    "created": file.is_new,
                    "added": file.added,
                    "removed": file.removed,
                }))
                .collect::<Vec<_>>(),
        })),
        affected_files: prepared.iter().map(|file| file.rel_path.clone()).collect(),
    }
}

/// Register the apply-patch tool.
pub fn register_patch_tools(registry: &mut ToolRegistry) -> Result<(), ToolRegistrationError> {
    let mut properties = serde_json::Map::new();
    properties.insert(
        "patch".to_owned(),
        string_arg(
            "Unified diff text with \"--- a/<path>\" / \"+++ b/<path>\" headers and @@ hunks.",
            1,
            200_000,
        ),
    );
    let schema = ToolArgsSchema::new(object_arg(properties, &["patch"]), None)?;

    let apply_patch = ToolDefinition {
        name: "apply-patch".into(),
        title: "Apply unified diff".into(),
        description: "Apply a unified diff inside the workspace. Context lines must match exactly; on any conflict nothing is written. Creates new files via \"--- /dev/null\". Never deletes files.".into(),
        capability: ToolCapability::Write,
        args_schema: Arc::new(schema),
        timeout_ms: PATCH_TIMEOUT_MS,
        max_output_chars: MAX_OUTPUT_CHARS,
        execute: Arc::new(|args, context| {
            if context.canceled() {
                return ToolResult::fail("apply-patch canceled before it started");
            }
            let patch_text = args["patch"].as_str().unwrap_or_default();
            let files = match parse_patch(patch_text) {
                Ok(files) => files,
                Err(error) => return ToolResult::fail(error.to_string()),
            };
            apply_files(&files, context, &real_stat)
        }),
    };
    registry.register(apply_patch)?;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::registry::call_tool;
    use serde_json::json;
    use std::path::PathBuf;

    fn registry() -> ToolRegistry {
        let mut registry = ToolRegistry::new();
        register_patch_tools(&mut registry).expect("registers");
        registry
    }

    fn ws() -> PathBuf {
        tempfile::Builder::new()
            .prefix("relay-patch-")
            .tempdir()
            .expect("temp dir")
            .keep()
    }

    fn apply(ws: &Path, patch: &str) -> ToolResult {
        let tool = registry().get("apply-patch").expect("tool");
        call_tool(&tool, &json!({ "patch": patch }), &ToolContext::new(ws))
    }

    #[test]
    fn applies_a_simple_modification_and_reports_changed_lines() {
        let ws = ws();
        std::fs::write(ws.join("a.txt"), "one\ntwo\nthree\n").expect("write");
        let result = apply(
            &ws,
            "--- a/a.txt\n+++ b/a.txt\n@@ -1,3 +1,3 @@\n one\n-two\n+TWO\n three\n",
        );
        assert!(result.ok, "{}", result.output);
        assert_eq!(result.affected_files, vec!["a.txt".to_owned()]);
        assert!(result.output.contains("+1 -1 lines"), "{}", result.output);
        assert_eq!(
            std::fs::read_to_string(ws.join("a.txt")).expect("read"),
            "one\nTWO\nthree\n"
        );
    }

    #[test]
    fn creates_new_files_via_dev_null_headers() {
        let ws = ws();
        let result = apply(
            &ws,
            "--- /dev/null\n+++ b/docs/created.md\n@@ -0,0 +1,2 @@\n+# Title\n+Body\n",
        );
        assert!(result.ok, "{}", result.output);
        assert_eq!(result.affected_files, vec!["docs/created.md".to_owned()]);
        assert_eq!(
            std::fs::read_to_string(ws.join("docs/created.md")).expect("read"),
            "# Title\nBody\n"
        );
    }

    #[test]
    fn applies_multiple_hunks_in_one_file_with_offset_bookkeeping() {
        let ws = ws();
        std::fs::write(ws.join("multi.txt"), "a\nb\nc\nd\ne\nf\n").expect("write");
        let patch = [
            "--- a/multi.txt",
            "+++ b/multi.txt",
            "@@ -1,2 +1,3 @@",
            " a",
            "+a2",
            " b",
            "@@ -5,2 +6,2 @@",
            " e",
            "-f",
            "+F",
        ]
        .join("\n");
        let result = apply(&ws, &patch);
        assert!(result.ok, "{}", result.output);
        assert_eq!(
            std::fs::read_to_string(ws.join("multi.txt")).expect("read"),
            "a\na2\nb\nc\nd\ne\nF\n"
        );
    }

    #[test]
    fn writes_nothing_when_any_hunk_conflicts_atomic_across_files() {
        let ws = ws();
        let good_before = "alpha\nbeta\n";
        let bad_before = "x\ny\n";
        std::fs::write(ws.join("good.txt"), good_before).expect("write");
        std::fs::write(ws.join("bad.txt"), bad_before).expect("write");
        let patch = [
            "--- a/good.txt",
            "+++ b/good.txt",
            "@@ -1,2 +1,2 @@",
            " alpha",
            "-beta",
            "+BETA",
            "--- a/bad.txt",
            "+++ b/bad.txt",
            "@@ -1,2 +1,2 @@",
            " x",
            "-not-present",
            "+changed",
        ]
        .join("\n");
        let result = apply(&ws, &patch);
        assert!(!result.ok);
        assert!(result.affected_files.is_empty());
        assert!(
            result.output.contains("\"bad.txt\" hunk 1"),
            "{}",
            result.output
        );
        assert_eq!(
            std::fs::read_to_string(ws.join("good.txt")).expect("read"),
            good_before
        );
        assert_eq!(
            std::fs::read_to_string(ws.join("bad.txt")).expect("read"),
            bad_before
        );
    }

    #[test]
    fn names_file_hunk_and_expected_content_in_conflict_messages() {
        let ws = ws();
        std::fs::write(ws.join("a.txt"), "one\ntwo\n").expect("write");
        let result = apply(
            &ws,
            "--- a/a.txt\n+++ b/a.txt\n@@ -1,2 +1,2 @@\n one\n-three\n+TWO\n",
        );
        assert!(!result.ok);
        assert!(
            result.output.contains("\"a.txt\" hunk 1"),
            "{}",
            result.output
        );
        assert!(result.output.contains("\"three\""), "{}", result.output);
        assert!(result.output.contains("\"two\""), "{}", result.output);
        assert!(
            result.output.contains("Nothing was written"),
            "{}",
            result.output
        );
    }

    #[test]
    fn rejects_traversal_and_absolute_target_paths() {
        let ws = ws();
        let traversal = apply(&ws, "--- a/x.txt\n+++ b/../evil.txt\n@@ -1 +1 @@\n-a\n+b\n");
        assert!(!traversal.ok);
        assert!(
            traversal
                .output
                .contains("not a safe workspace-relative path"),
            "{}",
            traversal.output
        );
        let absolute = apply(&ws, "--- a/x.txt\n+++ /etc/passwd\n@@ -1 +1 @@\n-a\n+b\n");
        assert!(!absolute.ok);
        assert!(
            absolute
                .output
                .contains("not a safe workspace-relative path"),
            "{}",
            absolute.output
        );
    }

    #[test]
    fn rejects_file_deletion_headers() {
        let ws = ws();
        std::fs::write(ws.join("victim.txt"), "bye\n").expect("write");
        let result = apply(
            &ws,
            "--- a/victim.txt\n+++ /dev/null\n@@ -1 +0,0 @@\n-bye\n",
        );
        assert!(!result.ok);
        assert!(
            result.output.contains("deletion is not supported"),
            "{}",
            result.output
        );
        assert_eq!(
            std::fs::read_to_string(ws.join("victim.txt")).expect("read"),
            "bye\n"
        );
    }

    #[test]
    fn rejects_malformed_hunk_headers_with_a_line_number() {
        let ws = ws();
        let result = apply(&ws, "--- a/a.txt\n+++ b/a.txt\n@@ nonsense @@\n-a\n+b\n");
        assert!(!result.ok);
        assert!(
            result
                .output
                .contains("patch line 3: malformed hunk header"),
            "{}",
            result.output
        );
    }

    #[test]
    fn rejects_hunks_whose_declared_line_counts_do_not_match_the_body() {
        let ws = ws();
        let result = apply(
            &ws,
            "--- a/a.txt\n+++ b/a.txt\n@@ -1,3 +1,2 @@\n one\n-two\n+TWO\n",
        );
        assert!(!result.ok);
        assert!(
            result.output.contains("declares 3 old and 2 new line"),
            "{}",
            result.output
        );
    }

    #[test]
    fn conflicts_when_a_patched_file_does_not_exist() {
        let ws = ws();
        let result = apply(
            &ws,
            "--- a/ghost.txt\n+++ b/ghost.txt\n@@ -1 +1 @@\n-old\n+new\n",
        );
        assert!(!result.ok);
        assert!(
            result.output.contains("does not exist"),
            "{}",
            result.output
        );
        assert!(result.output.contains("dev/null"), "{}", result.output);
    }

    #[test]
    fn refuses_to_create_a_file_that_already_exists() {
        let ws = ws();
        std::fs::write(ws.join("there.txt"), "already here\n").expect("write");
        let result = apply(
            &ws,
            "--- /dev/null\n+++ b/there.txt\n@@ -0,0 +1,1 @@\n+new\n",
        );
        assert!(!result.ok);
        assert!(
            result.output.contains("already exists"),
            "{}",
            result.output
        );
        assert_eq!(
            std::fs::read_to_string(ws.join("there.txt")).expect("read"),
            "already here\n"
        );
    }

    #[test]
    fn honors_the_no_newline_at_end_marker() {
        let ws = ws();
        std::fs::write(ws.join("noeol.txt"), "end").expect("write");
        let patch = "--- a/noeol.txt\n+++ b/noeol.txt\n@@ -1 +1 @@\n-end\n+END\n\\ No newline at end of file\n";
        let result = apply(&ws, patch);
        assert!(result.ok, "{}", result.output);
        assert_eq!(
            std::fs::read_to_string(ws.join("noeol.txt")).expect("read"),
            "END"
        );
    }

    #[test]
    fn rejects_patches_with_no_file_blocks() {
        let ws = ws();
        let result = apply(&ws, "just some text\n");
        assert!(!result.ok);
        assert!(
            result.output.contains("no file blocks") || result.output.contains("unexpected"),
            "{}",
            result.output
        );
    }

    /// The concurrency guard: the re-stat after preparing must observe the
    /// same facts as the stat before the read. This stat source reports a
    /// changed modification time on its second observation of each path,
    /// exactly as if the user had edited the file in between.
    #[test]
    fn aborts_when_the_target_changed_after_the_patch_read_it() {
        let ws = ws();
        std::fs::write(ws.join("watched.txt"), "alpha\nbeta\n").expect("write");
        let files = parse_patch(
            "--- a/watched.txt\n+++ b/watched.txt\n@@ -1,2 +1,2 @@\n alpha\n-beta\n+BETA\n",
        )
        .expect("parses");

        let observations: std::sync::Mutex<std::collections::HashMap<PathBuf, usize>> =
            std::sync::Mutex::new(std::collections::HashMap::new());
        let changing_stat = |path: &Path| -> Option<StatSnapshot> {
            let mut seen = observations.lock().unwrap_or_else(|p| p.into_inner());
            let count = seen.entry(path.to_path_buf()).or_insert(0);
            *count += 1;
            let mut snapshot = real_stat(path)?;
            if *count > 1 {
                snapshot.modified += std::time::Duration::from_secs(1_000 * (*count as u64));
            }
            Some(snapshot)
        };

        let result = apply_files(&files, &ToolContext::new(&ws), &changing_stat);
        assert!(!result.ok);
        assert!(result.affected_files.is_empty());
        assert!(
            result
                .output
                .contains("file changed while patch was prepared"),
            "{}",
            result.output
        );
        assert_eq!(
            std::fs::read_to_string(ws.join("watched.txt")).expect("read"),
            "alpha\nbeta\n"
        );
    }

    /// The same patch applies when the re-stat observes identical facts.
    #[test]
    fn still_applies_cleanly_when_stats_are_unchanged() {
        let ws = ws();
        std::fs::write(ws.join("stable.txt"), "alpha\nbeta\n").expect("write");
        let files = parse_patch(
            "--- a/stable.txt\n+++ b/stable.txt\n@@ -1,2 +1,2 @@\n alpha\n-beta\n+BETA\n",
        )
        .expect("parses");
        let result = apply_files(&files, &ToolContext::new(&ws), &real_stat);
        assert!(result.ok, "{}", result.output);
        assert_eq!(
            std::fs::read_to_string(ws.join("stable.txt")).expect("read"),
            "alpha\nBETA\n"
        );
    }

    #[test]
    fn quoting_shortens_and_json_escapes_long_values() {
        assert_eq!(quote("simple"), "\"simple\"");
        let long = "x".repeat(130);
        let quoted = quote(&long);
        assert_eq!(quoted.chars().count(), 117 + 3 + 2);
        assert!(quoted.ends_with("...\""));
    }
}
