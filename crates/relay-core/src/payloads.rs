//! Event payload types and their decoders.
//!
//! Every payload decoder mirrors the corresponding TypeScript Zod schema
//! exactly — including bounds, nullability, and the **text of the validation
//! messages** (Zod v4 wording, e.g. `Too small: expected string to have >=1
//! characters`), because those strings flow into `InvalidEventPayloadError`
//! messages that are part of the compatibility contract. Unknown payload keys
//! are ignored, matching Zod's default strip behavior (validated data never
//! carries them).

use crate::artifacts::ArtifactKind;
use crate::intents::{IntentKind, IntentProposal, IntentSource};
use crate::modes::{AutonomyBound, ToolCapability, WorkflowMode};
use crate::outcomes::OutcomeKind;
use serde::Serialize;
use serde_json::{Map, Value};

// ---------------------------------------------------------------------------
// Zod-style issue collection
// ---------------------------------------------------------------------------

/// Collected validation issues, each rendered as `path: message` exactly the
/// way `validateStoredEvent` renders Zod issues in the TypeScript core.
#[derive(Debug, Default)]
pub(crate) struct Issues {
    entries: Vec<String>,
}

impl Issues {
    /// Record one issue at `path` (empty path renders as `(root)`).
    pub(crate) fn push(&mut self, path: &str, message: impl Into<String>) {
        let message = message.into();
        if path.is_empty() {
            self.entries.push(format!("(root): {message}"));
        } else {
            self.entries.push(format!("{path}: {message}"));
        }
    }

    /// Whether no issue was recorded so far.
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Render the Zod-style detail string (issues joined with `; `).
    pub(crate) fn into_detail(self) -> String {
        self.entries.join("; ")
    }
}

/// Zod's `received` descriptor for a JSON value.
fn received(value: Option<&Value>) -> &str {
    match value {
        None => "undefined",
        Some(Value::Null) => "null",
        Some(Value::Bool(_)) => "boolean",
        Some(Value::Number(_)) => "number",
        Some(Value::String(_)) => "string",
        Some(Value::Array(_)) => "array",
        Some(Value::Object(_)) => "object",
    }
}

fn type_issue(issues: &mut Issues, path: &str, expected: &str, value: Option<&Value>) {
    issues.push(
        path,
        format!(
            "Invalid input: expected {}, received {}",
            expected,
            received(value)
        ),
    );
}

/// Read a required object field (missing counts as `undefined`).
fn field<'a>(map: &'a Map<String, Value>, key: &str) -> Option<&'a Value> {
    map.get(key)
}

// -- string fields -----------------------------------------------------------

/// Required string with min/max length (length counted in characters).
fn req_string(
    map: &Map<String, Value>,
    key: &str,
    min: i64,
    max: i64,
    path: &str,
    issues: &mut Issues,
) -> Option<String> {
    let value = field(map, key);
    let Value::String(text) = value? else {
        type_issue(issues, path, "string", value);
        return None;
    };
    let len = text.chars().count() as i64;
    if len < min {
        issues.push(
            path,
            format!("Too small: expected string to have >={min} characters"),
        );
        return None;
    }
    if len > max {
        issues.push(
            path,
            format!("Too big: expected string to have <={max} characters"),
        );
        return None;
    }
    Some(text.clone())
}

/// Required key whose value must be the literal string `expected`.
fn req_literal_str(
    map: &Map<String, Value>,
    key: &str,
    expected: &str,
    path: &str,
    issues: &mut Issues,
) -> Option<()> {
    match field(map, key) {
        Some(Value::String(text)) if text == expected => Some(()),
        _ => {
            issues.push(path, format!("Invalid input: expected \"{expected}\""));
            None
        }
    }
}

/// Required key whose value must be the literal integer `expected`.
fn req_literal_int(
    map: &Map<String, Value>,
    key: &str,
    expected: i64,
    path: &str,
    issues: &mut Issues,
) -> Option<()> {
    let is_match =
        matches!(field(map, key), Some(Value::Number(n)) if n.as_i64() == Some(expected));
    if is_match {
        Some(())
    } else {
        issues.push(path, format!("Invalid input: expected {expected}"));
        None
    }
}

/// Required string-enum field. Every failure mode (missing, null, wrong type,
/// unknown value) yields Zod's enum message, matching TypeScript behavior.
fn req_enum<T>(
    map: &Map<String, Value>,
    key: &str,
    parse: fn(&str) -> Option<T>,
    options: &str,
    path: &str,
    issues: &mut Issues,
) -> Option<T> {
    match field(map, key) {
        Some(Value::String(text)) => match parse(text) {
            Some(value) => Some(value),
            None => {
                issues.push(path, format!("Invalid option: expected one of {options}"));
                None
            }
        },
        _ => {
            issues.push(path, format!("Invalid option: expected one of {options}"));
            None
        }
    }
}

/// Nullable-but-required enum field (the key must be present; `null` allowed).
fn req_nullable_enum<T>(
    map: &Map<String, Value>,
    key: &str,
    parse: fn(&str) -> Option<T>,
    options: &str,
    path: &str,
    issues: &mut Issues,
) -> Option<Option<T>> {
    match field(map, key) {
        None | Some(Value::Null) => {
            if field(map, key).is_none() {
                issues.push(path, format!("Invalid option: expected one of {options}"));
                return None;
            }
            Some(None)
        }
        _ => req_enum(map, key, parse, options, path, issues).map(Some),
    }
}

/// Optional (nullish) enum field: missing or `null` yields `None`.
fn opt_enum<T>(
    map: &Map<String, Value>,
    key: &str,
    parse: fn(&str) -> Option<T>,
    options: &str,
    path: &str,
    issues: &mut Issues,
) -> Option<Option<T>> {
    match field(map, key) {
        None | Some(Value::Null) => Some(None),
        _ => req_enum(map, key, parse, options, path, issues).map(Some),
    }
}

/// Nullable-but-required string field (the key must be present; `null` allowed).
fn req_nullable_string(
    map: &Map<String, Value>,
    key: &str,
    max: i64,
    path: &str,
    issues: &mut Issues,
) -> Option<Option<String>> {
    match field(map, key) {
        Some(Value::Null) => Some(None),
        Some(Value::String(text)) => {
            if (text.chars().count() as i64) > max {
                issues.push(
                    path,
                    format!("Too big: expected string to have <={max} characters"),
                );
                return None;
            }
            Some(Some(text.clone()))
        }
        other => {
            type_issue(issues, path, "string", other);
            None
        }
    }
}

/// Optional (nullish) string field with min/max length.
fn opt_string(
    map: &Map<String, Value>,
    key: &str,
    min: i64,
    max: i64,
    path: &str,
    issues: &mut Issues,
) -> Option<Option<String>> {
    match field(map, key) {
        None | Some(Value::Null) => Some(None),
        Some(Value::String(text)) => {
            let len = text.chars().count() as i64;
            if len < min {
                issues.push(
                    path,
                    format!("Too small: expected string to have >={min} characters"),
                );
                return None;
            }
            if len > max {
                issues.push(
                    path,
                    format!("Too big: expected string to have <={max} characters"),
                );
                return None;
            }
            Some(Some(text.clone()))
        }
        other => {
            type_issue(issues, path, "string", other);
            None
        }
    }
}

