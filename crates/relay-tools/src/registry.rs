//! Tool registry contracts.
//!
//! Ported from `src/tools/registry.ts` (retired TypeScript tree, removed
//! from the repository 2026-10-14). Tools are schema-validated,
//! capability-classified, time-bounded, and cancelable. The registry is the
//! only surface the agent loop uses; it never spawns processes or touches the
//! filesystem directly.
//!
//! ## Argument validation
//!
//! The TypeScript runtime parses tool arguments with Zod and shows providers
//! the Zod-generated JSON Schema. The Rust port validates with the
//! `jsonschema` crate (draft 2020-12) compiled from the same schema the
//! provider sees, then:
//!
//! 1. unknown properties are stripped (Zod objects strip rather than reject,
//!    even though the provider-facing schema is closed),
//! 2. defaults are applied like Zod `.default()`,
//! 3. per-field refinements run with the reference messages
//!    (see [`crate::schemas`]).
//!
//! Issues render as `path: message` joined by `; `, matching the agent loop's
//! Zod issue rendering.

use std::sync::Arc;

use crate::capability::ToolCapability;
use relay_providers::contract::{CancelToken, JsonSchemaObject, ToolSpec};
use serde_json::Value;
use thiserror::Error;

/// Whether a tool call succeeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolOutcome {
    /// The tool ran to completion.
    Ok,
    /// The tool failed.
    Error,
}

/// The result of one tool execution.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolResult {
    /// Whether the tool succeeded.
    pub ok: bool,
    /// Success classification (kept as an explicit field like the reference).
    pub outcome: ToolOutcome,
    /// Human/model-readable output, already redacted and truncated by the tool.
    pub output: String,
    /// Structured payload for callers that want more than text.
    pub data: Option<Value>,
    /// Workspace-relative paths the tool changed (write tools only).
    pub affected_files: Vec<String>,
}

impl ToolResult {
    /// A failing result with no affected files.
    pub fn fail(output: impl Into<String>) -> Self {
        ToolResult {
            ok: false,
            outcome: ToolOutcome::Error,
            output: output.into(),
            data: None,
            affected_files: Vec::new(),
        }
    }

    /// A successful result.
    pub fn done(output: impl Into<String>, affected_files: Vec<String>) -> Self {
        ToolResult {
            ok: true,
            outcome: ToolOutcome::Ok,
            output: output.into(),
            data: None,
            affected_files,
        }
    }
}

/// A tool log sink.
pub type ToolLog = Arc<dyn Fn(&str) + Send + Sync>;

/// Everything a tool needs to run.
#[derive(Clone)]
pub struct ToolContext {
    /// Resolved absolute workspace root; tools must not escape it.
    pub workspace_root: std::path::PathBuf,
    /// Cancellation signal.
    pub signal: CancelToken,
    /// Diagnostic log sink (messages are recorded, never printed here).
    pub log: ToolLog,
}

impl ToolContext {
    /// A context rooted at `root` with a no-op log.
    pub fn new(workspace_root: impl Into<std::path::PathBuf>) -> Self {
        ToolContext {
            workspace_root: workspace_root.into(),
            signal: CancelToken::new(),
            log: Arc::new(|_message| {}),
        }
    }

    /// Whether the cancellation signal fired.
    pub fn canceled(&self) -> bool {
        self.signal.is_cancelled()
    }

    /// Emit a log line through the sink.
    pub fn log(&self, message: &str) {
        (self.log)(message);
    }
}

/// A per-tool refinement over parsed arguments, returning rendered issues
/// (`path: message`). Mirrors Zod `.refine` checks that JSON Schema cannot
/// express.
pub type RefineFn = fn(&Value) -> Vec<String>;

/// A tool's argument schema: the provider-facing JSON Schema (draft 2020-12)
/// plus a compiled validator and the runtime refinements.
pub struct ToolArgsSchema {
    json: Value,
    validator: Option<jsonschema::Validator>,
    refine: Option<RefineFn>,
}

