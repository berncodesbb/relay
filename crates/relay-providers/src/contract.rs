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

// ---------------------------------------------------------------------------
// Bounds
// ---------------------------------------------------------------------------

/// Maximum size, in bytes, of one assistant text block accumulated from a
/// provider stream. This is a fail-closed backstop far above any legitimate
/// single block Relay expects (final structured proposals are bounded to a
/// few tens of thousands of characters by the executor): a stream whose open
/// text block exceeds it fails with [`ProviderErrorKind::InvalidResponse`]
/// instead of being silently truncated or retained unbounded.
pub const MAX_TEXT_BLOCK_BYTES: usize = 512 * 1024;

/// Maximum size, in bytes, of one tool-result content string. Tool results
/// are Relay-produced (never provider-produced), so they are bounded at
/// creation by the caller — [`ModelMessage::tool_result`] enforces this
/// constant as the deterministic backstop. It sits above the tool layer's
/// own per-tool output bounds so legitimate bounded results always pass.
pub const MAX_TOOL_RESULT_CONTENT_BYTES: usize = 64 * 1024;

/// Maximum size, in bytes, of the opaque payload of one continuation block
/// (see [`AssistantContent::Continuation`]). A provider stream carrying a
/// larger continuation fails with [`ProviderErrorKind::InvalidResponse`].
pub const MAX_CONTINUATION_DATA_BYTES: usize = 16 * 1024;

/// Maximum length, in characters, of a continuation block's provider
/// discriminator (see [`AssistantContent::Continuation`]). A provider
/// stream carrying a longer discriminator fails with
/// [`ProviderErrorKind::InvalidResponse`].
pub const MAX_CONTINUATION_DISCRIMINATOR_CHARS: usize = 64;

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

/// One block of user-authored conversation content.
///
/// User turns are ordered block lists (`Vec<UserContent>`) so additional
/// input kinds — images, documents — can be added later without changing
/// the transcript shape. The variant set is deliberately minimal today:
/// plain text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum UserContent {
    /// User prose.
    Text {
        /// The text.
        text: String,
    },
}

impl UserContent {
    /// The visible text of this block (empty for non-text blocks).
    pub fn text(&self) -> &str {
        match self {
            UserContent::Text { text } => text,
        }
    }
}

/// One block of assistant message content, in true production order.
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
    /// Opaque, provider-specific continuation state (for example signed or
    /// redacted reasoning) that a compatible endpoint requires to be
    /// round-tripped on a later turn of the same conversation.
    ///
    /// This is the one deliberately non-neutral block in the contract, and
    /// it is firewalled:
    ///
    /// - Relay never interprets, renders, logs, or persists it as domain
    ///   truth; it never enters JSONL facts, SQLite projections, artifacts,
    ///   receipts, or user-facing output.
    /// - It is bounded ([`MAX_CONTINUATION_DATA_BYTES`] payload,
    ///   [`MAX_CONTINUATION_DISCRIMINATOR_CHARS`] discriminator).
    /// - It is excluded from the visible-text accessors
    ///   ([`ModelTurnResult::text`], [`ModelMessage::text`]).
    /// - It is replayed only to the same compatible route; routing must
    ///   never carry it across providers or conversations.
    Continuation {
        /// Provider-specific discriminator naming the continuation kind
        /// (bounded; used only to match the block back to its route).
        discriminator: String,
        /// The opaque bounded payload, carried as text.
        data: String,
    },
}

/// One message in a model conversation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "kebab-case")]
pub enum ModelMessage {
    /// A user turn (ordered content blocks).
    User {
        /// Ordered content blocks.
        content: Vec<UserContent>,
    },
    /// An assistant turn (text, tool calls, and opaque continuation blocks
    /// in true order).
    Assistant {
        /// Ordered content blocks.
        content: Vec<AssistantContent>,
    },
    /// The result of one tool call.
    ToolResult {
        /// Id of the answered tool call.
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        /// Tool output text, bounded at creation (see
        /// [`ModelMessage::tool_result`] and
        /// [`MAX_TOOL_RESULT_CONTENT_BYTES`]).
        content: String,
        /// Whether this result reports a failed, denied, or otherwise
        /// erroneous execution rather than success. Providers surface the
        /// flag to the model so an error result is never mistaken for a
        /// successful observation.
        #[serde(rename = "isError")]
        is_error: bool,
    },
}