// -- numeric fields ----------------------------------------------------------

/// Required boolean field.
fn req_bool(map: &Map<String, Value>, key: &str, path: &str, issues: &mut Issues) -> Option<bool> {
    match field(map, key) {
        Some(Value::Bool(flag)) => Some(*flag),
        other => {
            type_issue(issues, path, "boolean", other);
            None
        }
    }
}

/// Required integer field with bounds. Floats with a fraction, non-numbers,
/// and out-of-range values produce Zod's messages.
fn req_int(
    map: &Map<String, Value>,
    key: &str,
    min: i64,
    max: i64,
    path: &str,
    issues: &mut Issues,
) -> Option<i64> {
    let value = field(map, key);
    let Some(Value::Number(number)) = value else {
        type_issue(issues, path, "number", value);
        return None;
    };
    let as_int = if let Some(int) = number.as_i64() {
        Some(int)
    } else if let Some(float) = number.as_f64() {
        if float.fract() == 0.0 && float.is_finite() && float.abs() <= 9_007_199_254_740_992.0 {
            Some(float as i64)
        } else {
            None
        }
    } else {
        None
    };
    let Some(int) = as_int else {
        issues.push(
            path,
            "Invalid input: expected int, received number".to_string(),
        );
        return None;
    };
    if int < min {
        issues.push(path, format!("Too small: expected number to be >={min}"));
        return None;
    }
    if int > max {
        issues.push(path, format!("Too big: expected number to be <={max}"));
        return None;
    }
    Some(int)
}

/// Required number field interpreted as f64 with bounds (confidence).
fn req_bounded_float(
    map: &Map<String, Value>,
    key: &str,
    min: f64,
    max: f64,
    path: &str,
    issues: &mut Issues,
) -> Option<f64> {
    let value = field(map, key);
    let Some(Value::Number(number)) = value else {
        type_issue(issues, path, "number", value);
        return None;
    };
    let float = number
        .as_f64()
        .or_else(|| number.as_i64().map(|int| int as f64));
    let Some(float) = float else {
        type_issue(issues, path, "number", value);
        return None;
    };
    if float < min {
        issues.push(path, format!("Too small: expected number to be >={min}"));
        return None;
    }
    if float > max {
        issues.push(path, format!("Too big: expected number to be <={max}"));
        return None;
    }
    Some(float)
}

// -- composite fields --------------------------------------------------------

/// Required nested object field; recurses with `prefix.` paths.
fn req_object<'a>(
    map: &'a Map<String, Value>,
    key: &str,
    path_prefix: &str,
    issues: &mut Issues,
) -> Option<Map<String, Value>> {
    match field(map, key) {
        Some(Value::Object(nested)) => Some(nested.clone()),
        other => {
            let path = if path_prefix.is_empty() {
                key.to_string()
            } else {
                format!("{path_prefix}{key}")
            };
            type_issue(issues, &path, "object", other);
            None
        }
    }
}

/// Required array of strings with per-item length bounds and array size bounds.
fn req_string_array(
    map: &Map<String, Value>,
    key: &str,
    item_min: i64,
    item_max: i64,
    min_items: i64,
    max_items: i64,
    path: &str,
    issues: &mut Issues,
) -> Option<Vec<String>> {
    let value = field(map, key);
    let Some(Value::Array(items)) = value else {
        type_issue(issues, path, "array", value);
        return None;
    };
    let mut out = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let item_path = format!("{path}.{index}");
        match item {
            Value::String(text) => {
                let len = text.chars().count() as i64;
                if len < item_min {
                    issues.push(
                        &item_path,
                        format!("Too small: expected string to have >={item_min} characters"),
                    );
                    return None;
                }
                if len > item_max {
                    issues.push(
                        &item_path,
                        format!("Too big: expected string to have <={item_max} characters"),
                    );
                    return None;
                }
                out.push(text.clone());
            }
            other => {
                type_issue(issues, &item_path, "string", Some(other));
                return None;
            }
        }
    }
    let count = items.len() as i64;
    if count < min_items {
        issues.push(
            path,
            format!("Too small: expected array to have >={min_items} items"),
        );
        return None;
    }
    if count > max_items {
        issues.push(
            path,
            format!("Too big: expected array to have <={max_items} items"),
        );
        return None;
    }
    Some(out)
}

/// Require the payload root to be an object.
fn root_object<'a>(value: &'a Value, issues: &mut Issues) -> Option<&'a Map<String, Value>> {
    match value {
        Value::Object(map) => Some(map),
        other => {
            type_issue(issues, "", "object", Some(other));
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Shared wire enums
// ---------------------------------------------------------------------------

/// The `auto`/`user` source of a mode transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum AutoUserSource {
    /// The workflow advanced on its own.
    #[serde(rename = "auto")]
    Auto,
    /// The user requested the transition.
    #[serde(rename = "user")]
    User,
}

impl AutoUserSource {
    /// The wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            AutoUserSource::Auto => "auto",
            AutoUserSource::User => "user",
        }
    }

    /// Parse the wire spelling.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "auto" => Some(AutoUserSource::Auto),
            "user" => Some(AutoUserSource::User),
            _ => None,
        }
    }

    /// Zod-style option list for validation messages.
    pub fn zod_options() -> String {
        crate::modes::zod_option_list(["auto", "user"].into_iter())
    }
}

/// The `auto`/`user`/`intent` source of an autonomy change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum AutonomyChangeSource {
    /// Policy changed the autonomy bound.
    #[serde(rename = "auto")]
    Auto,
    /// The user changed the autonomy bound.
    #[serde(rename = "user")]
    User,
    /// An applied intent changed the autonomy bound.
    #[serde(rename = "intent")]
    Intent,
}

impl AutonomyChangeSource {
    /// Parse the wire spelling.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "auto" => Some(AutonomyChangeSource::Auto),
            "user" => Some(AutonomyChangeSource::User),
            "intent" => Some(AutonomyChangeSource::Intent),
            _ => None,
        }
    }

    /// Zod-style option list for validation messages.
    pub fn zod_options() -> String {
        crate::modes::zod_option_list(["auto", "user", "intent"].into_iter())
    }
}

/// Model failure classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum ModelErrorKind {
    /// Provider rate-limited the request.
    #[serde(rename = "rate-limit")]
    RateLimit,
    /// Provider-side failure.
    #[serde(rename = "server")]
    Server,
    /// Transport-level failure.
    #[serde(rename = "network")]
    Network,
    /// Request timed out.
    #[serde(rename = "timeout")]
    Timeout,
    /// The call was canceled.
    #[serde(rename = "canceled")]
    Canceled,
    /// Authentication failed.
    #[serde(rename = "auth")]
    Auth,
    /// The response could not be parsed.
    #[serde(rename = "invalid-response")]
    InvalidResponse,
    /// Anything else.
    #[serde(rename = "unknown")]
    Unknown,
}

