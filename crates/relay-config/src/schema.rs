//! Configuration schema: strict, actionable validation of one layer.
//!
//! Relay configuration is YAML, validated strictly: unknown providers, models,
//! routing keys, or out-of-range values are rejected with actionable messages
//! shaped like the TypeScript reference's Zod diagnostics (`path: message`).
//! Configuration never contains credentials — only the names of environment
//! variables that hold them.
//!
//! Routing entries may reference a named model role from the `models` catalog
//! (a role-name string such as `reasoning`) or use the legacy inline form (a
//! provider/model object); both are valid in the same file. The raw
//! string/object union is a configuration-only shape: it is normalized before
//! any runtime use (see [`crate::normalize_routing`]) and never leaks past the
//! configuration boundary.

use crate::normalized::INTENT_PURPOSE;
use crate::raw::{RawMapping, RawValue};
use indexmap::IndexMap;
use serde::Serialize;
use std::fmt;

/// Provider names accepted in configuration (`PROVIDER_NAMES` in the
/// TypeScript reference).
pub const PROVIDER_NAMES: [&str; 3] = ["openai", "anthropic", "fake"];

/// The configuration schema version (still 1 for the backward-compatible
/// role-catalog additions).
pub const CONFIG_SCHEMA_VERSION: i64 = 1;

/// The canonical role-name rule: lowercase kebab-case, 1-64 characters.
pub const ROLE_NAME_PATTERN: &str = "^[a-z][a-z0-9-]{0,63}$";

/// Diagnostic shown for role names that violate [`ROLE_NAME_PATTERN`].
pub const ROLE_NAME_MESSAGE: &str =
    "role names are lowercase kebab-case, 1-64 characters, e.g. reasoning, ux-writing";

/// Diagnostic shown for routing values that are neither a role name nor a
/// legacy inline route object.
pub const ROUTING_VALUE_MESSAGE: &str = "routing values must be a model-role name (a lowercase kebab-case string referencing a models entry) or an inline route object with provider and model";

/// Diagnostic shown for invalid autonomy bounds.
pub const AUTONOMY_MESSAGE: &str =
    "autonomy must be off, next-checkpoint, workflow, or until:<mode>";

/// The seven workflow modes, in canonical order.
pub const WORKFLOW_MODES: [&str; 7] = [
    "discovery",
    "specification",
    "technical-design",
    "planning",
    "implementation",
    "verification",
    "delivery",
];

/// Routing keys allowed in a layer: the seven workflow modes plus `intent`.
pub const ALLOWED_ROUTING_KEYS: [&str; 8] = [
    "discovery",
    "specification",
    "technical-design",
    "planning",
    "implementation",
    "verification",
    "delivery",
    INTENT_PURPOSE,
];

/// Evaluates the canonical [`ROLE_NAME_PATTERN`] rule for a candidate role
/// name (lowercase kebab-case, 1-64 characters, first character a letter).
pub fn is_valid_role_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_lowercase() => {}
        _ => return false,
    }
    let rest: usize = chars.count();
    if rest > 63 {
        return false;
    }
    name.chars()
        .skip(1)
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// A configured provider connection: infrastructure, not responsibility.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderName {
    /// The OpenAI adapter.
    OpenAi,
    /// The Anthropic adapter.
    Anthropic,
    /// The deterministic offline fake adapter.
    Fake,
}

impl ProviderName {
    /// The configuration spelling of this provider.
    pub fn as_str(&self) -> &'static str {
        match self {
            ProviderName::OpenAi => "openai",
            ProviderName::Anthropic => "anthropic",
            ProviderName::Fake => "fake",
        }
    }

    /// Parses a configured provider name.
    pub fn parse(value: &str) -> Option<ProviderName> {
        match value {
            "openai" => Some(ProviderName::OpenAi),
            "anthropic" => Some(ProviderName::Anthropic),
            "fake" => Some(ProviderName::Fake),
            _ => None,
        }
    }
}

impl fmt::Display for ProviderName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Optional per-route limits.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteLimits {
    /// Maximum output tokens for the route (256 ..= 1,000,000).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<i64>,
    /// Sampling temperature for the route (0 ..= 2).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
}

/// One fallback entry of a legacy inline route.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InlineFallback {
    /// The fallback's provider.
    pub provider: ProviderName,
    /// The fallback's model identifier.
    pub model: String,
}

