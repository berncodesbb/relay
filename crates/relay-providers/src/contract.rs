//! Provider-neutral model interface.
//!
//! Ported from `src/providers/provider.ts` (retired TypeScript tree, removed
//! from the repository 2026-10-14). This module is the only contract
//! between the runtime and model providers: it contains no SDK types, and
//! adapters translate provider responses into these Relay-owned types without
//! letting SDK objects cross their module boundary.

use std::fmt;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A JSON Schema object: an unordered set of string keys to JSON values.
pub type JsonSchemaObject = serde_json::Map<String, Value>;

/// Token usage reported by a provider. Provider-owned and neutral: no
/// crate above `relay-providers` needs to know where usage came from, and
/// `relay-providers` itself no longer depends on the frozen reference
/// domain for it. Wire spelling and semantics are unchanged from the
/// reference payload type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsage {
    /// Input tokens consumed.
    pub input_tokens: i64,
    /// Output tokens consumed.
    pub output_tokens: i64,
}

/// A tool exposed to a model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    /// Tool name (kebab-case, enforced by the tool registry).
    pub name: String,
    /// Human/model-readable description of the tool.
    pub description: String,
    /// JSON Schema for the tool arguments.
    pub parameters: JsonSchemaObject,
}

/// One tool invocation requested by a model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    /// Provider-assigned call id, echoed back with the tool result.
    pub id: String,
    /// Tool name.
    pub name: String,
    /// Tool arguments as parsed JSON (or the raw string when the model sent
    /// invalid JSON).
    pub arguments: Value,
}

/// One block of assistant message content.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum AssistantContent {
    /// Assistant prose.
    Text {
        /// The text.
        text: String,
    },
    /// A requested tool invocation.
    ToolCall {
        /// The call.
        call: ToolCall,
    },
}

/// One message in a model conversation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "kebab-case")]
pub enum ModelMessage {
    /// A user turn.
    User {
        /// The message text.
        content: String,
    },
    /// An assistant turn (text and/or tool calls in order).
    Assistant {
        /// Ordered content blocks.
        content: Vec<AssistantContent>,
    },
    /// The result of one tool call.
    ToolResult {
        /// Id of the answered tool call.
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        /// Tool output text.
        content: String,
    },
}

impl ModelMessage {
    /// The message text for user messages (empty string otherwise).
    pub fn text(&self) -> &str {
        match self {
            ModelMessage::User { content } => content,
            _ => "",
        }
    }
}

/// Why a model turn ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FinishReason {
    /// The model finished naturally.
    Stop,
    /// The model requested tool calls.
    ToolCalls,
    /// The token limit was reached.
    Length,
    /// The request was canceled.
    Canceled,
    /// The request failed.
    Error,
}

impl FinishReason {
    /// The wire spelling of this reason.
    pub fn as_str(self) -> &'static str {
        match self {
            FinishReason::Stop => "stop",
            FinishReason::ToolCalls => "tool-calls",
            FinishReason::Length => "length",
            FinishReason::Canceled => "canceled",
            FinishReason::Error => "error",
        }
    }

    /// Parse the wire spelling of a reason.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "stop" => Some(FinishReason::Stop),
            "tool-calls" => Some(FinishReason::ToolCalls),
            "length" => Some(FinishReason::Length),
            "canceled" => Some(FinishReason::Canceled),
            "error" => Some(FinishReason::Error),
            _ => None,
        }
    }
}

/// A request for schema-conformant structured output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StructuredOutputRequest {
    /// Logical name of the structured output, e.g. "intent-proposal".
    pub name: String,
    /// JSON Schema describing the expected object.
    pub schema: JsonSchemaObject,
}

/// Structured output captured from a completed turn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StructuredOutput {
    /// Logical name from the request.
    pub name: String,
    /// The parsed value the model produced.
    pub value: Value,
}

/// A cancellation flag shared with a provider request (Rust stand-in for the
/// TypeScript `AbortSignal`). Cheap to clone; canceling is observed by every
/// clone. The optional reason mirrors `AbortSignal.reason`.
#[derive(Debug, Clone, Default)]
pub struct CancelToken {
    flag: Arc<AtomicBool>,
    reason: Arc<Mutex<Option<String>>>,
}

