//! Read-only Git tools plus repository instruction discovery.
//!
//! Git is spawned exactly like any other child process: argument arrays, a
//! bounded environment, timeouts, and group termination. No mutating Git
//! operation (add/commit/push/checkout/reset/stash/clean) is registered, so
//! user worktree state can never be rewritten by a tool.

use std::sync::Arc;

use crate::capability::ToolCapability;
use serde_json::Value;

use crate::redact::{bound_output, redact_secrets};
use crate::registry::{ToolContext, ToolDefinition, ToolRegistry, ToolResult};
use crate::schemas::{
    boolean_arg_with_default, integer_arg_with_default, object_arg, relative_path_refinement,
    string_arg,
};
use crate::spawn::{BoundedSpawnRequest, BoundedSpawnResult, spawn_bounded};

const GIT_TIMEOUT_MS: u64 = 20_000;
const MAX_OUTPUT_CHARS: usize = 30_000;
const INSTRUCTION_FILES: [&str; 3] = ["CLAUDE.md", "AGENTS.md", "README.md"];
const INSTRUCTION_FILE_CHAR_LIMIT: usize = 20_000;

/// Run git with an argument array in the workspace. Not registered as a
/// tool; exported so future read-only helpers share identical safety
/// rails.
pub fn run_git(args: Vec<String>, context: &ToolContext) -> BoundedSpawnResult {
    let request = BoundedSpawnRequest {
        command: "git".to_owned(),
        args,
        cwd: context.workspace_root.clone(),
        timeout_ms: GIT_TIMEOUT_MS,
        signal: context.signal.clone(),
        extra_env: Vec::new(),
        max_stream_chars: None,
        env_source: None,
    };
    spawn_bounded(request)
}

fn not_a_repository(stderr: &str) -> bool {
    stderr.to_ascii_lowercase().contains("not a git repository")
        || stderr
            .to_ascii_lowercase()
            .contains("must be run in a work tree")
}

fn git_failure(context: &ToolContext, result: &BoundedSpawnResult) -> Option<ToolResult> {
    if result.canceled {
        return Some(ToolResult::fail("git command canceled"));
    }
    if result.timed_out {
        return Some(ToolResult::fail(format!(
            "git command timed out after {GIT_TIMEOUT_MS}ms"
        )));
    }
    if let Some(spawn_error) = &result.spawn_error {
        return Some(ToolResult::fail(format!(
            "git failed to start: {spawn_error} (is git installed and on PATH?)"
        )));
    }
    if result.exit_code == Some(0) {
        return None;
    }
    let stderr_lower = result.stderr.to_ascii_lowercase();
    if stderr_lower.contains("does not have any commits")
        || stderr_lower.contains("does not have a commit")
    {
        return Some(ToolResult::fail(
            "repository has no commits yet (history is empty)",
        ));
    }
    if not_a_repository(&result.stderr) {
        let name = context
            .workspace_root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| ".".to_owned());
        return Some(ToolResult::fail(format!(
            "{name} is not a git repository (no .git found in it or any parent). \
             Initialize one with \"git init\" or point Relay at a repository root."
        )));
    }
    match result.exit_code {
        Some(code) => Some(ToolResult::fail(format!(
            "git exited with code {code}:\n{}",
            result.stderr.trim()
        ))),
        None => Some(ToolResult::fail(format!(
            "git exited with code unknown:\n{}",
            result.stderr.trim()
        ))),
    }
}