/// A legacy inline route object (`{ provider, model, fallbacks?, limits? }`).
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteConfig {
    /// The route's provider.
    pub provider: ProviderName,
    /// The route's model identifier.
    pub model: String,
    /// Inline fallback routes (at most three).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallbacks: Option<Vec<InlineFallback>>,
    /// Optional per-route limits.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limits: Option<RouteLimits>,
}

/// A named model-role definition in the `models` catalog.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRoleConfig {
    /// The role's provider.
    pub provider: ProviderName,
    /// The role's model identifier.
    pub model: String,
    /// Role-name references, resolved against the catalog after layers merge
    /// (at most three).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallbacks: Option<Vec<String>>,
    /// Optional per-role limits.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limits: Option<RouteLimits>,
}

/// A routing value: a role-name reference or a legacy inline route object.
#[derive(Clone, Debug, PartialEq)]
pub enum RoutingValue {
    /// A reference to a `models` catalog role, such as `reasoning`.
    Role(String),
    /// A legacy inline route object.
    Inline(RouteConfig),
}

impl Serialize for RoutingValue {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        // A role reference renders as the bare role-name string and an inline
        // route as its object, matching the reference's YAML dump of the raw
        // string/object union.
        match self {
            RoutingValue::Role(name) => serializer.serialize_str(name),
            RoutingValue::Inline(route) => route.serialize(serializer),
        }
    }
}

/// Provider connection settings. `api_key_env` names an environment variable;
/// the key itself never lives in configuration.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSettings {
    /// Name of the environment variable that holds the API key.
    pub api_key_env: String,
    /// Optional proxy or self-hosted gateway endpoint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
}

/// The `providers` section: both vendor adapters, always present after
/// parsing (defaults fill omitted entries).
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Providers {
    /// OpenAI connection settings.
    pub openai: ProviderSettings,
    /// Anthropic connection settings.
    pub anthropic: ProviderSettings,
}

/// Budget limits for one run; every field has a default, so `budgets: {}` is
/// valid.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BudgetLimits {
    /// Agent-loop turns per run (1 ..= 500).
    pub max_turns: i64,
    /// Model calls per run (1 ..= 2,000).
    pub max_model_calls: i64,
    /// Tool executions per run (1 ..= 2,000).
    pub max_tool_calls: i64,
    /// Output tokens per run (1,024 ..= 10,000,000).
    pub max_output_tokens: i64,
    /// Wall-clock deadline in milliseconds (10,000 ..= 86,400,000).
    pub wall_clock_ms: i64,
    /// Identical tool calls allowed (2 ..= 20).
    pub max_repeated_actions: i64,
}

impl Default for BudgetLimits {
    fn default() -> BudgetLimits {
        BudgetLimits {
            max_turns: 24,
            max_model_calls: 48,
            max_tool_calls: 96,
            max_output_tokens: 400_000,
            wall_clock_ms: 1_800_000,
            max_repeated_actions: 3,
        }
    }
}

/// Tool execution settings; every field has a default.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolSettings {
    /// Child-process timeout in milliseconds (1,000 ..= 600,000).
    pub command_timeout_ms: i64,
    /// Maximum search results (1 ..= 10,000).
    pub max_search_results: i64,
    /// Maximum file read size in bytes (1,024 ..= 20,000,000).
    pub max_file_bytes: i64,
}

impl Default for ToolSettings {
    fn default() -> ToolSettings {
        ToolSettings {
            command_timeout_ms: 120_000,
            max_search_results: 200,
            max_file_bytes: 2_000_000,
        }
    }
}

/// A parsed, schema-valid configuration layer (or the merged effective
/// configuration, which satisfies the same shape).
///
/// Field order is the merged-object key order of the TypeScript reference and
/// drives serialization for `config show`-style dumps.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayConfig {
    /// Schema version (always 1 today).
    pub version: i64,
    /// Provider connection settings.
    pub providers: Providers,
    /// Named model-role catalog; absent (`None`) in legacy inline-only
    /// configurations.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub models: Option<IndexMap<String, ModelRoleConfig>>,
    /// Purpose-to-route table: the seven workflow modes plus optional
    /// purposes such as `intent`.
    pub routing: IndexMap<String, RoutingValue>,
    /// Run budget limits.
    pub budgets: BudgetLimits,
    /// Autonomy bound for the run.
    pub autonomy: String,
    /// Tool execution settings.
    pub tools: ToolSettings,
}