impl ModelErrorKind {
    /// Parse the wire spelling.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "rate-limit" => Some(ModelErrorKind::RateLimit),
            "server" => Some(ModelErrorKind::Server),
            "network" => Some(ModelErrorKind::Network),
            "timeout" => Some(ModelErrorKind::Timeout),
            "canceled" => Some(ModelErrorKind::Canceled),
            "auth" => Some(ModelErrorKind::Auth),
            "invalid-response" => Some(ModelErrorKind::InvalidResponse),
            "unknown" => Some(ModelErrorKind::Unknown),
            _ => None,
        }
    }

    /// The wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            ModelErrorKind::RateLimit => "rate-limit",
            ModelErrorKind::Server => "server",
            ModelErrorKind::Network => "network",
            ModelErrorKind::Timeout => "timeout",
            ModelErrorKind::Canceled => "canceled",
            ModelErrorKind::Auth => "auth",
            ModelErrorKind::InvalidResponse => "invalid-response",
            ModelErrorKind::Unknown => "unknown",
        }
    }

    /// Zod-style option list for validation messages.
    pub fn zod_options() -> String {
        crate::modes::zod_option_list(
            [
                "rate-limit",
                "server",
                "network",
                "timeout",
                "canceled",
                "auth",
                "invalid-response",
                "unknown",
            ]
            .into_iter(),
        )
    }
}

/// Tool execution outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum ToolOutcome {
    /// The tool succeeded.
    #[serde(rename = "ok")]
    Ok,
    /// The tool ran and failed.
    #[serde(rename = "error")]
    Error,
}

impl ToolOutcome {
    /// Parse the wire spelling.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "ok" => Some(ToolOutcome::Ok),
            "error" => Some(ToolOutcome::Error),
            _ => None,
        }
    }

    /// Zod-style option list for validation messages.
    pub fn zod_options() -> String {
        crate::modes::zod_option_list(["ok", "error"].into_iter())
    }
}

/// Who inspected an artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum InspectVia {
    /// The user inspected the artifact.
    #[serde(rename = "user")]
    User,
    /// A model inspected the artifact.
    #[serde(rename = "model")]
    Model,
}

impl InspectVia {
    /// Parse the wire spelling.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "user" => Some(InspectVia::User),
            "model" => Some(InspectVia::Model),
            _ => None,
        }
    }

    /// Zod-style option list for validation messages.
    pub fn zod_options() -> String {
        crate::modes::zod_option_list(["user", "model"].into_iter())
    }
}

/// Who paused the workflow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum PauseActor {
    /// The user paused the workflow.
    #[serde(rename = "user")]
    User,
    /// Policy paused the workflow.
    #[serde(rename = "policy")]
    Policy,
}

impl PauseActor {
    /// The wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            PauseActor::User => "user",
            PauseActor::Policy => "policy",
        }
    }

    /// Parse the wire spelling.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "user" => Some(PauseActor::User),
            "policy" => Some(PauseActor::Policy),
            _ => None,
        }
    }

    /// Zod-style option list for validation messages.
    pub fn zod_options() -> String {
        crate::modes::zod_option_list(["user", "policy"].into_iter())
    }
}

/// Verification criterion status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum VerificationStatus {
    /// The criterion passed.
    #[serde(rename = "pass")]
    Pass,
    /// The criterion failed.
    #[serde(rename = "fail")]
    Fail,
    /// The criterion was not evaluated.
    #[serde(rename = "unevaluated")]
    Unevaluated,
}

impl VerificationStatus {
    /// Parse the wire spelling.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "pass" => Some(VerificationStatus::Pass),
            "fail" => Some(VerificationStatus::Fail),
            "unevaluated" => Some(VerificationStatus::Unevaluated),
            _ => None,
        }
    }

    /// Zod-style option list for validation messages.
    pub fn zod_options() -> String {
        crate::modes::zod_option_list(["pass", "fail", "unevaluated"].into_iter())
    }
}

// ---------------------------------------------------------------------------
// Payload types
// ---------------------------------------------------------------------------

/// Token usage reported by a provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsage {
    /// Input tokens consumed.
    pub input_tokens: i64,
    /// Output tokens consumed.
    pub output_tokens: i64,
}

/// `project.initialized`
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectInitializedPayload {
    /// The product goal.
    pub goal: String,
    /// Absolute workspace root the project runs in.
    pub workspace_root: String,
    /// Initial autonomy bound as written (unparseable values fall back at fold time).
    pub initial_autonomy: String,
}

/// `session.started`
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStartedPayload {
    /// The session that started.
    pub session_id: String,
    /// Whether the session is interactive.
    pub interactive: bool,
}

/// `intent.proposed`
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IntentProposedPayload {
    /// Where the input came from.
    pub source: IntentSource,
    /// The raw user input.
    pub raw: String,
    /// The validated classification proposal.
    pub proposal: IntentProposalSer,
}

/// Serializable mirror of [`IntentProposal`] (the domain type carries enums as
/// newtypes; this is the exact wire shape).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct IntentProposalSer {
    /// The classified kind.
    pub kind: IntentKind,
    /// Classification confidence in [0, 1].
    pub confidence: f64,
    /// Why the input was classified this way.
    pub rationale: String,
    /// Target mode, when relevant.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<WorkflowMode>,
    /// Target autonomy bound, when relevant.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub autonomy: Option<String>,
    /// Target artifact kind, when relevant.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_kind: Option<ArtifactKind>,
    /// Focused question for ambiguous intents.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub question: Option<String>,
    /// Cleaned-up restatement of the input.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub free_text: Option<String>,
}

/// `intent.applied`
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IntentAppliedPayload {
    /// The applied intent kind (free-form string, as in the TypeScript schema).
    pub kind: String,
    /// Where the intent came from.
    pub source: IntentSource,
    /// Classification confidence.
    pub confidence: f64,
    /// Target mode, when the intent carried one.
    pub mode: Option<WorkflowMode>,
    /// Target autonomy bound (free-form string), when the intent carried one.
    pub autonomy: Option<String>,
    /// Target artifact kind, when the intent carried one.
    pub artifact_kind: Option<ArtifactKind>,
    /// Whether the controller overrode the proposal.
    pub overridden: bool,
}

/// `mode.entered`
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModeEnteredPayload {
    /// The mode left (`null` for the first entry).
    pub from: Option<WorkflowMode>,
    /// The mode entered.
    pub to: WorkflowMode,
    /// Why the transition happened.
    pub reason: String,
    /// Whether the transition was automatic or user-requested.
    pub source: AutoUserSource,
}

/// `mode.transition.rejected`
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModeTransitionRejectedPayload {
    /// The mode the transition was attempted from.
    pub from: WorkflowMode,
    /// The mode the transition targeted.
    pub to: WorkflowMode,
    /// Why policy rejected the transition.
    pub reason: String,
}