impl ModelMessage {
    /// A user message with a single text block (the common prompt-shaped
    /// turn).
    pub fn user_text(text: impl Into<String>) -> Self {
        ModelMessage::User {
            content: vec![UserContent::Text { text: text.into() }],
        }
    }

    /// A tool result, bounded at creation. Tool results are Relay-produced,
    /// never provider-produced: callers bound the content before sending,
    /// and this constructor is the deterministic backstop, rejecting
    /// content above [`MAX_TOOL_RESULT_CONTENT_BYTES`] with an actionable
    /// message instead of truncating a result the model would then act on
    /// as complete.
    pub fn tool_result(
        tool_call_id: impl Into<String>,
        content: impl Into<String>,
        is_error: bool,
    ) -> Result<Self, String> {
        let content = content.into();
        if content.len() > MAX_TOOL_RESULT_CONTENT_BYTES {
            return Err(format!(
                "tool result for call \"{}\" is {} bytes, above the {}-byte bound; bound the \
                 output before returning it to the model",
                tool_call_id.into(),
                content.len(),
                MAX_TOOL_RESULT_CONTENT_BYTES
            ));
        }
        Ok(ModelMessage::ToolResult {
            tool_call_id: tool_call_id.into(),
            content,
            is_error,
        })
    }

    /// The visible text of this message: concatenated text blocks for user
    /// and assistant turns (opaque continuation blocks excluded), and the
    /// content for tool results. Empty string when there is nothing
    /// visible.
    pub fn text(&self) -> String {
        match self {
            ModelMessage::User { content } => {
                content.iter().map(|block| block.text()).collect::<String>()
            }
            ModelMessage::Assistant { content } => content
                .iter()
                .filter_map(|block| match block {
                    AssistantContent::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect(),
            ModelMessage::ToolResult { content, .. } => content.clone(),
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
    /// A minimal request to `model` with one user text message.
    pub fn simple(model: impl Into<String>, user_text: impl Into<String>) -> Self {
        ModelRequest {
            model: model.into(),
            system: None,
            messages: vec![ModelMessage::user_text(user_text)],
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
///
/// # Ordering contract
///
/// Adapters must emit block-bearing events (`TextDelta`, `ToolCall`,
/// `Continuation`) in the true order the model produced the blocks, because
/// [`collect`](crate::collect_turn) reconstructs the ordered block list from
/// them alone:
///
/// - consecutive `TextDelta` fragments accumulate into one open text block;
/// - a complete `ToolCall` or `Continuation` event closes any open text
///   block first, so text → call → text survives as three ordered blocks;
/// - `Completed` and `Error` are terminal: nothing may follow them, and a
///   stream that ends without one of them is an invalid response (see
///   [`collect_turn`](crate::collect_turn)).
#[derive(Debug, Clone, PartialEq)]
pub enum ProviderStreamEvent {
    /// A fragment of assistant text; consecutive fragments join one open
    /// text block until a `ToolCall`, `Continuation`, or terminal event
    /// closes it.
    TextDelta {
        /// The text fragment.
        text: String,
    },
    /// A complete, validated, atomic tool call. Adapters accumulate argument
    /// fragments themselves and emit one event per call, in arrival order,
    /// only once the call is complete (never partial JSON that callers would
    /// have to parse).
    ToolCall {
        /// The call.
        call: ToolCall,
    },
    /// A complete, bounded, opaque provider continuation block (see
    /// [`AssistantContent::Continuation`]). Closes any open text block, so
    /// surrounding text keeps its true position around it.
    Continuation {
        /// Provider-specific discriminator naming the continuation kind.
        discriminator: String,
        /// The opaque bounded payload.
        data: String,
    },
    /// Reported token usage (may arrive more than once; the terminal
    /// `Completed` event carries the authoritative final usage).
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
/// which vendor it reaches: `anthropic` covers every Anthropic
/// Messages-compatible endpoint. The user-facing *connection* identity is a
/// configurable name (`providers.<name>` in Relay configuration); this enum
/// never grows one variant per vendor.
///
/// The historical `openai-chat` adapter (every OpenAI-compatible Chat
/// Completions endpoint) was intentionally removed; configuration carrying
/// that spelling fails at the configuration boundary with migration
/// guidance, and no runtime variant remains.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AdapterKind {
    /// The native Anthropic Messages API (the sole live external protocol).
    Anthropic,
    /// The deterministic in-process fake.
    Fake,
}

/// The adapter kinds Relay ships, in stable order.
pub const ADAPTER_KINDS: [AdapterKind; 2] = [AdapterKind::Anthropic, AdapterKind::Fake];

impl AdapterKind {
    /// The wire/configuration spelling of this adapter kind.
    pub fn as_str(self) -> &'static str {
        match self {
            AdapterKind::Anthropic => "anthropic",
            AdapterKind::Fake => "fake",
        }
    }

    /// Parse the wire/configuration spelling of an adapter kind. The removed
    /// `openai-chat` spelling (like every other unknown value) does not
    /// parse; configuration surfaces the migration diagnostic instead.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
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
    /// Historical: the developer-role spelling of the removed `openai-chat`
    /// adapter. No shipped adapter constructs it and configuration rejects
    /// the spelling with the removal diagnostic; retained only so the
    /// deserialized shape of in-flight values stays defined.
    #[serde(rename = "developer")]
    Developer,
    /// Instructions travel in the protocol's system parameter — the only
    /// style the shipped adapters use.
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
    /// Always `max_completion_tokens`. Historical: this spelling and the
    /// per-model heuristic below existed only for OpenAI-compatible
    /// endpoints and were removed with the `openai-chat` adapter; no shipped
    /// adapter selects them, and configuration rejects the spellings with
    /// migration guidance.
    #[serde(rename = "max-completion-tokens")]
    MaxCompletionTokens,
    /// Choose per model with the adapter's documented heuristic. Historical:
    /// removed with the `openai-chat` adapter (see
    /// [`OutputTokenParam::MaxCompletionTokens`]).
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
/// overrides. The list is currently empty: the historical `glm` preset
/// (OpenAI-compatible Chat Completions) was removed with the `openai-chat`
/// adapter, and configuration rejects that spelling with migration guidance.
/// The mechanism stays so a future preset can land without another contract
/// change.
pub const PRESET_NAMES: [&str; 0] = [];

/// The named preset profile, or `None` for an unknown preset name.
pub fn preset_capabilities(_name: &str) -> Option<AdapterCapabilities> {
    // No presets are currently defined (see [`PRESET_NAMES`]); every name is
    // unknown and callers surface the actionable unknown-preset error.
    None
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
            let available = if PRESET_NAMES.is_empty() {
                "no capability presets are currently defined; set explicit \
                 capability overrides instead"
                    .to_owned()
            } else {
                format!("available presets: {}", PRESET_NAMES.join(", "))
            };
            CapabilityError(format!(
                "unknown capability preset \"{preset}\" ({available})"
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

/// The adapter kind a named preset is defined against. No presets are
/// currently defined (see [`PRESET_NAMES`]), so this is only reachable for a
/// name [`preset_capabilities`] already accepted; the fake adapter is the
/// inert placeholder until a real preset exists.
pub fn preset_adapter(_name: &str) -> AdapterKind {
    AdapterKind::Fake
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
///
/// [`blocks`](Self::blocks) is the authoritative output: the assistant's
/// text, tool calls, and opaque continuation blocks in the true order the
/// model produced them. The accessors are conveniences over that order and
/// never replace it: [`text`](Self::text) concatenates the visible text
/// blocks, and [`tool_calls`](Self::tool_calls) collects the calls in
/// arrival order.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelTurnResult {
    /// Ordered assistant content blocks (authoritative).
    pub blocks: Vec<AssistantContent>,
    /// Captured structured output, when the turn produced one.
    pub structured_output: Option<StructuredOutput>,
    /// Why the turn ended.
    pub finish_reason: FinishReason,
    /// Token usage; the terminal `Completed` event's usage is authoritative.
    pub usage: TokenUsage,
}

impl ModelTurnResult {
    /// Concatenated visible text across all text blocks. Tool calls and
    /// opaque continuation blocks are excluded, so text before and after a
    /// tool call joins without a separator exactly like a folded stream of
    /// deltas.
    pub fn text(&self) -> String {
        self.blocks
            .iter()
            .filter_map(|block| match block {
                AssistantContent::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    /// Tool calls in arrival order.
    pub fn tool_calls(&self) -> Vec<ToolCall> {
        self.blocks
            .iter()
            .filter_map(|block| match block {
                AssistantContent::ToolCall { call } => Some(call.clone()),
                _ => None,
            })
            .collect()
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use serde_json::json;

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
        // Negative spellings stay negative, including the removed protocol's
        // vendor name and its historical adapter spelling.
        assert_eq!(AdapterKind::parse("openai"), None);
        assert_eq!(AdapterKind::parse("openai-chat"), None);
        assert_eq!(AdapterKind::parse("bedrock"), None);
    }

    #[test]
    fn capabilities_resolve_defaults_then_overrides() {
        let base = resolve_capabilities(
            AdapterKind::Anthropic,
            None,
            &CapabilityOverrides::default(),
        )
        .expect("defaults resolve");
        assert_eq!(base.instructions, InstructionStyle::SystemInstruction);
        assert_eq!(base.structured_output, StructuredOutputMode::JsonObject);

        let overrides = CapabilityOverrides {
            structured_output: Some(StructuredOutputMode::JsonSchema),
            ..CapabilityOverrides::default()
        };
        let forced =
            resolve_capabilities(AdapterKind::Anthropic, None, &overrides).expect("overrides win");
        assert_eq!(forced.structured_output, StructuredOutputMode::JsonSchema);
        assert_eq!(forced.instructions, InstructionStyle::SystemInstruction);
    }

    #[test]
    fn unknown_presets_fail_actionably_and_the_preset_list_is_empty() {
        assert!(PRESET_NAMES.is_empty());
        assert!(preset_capabilities("glm").is_none());
        assert!(preset_capabilities("anything").is_none());

        let error = resolve_capabilities(
            AdapterKind::Anthropic,
            Some("glm"),
            &CapabilityOverrides::default(),
        )
        .expect_err("the removed preset no longer resolves");
        assert!(
            error
                .to_string()
                .contains("unknown capability preset \"glm\""),
            "{error}"
        );
        assert!(
            error
                .to_string()
                .contains("no capability presets are currently defined"),
            "{error}"
        );
        assert!(
            error.to_string().contains("explicit capability overrides"),
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
        let message = ModelMessage::tool_result("call_1", "file body", false).expect("bounded");
        let value = serde_json::to_value(&message).expect("serializable");
        assert_eq!(value["role"], "tool-result");
        assert_eq!(value["toolCallId"], "call_1");
        assert_eq!(value["content"], "file body");
        assert_eq!(value["isError"], false);
        // and the explicit error flag round-trips through the same names
        let error_result =
            ModelMessage::tool_result("call_2", "denied by grant", true).expect("bounded");
        let value = serde_json::to_value(&error_result).expect("serializable");
        assert_eq!(value["isError"], true);
        let round: ModelMessage = serde_json::from_value(value).expect("deserializable");
        assert_eq!(round, error_result);
    }

    #[test]
    fn user_messages_carry_ordered_blocks_with_stable_field_names() {
        let message = ModelMessage::User {
            content: vec![
                UserContent::Text {
                    text: "first".into(),
                },
                UserContent::Text {
                    text: "second".into(),
                },
            ],
        };
        let value = serde_json::to_value(&message).expect("serializable");
        assert_eq!(
            value,
            json!({
                "role": "user",
                "content": [
                    { "type": "text", "text": "first" },
                    { "type": "text", "text": "second" },
                ],
            })
        );
        let round: ModelMessage = serde_json::from_value(value).expect("deserializable");
        assert_eq!(round, message);
        assert_eq!(round.text(), "firstsecond");
    }

    #[test]
    fn user_text_builds_a_single_block_and_reads_back() {
        let message = ModelMessage::user_text("the prompt");
        assert_eq!(
            message,
            ModelMessage::User {
                content: vec![UserContent::Text {
                    text: "the prompt".into()
                }]
            }
        );
        assert_eq!(message.text(), "the prompt");
    }

    #[test]
    fn tool_results_are_bounded_at_creation() {
        let ok =
            ModelMessage::tool_result("call_1", "x".repeat(MAX_TOOL_RESULT_CONTENT_BYTES), false);
        assert!(ok.is_ok(), "the bound itself is allowed");

        let oversized = "x".repeat(MAX_TOOL_RESULT_CONTENT_BYTES + 1);
        let error = ModelMessage::tool_result("call_1", oversized, true).expect_err("oversized");
        assert!(error.contains("above the"), "{error}");
        assert!(error.contains("call_1"), "{error}");
    }

    #[test]
    fn continuation_blocks_serialize_with_neutral_names_and_hide_from_text() {
        let message = ModelMessage::Assistant {
            content: vec![
                AssistantContent::Text {
                    text: "visible".into(),
                },
                AssistantContent::Continuation {
                    discriminator: "thinking".into(),
                    data: "opaque-payload".into(),
                },
            ],
        };
        let value = serde_json::to_value(&message).expect("serializable");
        assert_eq!(
            value["content"][1],
            json!({
                "type": "continuation",
                "discriminator": "thinking",
                "data": "opaque-payload",
            })
        );
        let round: ModelMessage = serde_json::from_value(value).expect("deserializable");
        assert_eq!(round, message);
        // Visible text accessors skip the opaque block entirely.
        assert_eq!(round.text(), "visible");
    }

    #[test]
    fn turn_result_accessors_expose_order_without_replacing_it() {
        let result = ModelTurnResult {
            blocks: vec![
                AssistantContent::Text {
                    text: "before ".into(),
                },
                AssistantContent::ToolCall {
                    call: ToolCall {
                        id: "c1".into(),
                        name: "read-file".into(),
                        arguments: json!({ "path": "a" }),
                    },
                },
                AssistantContent::Text {
                    text: "after".into(),
                },
                AssistantContent::Continuation {
                    discriminator: "thinking".into(),
                    data: "opaque".into(),
                },
            ],
            structured_output: None,
            finish_reason: FinishReason::ToolCalls,
            usage: TokenUsage::default(),
        };
        assert_eq!(result.text(), "before after");
        assert_eq!(
            result
                .tool_calls()
                .iter()
                .map(|call| call.id.as_str())
                .collect::<Vec<_>>(),
            ["c1"]
        );
        // The authoritative block list keeps the interleaved order.
        let kinds: Vec<&str> = result
            .blocks
            .iter()
            .map(|block| match block {
                AssistantContent::Text { .. } => "text",
                AssistantContent::ToolCall { .. } => "tool-call",
                AssistantContent::Continuation { .. } => "continuation",
            })
            .collect();
        assert_eq!(kinds, ["text", "tool-call", "text", "continuation"]);
    }

    #[test]
    fn provider_failure_display_is_the_message() {
        let failure = ProviderFailure::new(ProviderErrorKind::Auth, "nope").status(401);
        assert_eq!(failure.to_string(), "nope");
        assert_eq!(failure.status, Some(401));
        assert!(!failure.retryable);
    }
}