/// Validation failure for a configuration layer, listing every issue as
/// `path: message` under a source label (usually the layer file path).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigValidationError {
    /// Label of the configuration source the issues belong to.
    pub source: String,
    /// Formatted issues, in the TypeScript reference's report order.
    pub issues: Vec<String>,
}

impl ConfigValidationError {
    /// Builds an error from one or more already-formatted issues.
    pub fn new(source: impl Into<String>, issues: Vec<String>) -> ConfigValidationError {
        ConfigValidationError {
            source: source.into(),
            issues,
        }
    }
}

impl fmt::Display for ConfigValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Matches the TypeScript reference exactly: no trailing newline.
        writeln!(formatter, "Invalid Relay configuration in {}:", self.source)?;
        let issues = self
            .issues
            .iter()
            .map(|issue| format!("  - {issue}"))
            .collect::<Vec<_>>();
        write!(formatter, "{}", issues.join("\n"))
    }
}

impl std::error::Error for ConfigValidationError {}

/// One schema issue before formatting. `hard` marks the issue classes that
/// make a Zod union option "aborted"; inside a routing value any hard issue
/// collapses into the generic union diagnostic, while soft issues (string
/// minimum length, number range) surface with their own paths.
#[derive(Clone, Debug)]
struct Issue {
    path: String,
    message: String,
    hard: bool,
}

impl Issue {
    fn new(path: impl Into<String>, message: impl Into<String>, hard: bool) -> Issue {
        Issue {
            path: path.into(),
            message: message.into(),
            hard,
        }
    }

    fn formatted(&self) -> String {
        let path = if self.path.is_empty() {
            "(root)"
        } else {
            &self.path
        };
        format!("{path}: {}", self.message)
    }
}

fn invalid_input(expected: &str, received: &str) -> String {
    format!("Invalid input: expected {expected}, received {received}")
}

/// The provider section as filled in when the whole section is absent.
fn default_providers() -> Providers {
    Providers {
        openai: ProviderSettings {
            api_key_env: "OPENAI_API_KEY".to_string(),
            base_url: None,
        },
        anthropic: ProviderSettings {
            api_key_env: "ANTHROPIC_API_KEY".to_string(),
            base_url: None,
        },
    }
}

fn too_small_number(bound: i64) -> String {
    format!("Too small: expected number to be >={bound}")
}

fn too_big_number(bound: i64) -> String {
    format!("Too big: expected number to be <={bound}")
}

fn string_too_small() -> String {
    "Too small: expected string to have >=1 characters".to_string()
}

fn enum_option() -> String {
    "Invalid option: expected one of \"openai\"|\"anthropic\"|\"fake\"".to_string()
}

/// Parses a provider enum. Any input that is not one of the three names —
/// including the wrong type or a missing key — yields the enum diagnostic.
fn parse_provider(value: Option<&RawValue>) -> Result<ProviderName, Issue> {
    let name = match value {
        Some(RawValue::String(name)) => name,
        _ => return Err(Issue::new("", enum_option(), true)),
    };
    match ProviderName::parse(name) {
        Some(provider) => Ok(provider),
        None => Err(Issue::new("", enum_option(), true)),
    }
}

/// Parses a non-empty string. A too-short string is a soft issue; a non-string
/// is a hard one.
fn parse_nonempty_string(value: &RawValue) -> Result<String, Issue> {
    match value {
        RawValue::String(text) if !text.is_empty() => Ok(text.clone()),
        RawValue::String(_) => Err(Issue::new("", string_too_small(), false)),
        other => Err(Issue::new(
            "",
            invalid_input("string", other.type_name()),
            true,
        )),
    }
}

/// Parses an integer with bounds. Non-integral numbers and non-numbers are
/// hard issues; range violations are soft ones. Integer-valued floats
/// (`24.0`) are accepted, matching JavaScript number semantics.
fn parse_bounded_int(value: &RawValue, min: i64, max: i64) -> Result<i64, Issue> {
    match value {
        RawValue::Int(number) => check_int_range(*number, *number as f64, min, max),
        RawValue::Float(number) => {
            if number.is_nan() || number.fract() != 0.0 {
                Err(Issue::new("", invalid_input("int", "number"), true))
            } else {
                check_int_range(*number as i64, *number, min, max)
            }
        }
        other => Err(Issue::new(
            "",
            invalid_input("number", other.type_name()),
            true,
        )),
    }
}