/// `autonomy.changed`
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AutonomyChangedPayload {
    /// The previous bound (free-form string).
    pub from: String,
    /// The new bound (free-form string; unparseable values fall back at fold time).
    pub to: String,
    /// What changed the bound.
    pub source: AutonomyChangeSource,
}

/// `artifact.created`
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactCreatedPayload {
    /// The new artifact's id.
    pub artifact_id: String,
    /// The artifact's kind.
    pub kind: ArtifactKind,
    /// The artifact's title.
    pub title: String,
    /// Always 1 at creation.
    pub version: u32,
    /// Path relative to the workspace root.
    pub path: String,
    /// Short summary of the artifact.
    pub summary: String,
}

/// `artifact.revised`
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactRevisedPayload {
    /// The revised artifact's id.
    pub artifact_id: String,
    /// The artifact's kind.
    pub kind: ArtifactKind,
    /// The version before the revision.
    pub from_version: i64,
    /// The version after the revision (must be `from_version + 1`).
    pub to_version: i64,
    /// Path of the new version.
    pub path: String,
    /// Why the artifact was revised.
    pub reason: String,
}

/// `artifact.approval.requested`
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactApprovalRequestedPayload {
    /// The artifact awaiting approval.
    pub artifact_id: String,
    /// The artifact's kind.
    pub kind: ArtifactKind,
    /// The version awaiting approval.
    pub version: i64,
    /// Why approval is requested.
    pub reason: String,
}

/// `artifact.approved`
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactApprovedPayload {
    /// The approved artifact.
    pub artifact_id: String,
    /// The artifact's kind.
    pub kind: ArtifactKind,
    /// The approved version (must be the current version).
    pub version: i64,
    /// Always `user`; only the user approves.
    pub approved_by: &'static str,
}

/// Cause recorded with an invalidation.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InvalidationCause {
    /// The kind of the cause artifact.
    pub kind: ArtifactKind,
    /// The cause artifact's id.
    pub artifact_id: String,
    /// The cause artifact's version at the time.
    pub version: i64,
}

/// `artifact.invalidated`
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactInvalidatedPayload {
    /// Artifacts marked stale.
    pub artifact_ids: Vec<String>,
    /// What caused the invalidation.
    pub cause: InvalidationCause,
    /// Why the artifacts were invalidated.
    pub reason: String,
}

/// `artifact.inspected`
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactInspectedPayload {
    /// The inspected artifact.
    pub artifact_id: String,
    /// Who inspected it.
    pub via: InspectVia,
}

/// `transition.evaluated`
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransitionEvaluatedPayload {
    /// The evaluated outcome.
    pub outcome: OutcomeKind,
    /// The target mode for `ADVANCE`.
    pub to: Option<WorkflowMode>,
    /// The stage to route back to for `REVISE`.
    pub revise_stage: Option<WorkflowMode>,
    /// Why the outcome was chosen.
    pub reason: String,
    /// Supporting evidence entries.
    pub evidence: Vec<String>,
}

/// `model.requested`
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRequestedPayload {
    /// Provider name.
    pub provider: String,
    /// Model id.
    pub model: String,
    /// Routing purpose.
    pub purpose: String,
    /// Workflow mode context, when routed for a mode.
    pub mode: Option<WorkflowMode>,
    /// 1-based attempt number.
    pub attempt: i64,
    /// Semantic model-role name; absent for legacy inline routes and pre-role logs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

/// `model.responded`
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRespondedPayload {
    /// Provider name.
    pub provider: String,
    /// Model id.
    pub model: String,
    /// Routing purpose.
    pub purpose: String,
    /// Semantic model-role name, when routed from a role.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Token usage.
    pub usage: TokenUsage,
    /// Tool calls in the response.
    pub tool_calls: i64,
    /// Call duration in milliseconds.
    pub duration_ms: i64,
}

/// `model.failed`
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelFailedPayload {
    /// Provider name.
    pub provider: String,
    /// Model id.
    pub model: String,
    /// Routing purpose.
    pub purpose: String,
    /// Semantic model-role name, when routed from a role.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Failure classification.
    pub error_kind: ModelErrorKind,
    /// Redacted, header-free error summary.
    pub message: String,
    /// Attempts made before failing.
    pub attempts: i64,
    /// Total duration in milliseconds.
    pub duration_ms: i64,
}

/// `tool.executed`
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolExecutedPayload {
    /// Tool name.
    pub tool: String,
    /// Side-effect class.
    pub capability: ToolCapability,
    /// Workflow mode at execution time.
    pub mode: Option<WorkflowMode>,
    /// Execution outcome.
    pub outcome: ToolOutcome,
    /// Duration in milliseconds.
    pub duration_ms: i64,
    /// Redacted, length-bounded summary.
    pub summary: String,
    /// Files the tool affected.
    pub affected_files: Vec<String>,
}

/// `tool.denied`
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolDeniedPayload {
    /// Tool name.
    pub tool: String,
    /// Side-effect class.
    pub capability: ToolCapability,
    /// Workflow mode at denial time.
    pub mode: Option<WorkflowMode>,
    /// Why the tool was denied.
    pub reason: String,
}

/// One verification criterion result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationResult {
    /// The criterion being evaluated.
    pub criterion: String,
    /// pass/fail/unevaluated.
    pub status: VerificationStatus,
    /// Evidence backing the status.
    pub evidence: String,
}

/// `verification.completed`
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationCompletedPayload {
    /// Whether the run passed overall.
    pub passed: bool,
    /// Criterion-by-criterion results.
    pub results: Vec<VerificationResult>,
    /// The verification report artifact, when one exists.
    pub report_artifact_id: Option<String>,
}

/// `workflow.paused`
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowPausedPayload {
    /// Who paused the workflow.
    pub by: PauseActor,
    /// Why the workflow is paused.
    pub reason: String,
    /// A focused question to answer, when the pause asks one.
    pub question: Option<String>,
    /// Options offered to the user.
    pub options: Vec<String>,
}

/// `workflow.resumed`
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowResumedPayload {
    /// Always `user`; only the user resumes.
    pub by: &'static str,
    /// Free-form note about the resume.
    pub note: String,
}

/// `budget.exhausted`
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BudgetExhaustedPayload {
    /// Which budget was exhausted.
    pub budget: String,
    /// The budget's limit.
    pub limit: i64,
    /// Extra detail about the exhaustion.
    pub detail: String,
}

/// `run.interrupted`
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunInterruptedPayload {
    /// The phase that was running.
    pub phase: String,
    /// Why the run was interrupted.
    pub hint: String,
}

