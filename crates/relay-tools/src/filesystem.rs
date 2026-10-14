//! Filesystem tools: bounded reads, directory inventory, and create-only
//! writes.
//!
//! Ported from `src/tools/filesystem.ts` (retired TypeScript tree, removed
//! from the repository 2026-10-14). Reads never return binary blobs and
//! always pass through redaction and output bounds. Writes only create NEW
//! files (overwrites are refused so user changes survive) and are atomic via
//! write-temp-then-link with a rename fallback guarded by a re-stat.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::capability::ToolCapability;
use relay_config::ToolSettings;

use crate::path_guard::{PathEscapeError, normalize_workspace_path, resolve_workspace_path};
use crate::redact::{bound_output, redact_secrets};
use crate::registry::{
    ToolArgsSchema, ToolContext, ToolDefinition, ToolRegistrationError, ToolRegistry, ToolResult,
};
use crate::schemas::{
    integer_arg, integer_arg_with_default, object_arg, path_property, relative_path_refinement,
    string_arg, string_arg_with_default,
};

const READ_TIMEOUT_MS: u64 = 10_000;
const WRITE_TIMEOUT_MS: u64 = 10_000;
const MAX_OUTPUT_CHARS: usize = 30_000;
const MAX_LIST_ENTRIES: usize = 2_000;

/// Directory names never descended into (they are still listed).
const SKIPPED_DIR_NAMES: [&str; 5] = ["node_modules", ".git", "dist", ".relay", "coverage"];

/// Monotonic suffix for temp files (unique within a process, no randomness so
/// runs stay reproducible).
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_name(prefix: &str) -> String {
    let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!(".{prefix}-{}-{n}", std::process::id())
}

/// Resolve a validated relative path or convert the escape error to a failed
/// tool result.
fn resolve_rel(context: &ToolContext, rel: &str) -> Result<PathBuf, ToolResult> {
    resolve_workspace_path(&context.workspace_root, rel)
        .map_err(|error: PathEscapeError| ToolResult::fail(error.to_string()))
}

/// Read at most `max_bytes` from a file plus its total size.
fn read_bounded(absolute: &Path, max_bytes: u64) -> std::io::Result<(Vec<u8>, u64)> {
    let mut file = fs::File::open(absolute)?;
    let metadata = fs::metadata(absolute)?;
    let mut buffer = vec![0u8; max_bytes as usize];
    let mut read_total = 0usize;
    while read_total < buffer.len() {
        match file.read(&mut buffer[read_total..])? {
            0 => break,
            read => read_total += read,
        }
    }
    buffer.truncate(read_total);
    Ok((buffer, metadata.len()))
}