/// Register the read-only Git tools (`git-status`, `git-diff`, `git-log`,
/// `repo-instructions`).
pub fn register_git_tools(
    registry: &mut ToolRegistry,
) -> Result<(), crate::registry::ToolRegistrationError> {
    let no_args = object_arg(serde_json::Map::new(), &[]);

    // git-status
    let status_schema = crate::registry::ToolArgsSchema::new(no_args.clone(), None)?;
    let git_status = ToolDefinition {
        name: "git-status".into(),
        title: "Git status".into(),
        description:
            "Show `git status --porcelain=v1 -b` plus a one-line change summary. Read-only.".into(),
        capability: ToolCapability::Read,
        args_schema: Arc::new(status_schema),
        timeout_ms: GIT_TIMEOUT_MS,
        max_output_chars: MAX_OUTPUT_CHARS,
        execute: Arc::new(|_args, context| {
            let result = run_git(
                vec![
                    "status".to_owned(),
                    "--porcelain=v1".to_owned(),
                    "-b".to_owned(),
                ],
                context,
            );
            if let Some(failure) = git_failure(context, &result) {
                return failure;
            }
            let lines: Vec<&str> = result
                .stdout
                .split('\n')
                .filter(|l| !l.is_empty())
                .collect();
            let entries: Vec<&&str> = lines.iter().filter(|l| !l.starts_with("##")).collect();
            let untracked = entries.iter().filter(|l| l.starts_with("??")).count();
            let modified = entries
                .iter()
                .filter(|l| {
                    let b = l.as_bytes();
                    (b.len() > 1 && b" MARC".contains(&b[0]) && b"MD".contains(&b[1]))
                        || b.first() == Some(&b'M')
                })
                .count();
            let added = entries.iter().filter(|l| l.starts_with('A')).count();
            let deleted = entries
                .iter()
                .filter(|l| {
                    let b = l.as_bytes();
                    !l.starts_with("??")
                        && (b.first() == Some(&b'D')
                            || (b.len() > 1 && b"AMD".contains(&b[0]) && b[1] == b'D'))
                })
                .count();
            let n = entries.len();
            let entry_word = if n == 1 { "entry" } else { "entries" };
            let summary = format!(
                "{n} changed {entry_word} ({added} added, {modified} modified, {deleted} deleted, {untracked} untracked)"
            );
            let text = format!("{}\n{summary}", result.stdout.trim());
            finish(redact_secrets(&text), MAX_OUTPUT_CHARS)
        }),
    };

    // git-diff
    let mut diff_properties = serde_json::Map::new();
    diff_properties.insert(
        "path".to_owned(),
        string_arg("Limit the diff to this workspace-relative path.", 1, 200),
    );
    diff_properties.insert(
        "staged".to_owned(),
        boolean_arg_with_default(
            "Show staged (cached) changes instead of the working tree.",
            false,
        ),
    );
    let diff_schema = crate::registry::ToolArgsSchema::new(
        object_arg(diff_properties, &["path", "staged"]),
        Some(relative_path_refinement),
    )?;
    let git_diff = ToolDefinition {
        name: "git-diff".into(),
        title: "Git diff".into(),
        description: "Show `git diff` (unified) for the whole tree or one workspace-relative path; pass staged=true for --cached. Read-only."
            .into(),
        capability: ToolCapability::Read,
        args_schema: Arc::new(diff_schema),
        timeout_ms: GIT_TIMEOUT_MS,
        max_output_chars: MAX_OUTPUT_CHARS,
        execute: Arc::new(|args, context| {
            let mut git_args = vec!["diff".to_owned(), "--no-color".to_owned()];
            if args.get("staged").and_then(Value::as_bool) == Some(true) {
                git_args.push("--cached".to_owned());
            }
            if let Some(path) = args.get("path").and_then(Value::as_str) {
                git_args.push("--".to_owned());
                git_args.push(crate::path_guard::normalize_workspace_path(path));
            }
            let result = run_git(git_args, context);
            if let Some(failure) = git_failure(context, &result) {
                return failure;
            }
            let body = result.stdout.trim();
            let output = if body.is_empty() {
                "no changes".to_owned()
            } else {
                let count = body.split('\n').count();
                format!("{body}\n({count} diff lines)")
            };
            finish(redact_secrets(&output), MAX_OUTPUT_CHARS)
        }),
    };

    // git-log
    let mut log_properties = serde_json::Map::new();
    log_properties.insert(
        "limit".to_owned(),
        integer_arg_with_default("Number of commits to show.", 1, 200, 20),
    );
    let log_schema =
        crate::registry::ToolArgsSchema::new(object_arg(log_properties, &["limit"]), None)?;
    let git_log = ToolDefinition {
        name: "git-log".into(),
        title: "Git log".into(),
        description: "Show the most recent commits as `git log --oneline -n <limit>`. Read-only."
            .into(),
        capability: ToolCapability::Read,
        args_schema: Arc::new(log_schema),
        timeout_ms: GIT_TIMEOUT_MS,
        max_output_chars: MAX_OUTPUT_CHARS,
        execute: Arc::new(|args, context| {
            let limit = args.get("limit").and_then(Value::as_i64).unwrap_or(20);
            let result = run_git(
                vec![
                    "log".to_owned(),
                    "--oneline".to_owned(),
                    "--no-color".to_owned(),
                    "-n".to_owned(),
                    limit.to_string(),
                ],
                context,
            );
            if let Some(failure) = git_failure(context, &result) {
                return failure;
            }
            let body = result.stdout.trim();
            if body.is_empty() {
                return ToolResult::done("no commits yet", Vec::new());
            }
            let count = body.split('\n').count();
            let text = format!("{body}\n({count} commits)");
            finish(redact_secrets(&text), MAX_OUTPUT_CHARS)
        }),
    };

    // repo-instructions
    let instructions_schema = crate::registry::ToolArgsSchema::new(no_args, None)?;
    let repo_instructions = ToolDefinition {
        name: "repo-instructions".into(),
        title: "Repository instructions".into(),
        description: "Read repository instruction files from the workspace root in priority order: CLAUDE.md, AGENTS.md, README.md (first 20000 chars each)."
            .into(),
        capability: ToolCapability::Read,
        args_schema: Arc::new(instructions_schema),
        timeout_ms: GIT_TIMEOUT_MS,
        max_output_chars: 60_000,
        execute: Arc::new(|_args, context| {
            let mut sections: Vec<String> = Vec::new();
            for name in INSTRUCTION_FILES {
                if context.canceled() {
                    return ToolResult::fail("repo-instructions canceled");
                }
                let absolute = context.workspace_root.join(name);
                let Ok(content) = std::fs::read_to_string(&absolute) else {
                    continue;
                };
                let header = if content.chars().count() > INSTRUCTION_FILE_CHAR_LIMIT {
                    format!(
                        "# {name} (first {INSTRUCTION_FILE_CHAR_LIMIT} of {} chars)",
                        content.chars().count()
                    )
                } else {
                    format!("# {name} ({} chars)", content.chars().count())
                };
                let truncated: String = content
                    .chars()
                    .take(INSTRUCTION_FILE_CHAR_LIMIT)
                    .collect();
                sections.push(format!("{header}\n{truncated}"));
            }
            if sections.is_empty() {
                return ToolResult::done(
                    format!(
                        "no repository instruction files found at the workspace root (looked for {})",
                        INSTRUCTION_FILES.join(", ")
                    ),
                    Vec::new(),
                );
            }
            finish(redact_secrets(&sections.join("\n\n")), 60_000)
        }),
    };

    registry.register(git_status)?;
    registry.register(git_diff)?;
    registry.register(git_log)?;
    registry.register(repo_instructions)?;
    Ok(())
}