/// The typed payload of a validated event.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum EventPayload {
    /// `project.initialized`
    ProjectInitialized(ProjectInitializedPayload),
    /// `session.started`
    SessionStarted(SessionStartedPayload),
    /// `intent.proposed`
    IntentProposed(IntentProposedPayload),
    /// `intent.applied`
    IntentApplied(IntentAppliedPayload),
    /// `mode.entered`
    ModeEntered(ModeEnteredPayload),
    /// `mode.transition.rejected`
    ModeTransitionRejected(ModeTransitionRejectedPayload),
    /// `autonomy.changed`
    AutonomyChanged(AutonomyChangedPayload),
    /// `artifact.created`
    ArtifactCreated(ArtifactCreatedPayload),
    /// `artifact.revised`
    ArtifactRevised(ArtifactRevisedPayload),
    /// `artifact.approval.requested`
    ArtifactApprovalRequested(ArtifactApprovalRequestedPayload),
    /// `artifact.approved`
    ArtifactApproved(ArtifactApprovedPayload),
    /// `artifact.invalidated`
    ArtifactInvalidated(ArtifactInvalidatedPayload),
    /// `artifact.inspected`
    ArtifactInspected(ArtifactInspectedPayload),
    /// `transition.evaluated`
    TransitionEvaluated(TransitionEvaluatedPayload),
    /// `model.requested`
    ModelRequested(ModelRequestedPayload),
    /// `model.responded`
    ModelResponded(ModelRespondedPayload),
    /// `model.failed`
    ModelFailed(ModelFailedPayload),
    /// `tool.executed`
    ToolExecuted(ToolExecutedPayload),
    /// `tool.denied`
    ToolDenied(ToolDeniedPayload),
    /// `verification.completed`
    VerificationCompleted(VerificationCompletedPayload),
    /// `workflow.paused`
    WorkflowPaused(WorkflowPausedPayload),
    /// `workflow.resumed`
    WorkflowResumed(WorkflowResumedPayload),
    /// `budget.exhausted`
    BudgetExhausted(BudgetExhaustedPayload),
    /// `run.interrupted`
    RunInterrupted(RunInterruptedPayload),
}

// ---------------------------------------------------------------------------
// Decoders
// ---------------------------------------------------------------------------

fn parse_workflow_mode(value: &str) -> Option<WorkflowMode> {
    WorkflowMode::parse(value)
}

fn parse_artifact_kind(value: &str) -> Option<ArtifactKind> {
    ArtifactKind::parse(value)
}

fn parse_intent_kind(value: &str) -> Option<IntentKind> {
    IntentKind::parse(value)
}

fn parse_capability(value: &str) -> Option<ToolCapability> {
    ToolCapability::parse(value)
}

/// Decode the `intent.proposed` proposal object (shared with the intent
/// proposal contract). `path` is the prefix for nested issue paths.
pub(crate) fn decode_intent_proposal(
    value: Option<&Value>,
    path: &str,
    issues: &mut Issues,
) -> Option<IntentProposal> {
    let map = match value {
        Some(Value::Object(map)) => map,
        other => {
            issues.push(
                path,
                format!(
                    "Invalid input: expected object, received {}",
                    received(other)
                ),
            );
            return None;
        }
    };
    let at = |key: &str| {
        if path.is_empty() {
            key.to_string()
        } else {
            format!("{path}.{key}")
        }
    };

    let kind = req_enum(
        map,
        "kind",
        parse_intent_kind,
        &IntentKind::zod_options(),
        &at("kind"),
        issues,
    )?;
    let confidence = req_bounded_float(map, "confidence", 0.0, 1.0, &at("confidence"), issues)?;
    let rationale = req_string(map, "rationale", 1, 2000, &at("rationale"), issues)?;
    let mode = opt_enum(
        map,
        "mode",
        parse_workflow_mode,
        &WorkflowMode::zod_options(),
        &at("mode"),
        issues,
    )?;
    let artifact_kind = opt_enum(
        map,
        "artifactKind",
        parse_artifact_kind,
        &ArtifactKind::zod_options(),
        &at("artifactKind"),
        issues,
    )?;
    // autonomy: a string that must parse as a bound (Zod refine).
    let autonomy = match field(map, "autonomy") {
        None | Some(Value::Null) => None,
        Some(Value::String(text)) => {
            if crate::modes::parse_autonomy_bound(text).is_some() {
                Some(text.clone())
            } else {
                issues.push(
                    &at("autonomy"),
                    "autonomy must be off, next-checkpoint, workflow, or until:<mode>",
                );
                return None;
            }
        }
        other => {
            type_issue(issues, &at("autonomy"), "string", other);
            return None;
        }
    };
    let question = opt_string(map, "question", 0, 1000, &at("question"), issues)?;
    let free_text = opt_string(map, "freeText", 0, 20000, &at("freeText"), issues)?;

    Some(IntentProposal {
        kind,
        confidence,
        rationale,
        mode,
        autonomy: autonomy.map(|text| {
            crate::modes::parse_autonomy_bound(&text).unwrap_or(AutonomyBound::NextCheckpoint)
        }),
        artifact_kind,
        question,
        free_text,
    })
}

/// Serialize a domain [`IntentProposal`] back to its wire shape.
pub(crate) fn intent_proposal_to_wire(proposal: &IntentProposal) -> IntentProposalSer {
    IntentProposalSer {
        kind: proposal.kind,
        confidence: proposal.confidence,
        rationale: proposal.rationale.clone(),
        mode: proposal.mode,
        autonomy: proposal.autonomy.map(|bound| bound.as_str()),
        artifact_kind: proposal.artifact_kind,
        question: proposal.question.clone(),
        free_text: proposal.free_text.clone(),
    }
}

/// Decode a payload by event type; `Err` carries the Zod-style detail string.
pub(crate) fn decode_payload(
    value: &Value,
    decode: PayloadDecoder,
) -> Result<EventPayload, String> {
    let mut issues = Issues::default();
    let payload = decode(value, &mut issues);
    if issues.is_empty() {
        Ok(
            payload.unwrap_or(EventPayload::SessionStarted(SessionStartedPayload {
                session_id: String::new(),
                interactive: false,
            })),
        )
    } else {
        Err(issues.into_detail())
    }
}

/// The per-type decode function selected after envelope inspection.
pub(crate) type PayloadDecoder = fn(&Value, &mut Issues) -> Option<EventPayload>;

// -- individual decoders -----------------------------------------------------

#[allow(clippy::too_many_lines)]
fn decode_project_initialized(value: &Value, issues: &mut Issues) -> Option<EventPayload> {
    let map = root_object(value, issues)?;
    let goal = req_string(map, "goal", 1, 20000, "goal", issues)?;
    let workspace_root = req_string(map, "workspaceRoot", 1, i64::MAX, "workspaceRoot", issues)?;
    let initial_autonomy = match field(map, "initialAutonomy") {
        Some(Value::String(text)) => text.clone(),
        other => {
            type_issue(issues, "initialAutonomy", "string", other);
            return None;
        }
    };
    Some(EventPayload::ProjectInitialized(
        ProjectInitializedPayload {
            goal,
            workspace_root,
            initial_autonomy,
        },
    ))
}

fn decode_session_started(value: &Value, issues: &mut Issues) -> Option<EventPayload> {
    let map = root_object(value, issues)?;
    let session_id = req_string(map, "sessionId", 1, i64::MAX, "sessionId", issues)?;
    let interactive = req_bool(map, "interactive", "interactive", issues)?;
    Some(EventPayload::SessionStarted(SessionStartedPayload {
        session_id,
        interactive,
    }))
}