/// Register the read-file, list-dir, and write-file tools.
pub fn register_filesystem_tools(
    registry: &mut ToolRegistry,
    settings: &ToolSettings,
) -> Result<(), ToolRegistrationError> {
    // --- read-file ---------------------------------------------------------
    let mut read_properties = serde_json::Map::new();
    read_properties.insert("path".to_owned(), path_property());
    read_properties.insert(
        "maxBytes".to_owned(),
        integer_arg("Read at most this many bytes.", 1, 20_000_000),
    );
    let read_schema = ToolArgsSchema::new(
        object_arg(read_properties, &["path"]),
        Some(relative_path_refinement),
    )?;
    let read_max_file_bytes = settings.max_file_bytes.max(0) as u64;
    let read_file = ToolDefinition {
        name: "read-file".into(),
        title: "Read file".into(),
        description: "Read a text file inside the workspace. Returns up to maxBytes with a size header; binary content is refused.".into(),
        capability: ToolCapability::Read,
        args_schema: Arc::new(read_schema),
        timeout_ms: READ_TIMEOUT_MS,
        max_output_chars: MAX_OUTPUT_CHARS,
        execute: Arc::new(move |args, context| {
            if context.canceled() {
                return ToolResult::fail("read-file canceled before it started");
            }
            let rel = normalize_workspace_path(args["path"].as_str().unwrap_or_default());
            let absolute = match resolve_rel(context, &rel) {
                Ok(absolute) => absolute,
                Err(failure) => return failure,
            };
            let info = match fs::metadata(&absolute) {
                Ok(info) => info,
                Err(_) => return ToolResult::fail(format!("file not found: {rel}")),
            };
            if !info.is_file() {
                return ToolResult::fail(format!("not a regular file: {rel}"));
            }
            let requested = args["maxBytes"].as_u64();
            let max_bytes = requested
                .unwrap_or(read_max_file_bytes)
                .min(read_max_file_bytes)
                .max(1);
            let (bytes, size) = match read_bounded(&absolute, max_bytes) {
                Ok(result) => result,
                Err(error) => return ToolResult::fail(format!("cannot open {rel}: {error}")),
            };
            if let Some(offset) = bytes.iter().position(|byte| *byte == 0) {
                return ToolResult::fail(format!(
                    "refusing to read {rel}: binary content (NUL byte at offset {offset}); only text files are readable"
                ));
            }
            let content = String::from_utf8_lossy(&bytes);
            let header = format!("# {rel} (first {} of {size} bytes)", bytes.len());
            let text = format!("{header}\n{content}");
            finish_output(text)
        }),
    };
    registry.register(read_file)?;

    // --- list-dir ----------------------------------------------------------
    let mut list_properties = serde_json::Map::new();
    list_properties.insert(
        "path".to_owned(),
        string_arg_with_default(
            "Workspace-relative directory path; defaults to the workspace root.",
            1,
            1000,
            ".",
        ),
    );
    list_properties.insert(
        "depth".to_owned(),
        integer_arg_with_default("How many directory levels to list.", 1, 4, 1),
    );
    let list_schema = ToolArgsSchema::new(
        object_arg(list_properties, &["path", "depth"]),
        Some(relative_path_refinement),
    )?;
    let list_dir = ToolDefinition {
        name: "list-dir".into(),
        title: "List directory".into(),
        description: "List workspace directories as a tree (directories first), up to `depth` levels. Skips node_modules, .git, dist, .relay, and coverage.".into(),
        capability: ToolCapability::Read,
        args_schema: Arc::new(list_schema),
        timeout_ms: READ_TIMEOUT_MS,
        max_output_chars: MAX_OUTPUT_CHARS,
        execute: Arc::new(move |args, context| {
            if context.canceled() {
                return ToolResult::fail("list-dir canceled before it started");
            }
            let rel = normalize_workspace_path(args["path"].as_str().unwrap_or("."));
            let absolute = match resolve_rel(context, &rel) {
                Ok(absolute) => absolute,
                Err(failure) => return failure,
            };
            let info = match fs::metadata(&absolute) {
                Ok(info) => info,
                Err(_) => return ToolResult::fail(format!("directory not found: {rel}")),
            };
            if !info.is_dir() {
                return ToolResult::fail(format!("not a directory: {rel}"));
            }
            let depth = args["depth"].as_u64().unwrap_or(1) as usize;
            let mut lines: Vec<String> = Vec::new();
            let mut truncated = false;
            walk_tree(&absolute, depth, "", context, &mut lines, &mut truncated);
            if context.canceled() {
                return ToolResult::fail("list-dir canceled");
            }
            if truncated {
                lines.push("… (listing stopped at entry limit)".to_owned());
            }
            if lines.is_empty() {
                return ToolResult::done(format!("# {rel} (empty directory)"), Vec::new());
            }
            let text = format!("# {rel}\n{}", lines.join("\n"));
            finish_output(text)
        }),
    };
    registry.register(list_dir)?;

    // --- write-file --------------------------------------------------------
    let mut write_properties = serde_json::Map::new();
    write_properties.insert("path".to_owned(), path_property());
    write_properties.insert(
        "content".to_owned(),
        string_arg("Full file content to write.", 1, 2_000_000),
    );
    let write_schema = ToolArgsSchema::new(
        object_arg(write_properties, &["path", "content"]),
        Some(relative_path_refinement),
    )?;
    let write_file = ToolDefinition {
        name: "write-file".into(),
        title: "Write new file".into(),
        description: "Create a NEW text file inside the workspace (with parent directories). Refuses to overwrite existing files; use apply-patch to modify them.".into(),
        capability: ToolCapability::Write,
        args_schema: Arc::new(write_schema),
        timeout_ms: WRITE_TIMEOUT_MS,
        max_output_chars: MAX_OUTPUT_CHARS,
        execute: Arc::new(move |args, context| {
            if context.canceled() {
                return ToolResult::fail("write-file canceled before it started");
            }
            let rel = normalize_workspace_path(args["path"].as_str().unwrap_or_default());
            let content = args["content"].as_str().unwrap_or_default();
            let absolute = match resolve_workspace_path(&context.workspace_root, &rel) {
                Ok(absolute) => absolute,
                Err(error) => return ToolResult::fail(error.to_string()),
            };
            if fs::metadata(&absolute).is_ok() {
                return ToolResult::fail(format!(
                    "refusing to overwrite existing file {rel}; use apply-patch to modify it"
                ));
            }
            let Some(parent) = absolute.parent().map(Path::to_path_buf) else {
                return ToolResult::fail(format!("cannot write {rel}: no parent directory"));
            };
            if let Err(error) = fs::create_dir_all(&parent) {
                return ToolResult::fail(format!("cannot write {rel}: {error}"));
            }
            let temp = parent.join(temp_name("relay-tmp"));
            if let Err(error) = fs::write(&temp, content.as_bytes()) {
                return ToolResult::fail(format!("cannot write {rel}: {error}"));
            }
            // Create-only publication: a hard link cannot clobber an existing
            // file, so an overwrite race is detected atomically. Any link
            // failure falls back to rename after a re-stat proves the target
            // still does not exist (fail closed).
            let publish = match fs::hard_link(&temp, &absolute) {
                Ok(()) => Ok(()),
                Err(_) => {
                    if fs::metadata(&absolute).is_ok() {
                        let _ = fs::remove_file(&temp);
                        return ToolResult::fail(format!(
                            "refusing to overwrite existing file {rel}; use apply-patch to modify it"
                        ));
                    }
                    fs::rename(&temp, &absolute)
                }
            };
            let _ = fs::remove_file(&temp);
            if let Err(error) = publish {
                return ToolResult::fail(format!("cannot write {rel}: {error}"));
            }
            if context.canceled() {
                return ToolResult::fail(format!(
                    "write-file canceled (file {rel} was already created)"
                ));
            }
            let bytes = content.len();
            context.log(&format!("write-file created {rel} ({bytes} bytes)"));
            ToolResult::done(format!("created {rel} ({bytes} bytes)"), vec![rel])
        }),
    };
    registry.register(write_file)?;
    Ok(())
}