impl CancelToken {
    /// A fresh, un-canceled token.
    pub fn new() -> Self {
        Self::default()
    }

    /// Cancel the token, recording why.
    pub fn cancel(&self, reason: impl Into<String>) {
        self.flag.store(true, Ordering::SeqCst);
        let mut slot = self
            .reason
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if slot.is_none() {
            *slot = Some(reason.into());
        }
    }

    /// Whether the token has been canceled.
    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    /// The recorded cancellation reason, if any.
    pub fn reason(&self) -> Option<String> {
        let slot = self
            .reason
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        slot.clone()
    }
}

/// One request to a model provider.
#[derive(Debug, Clone)]
pub struct ModelRequest {
    /// Model id (provider-specific).
    pub model: String,
    /// System/developer prompt.
    pub system: Option<String>,
    /// Conversation so far.
    pub messages: Vec<ModelMessage>,
    /// Tools the model may call.
    pub tools: Option<Vec<ToolSpec>>,
    /// Request for structured output.
    pub structured_output: Option<StructuredOutputRequest>,
    /// Output token ceiling.
    pub max_output_tokens: Option<u64>,
    /// Sampling temperature.
    pub temperature: Option<f64>,
    /// Logical purpose (e.g. "mode:discovery", "intent") used for routing
    /// logs and fixtures.
    pub purpose: Option<String>,
    /// Cancellation signal.
    pub signal: Option<CancelToken>,
}

impl ModelRequest {
    /// A minimal request to `model` with one user message.
    pub fn simple(model: impl Into<String>, user_text: impl Into<String>) -> Self {
        ModelRequest {
            model: model.into(),
            system: None,
            messages: vec![ModelMessage::User {
                content: user_text.into(),
            }],
            tools: None,
            structured_output: None,
            max_output_tokens: None,
            temperature: None,
            purpose: None,
            signal: None,
        }
    }

    /// Attach a purpose to this request.
    pub fn with_purpose(mut self, purpose: impl Into<String>) -> Self {
        self.purpose = Some(purpose.into());
        self
    }
}

/// One normalized event from a provider turn.
#[derive(Debug, Clone, PartialEq)]
pub enum ProviderStreamEvent {
    /// A fragment of assistant text.
    TextDelta {
        /// The text fragment.
        text: String,
    },
    /// A tool call (complete; adapters accumulate fragments first).
    ToolCall {
        /// The call.
        call: ToolCall,
    },
    /// Reported token usage (may arrive more than once).
    Usage {
        /// The usage snapshot.
        usage: TokenUsage,
    },
    /// Parsed structured output.
    StructuredOutput {
        /// Logical name from the request.
        name: String,
        /// The parsed value.
        value: Value,
    },
    /// The turn completed.
    Completed {
        /// Why the turn ended.
        finish_reason: FinishReason,
        /// Authoritative final usage.
        usage: TokenUsage,
    },
    /// The turn failed.
    Error {
        /// The normalized failure.
        error: ProviderFailure,
    },
}

/// Classification of a provider failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProviderErrorKind {
    /// HTTP 429 or an equivalent provider throttle.
    RateLimit,
    /// HTTP 5xx from the provider.
    Server,
    /// Connection-level transport failure.
    Network,
    /// Request or connection timed out.
    Timeout,
    /// The request was canceled through its signal.
    Canceled,
    /// HTTP 401/403: credentials rejected.
    Auth,
    /// The provider response could not be normalized.
    InvalidResponse,
    /// The request requires a capability the connection's effective profile
    /// does not allow; rejected before any HTTP call. Never retried or
    /// fallen back from — the configuration must change.
    UnsupportedCapability,
    /// Anything else.
    Unknown,
}

impl ProviderErrorKind {
    /// The wire spelling of this kind.
    pub fn as_str(self) -> &'static str {
        match self {
            ProviderErrorKind::RateLimit => "rate-limit",
            ProviderErrorKind::Server => "server",
            ProviderErrorKind::Network => "network",
            ProviderErrorKind::Timeout => "timeout",
            ProviderErrorKind::Canceled => "canceled",
            ProviderErrorKind::Auth => "auth",
            ProviderErrorKind::InvalidResponse => "invalid-response",
            ProviderErrorKind::UnsupportedCapability => "unsupported-capability",
            ProviderErrorKind::Unknown => "unknown",
        }
    }
}