impl ToolArgsSchema {
    /// Compile a schema. The provider-facing copy is kept verbatim (the
    /// Zod `toJSONSchema` output-target shape, where defaulted fields are
    /// required); the validator is compiled from an internal copy without
    /// `additionalProperties` and without defaulted fields in `required`,
    /// so unknown keys are stripped (not rejected) and missing defaulted
    /// fields pass, exactly like a Zod object parse. Compilation failures
    /// are returned so registration fails closed.
    pub fn new(json: Value, refine: Option<RefineFn>) -> Result<Self, SchemaCompileError> {
        let mut validation_copy = json.clone();
        if let Some(object) = validation_copy.as_object_mut() {
            object.remove("additionalProperties");
            let defaulted: Vec<String> = object
                .get("properties")
                .and_then(Value::as_object)
                .map(|properties| {
                    properties
                        .iter()
                        .filter(|(_, property)| property.get("default").is_some())
                        .map(|(name, _)| name.clone())
                        .collect()
                })
                .unwrap_or_default();
            if let Some(required) = object.get_mut("required").and_then(Value::as_array_mut) {
                required.retain(|name| !defaulted.iter().any(|defaulted| defaulted == name));
            }
        }
        let validator = jsonschema::validator_for(&validation_copy)
            .map_err(|error| SchemaCompileError(error.to_string()))?;
        Ok(ToolArgsSchema {
            json,
            validator: Some(validator),
            refine,
        })
    }

    /// The provider-facing JSON Schema.
    pub fn json(&self) -> &Value {
        &self.json
    }

    /// Validate arguments: structural checks, then defaults + stripping,
    /// then refinements. Returns the cleaned argument object.
    pub fn parse(&self, args: &Value) -> Result<Value, ArgumentIssues> {
        let mut issues = Vec::new();
        if let Some(validator) = &self.validator {
            for error in validator.iter_errors(args) {
                issues.push(render_issue(&error, &self.json));
            }
        }
        if !issues.is_empty() {
            return Err(ArgumentIssues(issues));
        }
        let cleaned = apply_defaults_and_strip(args, &self.json);
        if let Some(refine) = self.refine {
            issues.extend(refine(&cleaned));
        }
        if issues.is_empty() {
            Ok(cleaned)
        } else {
            Err(ArgumentIssues(issues))
        }
    }
}

/// A tool schema failed to compile.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("tool argument schema failed to compile: {0}")]
pub struct SchemaCompileError(pub String);

impl From<SchemaCompileError> for ToolRegistrationError {
    fn from(error: SchemaCompileError) -> Self {
        ToolRegistrationError(error.to_string())
    }
}

/// Rendered argument issues (`path: message` each).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{}", self.0.join("; "))]
pub struct ArgumentIssues(pub Vec<String>);

/// Render one `jsonschema` validation error with Zod-issue wording.
fn render_issue(error: &jsonschema::ValidationError<'_>, schema: &Value) -> String {
    let path = dot_path(error.instance_path().to_string().as_str());
    use jsonschema::error::TypeKind;
    use jsonschema::error::ValidationErrorKind;
    let message = match error.kind() {
        ValidationErrorKind::Type { kind } => {
            let expected = match kind {
                TypeKind::Single(single) => zod_expected_type(*single).to_owned(),
                TypeKind::Multiple(set) => set
                    // Not produced by these schemas; report the first type.
                    .iter()
                    .next()
                    .map(zod_expected_type)
                    .unwrap_or("unknown")
                    .to_owned(),
            };
            format!(
                "Invalid input: expected {expected}, received {}",
                zod_instance_type(error.instance())
            )
        }
        ValidationErrorKind::MinLength { limit } => {
            format!("Too small: expected string to have >={limit} characters")
        }
        ValidationErrorKind::MaxLength { limit } => {
            format!("Too big: expected string to have <={limit} characters")
        }
        ValidationErrorKind::Minimum { limit } => {
            format!("Too small: expected number to be >={}", trim_number(limit))
        }
        ValidationErrorKind::Maximum { limit } => {
            format!("Too big: expected number to be <={}", trim_number(limit))
        }
        ValidationErrorKind::MinItems { limit } => {
            format!("Too small: expected array to have >={limit} items")
        }
        ValidationErrorKind::MaxItems { limit } => {
            format!("Too big: expected array to have <={limit} items")
        }
        ValidationErrorKind::Pattern { pattern } => match pattern.as_str() {
            "^[A-Za-z_][A-Za-z0-9_]*$" => "env entries must be variable names only".to_owned(),
            other => format!("Invalid string: must match pattern /{other}/"),
        },
        ValidationErrorKind::Required { property } => {
            // JSON Schema reports missing properties at the parent object;
            // Zod reports them at the missing property's own path.
            let property = property.as_str().unwrap_or_default().to_owned();
            let expected = schema
                .get("properties")
                .and_then(|properties| properties.get(property.as_str()))
                .and_then(|property| property.get("type"))
                .and_then(Value::as_str)
                .map(zod_type_name)
                .unwrap_or("unknown");
            return format!("{property}: Invalid input: expected {expected}, received undefined");
        }
        other => {
            // Unrecognized keyword: fall back to the keyword name, bounded.
            let fallback = format!("Invalid input: failed the {} check", other.keyword());
            fallback.chars().take(200).collect()
        }
    };
    format!("{path}: {message}")
}

