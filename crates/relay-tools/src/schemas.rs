//! Shared argument-schema helpers for tool definitions.
//!
//! Port of `src/tools/schemas.ts` (retired TypeScript tree, removed from
//! the repository 2026-10-14). In the TypeScript implementation tools
//! compose Zod builders; here each tool declares the JSON Schema
//! (draft 2020-12, byte-compatible with the Zod `toJSONSchema` output that
//! providers see) plus the runtime-only refinements Zod carried in `.refine`
//! calls. The registry validates incoming arguments against the same schema
//! and then runs these refinements, so every model-supplied argument is
//! validated the same way before a tool body runs.

use serde_json::{Value, json};

/// Refinement failure message for workspace-relative path arguments.
pub const RELATIVE_PATH_MESSAGE: &str = "path must be workspace-relative: not absolute, no \"~\" shortcut, no drive letter, no NUL byte, and no \"..\" segments";

/// Refinement failure message for the `command` argument of `run-command`.
pub const COMMAND_METACHAR_MESSAGE: &str = "command must be an executable name or absolute path, not a shell string (metacharacters ; | & $ ` > < and newlines are rejected); pass arguments in `args`";

/// Refinement failure message for NUL bytes inside `args` entries.
pub const ARGS_NUL_MESSAGE: &str = "arguments must not contain NUL bytes";

/// Port of `isSafeRelativePath`: non-empty, bounded, no absolutes, "~", drive
/// letters, NUL bytes, or ".." segments (on every platform, so Windows-shaped
/// paths are refused even on Unix).
pub fn is_safe_relative_path(value: &str) -> bool {
    !value.contains('\0')
        && !value.is_empty()
        && !value.starts_with('/')
        && !value.starts_with('\\')
        && !(value
            .chars()
            .next()
            .is_some_and(|first| first.is_ascii_alphabetic())
            && value.chars().nth(1) == Some(':'))
        && value != "~"
        && !value.starts_with("~/")
        && !value.starts_with("~\\")
        && !value.split(['/', '\\']).any(|segment| segment == "..")
}

/// Shell metacharacters forbidden in a bare command name.
const COMMAND_METACHARS: [char; 9] = [';', '|', '&', '$', '`', '>', '<', '\n', '\r'];

/// True when the command value is an absolute path or free of shell
/// metacharacters (port of the `commandArg` Zod refinement).
pub fn is_safe_command(value: &str) -> bool {
    if value.contains('\0') {
        return false;
    }
    if std::path::Path::new(value).is_absolute() {
        return true;
    }
    !value.chars().any(|c| COMMAND_METACHARS.contains(&c))
}

/// A rendered refinement issue: `path: message`.
fn issue(path: &str, message: &str) -> String {
    format!("{path}: {message}")
}

/// Refinement for tools whose `path` argument must be workspace-relative
/// (read-file, list-dir, write-file, git-diff).
pub fn relative_path_refinement(args: &Value) -> Vec<String> {
    let Some(path) = args.get("path").and_then(Value::as_str) else {
        return Vec::new();
    };
    if is_safe_relative_path(path) {
        return Vec::new();
    }
    vec![issue("path", RELATIVE_PATH_MESSAGE)]
}

/// Refinements for `run-command`: shell-free command names and NUL-free
/// argument entries.
pub fn command_arg_refinements(args: &Value) -> Vec<String> {
    let mut issues = Vec::new();
    if let Some(command) = args.get("command").and_then(Value::as_str)
        && !is_safe_command(command)
    {
        issues.push(issue("command", COMMAND_METACHAR_MESSAGE));
    }
    if let Some(entries) = args.get("args").and_then(Value::as_array) {
        for (index, entry) in entries.iter().enumerate() {
            if let Some(text) = entry.as_str()
                && text.contains('\0')
            {
                issues.push(issue(&format!("args.{index}"), ARGS_NUL_MESSAGE));
            }
        }
    }
    issues
}

// ---------------------------------------------------------------------------
// Schema builders producing the exact Zod `toJSONSchema` shapes
// ---------------------------------------------------------------------------

/// A bounded string property.
pub fn string_arg(description: &str, min_length: u64, max_length: u64) -> Value {
    json!({
        "description": description,
        "type": "string",
        "minLength": min_length,
        "maxLength": max_length,
    })
}

/// A bounded string property with a default applied at parse time.
pub fn string_arg_with_default(
    description: &str,
    min_length: u64,
    max_length: u64,
    default: &str,
) -> Value {
    json!({
        "default": default,
        "description": description,
        "type": "string",
        "minLength": min_length,
        "maxLength": max_length,
    })
}

/// An integer property with bounds.
pub fn integer_arg(description: &str, minimum: i64, maximum: i64) -> Value {
    json!({
        "description": description,
        "type": "integer",
        "minimum": minimum,
        "maximum": maximum,
    })
}