fn check_int_range(as_int: i64, as_float: f64, min: i64, max: i64) -> Result<i64, Issue> {
    if as_float < min as f64 {
        Err(Issue::new("", too_small_number(min), false))
    } else if as_float > max as f64 {
        Err(Issue::new("", too_big_number(max), false))
    } else {
        Ok(as_int)
    }
}

/// Parses a floating-point number with bounds; range violations are soft.
fn parse_bounded_float(value: &RawValue, min: f64, max: f64) -> Result<f64, Issue> {
    let number = match value {
        RawValue::Int(number) => *number as f64,
        RawValue::Float(number) => *number,
        other => {
            return Err(Issue::new(
                "",
                invalid_input("number", other.type_name()),
                true,
            ));
        }
    };
    if number.is_nan() || number < min {
        Err(Issue::new("", too_small_number(min as i64), false))
    } else if number > max {
        Err(Issue::new("", too_big_number(max as i64), false))
    } else {
        Ok(number)
    }
}

/// Field-level helpers: look up `key` in `mapping`, parse it, and on failure
/// push an issue carrying the field's dotted path. Defaults apply when the key
/// is absent.
fn int_field(
    mapping: &RawMapping,
    key: &str,
    path: &str,
    min: i64,
    max: i64,
    default: i64,
    issues: &mut Vec<Issue>,
) -> i64 {
    match mapping.get(key) {
        None => default,
        Some(value) => match parse_bounded_int(value, min, max) {
            Ok(number) => number,
            Err(issue) => {
                issues.push(Issue::new(
                    format!("{path}.{key}"),
                    issue.message,
                    issue.hard,
                ));
                default
            }
        },
    }
}

fn float_field(
    mapping: &RawMapping,
    key: &str,
    path: &str,
    min: f64,
    max: f64,
    issues: &mut Vec<Issue>,
) -> Option<f64> {
    match mapping.get(key) {
        None => None,
        Some(value) => match parse_bounded_float(value, min, max) {
            Ok(number) => Some(number),
            Err(issue) => {
                issues.push(Issue::new(
                    format!("{path}.{key}"),
                    issue.message,
                    issue.hard,
                ));
                None
            }
        },
    }
}

/// Validates a `baseUrl` the way Zod's `.url()` check does for the corpus of
/// endpoints Relay supports: a scheme is required, and the special WHATWG
/// schemes (`http`, `https`, `ws`, `wss`, `ftp`) additionally require a
/// non-empty host.
fn is_valid_url(value: &str) -> bool {
    let Some(colon) = value.find(':') else {
        return false;
    };
    let scheme = &value[..colon];
    let mut chars = scheme.chars();
    let valid_scheme = match chars.next() {
        Some(first) if first.is_ascii_alphabetic() => {
            chars.all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.')
        }
        _ => false,
    };
    if !valid_scheme {
        return false;
    }
    const SPECIAL_SCHEMES: [&str; 5] = ["http", "https", "ws", "wss", "ftp"];
    if !SPECIAL_SCHEMES.contains(&scheme) {
        return true;
    }
    let rest = value[colon + 1..].trim_start_matches('/');
    match rest.find(['/', '?', '#']) {
        Some(end) => !rest[..end].is_empty(),
        None => !rest.is_empty(),
    }
}

fn parse_provider_settings(
    value: Option<&RawValue>,
    path: &str,
    absent_default_env: &str,
    issues: &mut Vec<Issue>,
) -> ProviderSettings {
    let Some(mapping) = value.and_then(RawValue::as_mapping) else {
        if let Some(other) = value {
            issues.push(Issue::new(
                path,
                invalid_input("object", other.type_name()),
                true,
            ));
        }
        return ProviderSettings {
            api_key_env: absent_default_env.to_string(),
            base_url: None,
        };
    };
    let api_key_env = match mapping.get("apiKeyEnv") {
        None => "OPENAI_API_KEY".to_string(),
        Some(RawValue::String(name)) if !name.is_empty() => name.clone(),
        Some(RawValue::String(_)) => {
            issues.push(Issue::new(
                format!("{path}.apiKeyEnv"),
                string_too_small(),
                false,
            ));
            "OPENAI_API_KEY".to_string()
        }
        Some(other) => {
            issues.push(Issue::new(
                format!("{path}.apiKeyEnv"),
                invalid_input("string", other.type_name()),
                true,
            ));
            "OPENAI_API_KEY".to_string()
        }
    };
    let base_url = match mapping.get("baseUrl") {
        None => None,
        Some(RawValue::String(url)) if is_valid_url(url) => Some(url.clone()),
        Some(RawValue::String(_)) => {
            issues.push(Issue::new(format!("{path}.baseUrl"), "Invalid URL", false));
            None
        }
        Some(other) => {
            issues.push(Issue::new(
                format!("{path}.baseUrl"),
                invalid_input("string", other.type_name()),
                true,
            ));
            None
        }
    };
    ProviderSettings {
        api_key_env,
        base_url,
    }
}

