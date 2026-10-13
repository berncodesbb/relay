//! Bounded subprocess tool.
//!
//! Ported from `src/tools/process.ts`. Runs a single executable with an
//! argument array — never a shell string — in the workspace (or a
//! workspace-relative subdirectory), with an explicit environment allowlist,
//! separate stream-collected stdout/stderr, a timeout that terminates the
//! whole process tree, and cancellation support.

use std::sync::Arc;

use crate::capability::ToolCapability;
use relay_config::ToolSettings;
use serde_json::json;

use crate::path_guard::resolve_workspace_path;
use crate::redact::{bound_output, env_name_looks_secret, redact_secrets, sanitize_env_for_record};
use crate::registry::{
    ToolArgsSchema, ToolDefinition, ToolRegistrationError, ToolRegistry, ToolResult,
};
use crate::schemas::{
    command_arg_refinements, env_name_array_arg, integer_arg, object_arg, string_arg,
    string_array_arg_with_default,
};
use crate::spawn::{BoundedSpawnRequest, spawn_bounded};

const MAX_OUTPUT_CHARS: usize = 40_000;

/// Render one command-line part for the echoed display line, JSON-quoting
/// values that contain whitespace or quote characters.
fn display_arg(value: &str) -> String {
    if value
        .chars()
        .any(|c| c.is_whitespace() || c == '"' || c == '\'' || c == '\\')
    {
        serde_json::to_string(value).unwrap_or_else(|_| value.to_owned())
    } else {
        value.to_owned()
    }
}