/// A normalized, bounded, redacted provider failure.
///
/// The Rust port of the TypeScript `ProviderFailure` error class: kind,
/// message, retryability hint, and the HTTP status that produced it (if any).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderFailure {
    /// Failure classification.
    pub kind: ProviderErrorKind,
    /// Bounded, redacted human-readable description.
    pub message: String,
    /// Whether Relay's retry policy may retry this failure.
    pub retryable: bool,
    /// HTTP status code, when the failure came from an HTTP response.
    pub status: Option<u16>,
}

impl ProviderFailure {
    /// A failure with default (non-retryable, no status) hints.
    pub fn new(kind: ProviderErrorKind, message: impl Into<String>) -> Self {
        ProviderFailure {
            kind,
            message: message.into(),
            retryable: false,
            status: None,
        }
    }

    /// Set the retryability hint.
    pub fn retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
    }

    /// Attach the HTTP status that produced this failure.
    pub fn status(mut self, status: u16) -> Self {
        self.status = Some(status);
        self
    }
}

impl fmt::Display for ProviderFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ProviderFailure {}

/// The wire-protocol adapter kinds Relay ships.
///
/// Adapter identity is which *protocol* a provider connection speaks, not
/// which vendor it reaches: `openai-chat` covers every OpenAI-compatible
/// Chat Completions endpoint (OpenAI, GLM, Groq, OpenRouter, local
/// gateways). The user-facing *connection* identity is a configurable name
/// (`providers.<name>` in Relay configuration); this enum never grows one
/// variant per vendor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AdapterKind {
    /// OpenAI-compatible Chat Completions (`/chat/completions`).
    OpenaiChat,
    /// The native Anthropic Messages API.
    Anthropic,
    /// The deterministic in-process fake.
    Fake,
}

/// The adapter kinds Relay ships, in stable order.
pub const ADAPTER_KINDS: [AdapterKind; 3] = [
    AdapterKind::OpenaiChat,
    AdapterKind::Anthropic,
    AdapterKind::Fake,
];

impl AdapterKind {
    /// The wire/configuration spelling of this adapter kind.
    pub fn as_str(self) -> &'static str {
        match self {
            AdapterKind::OpenaiChat => "openai-chat",
            AdapterKind::Anthropic => "anthropic",
            AdapterKind::Fake => "fake",
        }
    }

    /// Parse the wire/configuration spelling of an adapter kind.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "openai-chat" => Some(AdapterKind::OpenaiChat),
            "anthropic" => Some(AdapterKind::Anthropic),
            "fake" => Some(AdapterKind::Fake),
            _ => None,
        }
    }
}

impl fmt::Display for AdapterKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How an adapter represents Relay's system/developer instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InstructionStyle {
    /// Sent as the protocol's developer-role message.
    #[serde(rename = "developer")]
    Developer,
    /// Translated to a system-role message (documented safe fallback for
    /// compatibility endpoints that do not recognize a developer role).
    #[serde(rename = "system-instruction")]
    SystemInstruction,
}

/// Whether a feature is available on an adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FeatureSupport {
    /// The adapter implements the feature.
    #[serde(rename = "supported")]
    Supported,
    /// The adapter does not implement the feature; requests that require it
    /// fail before any HTTP call rather than being silently downgraded.
    #[serde(rename = "unsupported")]
    Unsupported,
}

/// How an adapter carries Relay's structured-output request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StructuredOutputMode {
    /// Native `json_schema` response formats.
    #[serde(rename = "json-schema")]
    JsonSchema,
    /// `json_object` response format with the JSON Schema embedded in the
    /// instruction text (documented translation).
    #[serde(rename = "json-object")]
    JsonObject,
    /// Structured output cannot be requested; fail before any HTTP call.
    #[serde(rename = "unsupported")]
    Unsupported,
}

/// The wire parameter an adapter uses for Relay's output-token ceiling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutputTokenParam {
    /// Always `max_tokens`.
    #[serde(rename = "max-tokens")]
    MaxTokens,
    /// Always `max_completion_tokens`.
    #[serde(rename = "max-completion-tokens")]
    MaxCompletionTokens,
    /// Choose per model with the adapter's documented heuristic (the
    /// OpenAI-compatible default: `gpt-5*`/`o<digit>*` models use
    /// `max_completion_tokens`, others `max_tokens`).
    #[serde(rename = "model-default")]
    ModelDefault,
}