/// An integer property with bounds and a default.
pub fn integer_arg_with_default(
    description: &str,
    minimum: i64,
    maximum: i64,
    default: i64,
) -> Value {
    json!({
        "default": default,
        "description": description,
        "type": "integer",
        "minimum": minimum,
        "maximum": maximum,
    })
}

/// A boolean property with a default.
pub fn boolean_arg_with_default(description: &str, default: bool) -> Value {
    json!({
        "default": default,
        "description": description,
        "type": "boolean",
    })
}

/// A bounded array-of-strings property with a default.
pub fn string_array_arg_with_default(
    description: &str,
    max_items: u64,
    item_min_length: u64,
    item_max_length: u64,
    default: &[&str],
) -> Value {
    json!({
        "default": default,
        "description": description,
        "maxItems": max_items,
        "type": "array",
        "items": {
            "type": "string",
            "minLength": item_min_length,
            "maxLength": item_max_length,
        },
    })
}

/// An array of variable-name strings (the `env` argument of `run-command`).
pub fn env_name_array_arg(description: &str, max_items: u64) -> Value {
    json!({
        "description": description,
        "maxItems": max_items,
        "type": "array",
        "items": {
            "type": "string",
            "pattern": "^[A-Za-z_][A-Za-z0-9_]*$",
        },
    })
}

/// Assemble an object schema in the Zod `toJSONSchema({target: 'draft-2020-12'})`
/// shape: `$schema`, `type`, `properties`, `required` (omitted when empty —
/// matching Zod, which omits `required` for empty objects), and
/// `additionalProperties: false`.
///
/// `required` must list every property except genuinely optional ones
/// (`.optional()` in the reference), including properties that carry
/// defaults: the provider-facing Zod output-target schema marks them
/// required, while the runtime validator built by
/// [`crate::registry::ToolArgsSchema`] relaxes them again so a missing
/// defaulted field parses like a Zod `.default()`.
pub fn object_arg(properties: serde_json::Map<String, Value>, required: &[&str]) -> Value {
    let mut schema = serde_json::Map::new();
    schema.insert(
        "$schema".to_owned(),
        json!("https://json-schema.org/draft/2020-12/schema"),
    );
    schema.insert("type".to_owned(), json!("object"));
    schema.insert("properties".to_owned(), Value::Object(properties));
    if !required.is_empty() {
        schema.insert(
            "required".to_owned(),
            Value::Array(
                required
                    .iter()
                    .map(|name| Value::String((*name).to_owned()))
                    .collect(),
            ),
        );
    }
    schema.insert("additionalProperties".to_owned(), json!(false));
    Value::Object(schema)
}

/// The `path` property shared by the filesystem tools.
pub fn path_property() -> Value {
    string_arg(
        "Workspace-relative path (never absolute, never containing \"..\").",
        1,
        1000,
    )
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn safe_relative_paths_match_the_reference_predicate() {
        for value in [
            "a.txt",
            "src/deep/file.rs",
            "a\\b\\c.txt",
            ".hidden",
            "a..b",
        ] {
            assert!(is_safe_relative_path(value), "{value} should be safe");
        }
        for value in [
            "",
            "/etc/passwd",
            "\\Windows\\win.ini",
            "C:/Users/x",
            "~",
            "~/secret",
            "~\\secret",
            "../escape",
            "a/../b",
            "a\\..\\b",
            "a\0b",
        ] {
            assert!(!is_safe_relative_path(value), "{value} should be unsafe");
        }
    }

    #[test]
    fn safe_commands_allow_absolute_paths_and_plain_names_only() {
        assert!(is_safe_command("node"));
        assert!(is_safe_command("/usr/bin/node"));
        assert!("/usr/bin/node".starts_with('/'));
        for value in [
            "echo hi; rm -rf /",
            "a|b",
            "a&b",
            "a$b",
            "a`b`",
            "a>b",
            "a<b",
            "a\nb",
            "a\rb",
            "a\0b",
        ] {
            assert!(!is_safe_command(value), "{value} should be refused");
        }
    }

    #[test]
    fn command_refinements_report_field_and_index_paths() {
        let args = json!({
            "command": "echo hi; rm -rf /",
            "args": ["ok", "bad\0nul"],
        });
        let issues = command_arg_refinements(&args);
        assert_eq!(
            issues,
            vec![
                format!("command: {COMMAND_METACHAR_MESSAGE}"),
                format!("args.1: {ARGS_NUL_MESSAGE}"),
            ]
        );
    }

    #[test]
    fn relative_path_refinement_uses_the_reference_message() {
        let issues = relative_path_refinement(&json!({ "path": "../evil.txt" }));
        assert_eq!(issues, vec![format!("path: {RELATIVE_PATH_MESSAGE}")]);
        assert!(relative_path_refinement(&json!({ "path": "ok.txt" })).is_empty());
    }
}