fn parse_limits(
    value: Option<&RawValue>,
    path: &str,
    issues: &mut Vec<Issue>,
) -> Option<RouteLimits> {
    let Some(mapping) = value.and_then(RawValue::as_mapping) else {
        if let Some(other) = value {
            issues.push(Issue::new(
                path,
                invalid_input("object", other.type_name()),
                true,
            ));
        }
        return None;
    };
    let max_output_tokens =
        float_int_limit_field(mapping, "maxOutputTokens", path, 256, 1_000_000, issues);
    let temperature = float_field(mapping, "temperature", path, 0.0, 2.0, issues);
    Some(RouteLimits {
        max_output_tokens,
        temperature,
    })
}

/// `maxOutputTokens` is an integer limit; `temperature` is a plain float.
fn float_int_limit_field(
    mapping: &RawMapping,
    key: &str,
    path: &str,
    min: i64,
    max: i64,
    issues: &mut Vec<Issue>,
) -> Option<i64> {
    match mapping.get(key) {
        None => None,
        Some(value) => match parse_bounded_int(value, min, max) {
            Ok(number) => Some(number),
            Err(issue) => {
                issues.push(Issue::new(
                    format!("{path}.{key}"),
                    issue.message,
                    issue.hard,
                ));
                None
            }
        },
    }
}

/// Parses a legacy inline route object, collecting every issue (used directly
/// inside the `models`-free catalog context and wrapped by the routing-value
/// union).
fn parse_route_config(value: &RawValue, path: &str) -> (RouteConfig, Vec<Issue>) {
    let mut issues = Vec::new();
    let empty = RouteConfig {
        provider: ProviderName::OpenAi,
        model: String::new(),
        fallbacks: None,
        limits: None,
    };
    let Some(mapping) = value.as_mapping() else {
        issues.push(Issue::new(
            path,
            invalid_input("object", value.type_name()),
            true,
        ));
        return (empty, issues);
    };
    let provider = match parse_provider(mapping.get("provider")) {
        Ok(provider) => provider,
        Err(issue) => {
            issues.push(Issue::new(
                format!("{path}.provider"),
                issue.message,
                issue.hard,
            ));
            empty.provider
        }
    };
    let model = match parse_nonempty_string(mapping.get("model").unwrap_or(&RawValue::Null)) {
        Ok(model) => model,
        Err(issue) => {
            issues.push(Issue::new(
                format!("{path}.model"),
                issue.message,
                issue.hard,
            ));
            String::new()
        }
    };
    let fallbacks = parse_inline_fallbacks(mapping.get("fallbacks"), path, &mut issues);
    let limits = parse_limits(
        mapping.get("limits"),
        &format!("{path}.limits"),
        &mut issues,
    );
    (
        RouteConfig {
            provider,
            model,
            fallbacks,
            limits,
        },
        issues,
    )
}