/// The Zod spelling of a JSON Schema type.
fn zod_type_name(json_type: &str) -> &'static str {
    match json_type {
        "string" => "string",
        "integer" => "int",
        "number" => "number",
        "boolean" => "boolean",
        "array" => "array",
        "object" => "object",
        "null" => "null",
        _ => "unknown",
    }
}

fn zod_expected_type(json_type: jsonschema::JsonType) -> &'static str {
    zod_type_name(json_type.as_str())
}

/// The Zod spelling of an instance's JSON type.
fn zod_instance_type(instance: &Value) -> &'static str {
    match instance {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Numbers render without a trailing `.0`, like Zod's messages.
fn trim_number(limit: &Value) -> String {
    let text = limit.to_string();
    text.trim_end_matches(".0").to_owned()
}

/// Convert a JSON-pointer instance path to a Zod dot path.
fn dot_path(pointer: &str) -> String {
    let trimmed = pointer.strip_prefix('/').unwrap_or(pointer);
    if trimmed.is_empty() {
        return "(root)".to_owned();
    }
    trimmed
        .split('/')
        .map(|segment| segment.replace("~1", "/").replace("~0", "~"))
        .collect::<Vec<_>>()
        .join(".")
}

/// Apply schema defaults and strip unknown keys, like a Zod object parse of a
/// valid instance.
fn apply_defaults_and_strip(args: &Value, schema: &Value) -> Value {
    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        return args.clone();
    };
    let Some(instance) = args.as_object() else {
        return args.clone();
    };
    let mut cleaned = serde_json::Map::new();
    for (name, property) in properties {
        match instance.get(name) {
            Some(value) => {
                cleaned.insert(name.clone(), value.clone());
            }
            None => {
                if let Some(default) = property.get("default") {
                    cleaned.insert(name.clone(), default.clone());
                }
            }
        }
    }
    Value::Object(cleaned)
}

/// The executor a tool registers: receives parsed (validated, defaulted)
/// arguments.
pub type ToolExecute = Arc<dyn Fn(&Value, &ToolContext) -> ToolResult + Send + Sync>;

/// A registered tool definition. Port of the TypeScript `ToolDefinition`
/// with the executor boxed instead of a method.
#[derive(Clone)]
pub struct ToolDefinition {
    /// Kebab-case tool name.
    pub name: String,
    /// Human-readable title.
    pub title: String,
    /// Model-facing description.
    pub description: String,
    /// Side-effect class.
    pub capability: ToolCapability,
    /// Argument schema (provider-facing JSON Schema + refinements).
    pub args_schema: Arc<ToolArgsSchema>,
    /// Execution budget in milliseconds (enforced by the runtime).
    pub timeout_ms: u64,
    /// Hard cap on returned output length in characters.
    pub max_output_chars: usize,
    /// The tool body; never called with invalid arguments.
    pub execute: ToolExecute,
}

/// A tool name is not registered.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("Unknown tool \"{0}\". Registered tools are listed by /status and the tool registry.")]
pub struct UnknownToolError(pub String);

/// A tool could not be registered.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("Invalid tool registration: {0}")]
pub struct ToolRegistrationError(pub String);

/// Decides whether a tool call may run. Deterministic policy code, never a
/// model.
pub type PermissionGate = dyn Fn(&ToolDefinition, &Value) -> PermissionDecision + Send + Sync;

/// One permission decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionDecision {
    /// The call may run.
    Allow,
    /// The call must not run; `needs_user` marks decisions a user could
    /// override.
    Deny {
        /// Why the call was denied.
        reason: String,
        /// Whether user intervention could permit this call.
        needs_user: bool,
    },
}

/// The tool registry: register/lookup plus provider-facing spec generation.
#[derive(Default)]
pub struct ToolRegistry {
    tools: Vec<Arc<ToolDefinition>>,
}