/// The effective capability profile of one adapter: what a connection built
/// on that adapter can do, and which documented translations apply.
///
/// Effective profiles resolve deterministically as
/// `adapter defaults -> optional preset -> explicit overrides`
/// (see [`resolve_capabilities`]); a requested Relay feature is either
/// translated by a documented rule recorded here or rejected with an
/// actionable error before any HTTP request is issued.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdapterCapabilities {
    /// How `system`/developer instructions are represented.
    pub instructions: InstructionStyle,
    /// Whether local function tools can be requested.
    pub tool_calls: FeatureSupport,
    /// Whether tool arguments may stream incrementally.
    pub streamed_tool_arguments: FeatureSupport,
    /// Whether parallel tool calls may be requested.
    pub parallel_tool_calls: FeatureSupport,
    /// How structured output is carried.
    pub structured_output: StructuredOutputMode,
    /// Whether token usage is reported on the stream.
    pub streamed_usage: FeatureSupport,
    /// The output-token parameter convention.
    pub output_token_param: OutputTokenParam,
    /// Whether image/file inputs can be sent (unsupported everywhere for
    /// now; recorded so the gap fails early when Relay requests it).
    pub image_input: FeatureSupport,
}

impl AdapterCapabilities {
    /// The conservative adapter defaults for a kind.
    pub fn defaults_for(kind: AdapterKind) -> Self {
        match kind {
            AdapterKind::OpenaiChat => AdapterCapabilities {
                instructions: InstructionStyle::Developer,
                tool_calls: FeatureSupport::Supported,
                streamed_tool_arguments: FeatureSupport::Supported,
                parallel_tool_calls: FeatureSupport::Unsupported,
                structured_output: StructuredOutputMode::JsonSchema,
                streamed_usage: FeatureSupport::Supported,
                output_token_param: OutputTokenParam::ModelDefault,
                image_input: FeatureSupport::Unsupported,
            },
            AdapterKind::Anthropic => AdapterCapabilities {
                instructions: InstructionStyle::SystemInstruction,
                tool_calls: FeatureSupport::Supported,
                streamed_tool_arguments: FeatureSupport::Supported,
                parallel_tool_calls: FeatureSupport::Unsupported,
                structured_output: StructuredOutputMode::JsonObject,
                streamed_usage: FeatureSupport::Supported,
                output_token_param: OutputTokenParam::MaxTokens,
                image_input: FeatureSupport::Unsupported,
            },
            AdapterKind::Fake => AdapterCapabilities {
                instructions: InstructionStyle::SystemInstruction,
                tool_calls: FeatureSupport::Supported,
                streamed_tool_arguments: FeatureSupport::Supported,
                parallel_tool_calls: FeatureSupport::Supported,
                structured_output: StructuredOutputMode::JsonSchema,
                streamed_usage: FeatureSupport::Supported,
                output_token_param: OutputTokenParam::MaxTokens,
                image_input: FeatureSupport::Unsupported,
            },
        }
    }
}

/// Explicit per-field capability overrides from configuration. `None` fields
/// inherit the preset value (when a preset is set) or the adapter default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityOverrides {
    /// Override the instruction representation.
    pub instructions: Option<InstructionStyle>,
    /// Override function-tool support.
    pub tool_calls: Option<FeatureSupport>,
    /// Override streamed tool arguments.
    pub streamed_tool_arguments: Option<FeatureSupport>,
    /// Override parallel tool calls.
    pub parallel_tool_calls: Option<FeatureSupport>,
    /// Override structured-output mode.
    pub structured_output: Option<StructuredOutputMode>,
    /// Override streamed usage.
    pub streamed_usage: Option<FeatureSupport>,
    /// Override the output-token parameter convention.
    pub output_token_param: Option<OutputTokenParam>,
    /// Override image/file input support.
    pub image_input: Option<FeatureSupport>,
}

impl CapabilityOverrides {
    /// Whether every field is unset.
    pub fn is_empty(&self) -> bool {
        *self == CapabilityOverrides::default()
    }
}