fn parse_inline_fallbacks(
    value: Option<&RawValue>,
    path: &str,
    issues: &mut Vec<Issue>,
) -> Option<Vec<InlineFallback>> {
    let raw = value?;
    let RawValue::Sequence(items) = raw else {
        issues.push(Issue::new(
            format!("{path}.fallbacks"),
            invalid_input("array", raw.type_name()),
            true,
        ));
        return None;
    };
    let mut fallbacks = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let item_path = format!("{path}.fallbacks.{index}");
        let Some(mapping) = item.as_mapping() else {
            issues.push(Issue::new(
                item_path,
                invalid_input("object", item.type_name()),
                true,
            ));
            continue;
        };
        let provider = match parse_provider(mapping.get("provider")) {
            Ok(provider) => provider,
            Err(issue) => {
                issues.push(Issue::new(
                    format!("{item_path}.provider"),
                    issue.message,
                    issue.hard,
                ));
                ProviderName::OpenAi
            }
        };
        let model = match parse_nonempty_string(mapping.get("model").unwrap_or(&RawValue::Null)) {
            Ok(model) => model,
            Err(issue) => {
                issues.push(Issue::new(
                    format!("{item_path}.model"),
                    issue.message,
                    issue.hard,
                ));
                String::new()
            }
        };
        fallbacks.push(InlineFallback { provider, model });
    }
    if items.len() > 3 {
        issues.push(Issue::new(
            format!("{path}.fallbacks"),
            "Too big: expected array to have <=3 items",
            true,
        ));
    }
    if issues.iter().any(|issue| issue.hard) {
        // Keep the element issues (the caller decides how to present them);
        // the list is only consulted when parsing failed anyway.
        return None;
    }
    Some(fallbacks)
}

/// Parses a named model-role definition, collecting every issue.
fn parse_model_role(value: &RawValue, path: &str) -> (ModelRoleConfig, Vec<Issue>) {
    let mut issues = Vec::new();
    let empty = ModelRoleConfig {
        provider: ProviderName::OpenAi,
        model: String::new(),
        fallbacks: None,
        limits: None,
    };
    let Some(mapping) = value.as_mapping() else {
        issues.push(Issue::new(
            path,
            invalid_input("object", value.type_name()),
            true,
        ));
        return (empty, issues);
    };
    let provider = match parse_provider(mapping.get("provider")) {
        Ok(provider) => provider,
        Err(issue) => {
            issues.push(Issue::new(
                format!("{path}.provider"),
                issue.message,
                issue.hard,
            ));
            empty.provider
        }
    };
    let model = match parse_nonempty_string(mapping.get("model").unwrap_or(&RawValue::Null)) {
        Ok(model) => model,
        Err(issue) => {
            issues.push(Issue::new(
                format!("{path}.model"),
                issue.message,
                issue.hard,
            ));
            String::new()
        }
    };
    let fallbacks = parse_role_fallbacks(mapping.get("fallbacks"), path, &mut issues);
    let limits = parse_limits(
        mapping.get("limits"),
        &format!("{path}.limits"),
        &mut issues,
    );
    (
        ModelRoleConfig {
            provider,
            model,
            fallbacks,
            limits,
        },
        issues,
    )
}

fn parse_role_fallbacks(
    value: Option<&RawValue>,
    path: &str,
    issues: &mut Vec<Issue>,
) -> Option<Vec<String>> {
    let raw = value?;
    let RawValue::Sequence(items) = raw else {
        issues.push(Issue::new(
            format!("{path}.fallbacks"),
            invalid_input("array", raw.type_name()),
            true,
        ));
        return None;
    };
    let mut fallbacks = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let item_path = format!("{path}.fallbacks.{index}");
        match item {
            RawValue::String(name) if is_valid_role_name(name) => fallbacks.push(name.clone()),
            RawValue::String(_) => issues.push(Issue::new(item_path, ROLE_NAME_MESSAGE, false)),
            other => issues.push(Issue::new(
                item_path,
                invalid_input("string", other.type_name()),
                true,
            )),
        }
    }
    if items.len() > 3 {
        issues.push(Issue::new(
            format!("{path}.fallbacks"),
            "Too big: expected array to have <=3 items",
            true,
        ));
    }
    if issues.iter().any(|issue| issue.hard) {
        return None;
    }
    Some(fallbacks)
}

/// Parses one routing-table value. A string is validated as a role reference;
/// a mapping is parsed as a legacy inline route, where any "hard" issue
/// collapses into the generic union diagnostic and soft issues keep their
/// own dotted paths (Zod's non-aborted union result).
fn parse_routing_value(key: &str, value: &RawValue) -> Result<RoutingValue, Vec<Issue>> {
    let path = format!("routing.{key}");
    match value {
        RawValue::String(name) if is_valid_role_name(name) => Ok(RoutingValue::Role(name.clone())),
        RawValue::String(_) => Err(vec![Issue::new(path, ROLE_NAME_MESSAGE, false)]),
        RawValue::Mapping(_) => {
            let (route, issues) = parse_route_config(value, &path);
            if issues.is_empty() {
                return Ok(RoutingValue::Inline(route));
            }
            if issues.iter().any(|issue| issue.hard) {
                Err(vec![Issue::new(path, ROUTING_VALUE_MESSAGE, true)])
            } else {
                Err(issues)
            }
        }
        _ => Err(vec![Issue::new(path, ROUTING_VALUE_MESSAGE, true)]),
    }
}