fn decode_intent_proposed(value: &Value, issues: &mut Issues) -> Option<EventPayload> {
    let map = root_object(value, issues)?;
    let source = req_enum(
        map,
        "source",
        |text| match text {
            "natural-language" => Some(IntentSource::NaturalLanguage),
            "command" => Some(IntentSource::Command),
            _ => None,
        },
        &IntentSource::zod_options(),
        "source",
        issues,
    )?;
    let raw = req_string(map, "raw", 0, 20000, "raw", issues)?;
    let proposal = decode_intent_proposal(field(map, "proposal"), "proposal", issues)?;
    Some(EventPayload::IntentProposed(IntentProposedPayload {
        source,
        raw,
        proposal: intent_proposal_to_wire(&proposal),
    }))
}

fn decode_intent_applied(value: &Value, issues: &mut Issues) -> Option<EventPayload> {
    let map = root_object(value, issues)?;
    let kind = req_string(map, "kind", 1, i64::MAX, "kind", issues)?;
    let source = req_enum(
        map,
        "source",
        |text| match text {
            "natural-language" => Some(IntentSource::NaturalLanguage),
            "command" => Some(IntentSource::Command),
            _ => None,
        },
        &IntentSource::zod_options(),
        "source",
        issues,
    )?;
    let confidence = req_bounded_float(map, "confidence", 0.0, 1.0, "confidence", issues)?;
    let mode = req_nullable_enum(
        map,
        "mode",
        parse_workflow_mode,
        &WorkflowMode::zod_options(),
        "mode",
        issues,
    )?;
    let autonomy = req_nullable_string(map, "autonomy", i64::MAX, "autonomy", issues)?;
    let artifact_kind = req_nullable_enum(
        map,
        "artifactKind",
        parse_artifact_kind,
        &ArtifactKind::zod_options(),
        "artifactKind",
        issues,
    )?;
    let overridden = req_bool(map, "overridden", "overridden", issues)?;
    Some(EventPayload::IntentApplied(IntentAppliedPayload {
        kind,
        source,
        confidence,
        mode,
        autonomy,
        artifact_kind,
        overridden,
    }))
}

fn decode_mode_entered(value: &Value, issues: &mut Issues) -> Option<EventPayload> {
    let map = root_object(value, issues)?;
    let from = req_nullable_enum(
        map,
        "from",
        parse_workflow_mode,
        &WorkflowMode::zod_options(),
        "from",
        issues,
    )?;
    let to = req_enum(
        map,
        "to",
        parse_workflow_mode,
        &WorkflowMode::zod_options(),
        "to",
        issues,
    )?;
    let reason = req_string(map, "reason", 1, 2000, "reason", issues)?;
    let source = req_enum(
        map,
        "source",
        AutoUserSource::parse,
        &AutoUserSource::zod_options(),
        "source",
        issues,
    )?;
    Some(EventPayload::ModeEntered(ModeEnteredPayload {
        from,
        to,
        reason,
        source,
    }))
}

fn decode_mode_transition_rejected(value: &Value, issues: &mut Issues) -> Option<EventPayload> {
    let map = root_object(value, issues)?;
    let from = req_enum(
        map,
        "from",
        parse_workflow_mode,
        &WorkflowMode::zod_options(),
        "from",
        issues,
    )?;
    let to = req_enum(
        map,
        "to",
        parse_workflow_mode,
        &WorkflowMode::zod_options(),
        "to",
        issues,
    )?;
    let reason = req_string(map, "reason", 1, 2000, "reason", issues)?;
    Some(EventPayload::ModeTransitionRejected(
        ModeTransitionRejectedPayload { from, to, reason },
    ))
}

fn decode_autonomy_changed(value: &Value, issues: &mut Issues) -> Option<EventPayload> {
    let map = root_object(value, issues)?;
    let from = match field(map, "from") {
        Some(Value::String(text)) => text.clone(),
        other => {
            type_issue(issues, "from", "string", other);
            return None;
        }
    };
    let to = match field(map, "to") {
        Some(Value::String(text)) => text.clone(),
        other => {
            type_issue(issues, "to", "string", other);
            return None;
        }
    };
    let source = req_enum(
        map,
        "source",
        AutonomyChangeSource::parse,
        &AutonomyChangeSource::zod_options(),
        "source",
        issues,
    )?;
    Some(EventPayload::AutonomyChanged(AutonomyChangedPayload {
        from,
        to,
        source,
    }))
}

fn decode_artifact_created(value: &Value, issues: &mut Issues) -> Option<EventPayload> {
    let map = root_object(value, issues)?;
    let artifact_id = req_string(map, "artifactId", 1, i64::MAX, "artifactId", issues)?;
    let kind = req_enum(
        map,
        "kind",
        parse_artifact_kind,
        &ArtifactKind::zod_options(),
        "kind",
        issues,
    )?;
    let title = req_string(map, "title", 1, 300, "title", issues)?;
    req_literal_int(map, "version", 1, "version", issues)?;
    let path = req_string(map, "path", 1, i64::MAX, "path", issues)?;
    let summary = req_string(map, "summary", 0, 4000, "summary", issues)?;
    Some(EventPayload::ArtifactCreated(ArtifactCreatedPayload {
        artifact_id,
        kind,
        title,
        version: 1,
        path,
        summary,
    }))
}

fn decode_artifact_revised(value: &Value, issues: &mut Issues) -> Option<EventPayload> {
    let map = root_object(value, issues)?;
    let artifact_id = req_string(map, "artifactId", 1, i64::MAX, "artifactId", issues)?;
    let kind = req_enum(
        map,
        "kind",
        parse_artifact_kind,
        &ArtifactKind::zod_options(),
        "kind",
        issues,
    )?;
    let from_version = req_int(map, "fromVersion", 1, i64::MAX, "fromVersion", issues)?;
    let to_version = req_int(map, "toVersion", 2, i64::MAX, "toVersion", issues)?;
    let path = req_string(map, "path", 1, i64::MAX, "path", issues)?;
    let reason = req_string(map, "reason", 1, 2000, "reason", issues)?;
    Some(EventPayload::ArtifactRevised(ArtifactRevisedPayload {
        artifact_id,
        kind,
        from_version,
        to_version,
        path,
        reason,
    }))
}

fn decode_artifact_approval_requested(value: &Value, issues: &mut Issues) -> Option<EventPayload> {
    let map = root_object(value, issues)?;
    let artifact_id = req_string(map, "artifactId", 1, i64::MAX, "artifactId", issues)?;
    let kind = req_enum(
        map,
        "kind",
        parse_artifact_kind,
        &ArtifactKind::zod_options(),
        "kind",
        issues,
    )?;
    let version = req_int(map, "version", 1, i64::MAX, "version", issues)?;
    let reason = req_string(map, "reason", 1, 2000, "reason", issues)?;
    Some(EventPayload::ArtifactApprovalRequested(
        ArtifactApprovalRequestedPayload {
            artifact_id,
            kind,
            version,
            reason,
        },
    ))
}