/// The named capability presets Relay knows, in stable order. Presets are
/// complete profiles layered over the adapter defaults before explicit
/// overrides; the first is `glm` (OpenAI-compatible Chat Completions with
/// conservative compatibility behavior).
pub const PRESET_NAMES: [&str; 1] = ["glm"];

/// The named preset profile, or `None` for an unknown preset name.
pub fn preset_capabilities(name: &str) -> Option<AdapterCapabilities> {
    match name {
        // GLM's OpenAI-compatible endpoint: system instructions rather than
        // the developer role, json-object structured output, no streamed
        // usage guarantee, and plain max_tokens.
        "glm" => Some(AdapterCapabilities {
            instructions: InstructionStyle::SystemInstruction,
            structured_output: StructuredOutputMode::JsonObject,
            streamed_usage: FeatureSupport::Unsupported,
            output_token_param: OutputTokenParam::MaxTokens,
            ..AdapterCapabilities::defaults_for(AdapterKind::OpenaiChat)
        }),
        _ => None,
    }
}

/// An invalid capability/preset combination, with an actionable message.
#[derive(Debug, Clone, Eq, PartialEq, thiserror::Error)]
#[error("{0}")]
pub struct CapabilityError(pub String);

/// Resolve the effective capability profile: adapter defaults, then the
/// named preset (when set), then explicit overrides (highest precedence).
/// Unknown preset names fail with an actionable message.
pub fn resolve_capabilities(
    kind: AdapterKind,
    preset: Option<&str>,
    overrides: &CapabilityOverrides,
) -> Result<AdapterCapabilities, CapabilityError> {
    let mut capabilities = AdapterCapabilities::defaults_for(kind);
    if let Some(preset) = preset {
        let profile = preset_capabilities(preset).ok_or_else(|| {
            CapabilityError(format!(
                "unknown capability preset \"{preset}\" (available presets: {})",
                PRESET_NAMES.join(", ")
            ))
        })?;
        // A preset only refines the profile when the adapter kind matches
        // the preset's protocol family; otherwise it is a configuration
        // error rather than a silent mismatch.
        let expected = preset_adapter(preset);
        if expected != kind {
            return Err(CapabilityError(format!(
                "capability preset \"{preset}\" applies to the {expected} adapter, not {kind}"
            )));
        }
        capabilities = profile;
    }
    if let Some(value) = overrides.instructions {
        capabilities.instructions = value;
    }
    if let Some(value) = overrides.tool_calls {
        capabilities.tool_calls = value;
    }
    if let Some(value) = overrides.streamed_tool_arguments {
        capabilities.streamed_tool_arguments = value;
    }
    if let Some(value) = overrides.parallel_tool_calls {
        capabilities.parallel_tool_calls = value;
    }
    if let Some(value) = overrides.structured_output {
        capabilities.structured_output = value;
    }
    if let Some(value) = overrides.streamed_usage {
        capabilities.streamed_usage = value;
    }
    if let Some(value) = overrides.output_token_param {
        capabilities.output_token_param = value;
    }
    if let Some(value) = overrides.image_input {
        capabilities.image_input = value;
    }
    Ok(capabilities)
}

/// The adapter kind a named preset is defined against.
pub fn preset_adapter(name: &str) -> AdapterKind {
    match name {
        "glm" => AdapterKind::OpenaiChat,
        _ => AdapterKind::Fake,
    }
}

/// A provider turn is a lazy stream of normalized events.
pub type ProviderEventStream<'a> = Box<dyn Iterator<Item = ProviderStreamEvent> + Send + 'a>;

/// A model provider: name, advertised models, and a streaming call.
///
/// A model provider returns a stream of Relay-owned events. Relay's event log
/// and context builder remain the authoritative conversation record;
/// provider-side conversation ids are optional optimizations adapters may
/// expose separately.
pub trait ModelProvider: Send + Sync {
    /// The adapter kind this provider implements (protocol identity, not a
    /// configured connection name).
    fn name(&self) -> AdapterKind;

    /// Model ids this provider reports for configuration validation.
    fn models(&self) -> Vec<String>;

    /// Stream one model turn. Failures are reported as
    /// [`ProviderStreamEvent::Error`] events, never by panicking or by
    /// letting provider SDK types escape.
    fn stream(&self, request: &ModelRequest) -> ProviderEventStream<'_>;
}