/// Redact then bound a tool's textual output.
pub(crate) fn finish_output(text: String) -> ToolResult {
    match bound_output(&redact_secrets(&text), MAX_OUTPUT_CHARS) {
        Ok(output) => ToolResult::done(output, Vec::new()),
        Err(error) => ToolResult::fail(error.to_string()),
    }
}

/// One directory entry as observed without following symlinks.
struct TreeEntry {
    name: String,
    is_dir: bool,
    is_symlink: bool,
}

/// Recursive tree walk for list-dir: directories first, sorted by name,
/// symlinks labeled and never followed.
fn walk_tree(
    dir: &Path,
    remaining: usize,
    indent: &str,
    context: &ToolContext,
    lines: &mut Vec<String>,
    truncated: &mut bool,
) {
    if *truncated || context.canceled() {
        return;
    }
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) => {
            lines.push(format!("{indent}(unreadable: {error})"));
            return;
        }
    };
    let mut collected: Vec<TreeEntry> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let file_type = entry.file_type().ok()?;
            Some(TreeEntry {
                name,
                is_dir: file_type.is_dir(),
                is_symlink: file_type.is_symlink(),
            })
        })
        .collect();
    collected.sort_by(|a, b| a.name.cmp(&b.name));

    let is_listable_dir =
        |entry: &TreeEntry| entry.is_dir && !SKIPPED_DIR_NAMES.contains(&entry.name.as_str());
    let dirs: Vec<&TreeEntry> = collected
        .iter()
        .filter(|entry| is_listable_dir(entry))
        .collect();
    let others: Vec<&TreeEntry> = collected
        .iter()
        .filter(|entry| !is_listable_dir(entry))
        .collect();

    for entry in dirs.into_iter().chain(others) {
        if lines.len() >= MAX_LIST_ENTRIES {
            *truncated = true;
            return;
        }
        let label = if entry.is_symlink {
            format!("{} (symlink)", entry.name)
        } else if entry.is_dir {
            format!("{}/", entry.name)
        } else {
            entry.name.clone()
        };
        lines.push(format!("{indent}{label}"));
        if is_listable_dir(entry) && remaining > 1 {
            walk_tree(
                &dir.join(&entry.name),
                remaining - 1,
                &format!("{indent}  "),
                context,
                lines,
                truncated,
            );
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::registry::call_tool;
    use serde_json::json;

    fn settings(max_file_bytes: i64) -> ToolSettings {
        ToolSettings {
            command_timeout_ms: 10_000,
            max_search_results: 200,
            max_file_bytes,
        }
    }

    fn registry(max_file_bytes: i64) -> ToolRegistry {
        let mut registry = ToolRegistry::new();
        register_filesystem_tools(&mut registry, &settings(max_file_bytes)).expect("registers");
        registry
    }

    fn ws() -> PathBuf {
        tempfile::Builder::new()
            .prefix("relay-fs-")
            .tempdir()
            .expect("temp dir")
            .keep()
    }

    #[test]
    fn read_file_reads_content_with_a_size_header() {
        let workspace = ws();
        fs::write(workspace.join("notes.txt"), "hello\nworld\n").expect("write");
        let tool = registry(1_000_000).get("read-file").expect("tool");
        let result = call_tool(
            &tool,
            &json!({ "path": "notes.txt" }),
            &ToolContext::new(&workspace),
        );
        assert!(result.ok, "{}", result.output);
        assert!(
            result
                .output
                .starts_with("# notes.txt (first 12 of 12 bytes)"),
            "{}",
            result.output
        );
        assert!(result.output.contains("hello\nworld"));
        assert!(result.affected_files.is_empty());
    }

    #[test]
    fn read_file_caps_reads_at_the_configured_byte_limit() {
        let workspace = ws();
        fs::write(workspace.join("big.txt"), "x".repeat(200)).expect("write");
        let tool = registry(64).get("read-file").expect("tool");
        let result = call_tool(
            &tool,
            &json!({ "path": "big.txt" }),
            &ToolContext::new(&workspace),
        );
        assert!(result.ok, "{}", result.output);
        assert!(
            result
                .output
                .starts_with("# big.txt (first 64 of 200 bytes)"),
            "{}",
            result.output
        );
        assert!(!result.output.contains(&"x".repeat(65)));
    }

    #[test]
    fn read_file_refuses_binary_content() {
        let workspace = ws();
        fs::write(workspace.join("blob.bin"), [0x61u8, 0x62, 0x00, 0x63]).expect("write");
        let tool = registry(1_000_000).get("read-file").expect("tool");
        let result = call_tool(
            &tool,
            &json!({ "path": "blob.bin" }),
            &ToolContext::new(&workspace),
        );
        assert!(!result.ok);
        assert!(
            result.output.contains("binary content"),
            "{}",
            result.output
        );
    }

    #[test]
    fn read_file_redacts_secret_looking_content() {
        let workspace = ws();
        fs::write(
            workspace.join("config.txt"),
            "token=sk-abcdefghijklmnop1234567890\n",
        )
        .expect("write");
        let tool = registry(1_000_000).get("read-file").expect("tool");
        let result = call_tool(
            &tool,
            &json!({ "path": "config.txt" }),
            &ToolContext::new(&workspace),
        );
        assert!(result.ok, "{}", result.output);
        assert!(result.output.contains("[redacted:api-key]"));
        assert!(!result.output.contains("sk-abcdefghijklmnop1234567890"));
    }

    #[test]
    fn read_file_errors_for_missing_files_and_directories() {
        let workspace = ws();
        fs::create_dir(workspace.join("pkg")).expect("mkdir");
        let tool = registry(1_000_000).get("read-file").expect("tool");
        let missing = call_tool(
            &tool,
            &json!({ "path": "nope.txt" }),
            &ToolContext::new(&workspace),
        );
        assert!(!missing.ok);
        assert!(
            missing.output.contains("file not found"),
            "{}",
            missing.output
        );
        let dir = call_tool(
            &tool,
            &json!({ "path": "pkg" }),
            &ToolContext::new(&workspace),
        );
        assert!(!dir.ok);
        assert!(dir.output.contains("not a regular file"), "{}", dir.output);
    }

    #[test]
    fn read_file_rejects_absolute_and_traversing_paths_at_validation() {
        let workspace = ws();
        let tool = registry(1_000_000).get("read-file").expect("tool");
        let absolute = call_tool(
            &tool,
            &json!({ "path": "/etc/passwd" }),
            &ToolContext::new(&workspace),
        );
        assert!(!absolute.ok);
        assert!(
            absolute.output.contains("invalid arguments"),
            "{}",
            absolute.output
        );
        let traversal = call_tool(
            &tool,
            &json!({ "path": "../ws/notes.txt" }),
            &ToolContext::new(&workspace),
        );
        assert!(!traversal.ok);
        assert!(
            traversal.output.contains("invalid arguments"),
            "{}",
            traversal.output
        );
    }

    #[cfg(unix)]
    #[test]
    fn read_file_refuses_to_read_through_a_symlink_that_leaves_the_workspace() {
        let workspace = ws();
        let outside = tempfile::Builder::new()
            .prefix("relay-outside-")
            .tempdir()
            .expect("outside dir")
            .keep();
        fs::write(outside.join("secret.txt"), "secret").expect("write");
        std::os::unix::fs::symlink(&outside, workspace.join("escape")).expect("symlink");
        let tool = registry(1_000_000).get("read-file").expect("tool");
        let result = call_tool(
            &tool,
            &json!({ "path": "escape/secret.txt" }),
            &ToolContext::new(&workspace),
        );
        assert!(!result.ok);
        assert!(result.output.contains("symlink"), "{}", result.output);
    }

    #[test]
    fn write_file_creates_a_new_file_with_parent_directories() {
        let workspace = ws();
        let tool = registry(1_000_000).get("write-file").expect("tool");
        let result = call_tool(
            &tool,
            &json!({ "path": "src/deep/new.ts", "content": "export const x = 1;\n" }),
            &ToolContext::new(&workspace),
        );
        assert!(result.ok, "{}", result.output);
        assert_eq!(result.affected_files, vec!["src/deep/new.ts".to_owned()]);
        assert_eq!(
            fs::read_to_string(workspace.join("src/deep/new.ts")).expect("read"),
            "export const x = 1;\n"
        );
    }

    #[test]
    fn write_file_refuses_to_overwrite_an_existing_file() {
        let workspace = ws();
        fs::write(workspace.join("existing.txt"), "user content\n").expect("write");
        let tool = registry(1_000_000).get("write-file").expect("tool");
        let result = call_tool(
            &tool,
            &json!({ "path": "existing.txt", "content": "replacement\n" }),
            &ToolContext::new(&workspace),
        );
        assert_eq!(
            result.output,
            "refusing to overwrite existing file existing.txt; use apply-patch to modify it"
        );
        assert_eq!(
            fs::read_to_string(workspace.join("existing.txt")).expect("read"),
            "user content\n"
        );
    }

    #[test]
    fn write_file_refuses_paths_that_leave_the_workspace() {
        let workspace = ws();
        let tool = registry(1_000_000).get("write-file").expect("tool");
        let traversal = call_tool(
            &tool,
            &json!({ "path": "../evil.txt", "content": "nope\n" }),
            &ToolContext::new(&workspace),
        );
        assert!(!traversal.ok);
        assert!(
            traversal.output.contains("invalid arguments"),
            "{}",
            traversal.output
        );
        let absolute = call_tool(
            &tool,
            &json!({ "path": "/tmp/relay-absolute-evil.txt", "content": "nope\n" }),
            &ToolContext::new(&workspace),
        );
        assert!(!absolute.ok);
    }

    #[cfg(unix)]
    #[test]
    fn write_file_refuses_to_create_files_through_an_escaping_symlink() {
        let workspace = ws();
        let outside = tempfile::Builder::new()
            .prefix("relay-outside-")
            .tempdir()
            .expect("outside dir")
            .keep();
        std::os::unix::fs::symlink(&outside, workspace.join("escape")).expect("symlink");
        let tool = registry(1_000_000).get("write-file").expect("tool");
        let result = call_tool(
            &tool,
            &json!({ "path": "escape/new.txt", "content": "nope\n" }),
            &ToolContext::new(&workspace),
        );
        assert!(!result.ok);
        assert!(result.output.contains("symlink"), "{}", result.output);
        assert!(!outside.join("new.txt").exists());
    }

    #[test]
    fn list_dir_lists_directories_first_and_skips_vendored_directories() {
        let workspace = ws();
        fs::create_dir_all(workspace.join("src/sub")).expect("mkdir");
        fs::create_dir(workspace.join("docs")).expect("mkdir");
        fs::create_dir_all(workspace.join("node_modules/pkg")).expect("mkdir");
        fs::write(workspace.join("root.txt"), "x").expect("write");
        fs::write(workspace.join("src/a.ts"), "x").expect("write");
        fs::write(workspace.join("src/sub/b.ts"), "x").expect("write");
        fs::write(workspace.join("docs/readme.md"), "x").expect("write");
        fs::write(workspace.join("node_modules/pkg/skip.js"), "x").expect("write");

        let tool = registry(1_000_000).get("list-dir").expect("tool");
        let result = call_tool(
            &tool,
            &json!({ "path": ".", "depth": 2 }),
            &ToolContext::new(&workspace),
        );
        assert!(result.ok, "{}", result.output);
        for expected in ["src/", "docs/", "root.txt", "sub/"] {
            assert!(result.output.contains(expected), "missing {expected}");
        }
        // Vendored directories are listed but never descended into.
        assert!(result.output.contains("node_modules/"));
        assert!(!result.output.contains("pkg/"));
        assert!(!result.output.contains("skip.js"));
        let dirs_first = result.output.find("src/").expect("src listed");
        assert!(dirs_first < result.output.find("root.txt").expect("root listed"));
    }

    #[cfg(unix)]
    #[test]
    fn list_dir_marks_symlinks_without_following_them() {
        let workspace = ws();
        fs::create_dir(workspace.join("real")).expect("mkdir");
        std::os::unix::fs::symlink(workspace.join("real"), workspace.join("alias"))
            .expect("symlink");
        let tool = registry(1_000_000).get("list-dir").expect("tool");
        let result = call_tool(&tool, &json!({ "depth": 1 }), &ToolContext::new(&workspace));
        assert!(result.ok, "{}", result.output);
        assert!(
            result.output.contains("alias (symlink)"),
            "{}",
            result.output
        );
        assert!(result.output.contains("real/"), "{}", result.output);
    }

    #[test]
    fn list_dir_errors_for_missing_directories() {
        let workspace = ws();
        let tool = registry(1_000_000).get("list-dir").expect("tool");
        let result = call_tool(
            &tool,
            &json!({ "path": "missing" }),
            &ToolContext::new(&workspace),
        );
        assert!(!result.ok);
        assert!(result.output.contains("not found"), "{}", result.output);
    }
}