fn decode_artifact_approved(value: &Value, issues: &mut Issues) -> Option<EventPayload> {
    let map = root_object(value, issues)?;
    let artifact_id = req_string(map, "artifactId", 1, i64::MAX, "artifactId", issues)?;
    let kind = req_enum(
        map,
        "kind",
        parse_artifact_kind,
        &ArtifactKind::zod_options(),
        "kind",
        issues,
    )?;
    let version = req_int(map, "version", 1, i64::MAX, "version", issues)?;
    req_literal_str(map, "approvedBy", "user", "approvedBy", issues)?;
    Some(EventPayload::ArtifactApproved(ArtifactApprovedPayload {
        artifact_id,
        kind,
        version,
        approved_by: "user",
    }))
}

fn decode_artifact_invalidated(value: &Value, issues: &mut Issues) -> Option<EventPayload> {
    let map = root_object(value, issues)?;
    let artifact_ids = req_string_array(
        map,
        "artifactIds",
        1,
        i64::MAX,
        1,
        i64::MAX,
        "artifactIds",
        issues,
    )?;
    let cause_map = req_object(map, "cause", "", issues)?;
    let cause_kind = req_enum(
        &cause_map,
        "kind",
        parse_artifact_kind,
        &ArtifactKind::zod_options(),
        "cause.kind",
        issues,
    )?;
    let cause_artifact_id = req_string(
        &cause_map,
        "artifactId",
        1,
        i64::MAX,
        "cause.artifactId",
        issues,
    )?;
    let cause_version = req_int(&cause_map, "version", 1, i64::MAX, "cause.version", issues)?;
    let reason = req_string(map, "reason", 1, 2000, "reason", issues)?;
    Some(EventPayload::ArtifactInvalidated(
        ArtifactInvalidatedPayload {
            artifact_ids,
            cause: InvalidationCause {
                kind: cause_kind,
                artifact_id: cause_artifact_id,
                version: cause_version,
            },
            reason,
        },
    ))
}

fn decode_artifact_inspected(value: &Value, issues: &mut Issues) -> Option<EventPayload> {
    let map = root_object(value, issues)?;
    let artifact_id = req_string(map, "artifactId", 1, i64::MAX, "artifactId", issues)?;
    let via = req_enum(
        map,
        "via",
        InspectVia::parse,
        &InspectVia::zod_options(),
        "via",
        issues,
    )?;
    Some(EventPayload::ArtifactInspected(ArtifactInspectedPayload {
        artifact_id,
        via,
    }))
}

fn decode_transition_evaluated(value: &Value, issues: &mut Issues) -> Option<EventPayload> {
    let map = root_object(value, issues)?;
    let outcome = req_enum(
        map,
        "outcome",
        OutcomeKind::parse,
        &OutcomeKind::zod_options(),
        "outcome",
        issues,
    )?;
    let to = req_nullable_enum(
        map,
        "to",
        parse_workflow_mode,
        &WorkflowMode::zod_options(),
        "to",
        issues,
    )?;
    let revise_stage = req_nullable_enum(
        map,
        "reviseStage",
        parse_workflow_mode,
        &WorkflowMode::zod_options(),
        "reviseStage",
        issues,
    )?;
    let reason = req_string(map, "reason", 1, 2000, "reason", issues)?;
    let evidence = req_string_array(map, "evidence", 0, 1000, 0, 20, "evidence", issues)?;
    Some(EventPayload::TransitionEvaluated(
        TransitionEvaluatedPayload {
            outcome,
            to,
            revise_stage,
            reason,
            evidence,
        },
    ))
}

fn decode_model_requested(value: &Value, issues: &mut Issues) -> Option<EventPayload> {
    let map = root_object(value, issues)?;
    let provider = req_string(map, "provider", 1, i64::MAX, "provider", issues)?;
    let model = req_string(map, "model", 1, i64::MAX, "model", issues)?;
    let purpose = req_string(map, "purpose", 1, 200, "purpose", issues)?;
    let mode = req_nullable_enum(
        map,
        "mode",
        parse_workflow_mode,
        &WorkflowMode::zod_options(),
        "mode",
        issues,
    )?;
    let attempt = req_int(map, "attempt", 1, i64::MAX, "attempt", issues)?;
    // Optional since the role-routing work; absent on legacy pre-role logs.
    let role = opt_string(map, "role", 1, 100, "role", issues)?;
    Some(EventPayload::ModelRequested(ModelRequestedPayload {
        provider,
        model,
        purpose,
        mode,
        attempt,
        role,
    }))
}

fn decode_model_responded(value: &Value, issues: &mut Issues) -> Option<EventPayload> {
    let map = root_object(value, issues)?;
    let provider = req_string(map, "provider", 1, i64::MAX, "provider", issues)?;
    let model = req_string(map, "model", 1, i64::MAX, "model", issues)?;
    let purpose = req_string(map, "purpose", 1, 200, "purpose", issues)?;
    let role = opt_string(map, "role", 1, 100, "role", issues)?;
    let usage_map = req_object(map, "usage", "", issues)?;
    let input_tokens = req_int(
        &usage_map,
        "inputTokens",
        0,
        i64::MAX,
        "usage.inputTokens",
        issues,
    )?;
    let output_tokens = req_int(
        &usage_map,
        "outputTokens",
        0,
        i64::MAX,
        "usage.outputTokens",
        issues,
    )?;
    let tool_calls = req_int(map, "toolCalls", 0, i64::MAX, "toolCalls", issues)?;
    let duration_ms = req_int(map, "durationMs", 0, i64::MAX, "durationMs", issues)?;
    Some(EventPayload::ModelResponded(ModelRespondedPayload {
        provider,
        model,
        purpose,
        role,
        usage: TokenUsage {
            input_tokens,
            output_tokens,
        },
        tool_calls,
        duration_ms,
    }))
}

fn decode_model_failed(value: &Value, issues: &mut Issues) -> Option<EventPayload> {
    let map = root_object(value, issues)?;
    let provider = req_string(map, "provider", 1, i64::MAX, "provider", issues)?;
    let model = req_string(map, "model", 1, i64::MAX, "model", issues)?;
    let purpose = req_string(map, "purpose", 1, 200, "purpose", issues)?;
    let role = opt_string(map, "role", 1, 100, "role", issues)?;
    let error_kind = req_enum(
        map,
        "errorKind",
        ModelErrorKind::parse,
        &ModelErrorKind::zod_options(),
        "errorKind",
        issues,
    )?;
    let message = req_string(map, "message", 0, 2000, "message", issues)?;
    let attempts = req_int(map, "attempts", 1, i64::MAX, "attempts", issues)?;
    let duration_ms = req_int(map, "durationMs", 0, i64::MAX, "durationMs", issues)?;
    Some(EventPayload::ModelFailed(ModelFailedPayload {
        provider,
        model,
        purpose,
        role,
        error_kind,
        message,
        attempts,
        duration_ms,
    }))
}