/// Aggregated result of one model turn, produced from a stream.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelTurnResult {
    /// Concatenated text deltas.
    pub text: String,
    /// Tool calls in arrival order.
    pub tool_calls: Vec<ToolCall>,
    /// Captured structured output, when the turn produced one.
    pub structured_output: Option<StructuredOutput>,
    /// Why the turn ended.
    pub finish_reason: FinishReason,
    /// Token usage.
    pub usage: TokenUsage,
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn finish_reason_round_trips_wire_spellings() {
        for reason in [
            FinishReason::Stop,
            FinishReason::ToolCalls,
            FinishReason::Length,
            FinishReason::Canceled,
            FinishReason::Error,
        ] {
            assert_eq!(FinishReason::parse(reason.as_str()), Some(reason));
        }
        assert_eq!(FinishReason::parse("nope"), None);
    }

    #[test]
    fn adapter_kind_round_trips_wire_spellings() {
        for kind in ADAPTER_KINDS {
            assert_eq!(AdapterKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(AdapterKind::parse("openai"), None);
        assert_eq!(AdapterKind::parse("bedrock"), None);
    }

    #[test]
    fn capabilities_resolve_defaults_preset_then_overrides() {
        let base = resolve_capabilities(
            AdapterKind::OpenaiChat,
            None,
            &CapabilityOverrides::default(),
        )
        .expect("defaults resolve");
        assert_eq!(base.instructions, InstructionStyle::Developer);
        assert_eq!(base.structured_output, StructuredOutputMode::JsonSchema);

        let glm = resolve_capabilities(
            AdapterKind::OpenaiChat,
            Some("glm"),
            &CapabilityOverrides::default(),
        )
        .expect("preset resolves");
        assert_eq!(glm.instructions, InstructionStyle::SystemInstruction);
        assert_eq!(glm.structured_output, StructuredOutputMode::JsonObject);
        assert_eq!(glm.streamed_usage, FeatureSupport::Unsupported);
        assert_eq!(glm.output_token_param, OutputTokenParam::MaxTokens);

        let overrides = CapabilityOverrides {
            structured_output: Some(StructuredOutputMode::JsonSchema),
            ..CapabilityOverrides::default()
        };
        let forced = resolve_capabilities(AdapterKind::OpenaiChat, Some("glm"), &overrides)
            .expect("overrides win");
        assert_eq!(forced.structured_output, StructuredOutputMode::JsonSchema);
        assert_eq!(forced.instructions, InstructionStyle::SystemInstruction);

        let error = resolve_capabilities(AdapterKind::OpenaiChat, Some("grok"), &overrides)
            .expect_err("unknown preset");
        assert!(
            error.to_string().contains("unknown capability preset"),
            "{error}"
        );

        let error = resolve_capabilities(AdapterKind::Anthropic, Some("glm"), &overrides)
            .expect_err("preset/adapter mismatch");
        assert!(
            error
                .to_string()
                .contains("applies to the openai-chat adapter"),
            "{error}"
        );
    }

    #[test]
    fn cancel_token_records_reason_once_and_shares_state() {
        let token = CancelToken::new();
        let clone = token.clone();
        assert!(!token.is_cancelled());
        token.cancel("user pressed ctrl-c");
        assert!(clone.is_cancelled());
        assert_eq!(clone.reason().as_deref(), Some("user pressed ctrl-c"));
        token.cancel("second reason is ignored");
        assert_eq!(token.reason().as_deref(), Some("user pressed ctrl-c"));
    }

    #[test]
    fn model_message_serializes_with_reference_field_names() {
        let message = ModelMessage::ToolResult {
            tool_call_id: "call_1".into(),
            content: "file body".into(),
        };
        let value = serde_json::to_value(&message).expect("serializable");
        assert_eq!(value["role"], "tool-result");
        assert_eq!(value["toolCallId"], "call_1");
        assert_eq!(value["content"], "file body");
    }

    #[test]
    fn provider_failure_display_is_the_message() {
        let failure = ProviderFailure::new(ProviderErrorKind::Auth, "nope").status(401);
        assert_eq!(failure.to_string(), "nope");
        assert_eq!(failure.status, Some(401));
        assert!(!failure.retryable);
    }
}
