//! Workspace text search tool.
//!
//! Ported from `src/tools/search.ts`. Walks the workspace without following
//! symlinks, skipping vendored/build directories and oversized files, and
//! matches per line: literal substring by default, or a caller-supplied
//! regular expression. Output is always redacted and bounded, with a
//! match/file summary line.

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::capability::ToolCapability;
use regex::Regex;
use relay_config::ToolSettings;
use serde_json::json;

use crate::path_guard::resolve_workspace_path;
use crate::redact::{bound_output, redact_secrets};
use crate::registry::{
    ToolArgsSchema, ToolContext, ToolDefinition, ToolRegistrationError, ToolRegistry, ToolResult,
};
use crate::schemas::{object_arg, string_arg};

const SEARCH_TIMEOUT_MS: u64 = 30_000;
const SEARCH_DEADLINE_MS: u64 = 25_000;
const MAX_OUTPUT_CHARS: usize = 30_000;

/// Directory names never descended into.
const SKIPPED_DIR_NAMES: [&str; 5] = ["node_modules", ".git", "dist", ".relay", "coverage"];

/// Hard cap on files scanned in one search.
const MAX_FILES: usize = 5_000;

/// Convert a simple wildcard glob ('*.ts') to an anchored regular expression.
/// `*` matches any characters (including separators, so a bare '*.ts' also
/// describes nested files); `?` matches one non-separator character.
/// Returns `None` only if the escaped pattern fails to compile, which the
/// tests show it never does; callers then match nothing (fail closed).
pub(crate) fn glob_to_regex(glob: &str) -> Option<Regex> {
    let mut escaped = String::new();
    for character in glob.chars() {
        match character {
            '*' => escaped.push_str(".*"),
            '?' => escaped.push_str("[^/]"),
            '.' | '+' | '^' | '$' | '{' | '}' | '(' | ')' | '|' | '[' | ']' | '\\' => {
                escaped.push('\\');
                escaped.push(character);
            }
            other => escaped.push(other),
        }
    }
    Regex::new(&format!("^{escaped}$")).ok()
}