fn is_valid_autonomy(value: &str) -> bool {
    if value == "off" || value == "next-checkpoint" || value == "workflow" {
        return true;
    }
    match value.strip_prefix("until:") {
        Some(mode) => WORKFLOW_MODES.contains(&mode),
        None => false,
    }
}

/// Validates one configuration layer (defaults filled) against the schema,
/// then rejects unknown routing keys and routing tables that miss any of the
/// seven workflow modes.
///
/// This mirrors `parseRelayConfig` in the TypeScript reference: the source
/// label (usually a file path) heads the error, and unknown top-level fields
/// are ignored, exactly like Zod's default object behavior.
pub fn parse_relay_config(
    source: &str,
    value: &RawValue,
) -> Result<RelayConfig, ConfigValidationError> {
    let fail = |issues: Vec<Issue>| -> ConfigValidationError {
        ConfigValidationError::new(source, issues.iter().map(Issue::formatted).collect())
    };

    let Some(root) = value.as_mapping() else {
        return Err(fail(vec![Issue::new(
            "",
            invalid_input("object", value.type_name()),
            true,
        )]));
    };

    let mut issues: Vec<Issue> = Vec::new();

    let version = match root.get("version") {
        None => CONFIG_SCHEMA_VERSION,
        Some(RawValue::Int(1)) => 1,
        Some(RawValue::Float(number)) if *number == 1.0 => 1,
        Some(_) => {
            issues.push(Issue::new("version", "Invalid input: expected 1", true));
            CONFIG_SCHEMA_VERSION
        }
    };

    let providers = match root.get("providers") {
        None => default_providers(),
        Some(raw) => match raw.as_mapping() {
            Some(mapping) => {
                // Zod quirk preserved: an *absent* provider key takes the
                // object-level default (ANTHROPIC_API_KEY for anthropic),
                // while a present-but-empty object takes the field-level
                // default (OPENAI_API_KEY for both).
                let openai = parse_provider_settings(
                    mapping.get("openai"),
                    "providers.openai",
                    "OPENAI_API_KEY",
                    &mut issues,
                );
                let anthropic = parse_provider_settings(
                    mapping.get("anthropic"),
                    "providers.anthropic",
                    "ANTHROPIC_API_KEY",
                    &mut issues,
                );
                Providers { openai, anthropic }
            }
            None => {
                issues.push(Issue::new(
                    "providers",
                    invalid_input("object", raw.type_name()),
                    true,
                ));
                default_providers()
            }
        },
    };

    let mut routing: IndexMap<String, RoutingValue> = IndexMap::new();
    match root.get("routing") {
        None => issues.push(Issue::new(
            "routing",
            invalid_input("record", "undefined"),
            true,
        )),
        Some(RawValue::Mapping(table)) => {
            for (key, value) in table {
                match parse_routing_value(key, value) {
                    Ok(parsed) => {
                        routing.insert(key.clone(), parsed);
                    }
                    Err(mut value_issues) => issues.append(&mut value_issues),
                }
            }
        }
        Some(other) => issues.push(Issue::new(
            "routing",
            invalid_input("record", other.type_name()),
            true,
        )),
    }

    let mut models: Option<IndexMap<String, ModelRoleConfig>> = None;
    match root.get("models") {
        None => {}
        Some(RawValue::Mapping(catalog)) => {
            let mut parsed_catalog: IndexMap<String, ModelRoleConfig> = IndexMap::new();
            for (role, definition) in catalog {
                let (role_config, mut role_issues) =
                    parse_model_role(definition, &format!("models.{role}"));
                issues.append(&mut role_issues);
                parsed_catalog.insert(role.clone(), role_config);
            }
            // The key refinement runs after every value is parsed, so key
            // diagnostics follow value diagnostics in the report.
            for role in catalog.keys() {
                if !is_valid_role_name(role) {
                    issues.push(Issue::new(
                        format!("models.{role}"),
                        ROLE_NAME_MESSAGE,
                        false,
                    ));
                }
            }
            models = Some(parsed_catalog);
        }
        Some(other) => issues.push(Issue::new(
            "models",
            invalid_input("record", other.type_name()),
            true,
        )),
    }

    let budgets = match root.get("budgets") {
        None => {
            issues.push(Issue::new(
                "budgets",
                invalid_input("object", "undefined"),
                true,
            ));
            BudgetLimits::default()
        }
        Some(raw) => match raw.as_mapping() {
            None => {
                issues.push(Issue::new(
                    "budgets",
                    invalid_input("object", raw.type_name()),
                    true,
                ));
                BudgetLimits::default()
            }
            Some(mapping) => BudgetLimits {
                max_turns: int_field(mapping, "maxTurns", "budgets", 1, 500, 24, &mut issues),
                max_model_calls: int_field(
                    mapping,
                    "maxModelCalls",
                    "budgets",
                    1,
                    2_000,
                    48,
                    &mut issues,
                ),
                max_tool_calls: int_field(
                    mapping,
                    "maxToolCalls",
                    "budgets",
                    1,
                    2_000,
                    96,
                    &mut issues,
                ),
                max_output_tokens: int_field(
                    mapping,
                    "maxOutputTokens",
                    "budgets",
                    1_024,
                    10_000_000,
                    400_000,
                    &mut issues,
                ),
                wall_clock_ms: int_field(
                    mapping,
                    "wallClockMs",
                    "budgets",
                    10_000,
                    86_400_000,
                    1_800_000,
                    &mut issues,
                ),
                max_repeated_actions: int_field(
                    mapping,
                    "maxRepeatedActions",
                    "budgets",
                    2,
                    20,
                    3,
                    &mut issues,
                ),
            },
        },
    };

    let autonomy = match root.get("autonomy") {
        None => "next-checkpoint".to_string(),
        Some(RawValue::String(bound)) => {
            if is_valid_autonomy(bound) {
                bound.clone()
            } else {
                issues.push(Issue::new("autonomy", AUTONOMY_MESSAGE, false));
                "next-checkpoint".to_string()
            }
        }
        Some(other) => {
            issues.push(Issue::new(
                "autonomy",
                invalid_input("string", other.type_name()),
                true,
            ));
            "next-checkpoint".to_string()
        }
    };

    let tools = match root.get("tools") {
        None => ToolSettings::default(),
        Some(raw) => match raw.as_mapping() {
            None => {
                issues.push(Issue::new(
                    "tools",
                    invalid_input("object", raw.type_name()),
                    true,
                ));
                ToolSettings::default()
            }
            Some(mapping) => ToolSettings {
                command_timeout_ms: int_field(
                    mapping,
                    "commandTimeoutMs",
                    "tools",
                    1_000,
                    600_000,
                    120_000,
                    &mut issues,
                ),
                max_search_results: int_field(
                    mapping,
                    "maxSearchResults",
                    "tools",
                    1,
                    10_000,
                    200,
                    &mut issues,
                ),
                max_file_bytes: int_field(
                    mapping,
                    "maxFileBytes",
                    "tools",
                    1_024,
                    20_000_000,
                    2_000_000,
                    &mut issues,
                ),
            },
        },
    };

    if !issues.is_empty() {
        return Err(fail(issues));
    }

    let unknown_keys: Vec<&String> = routing
        .keys()
        .filter(|key| !ALLOWED_ROUTING_KEYS.contains(&key.as_str()))
        .collect();
    if !unknown_keys.is_empty() {
        let joined = unknown_keys
            .iter()
            .map(|key| key.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(ConfigValidationError::new(
            source,
            vec![format!(
                "routing contains unknown modes: {joined} (allowed keys: {}, and {INTENT_PURPOSE})",
                WORKFLOW_MODES.join(", ")
            )],
        ));
    }

    let missing_modes: Vec<&str> = WORKFLOW_MODES
        .iter()
        .filter(|mode| !routing.contains_key(**mode))
        .copied()
        .collect();
    if !missing_modes.is_empty() {
        return Err(ConfigValidationError::new(
            source,
            vec![format!(
                "routing is missing modes: {}",
                missing_modes.join(", ")
            )],
        ));
    }

    Ok(RelayConfig {
        version,
        providers,
        models,
        routing,
        budgets,
        autonomy,
        tools,
    })
}