fn decode_tool_executed(value: &Value, issues: &mut Issues) -> Option<EventPayload> {
    let map = root_object(value, issues)?;
    let tool = req_string(map, "tool", 1, i64::MAX, "tool", issues)?;
    let capability = req_enum(
        map,
        "capability",
        parse_capability,
        &capability_options(),
        "capability",
        issues,
    )?;
    let mode = req_nullable_enum(
        map,
        "mode",
        parse_workflow_mode,
        &WorkflowMode::zod_options(),
        "mode",
        issues,
    )?;
    let outcome = req_enum(
        map,
        "outcome",
        ToolOutcome::parse,
        &ToolOutcome::zod_options(),
        "outcome",
        issues,
    )?;
    let duration_ms = req_int(map, "durationMs", 0, i64::MAX, "durationMs", issues)?;
    let summary = req_string(map, "summary", 0, 2000, "summary", issues)?;
    let affected_files = req_string_array(
        map,
        "affectedFiles",
        0,
        1000,
        0,
        500,
        "affectedFiles",
        issues,
    )?;
    Some(EventPayload::ToolExecuted(ToolExecutedPayload {
        tool,
        capability,
        mode,
        outcome,
        duration_ms,
        summary,
        affected_files,
    }))
}

fn decode_tool_denied(value: &Value, issues: &mut Issues) -> Option<EventPayload> {
    let map = root_object(value, issues)?;
    let tool = req_string(map, "tool", 1, i64::MAX, "tool", issues)?;
    let capability = req_enum(
        map,
        "capability",
        parse_capability,
        &capability_options(),
        "capability",
        issues,
    )?;
    let mode = req_nullable_enum(
        map,
        "mode",
        parse_workflow_mode,
        &WorkflowMode::zod_options(),
        "mode",
        issues,
    )?;
    let reason = req_string(map, "reason", 1, 2000, "reason", issues)?;
    Some(EventPayload::ToolDenied(ToolDeniedPayload {
        tool,
        capability,
        mode,
        reason,
    }))
}

fn capability_options() -> String {
    crate::modes::zod_option_list(["read", "write", "execute"].into_iter())
}

fn decode_verification_completed(value: &Value, issues: &mut Issues) -> Option<EventPayload> {
    let map = root_object(value, issues)?;
    let passed = req_bool(map, "passed", "passed", issues)?;
    let Some(Value::Array(items)) = field(map, "results") else {
        type_issue(issues, "results", "array", field(map, "results"));
        return None;
    };
    if items.len() > 200 {
        issues.push("results", "Too big: expected array to have <=200 items");
        return None;
    }
    let mut results = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let Value::Object(item_map) = item else {
            type_issue(issues, &format!("results.{index}"), "object", Some(item));
            return None;
        };
        let criterion = req_string(
            item_map,
            "criterion",
            1,
            2000,
            &format!("results.{index}.criterion"),
            issues,
        )?;
        let status = req_enum(
            item_map,
            "status",
            VerificationStatus::parse,
            &VerificationStatus::zod_options(),
            &format!("results.{index}.status"),
            issues,
        )?;
        let evidence = req_string(
            item_map,
            "evidence",
            0,
            4000,
            &format!("results.{index}.evidence"),
            issues,
        )?;
        results.push(VerificationResult {
            criterion,
            status,
            evidence,
        });
    }
    let report_artifact_id = req_nullable_string(
        map,
        "reportArtifactId",
        i64::MAX,
        "reportArtifactId",
        issues,
    )?;
    Some(EventPayload::VerificationCompleted(
        VerificationCompletedPayload {
            passed,
            results,
            report_artifact_id,
        },
    ))
}

fn decode_workflow_paused(value: &Value, issues: &mut Issues) -> Option<EventPayload> {
    let map = root_object(value, issues)?;
    let by = req_enum(
        map,
        "by",
        PauseActor::parse,
        &PauseActor::zod_options(),
        "by",
        issues,
    )?;
    let reason = req_string(map, "reason", 1, 2000, "reason", issues)?;
    let question = req_nullable_string(map, "question", 2000, "question", issues)?;
    let options = req_string_array(map, "options", 0, 300, 0, 6, "options", issues)?;
    Some(EventPayload::WorkflowPaused(WorkflowPausedPayload {
        by,
        reason,
        question,
        options,
    }))
}

fn decode_workflow_resumed(value: &Value, issues: &mut Issues) -> Option<EventPayload> {
    let map = root_object(value, issues)?;
    req_literal_str(map, "by", "user", "by", issues)?;
    let note = req_string(map, "note", 0, 2000, "note", issues)?;
    Some(EventPayload::WorkflowResumed(WorkflowResumedPayload {
        by: "user",
        note,
    }))
}

fn decode_budget_exhausted(value: &Value, issues: &mut Issues) -> Option<EventPayload> {
    let map = root_object(value, issues)?;
    let budget = req_string(map, "budget", 1, 100, "budget", issues)?;
    let limit = req_int(map, "limit", 0, i64::MAX, "limit", issues)?;
    let detail = req_string(map, "detail", 0, 2000, "detail", issues)?;
    Some(EventPayload::BudgetExhausted(BudgetExhaustedPayload {
        budget,
        limit,
        detail,
    }))
}

fn decode_run_interrupted(value: &Value, issues: &mut Issues) -> Option<EventPayload> {
    let map = root_object(value, issues)?;
    let phase = req_string(map, "phase", 0, 200, "phase", issues)?;
    let hint = req_string(map, "hint", 0, 2000, "hint", issues)?;
    Some(EventPayload::RunInterrupted(RunInterruptedPayload {
        phase,
        hint,
    }))
}

/// Select the payload decoder for an event type.
pub(crate) fn decoder_for(event_type: &crate::events::RelayEventType) -> PayloadDecoder {
    use crate::events::RelayEventType as T;
    match event_type {
        T::ProjectInitialized => decode_project_initialized,
        T::SessionStarted => decode_session_started,
        T::IntentProposed => decode_intent_proposed,
        T::IntentApplied => decode_intent_applied,
        T::ModeEntered => decode_mode_entered,
        T::ModeTransitionRejected => decode_mode_transition_rejected,
        T::AutonomyChanged => decode_autonomy_changed,
        T::ArtifactCreated => decode_artifact_created,
        T::ArtifactRevised => decode_artifact_revised,
        T::ArtifactApprovalRequested => decode_artifact_approval_requested,
        T::ArtifactApproved => decode_artifact_approved,
        T::ArtifactInvalidated => decode_artifact_invalidated,
        T::ArtifactInspected => decode_artifact_inspected,
        T::TransitionEvaluated => decode_transition_evaluated,
        T::ModelRequested => decode_model_requested,
        T::ModelResponded => decode_model_responded,
        T::ModelFailed => decode_model_failed,
        T::ToolExecuted => decode_tool_executed,
        T::ToolDenied => decode_tool_denied,
        T::VerificationCompleted => decode_verification_completed,
        T::WorkflowPaused => decode_workflow_paused,
        T::WorkflowResumed => decode_workflow_resumed,
        T::BudgetExhausted => decode_budget_exhausted,
        T::RunInterrupted => decode_run_interrupted,
    }
}