/// Register the search-text tool.
pub fn register_search_tools(
    registry: &mut ToolRegistry,
    settings: &ToolSettings,
) -> Result<(), ToolRegistrationError> {
    let mut properties = serde_json::Map::new();
    properties.insert(
        "query".to_owned(),
        string_arg(
            "Literal substring to find, or a regular expression when regex is true.",
            1,
            500,
        ),
    );
    properties.insert(
        "regex".to_owned(),
        crate::schemas::boolean_arg_with_default(
            "Interpret `query` as a JavaScript regular expression.",
            false,
        ),
    );
    properties.insert(
        "includeGlob".to_owned(),
        string_arg(
            "Simple filename filter such as '*.ts' (wildcards, no traversal).",
            1,
            200,
        ),
    );
    properties.insert(
        "maxResults".to_owned(),
        crate::schemas::integer_arg("Stop after this many matching lines.", 1, 10_000),
    );
    let schema = ToolArgsSchema::new(object_arg(properties, &["query", "regex"]), None)?;
    let max_search_results = settings.max_search_results.max(1) as usize;
    let max_file_bytes = settings.max_file_bytes.max(0) as u64;

    let search_text = ToolDefinition {
        name: "search-text".into(),
        title: "Search workspace text".into(),
        description: "Search all text files under the workspace root for a literal string (or regular expression with regex=true). Returns `path:line: text` matches and a summary.".into(),
        capability: ToolCapability::Read,
        args_schema: Arc::new(schema),
        timeout_ms: SEARCH_TIMEOUT_MS,
        max_output_chars: MAX_OUTPUT_CHARS,
        execute: Arc::new(move |args, context| {
            let requested_limit = args["maxResults"].as_u64().map(|value| value as usize);
            let max_results = requested_limit
                .unwrap_or(max_search_results)
                .min(max_search_results);

            let use_regex = args["regex"].as_bool().unwrap_or(false);
            let query = args["query"].as_str().unwrap_or_default();
            let regex: Option<Regex> = if use_regex {
                match Regex::new(query) {
                    Ok(regex) => Some(regex),
                    Err(error) => {
                        return ToolResult::fail(format!(
                            "invalid regular expression /{query}/: {error}; fix the pattern or pass regex=false for a literal search"
                        ));
                    }
                }
            } else {
                None
            };
            let matches_line = |line: &str| match &regex {
                Some(pattern) => pattern.is_match(line),
                None => line.contains(query),
            };

            let include_glob = args["includeGlob"].as_str();
            let (name_filter, matches_path): (Option<Regex>, bool) = match include_glob {
                Some(glob) => {
                    if glob.contains("..") || glob.starts_with('/') {
                        return ToolResult::fail(format!(
                            "invalid includeGlob \"{glob}\": use a simple pattern like '*.ts'"
                        ));
                    }
                    (glob_to_regex(glob), glob.contains('/'))
                }
                None => (None, false),
            };

            let Ok(root) = resolve_workspace_path(&context.workspace_root, ".") else {
                return ToolResult::fail("cannot resolve workspace");
            };

            let deadline = Instant::now() + Duration::from_millis(SEARCH_DEADLINE_MS);
            let name_filter_ref = name_filter.as_ref();
            let mut files: Vec<String> = Vec::new();
            let mut aborted = false;
            let mut deadline_hit = false;
            collect_files(
                &root,
                "",
                context,
                &deadline,
                name_filter_ref,
                matches_path,
                &mut files,
                &mut aborted,
                &mut deadline_hit,
            );

            if aborted {
                return ToolResult::fail("search-text canceled");
            }

            let mut matched_files: Vec<String> = Vec::new();
            let mut out: Vec<String> = Vec::new();
            let mut stopped_at_limit = false;
            'files: for relative in files {
                if context.canceled() {
                    return ToolResult::fail("search-text canceled");
                }
                let absolute = root.join(&relative);
                let Ok(info) = fs::metadata(&absolute) else {
                    continue;
                };
                if !info.is_file() || info.len() > max_file_bytes {
                    continue;
                }
                let Ok(bytes) = fs::read(&absolute) else {
                    continue;
                };
                if bytes.contains(&0) {
                    continue;
                }
                let content = String::from_utf8_lossy(&bytes);
                for (index, line) in content.split('\n').enumerate() {
                    if !matches_line(line) {
                        continue;
                    }
                    out.push(format!("{}:{}: {}", relative, index + 1, line.trim()));
                    if !matched_files.iter().any(|seen| seen == &relative) {
                        matched_files.push(relative.clone());
                    }
                    if out.len() >= max_results {
                        stopped_at_limit = true;
                        break 'files;
                    }
                }
            }

            if context.canceled() {
                return ToolResult::fail("search-text canceled");
            }

            let match_count = out.len();
            let files_count = matched_files.len();
            let match_word = if match_count == 1 { "match" } else { "matches" };
            let file_word = if files_count == 1 { "file" } else { "files" };
            let mut summary = format!("{match_count} {match_word} in {files_count} {file_word}");
            if stopped_at_limit {
                summary.push_str(" (stopped at limit)");
            }
            if deadline_hit {
                summary.push_str(" (stopped early: scan deadline reached)");
            }
            let body = if out.is_empty() {
                if use_regex {
                    format!("no matches for /{query}/")
                } else {
                    format!("no matches for {}", serde_json::to_string(query).unwrap_or_default())
                }
            } else {
                out.join("\n")
            };
            let text = format!("{body}\n{summary}");
            let output = match bound_output(&redact_secrets(&text), MAX_OUTPUT_CHARS) {
                Ok(output) => output,
                Err(error) => return ToolResult::fail(error.to_string()),
            };
            ToolResult {
                ok: true,
                outcome: crate::registry::ToolOutcome::Ok,
                output,
                data: Some(json!({
                    "matches": match_count,
                    "files": files_count,
                    "stoppedAtLimit": stopped_at_limit,
                })),
                affected_files: Vec::new(),
            }
        }),
    };
    registry.register(search_text)?;
    Ok(())
}