/// Bound already-redacted text, collapsing the (unreachable) bound failure
/// into an error result.
fn finish(redacted: String, max_chars: usize) -> ToolResult {
    match bound_output(&redacted, max_chars) {
        Ok(bounded) => ToolResult::done(bounded, Vec::new()),
        Err(_) => ToolResult::fail("tool output exceeded its bound"),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use std::path::PathBuf;

    use crate::registry::{ToolContext, ToolRegistry};

    use super::register_git_tools;

    fn ws() -> PathBuf {
        tempfile::Builder::new()
            .prefix("relay-git-")
            .tempdir()
            .expect("temp dir")
            .keep()
    }

    fn registry() -> ToolRegistry {
        let mut registry = ToolRegistry::new();
        register_git_tools(&mut registry).expect("registers");
        registry
    }

    #[test]
    fn registers_the_read_only_tools() {
        let registry = registry();
        for name in ["git-status", "git-diff", "git-log", "repo-instructions"] {
            assert!(registry.has(name), "missing {name}");
        }
    }

    #[test]
    fn reports_not_a_repository() {
        let registry = registry();
        let context = ToolContext::new(ws());
        let tool = registry.get("git-status").expect("tool");
        let result = crate::registry::call_tool(&tool, &serde_json::json!({}), &context);
        assert!(!result.ok);
        let text = result.output;
        assert!(
            text.contains("is not a git repository"),
            "unexpected output: {text}"
        );
    }
}