/// Register the run-command tool.
pub fn register_process_tools(
    registry: &mut ToolRegistry,
    settings: &ToolSettings,
) -> Result<(), ToolRegistrationError> {
    let mut properties = serde_json::Map::new();
    properties.insert(
        "command".to_owned(),
        string_arg(
            "Executable name or absolute path. Never a shell string.",
            1,
            500,
        ),
    );
    properties.insert(
        "args".to_owned(),
        string_array_arg_with_default(
            "Argument array passed verbatim (no shell interpretation).",
            200,
            0,
            20_000,
            &[],
        ),
    );
    properties.insert(
        "cwd".to_owned(),
        string_arg(
            "Workspace-relative working directory; defaults to the workspace root.",
            1,
            1000,
        ),
    );
    properties.insert(
        "timeoutMs".to_owned(),
        integer_arg(
            "Kill the process group after this long (capped by settings).",
            100,
            600_000,
        ),
    );
    properties.insert(
        "env".to_owned(),
        env_name_array_arg(
            "Names of extra environment variables to inherit (names only; secret-shaped names are refused).",
            50,
        ),
    );
    let schema = ToolArgsSchema::new(
        object_arg(properties, &["command", "args"]),
        Some(command_arg_refinements),
    )?;
    let command_timeout_ms = settings.command_timeout_ms.max(1) as u64;

    let run_command = ToolDefinition {
        name: "run-command".into(),
        title: "Run command".into(),
        description: "Run an executable with an argument array inside the workspace (no shell). Bounded env inheritance, separate stdout/stderr, timeout and cancellation kill the whole process group.".into(),
        capability: ToolCapability::Execute,
        args_schema: Arc::new(schema),
        timeout_ms: command_timeout_ms,
        max_output_chars: MAX_OUTPUT_CHARS,
        execute: Arc::new(move |args, context| {
            if context.canceled() {
                return ToolResult::fail("run-command canceled before it started");
            }

            let command = args["command"].as_str().unwrap_or_default().to_owned();
            let arg_list: Vec<String> = args["args"]
                .as_array()
                .map(|entries| {
                    entries
                        .iter()
                        .filter_map(|entry| entry.as_str().map(str::to_owned))
                        .collect::<Vec<String>>()
                })
                .unwrap_or_default();
            let cwd = match args["cwd"].as_str() {
                Some(cwd_arg) => match resolve_workspace_path(&context.workspace_root, cwd_arg) {
                    Ok(path) => path,
                    Err(error) => return ToolResult::fail(error.to_string()),
                },
                None => context.workspace_root.clone(),
            };

            let requested_timeout = args["timeoutMs"].as_u64();
            let timeout_ms = requested_timeout
                .unwrap_or(command_timeout_ms)
                .min(command_timeout_ms);
            let extra_env = sanitize_env_for_record(
                &args["env"]
                    .as_array()
                    .map(|entries| {
                        entries
                            .iter()
                            .filter_map(|entry| entry.as_str().map(str::to_owned))
                            .collect::<Vec<String>>()
                    })
                    .unwrap_or_default(),
            );
            let refused: Vec<String> = extra_env
                .iter()
                .filter(|name| env_name_looks_secret(name))
                .cloned()
                .collect();
            if !refused.is_empty() {
                return ToolResult::fail(format!(
                    "refusing to inherit secret-looking environment variables: {}. Secret-shaped names are never passed to child processes; set a non-secret variable instead.",
                    refused.join(", ")
                ));
            }
            let display_parts: Vec<String> = std::iter::once(command.clone())
                .chain(arg_list.iter().cloned())
                .map(|part| display_arg(&part))
                .collect();
            let display = display_parts.join(" ");

            context.log(&format!(
                "run-command: {command} ({} args, timeout {timeout_ms}ms)",
                arg_list.len()
            ));

            let request = BoundedSpawnRequest {
                command: command.clone(),
                args: arg_list.clone(),
                cwd: cwd.clone(),
                timeout_ms,
                signal: context.signal.clone(),
                extra_env: extra_env.clone(),
                max_stream_chars: None,
                env_source: None,
            };
            let result = spawn_bounded(request);

            let env_note = if extra_env.is_empty() {
                String::new()
            } else {
                format!("\nextra env inherited: {}", extra_env.join(", "))
            };
            let exit_text = result
                .exit_code
                .map(|code| code.to_string())
                .unwrap_or_else(|| "unknown".to_owned());
            let signal_note = result
                .term_signal
                .as_ref()
                .map(|signal| format!(" (signal {signal})"))
                .unwrap_or_default();
            let status_line = if let Some(spawn_error) = &result.spawn_error {
                format!("failed to start: {spawn_error}")
            } else if result.canceled {
                "canceled before completion".to_owned()
            } else if result.timed_out {
                format!("timed out after {timeout_ms}ms")
            } else {
                format!("exit code: {exit_text}{signal_note}")
            };

            let cwd_relative = cwd
                .strip_prefix(&context.workspace_root)
                .ok()
                .map(|rel| rel.to_string_lossy().into_owned())
                .filter(|rel| !rel.is_empty())
                .unwrap_or_else(|| ".".to_owned());
            let body = [
                format!("$ {display} (cwd: {cwd_relative})"),
                status_line,
                format!("duration: {}ms{env_note}", result.duration_ms),
                "--- stdout ---".to_owned(),
                if result.stdout.is_empty() {
                    "(empty)".to_owned()
                } else {
                    result.stdout.clone()
                },
                "--- stderr ---".to_owned(),
                if result.stderr.is_empty() {
                    "(empty)".to_owned()
                } else {
                    result.stderr.clone()
                },
            ]
            .join("\n");
            let output = match bound_output(&redact_secrets(&body), MAX_OUTPUT_CHARS) {
                Ok(output) => output,
                Err(error) => return ToolResult::fail(error.to_string()),
            };

            let ok = !result.canceled
                && !result.timed_out
                && result.spawn_error.is_none()
                && result.exit_code == Some(0);
            ToolResult {
                ok,
                outcome: if ok {
                    crate::registry::ToolOutcome::Ok
                } else {
                    crate::registry::ToolOutcome::Error
                },
                output,
                data: Some(json!({
                    "exitCode": result.exit_code,
                    "termSignal": result.term_signal,
                    "durationMs": result.duration_ms,
                    "timedOut": result.timed_out,
                    "canceled": result.canceled,
                    "envInherited": sanitize_env_for_record(&result.inherited_env),
                })),
                affected_files: Vec::new(),
            }
        }),
    };
    registry.register(run_command)?;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::registry::{ToolContext, call_tool};
    use serde_json::json;
    use std::path::PathBuf;

    fn settings(command_timeout_ms: i64) -> ToolSettings {
        ToolSettings {
            command_timeout_ms,
            max_search_results: 200,
            max_file_bytes: 1_000_000,
        }
    }

    fn registry(command_timeout_ms: i64) -> ToolRegistry {
        let mut registry = ToolRegistry::new();
        register_process_tools(&mut registry, &settings(command_timeout_ms)).expect("registers");
        registry
    }

    fn ws() -> PathBuf {
        tempfile::Builder::new()
            .prefix("relay-process-")
            .tempdir()
            .expect("temp dir")
            .keep()
    }

    #[cfg(unix)]
    #[test]
    fn runs_a_command_and_reports_exit_code_and_stdout() {
        let workspace = ws();
        let tool = registry(10_000).get("run-command").expect("tool");
        let result = call_tool(
            &tool,
            &json!({ "command": "/bin/echo", "args": ["hello relay"] }),
            &ToolContext::new(&workspace),
        );
        assert!(result.ok, "{}", result.output);
        assert!(result.output.contains("exit code: 0"), "{}", result.output);
        assert!(result.output.contains("hello relay"), "{}", result.output);
        assert!(
            result.output.contains("--- stderr ---"),
            "{}",
            result.output
        );
        assert!(result.affected_files.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn passes_arguments_with_metacharacters_verbatim_no_shell() {
        let workspace = ws();
        let tricky = [
            "a b;c",
            "x|y",
            "$HOME",
            "`id`",
            "\"quoted\"",
            "single'quote",
        ];
        let tool = registry(10_000).get("run-command").expect("tool");
        let result = call_tool(
            &tool,
            &json!({
                "command": "/bin/echo",
                "args": tricky,
            }),
            &ToolContext::new(&workspace),
        );
        assert!(result.ok, "{}", result.output);
        let stdout = result
            .output
            .split("--- stdout ---")
            .nth(1)
            .and_then(|section| section.split("--- stderr ---").next())
            .map(str::trim)
            .unwrap_or_default();
        assert_eq!(stdout, tricky.join(" "));
    }

    #[test]
    fn rejects_shell_strings_as_the_command() {
        let workspace = ws();
        let tool = registry(10_000).get("run-command").expect("tool");
        let result = call_tool(
            &tool,
            &json!({ "command": "echo hi; rm -rf /", "args": [] }),
            &ToolContext::new(&workspace),
        );
        assert!(!result.ok);
        assert!(
            result.output.contains("invalid arguments"),
            "{}",
            result.output
        );
        assert!(result.output.contains("shell"), "{}", result.output);
    }

    #[test]
    fn rejects_an_empty_command() {
        let workspace = ws();
        let tool = registry(10_000).get("run-command").expect("tool");
        let result = call_tool(
            &tool,
            &json!({ "command": "", "args": [] }),
            &ToolContext::new(&workspace),
        );
        assert!(!result.ok);
        assert!(
            result.output.contains("invalid arguments"),
            "{}",
            result.output
        );
    }

    #[cfg(unix)]
    #[test]
    fn kills_the_process_group_on_timeout() {
        let workspace = ws();
        let tool = registry(500).get("run-command").expect("tool");
        let result = call_tool(
            &tool,
            &json!({ "command": "/bin/sleep", "args": ["60"] }),
            &ToolContext::new(&workspace),
        );
        assert!(!result.ok);
        assert!(
            result.output.contains("timed out after 500ms"),
            "{}",
            result.output
        );
    }

    #[cfg(unix)]
    #[test]
    fn kills_the_process_group_promptly_on_abort() {
        let workspace = ws();
        let context = ToolContext::new(&workspace);
        let signal = context.signal.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(150));
            signal.cancel("test");
        });
        let tool = registry(10_000).get("run-command").expect("tool");
        let started = std::time::Instant::now();
        let result = call_tool(
            &tool,
            &json!({ "command": "/bin/sleep", "args": ["60"] }),
            &context,
        );
        assert!(!result.ok);
        assert!(result.output.contains("canceled"), "{}", result.output);
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
    }

    #[cfg(unix)]
    #[test]
    fn does_not_inherit_non_allowlisted_environment_variables() {
        let workspace = ws();
        // The child sees only the allowlist, so an arbitrary parent variable
        // is absent even though the parent process has it.
        let tool = registry(10_000).get("run-command").expect("tool");
        let result = call_tool(
            &tool,
            &json!({
                "command": "/bin/sh",
                "args": ["-c", "echo \"value=${RELAY_TOOLS_PROBE_UNSET:-absent}\""]
            }),
            &ToolContext::new(&workspace),
        );
        assert!(result.ok, "{}", result.output);
        assert!(result.output.contains("absent"), "{}", result.output);
    }

    #[cfg(unix)]
    #[test]
    fn inherits_base_variables_like_path() {
        let workspace = ws();
        let tool = registry(10_000).get("run-command").expect("tool");
        let result = call_tool(
            &tool,
            &json!({
                "command": "/bin/sh",
                "args": ["-c", "if [ -n \"$PATH\" ] && [ -n \"$HOME\" ]; then echo env-ok; else echo env-missing; fi"]
            }),
            &ToolContext::new(&workspace),
        );
        assert!(result.ok, "{}", result.output);
        assert!(result.output.contains("env-ok"), "{}", result.output);
    }

    #[test]
    fn refuses_to_inherit_secret_looking_variables_even_when_named() {
        let workspace = ws();
        let tool = registry(10_000).get("run-command").expect("tool");
        let result = call_tool(
            &tool,
            &json!({
                "command": "/bin/echo",
                "args": ["1"],
                "env": ["RELAY_TEST_API_KEY"],
            }),
            &ToolContext::new(&workspace),
        );
        assert!(!result.ok);
        assert!(
            result.output.contains("refusing to inherit secret-looking"),
            "{}",
            result.output
        );
        assert!(result.output.contains("RELAY_TEST_API_KEY"));
    }

    #[test]
    fn reports_a_missing_executable_clearly() {
        let workspace = ws();
        let tool = registry(10_000).get("run-command").expect("tool");
        let result = call_tool(
            &tool,
            &json!({
                "command": "definitely-not-a-real-binary-xyz",
                "args": ["--flag"],
            }),
            &ToolContext::new(&workspace),
        );
        assert!(!result.ok);
        assert!(
            result.output.contains("failed to start"),
            "{}",
            result.output
        );
    }

    #[test]
    fn rejects_bad_env_names_at_validation() {
        let workspace = ws();
        let tool = registry(10_000).get("run-command").expect("tool");
        let result = call_tool(
            &tool,
            &json!({ "command": "/bin/echo", "args": [], "env": ["BAD-NAME"] }),
            &ToolContext::new(&workspace),
        );
        assert!(!result.ok);
        assert!(
            result.output.contains("invalid arguments"),
            "{}",
            result.output
        );
        assert!(
            result
                .output
                .contains("env entries must be variable names only"),
            "{}",
            result.output
        );
    }

    #[cfg(unix)]
    #[test]
    fn reports_non_zero_exit_codes_as_errors() {
        let workspace = ws();
        let tool = registry(10_000).get("run-command").expect("tool");
        let result = call_tool(
            &tool,
            &json!({ "command": "/bin/sh", "args": ["-c", "exit 3"] }),
            &ToolContext::new(&workspace),
        );
        assert!(!result.ok);
        assert!(result.output.contains("exit code: 3"), "{}", result.output);
    }

    #[cfg(unix)]
    #[test]
    fn honors_a_workspace_relative_cwd() {
        let workspace = ws();
        std::fs::create_dir(workspace.join("inner")).expect("mkdir");
        let tool = registry(10_000).get("run-command").expect("tool");
        let result = call_tool(
            &tool,
            &json!({
                "command": "/bin/sh",
                "args": ["-c", "pwd"],
                "cwd": "inner",
            }),
            &ToolContext::new(&workspace),
        );
        assert!(result.ok, "{}", result.output);
        assert!(result.output.contains("inner"), "{}", result.output);
    }

    #[cfg(unix)]
    #[test]
    fn refuses_a_cwd_that_leaves_the_workspace() {
        let workspace = ws();
        let tool = registry(10_000).get("run-command").expect("tool");
        let result = call_tool(
            &tool,
            &json!({ "command": "/bin/echo", "args": [], "cwd": "../elsewhere" }),
            &ToolContext::new(&workspace),
        );
        assert!(!result.ok);
        assert!(
            result.output.contains("refusing workspace path"),
            "{}",
            result.output
        );
    }

    #[cfg(unix)]
    #[test]
    fn bounds_oversized_stdout() {
        let workspace = ws();
        let tool = registry(10_000).get("run-command").expect("tool");
        let result = call_tool(
            &tool,
            &json!({
                "command": "/bin/sh",
                "args": ["-c", "yes zzz | head -c 300000"],
            }),
            &ToolContext::new(&workspace),
        );
        assert!(result.ok, "{}", result.output);
        assert!(result.output.chars().count() <= 40_000);
        assert!(result.output.contains("truncated"), "{}", result.output);
    }
}