impl ToolRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        ToolRegistry::default()
    }

    /// Register a tool, mirroring the reference validation errors.
    pub fn register(&mut self, definition: ToolDefinition) -> Result<(), ToolRegistrationError> {
        let is_kebab = !definition.name.is_empty()
            && definition
                .name
                .chars()
                .next()
                .is_some_and(|first| first.is_ascii_lowercase())
            && definition
                .name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
        if !is_kebab {
            return Err(ToolRegistrationError(format!(
                "tool name \"{}\" must be kebab-case",
                definition.name
            )));
        }
        if self.tools.iter().any(|tool| tool.name == definition.name) {
            return Err(ToolRegistrationError(format!(
                "tool \"{}\" is already registered",
                definition.name
            )));
        }
        if definition.timeout_ms == 0 || definition.max_output_chars == 0 {
            return Err(ToolRegistrationError(format!(
                "tool \"{}\" needs positive timeout and output limits",
                definition.name
            )));
        }
        self.tools.push(Arc::new(definition));
        Ok(())
    }

    /// Look up a tool by name.
    pub fn get(&self, name: &str) -> Result<Arc<ToolDefinition>, UnknownToolError> {
        self.tools
            .iter()
            .find(|tool| tool.name == name)
            .cloned()
            .ok_or_else(|| UnknownToolError(name.to_owned()))
    }

    /// Whether a tool with this name is registered.
    pub fn has(&self, name: &str) -> bool {
        self.tools.iter().any(|tool| tool.name == name)
    }

    /// Every registered tool, in registration order.
    pub fn list(&self) -> Vec<Arc<ToolDefinition>> {
        self.tools.clone()
    }

    /// Tools classified with one capability.
    pub fn list_by_capability(&self, capability: ToolCapability) -> Vec<Arc<ToolDefinition>> {
        self.tools
            .iter()
            .filter(|tool| tool.capability == capability)
            .cloned()
            .collect()
    }

    /// Provider-facing tool specs with JSON Schema argument descriptions.
    pub fn to_provider_tools(&self, capabilities: Option<&[ToolCapability]>) -> Vec<ToolSpec> {
        self.tools
            .iter()
            .filter(|tool| capabilities.is_none_or(|set| set.contains(&tool.capability)))
            .map(|tool| ToolSpec {
                name: tool.name.clone(),
                description: tool.description.clone(),
                parameters: tool_json_schema(tool),
            })
            .collect()
    }
}

/// JSON Schema describing a tool's arguments, as shown to providers.
pub fn tool_json_schema(tool: &ToolDefinition) -> JsonSchemaObject {
    match tool.args_schema.json() {
        Value::Object(map) => map.clone(),
        _ => serde_json::Map::new(),
    }
}

/// Validate tool arguments through the tool's schema, as the runtime does.
/// Failures surface as [`ArgumentIssues`] with Zod-style rendering.
pub fn parse_tool_arguments(tool: &ToolDefinition, args: &Value) -> Result<Value, ArgumentIssues> {
    tool.args_schema.parse(args)
}

