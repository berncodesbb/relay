//! Provider-neutral model interface.
//!
//! Ported from `src/providers/provider.ts`. This module is the only contract
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

    /// Attach a cancellation signal to this request.
    pub fn with_signal(mut self, signal: CancelToken) -> Self {
        self.signal = Some(signal);
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

/// Names of the built-in providers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProviderName {
    /// The OpenAI chat-completions adapter.
    Openai,
    /// The Anthropic Messages adapter.
    Anthropic,
    /// The deterministic in-process fake.
    Fake,
}

impl ProviderName {
    /// The wire spelling of this provider.
    pub fn as_str(self) -> &'static str {
        match self {
            ProviderName::Openai => "openai",
            ProviderName::Anthropic => "anthropic",
            ProviderName::Fake => "fake",
        }
    }

    /// Parse the wire spelling of a provider.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "openai" => Some(ProviderName::Openai),
            "anthropic" => Some(ProviderName::Anthropic),
            "fake" => Some(ProviderName::Fake),
            _ => None,
        }
    }
}

impl fmt::Display for ProviderName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
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
    /// The provider's name.
    fn name(&self) -> ProviderName;

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
    fn provider_name_round_trips_wire_spellings() {
        for provider in [
            ProviderName::Openai,
            ProviderName::Anthropic,
            ProviderName::Fake,
        ] {
            assert_eq!(ProviderName::parse(provider.as_str()), Some(provider));
        }
        assert_eq!(ProviderName::parse("bedrock"), None);
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