/// Walk the workspace collecting candidate files (deterministically sorted by
/// name at each level) without following symlinks. `name_filter` selects
/// files by name (or by whole relative path when `matches_path`).
#[allow(clippy::too_many_arguments)] // one recursive walker state bag
fn collect_files(
    dir: &Path,
    prefix: &str,
    context: &ToolContext,
    deadline: &Instant,
    name_filter: Option<&Regex>,
    matches_path: bool,
    files: &mut Vec<String>,
    aborted: &mut bool,
    deadline_hit: &mut bool,
) {
    if *aborted || files.len() >= MAX_FILES {
        return;
    }
    if context.canceled() {
        *aborted = true;
        return;
    }
    if Instant::now() > *deadline {
        *deadline_hit = true;
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut collected: Vec<(String, bool, bool)> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(file_type) = entry.file_type().ok() else {
            continue;
        };
        collected.push((name, file_type.is_dir(), file_type.is_symlink()));
    }
    collected.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, is_dir, is_symlink) in collected {
        if context.canceled() {
            *aborted = true;
            return;
        }
        let relative = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        if is_dir {
            if !SKIPPED_DIR_NAMES.contains(&name.as_str()) {
                collect_files(
                    &dir.join(&name),
                    &relative,
                    context,
                    deadline,
                    name_filter,
                    matches_path,
                    files,
                    aborted,
                    deadline_hit,
                );
            }
            continue;
        }
        if is_symlink {
            continue;
        }
        if let Some(filter) = name_filter {
            let candidate = if matches_path {
                relative.as_str()
            } else {
                name.as_str()
            };
            if !filter.is_match(candidate) {
                continue;
            }
        }
        files.push(relative);
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::registry::call_tool;
    use serde_json::json;
    use std::path::PathBuf;

    #[test]
    fn glob_patterns_compile_and_match_filenames() {
        let ts = glob_to_regex("*.ts").expect("compiles");
        assert!(ts.is_match("a.ts"));
        assert!(ts.is_match("deep/b.ts"));
        assert!(!ts.is_match("a.js"));
        let fixed = glob_to_regex("a?c.txt").expect("compiles");
        assert!(fixed.is_match("abc.txt"));
        assert!(!fixed.is_match("a/c.txt"));
    }

    fn settings(max_search_results: i64, max_file_bytes: i64) -> ToolSettings {
        ToolSettings {
            command_timeout_ms: 10_000,
            max_search_results,
            max_file_bytes,
        }
    }

    fn registry(max_search_results: i64, max_file_bytes: i64) -> ToolRegistry {
        let mut registry = ToolRegistry::new();
        register_search_tools(&mut registry, &settings(max_search_results, max_file_bytes))
            .expect("registers");
        registry
    }

    fn ws() -> PathBuf {
        tempfile::Builder::new()
            .prefix("relay-search-")
            .tempdir()
            .expect("temp dir")
            .keep()
    }

    fn seed(workspace: &Path) {
        fs::create_dir_all(workspace.join("node_modules/pkg")).expect("mkdir");
        fs::create_dir(workspace.join("sub")).expect("mkdir");
        fs::write(
            workspace.join("a.ts"),
            "import x\nconst needle = 1\nexport { x }\n",
        )
        .expect("write");
        fs::write(
            workspace.join("b.md"),
            "# doc\nneedle here\nand needle again\n",
        )
        .expect("write");
        fs::write(workspace.join("sub/c.ts"), "NEEDLE uppercase\n").expect("write");
        fs::write(
            workspace.join("node_modules/pkg/vendor.ts"),
            "needle hidden\n",
        )
        .expect("write");
    }

    #[test]
    fn finds_literal_matches_with_file_and_line_numbers() {
        let workspace = ws();
        seed(&workspace);
        let tool = registry(200, 1_000_000).get("search-text").expect("tool");
        let result = call_tool(
            &tool,
            &json!({ "query": "needle" }),
            &ToolContext::new(&workspace),
        );
        assert!(result.ok, "{}", result.output);
        assert!(
            result.output.contains("a.ts:2: const needle = 1"),
            "{}",
            result.output
        );
        assert!(
            result.output.contains("b.md:2: needle here"),
            "{}",
            result.output
        );
        assert!(
            result.output.contains("b.md:3: and needle again"),
            "{}",
            result.output
        );
        assert!(
            result.output.contains("3 matches in 2 files"),
            "{}",
            result.output
        );
        assert!(!result.output.contains("node_modules"), "{}", result.output);
        assert!(
            !result.output.contains("NEEDLE uppercase"),
            "{}",
            result.output
        );
    }

    #[test]
    fn skips_vendored_directories_entirely() {
        let workspace = ws();
        seed(&workspace);
        let tool = registry(200, 1_000_000).get("search-text").expect("tool");
        let result = call_tool(
            &tool,
            &json!({ "query": "hidden" }),
            &ToolContext::new(&workspace),
        );
        assert!(result.ok, "{}", result.output);
        assert!(
            result.output.contains("0 matches in 0 files"),
            "{}",
            result.output
        );
    }

    #[test]
    fn supports_regular_expression_mode_and_literal_mode_treats_regex_syntax_as_text() {
        let workspace = ws();
        seed(&workspace);
        let tool = registry(200, 1_000_000).get("search-text").expect("tool");
        let regex = call_tool(
            &tool,
            &json!({ "query": "nee[d]+le", "regex": true }),
            &ToolContext::new(&workspace),
        );
        assert!(regex.ok, "{}", regex.output);
        assert!(
            regex.output.contains("3 matches in 2 files"),
            "{}",
            regex.output
        );
        let literal = call_tool(
            &tool,
            &json!({ "query": "nee[d]+le" }),
            &ToolContext::new(&workspace),
        );
        assert!(literal.ok, "{}", literal.output);
        assert!(
            literal.output.contains("0 matches in 0 files"),
            "{}",
            literal.output
        );
    }

    #[test]
    fn reports_invalid_regular_expressions_with_an_actionable_message() {
        let workspace = ws();
        let tool = registry(200, 1_000_000).get("search-text").expect("tool");
        let result = call_tool(
            &tool,
            &json!({ "query": "a(b", "regex": true }),
            &ToolContext::new(&workspace),
        );
        assert!(!result.ok);
        assert!(
            result.output.contains("invalid regular expression"),
            "{}",
            result.output
        );
        assert!(result.output.contains("regex=false"), "{}", result.output);
    }

    #[test]
    fn stops_at_the_configured_result_limit() {
        let workspace = ws();
        seed(&workspace);
        let tool = registry(2, 1_000_000).get("search-text").expect("tool");
        let result = call_tool(
            &tool,
            &json!({ "query": "needle" }),
            &ToolContext::new(&workspace),
        );
        assert!(result.ok, "{}", result.output);
        assert!(
            result.output.contains("(stopped at limit)"),
            "{}",
            result.output
        );
        let matching_lines: Vec<&str> = result
            .output
            .lines()
            .filter(|line| {
                let mut parts = line.split(':');
                let first = parts.next().unwrap_or_default();
                let second = parts.next().unwrap_or_default();
                !first.is_empty() && second.parse::<usize>().is_ok()
            })
            .collect();
        assert_eq!(matching_lines.len(), 2, "{}", result.output);
    }

    #[test]
    fn honors_include_glob_filters() {
        let workspace = ws();
        seed(&workspace);
        let tool = registry(200, 1_000_000).get("search-text").expect("tool");
        let result = call_tool(
            &tool,
            &json!({ "query": "needle", "includeGlob": "*.md" }),
            &ToolContext::new(&workspace),
        );
        assert!(result.ok, "{}", result.output);
        assert!(result.output.contains("b.md:2:"), "{}", result.output);
        assert!(!result.output.contains("a.ts"), "{}", result.output);
        assert!(
            result.output.contains("2 matches in 1 file"),
            "{}",
            result.output
        );
    }

    #[test]
    fn rejects_traversal_in_include_glob() {
        let workspace = ws();
        let tool = registry(200, 1_000_000).get("search-text").expect("tool");
        let result = call_tool(
            &tool,
            &json!({ "query": "needle", "includeGlob": "../**" }),
            &ToolContext::new(&workspace),
        );
        assert!(!result.ok);
        assert!(
            result.output.contains("invalid includeGlob"),
            "{}",
            result.output
        );
    }

    #[test]
    fn skips_files_larger_than_the_byte_limit() {
        let workspace = ws();
        seed(&workspace);
        fs::write(
            workspace.join("huge.log"),
            format!("needle\n{}\n", "y".repeat(5_000)),
        )
        .expect("write");
        let tool = registry(200, 1_000).get("search-text").expect("tool");
        let result = call_tool(
            &tool,
            &json!({ "query": "needle" }),
            &ToolContext::new(&workspace),
        );
        assert!(result.ok, "{}", result.output);
        assert!(!result.output.contains("huge.log"), "{}", result.output);
    }
}