/// Validate arguments through the tool's schema (as the runtime does) and
/// execute. Schema failures surface as an error result naming the issues —
/// the test-kit behavior the TypeScript suite exercises.
pub fn call_tool(tool: &ToolDefinition, args: &Value, context: &ToolContext) -> ToolResult {
    match tool.args_schema.parse(args) {
        Ok(parsed) => (tool.execute)(&parsed, context),
        Err(issues) => ToolResult::fail(format!("invalid arguments: {issues}")),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use serde_json::json;

    fn definition(name: &str) -> ToolDefinition {
        ToolDefinition {
            name: name.to_owned(),
            title: "Test".into(),
            description: "A test tool.".into(),
            capability: ToolCapability::Read,
            args_schema: Arc::new(
                ToolArgsSchema::new(
                    json!({
                        "$schema": "https://json-schema.org/draft/2020-12/schema",
                        "type": "object",
                        "properties": {
                            "path": { "type": "string", "minLength": 1, "maxLength": 5 },
                            "count": { "type": "integer", "minimum": 1, "maximum": 10, "default": 2 },
                        },
                        "required": ["path"],
                        "additionalProperties": false,
                    }),
                    None,
                )
                .expect("schema compiles"),
            ),
            timeout_ms: 1_000,
            max_output_chars: 100,
            execute: Arc::new(|args, _context| {
                ToolResult::done(serde_json::to_string(args).unwrap(), Vec::new())
            }),
        }
    }

    #[test]
    fn parses_apply_defaults_and_strip_unknown_keys() {
        let tool = definition("test-tool");
        let parsed = tool
            .args_schema
            .parse(&json!({ "path": "abc", "extra": true }))
            .expect("valid");
        assert_eq!(
            parsed,
            json!({ "path": "abc", "count": 2 }),
            "unknown keys stripped, default applied"
        );
    }

    #[test]
    fn renders_zod_style_issue_messages() {
        let tool = definition("test-tool");
        let issues = tool
            .args_schema
            .parse(&json!({ "path": 123 }))
            .expect_err("type error");
        assert_eq!(
            issues.0,
            vec!["path: Invalid input: expected string, received number".to_owned()]
        );

        let issues = tool
            .args_schema
            .parse(&json!({}))
            .expect_err("missing property");
        assert_eq!(
            issues.0,
            vec!["path: Invalid input: expected string, received undefined".to_owned()]
        );

        let issues = tool
            .args_schema
            .parse(&json!({ "path": "toolongvalue" }))
            .expect_err("max length");
        assert_eq!(
            issues.0,
            vec!["Too big: expected string to have <=5 characters".to_owned()]
                .into_iter()
                .map(|message| format!("path: {message}"))
                .collect::<Vec<_>>()
        );

        let issues = tool
            .args_schema
            .parse(&json!({ "path": "a", "count": 0 }))
            .expect_err("below minimum");
        assert_eq!(
            issues.0,
            vec!["count: Too small: expected number to be >=1".to_owned()]
        );

        let issues = tool
            .args_schema
            .parse(&json!({ "path": "a", "count": 1.5 }))
            .expect_err("float for int");
        assert_eq!(
            issues.0,
            vec!["count: Invalid input: expected int, received number".to_owned()]
        );
    }

    #[test]
    fn non_object_arguments_report_a_root_issue() {
        let tool = definition("test-tool");
        let issues = tool.args_schema.parse(&json!(5)).expect_err("root type");
        assert_eq!(
            issues.0,
            vec!["(root): Invalid input: expected object, received number".to_owned()]
        );
    }

    #[test]
    fn call_tool_reports_invalid_arguments() {
        let tool = definition("test-tool");
        let result = call_tool(&tool, &json!({ "path": 7 }), &ToolContext::new("/tmp"));
        assert!(!result.ok);
        assert!(
            result.output.starts_with("invalid arguments: path: "),
            "{}",
            result.output
        );
    }

    #[test]
    fn registration_validates_names_duplicates_and_limits() {
        let mut registry = ToolRegistry::new();
        registry
            .register(definition("read-file"))
            .expect("first ok");
        let duplicate = registry.register(definition("read-file"));
        assert_eq!(
            duplicate.unwrap_err().to_string(),
            "Invalid tool registration: tool \"read-file\" is already registered"
        );
        assert_eq!(
            registry
                .register(definition("BadName"))
                .unwrap_err()
                .to_string(),
            "Invalid tool registration: tool name \"BadName\" must be kebab-case"
        );
        let mut zero_timeout = definition("zero-timeout");
        zero_timeout.timeout_ms = 0;
        assert_eq!(
            registry.register(zero_timeout).unwrap_err().to_string(),
            "Invalid tool registration: tool \"zero-timeout\" needs positive timeout and output limits"
        );
        assert!(
            matches!(registry.get("no-such-tool"), Err(UnknownToolError(name)) if name == "no-such-tool")
        );
    }

    #[test]
    fn unknown_tool_error_carries_the_reference_message() {
        let error = UnknownToolError("ghost".into());
        assert_eq!(
            error.to_string(),
            "Unknown tool \"ghost\". Registered tools are listed by /status and the tool registry."
        );
    }

    #[test]
    fn to_provider_tools_filters_by_capability() {
        let mut registry = ToolRegistry::new();
        registry
            .register(definition("read-file"))
            .expect("registered");
        let specs = registry.to_provider_tools(None);
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].parameters["additionalProperties"], json!(false));
        let by_capability = registry.to_provider_tools(Some(&[ToolCapability::Write]));
        assert!(by_capability.is_empty());
        let by_capability = registry.to_provider_tools(Some(&[ToolCapability::Read]));
        assert_eq!(by_capability.len(), 1);
    }
}
