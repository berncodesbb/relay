//! Anthropic adapter.
//!
//! Ported from `src/providers/anthropic.ts` (retired TypeScript tree,
//! removed from the repository 2026-10-14). Translates Relay-owned requests
//! into Anthropic Messages API wire calls and normalizes the raw stream events
//! back into Relay-owned provider events. Anthropic requires `max_tokens`
//! (defaulted here), has no native JSON-schema response mode (structured
//! output is requested via a system instruction), and requires alternating
//! roles, so consecutive tool results are merged into a single user message.
//! Wire types stay private to this module.
//!
//! # Stream normalization
//!
//! Block-bearing events are emitted in the true order the model produced
//! them: text deltas stream as [`ProviderStreamEvent::TextDelta`] fragments,
//! and each tool-use or thinking block is emitted as one complete, validated
//! [`ProviderStreamEvent::ToolCall`] / [`ProviderStreamEvent::Continuation`]
//! event the moment its `content_block_stop` arrives, so text → tool → text
//! survives as ordered blocks. The terminal
//! [`ProviderStreamEvent::Completed`] fires on `message_stop` only.
//!
//! The stream fails closed — it ends with a typed
//! [`ProviderErrorKind::InvalidResponse`] error event instead of a
//! permissive fallback — on:
//!
//! - a duplicate `content_block_start` index, or a delta/stop for an index
//!   that was never announced or has already closed;
//! - a delta of a known kind applied to a block of the wrong kind (for
//!   example `input_json_delta` on a text block);
//! - a `tool_use` block missing its `id` or `name`;
//! - a tool block whose accumulated `partial_json` is non-empty and not
//!   valid JSON at block stop (an empty buffer maps to `{}`);
//! - a stream that ends (EOF) without `message_stop`;
//! - a text block, tool-argument buffer, or thinking/redacted-thinking
//!   continuation payload above the contract bounds.
//!
//! Unknown *event types* (and `ping`) stay tolerated, mirroring the
//! reference adapter's default branch.
//!
//! # Thinking continuations
//!
//! `thinking` and `redacted_thinking` blocks are captured as bounded opaque
//! [`AssistantContent::Continuation`] blocks (discriminators `thinking` and
//! `redacted-thinking`) and never contribute visible text. When those blocks
//! reappear in the request transcript they are mapped back onto the wire so a
//! compatible endpoint can continue the conversation; unknown discriminators
//! or undecodable payloads fail the request before any HTTP call rather than
//! being silently dropped.
//!
//! # Capability enforcement
//!
//! The connection's [`AdapterCapabilities`] (adapter defaults, then an
//! optional preset, then explicit overrides, resolved by the caller through
//! [`crate::contract::resolve_capabilities`]) is enforced as a preflight
//! inside [`ModelProvider::stream`], before any HTTP request is built:
//!
//! - `structured_output: JsonObject` — the default and only implemented
//!   mode: the JSON Schema is embedded in the `system` prompt and the reply
//!   text is parsed with the shared `parse_jsonish` helper into a
//!   `StructuredOutput` event.
//! - `structured_output: JsonSchema` — rejected before any HTTP call: the
//!   Messages API has no `json_schema` response format, so the native mode
//!   cannot be honored (configure `json-object` instead).
//! - `structured_output: Unsupported` and `tool_calls: Unsupported` —
//!   requests carrying those features fail with actionable errors before
//!   any HTTP call.
//! - `instructions` is recorded as `SystemInstruction`; the Messages API has
//!   no developer role, so instructions always travel in the `system`
//!   parameter (a configured `developer` style is this documented
//!   translation, never a silent drop).
//! - `output_token_param` is `MaxTokens` (the only parameter the protocol
//!   accepts); `parallel_tool_calls` and `image_input` are recorded
//!   capabilities only, enforced when Relay begins requesting them.

use std::collections::HashMap;
use std::sync::mpsc::{Receiver as StdReceiver, RecvTimeoutError};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::contract::{
    AdapterCapabilities, AdapterKind, AssistantContent, CancelToken, FeatureSupport, FinishReason,
    MAX_CONTINUATION_DATA_BYTES, MAX_CONTINUATION_DISCRIMINATOR_CHARS, MAX_TEXT_BLOCK_BYTES,
    ModelMessage, ModelProvider, ModelRequest, ProviderErrorKind, ProviderEventStream,
    ProviderFailure, ProviderStreamEvent, StructuredOutputMode, TokenUsage, ToolCall, ToolSpec,
};
use crate::errors::to_provider_failure;
use crate::sse::{FetchItem, fetch_sse};

/// Default Anthropic API base (the SDK's default).
const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";

/// Total request timeout, matching the Anthropic SDK's 10-minute default.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(600);

/// Environment variable read when no API key is passed in.
const API_KEY_ENV: &str = "ANTHROPIC_API_KEY";

/// Anthropic requires `anthropic-version` on every request.
const API_VERSION: &str = "2023-06-01";

/// Anthropic requires max_tokens on every request.
const DEFAULT_MAX_TOKENS: u64 = 8192;

/// Continuation discriminator for `thinking` blocks.
const CONTINUATION_THINKING: &str = "thinking";

/// Continuation discriminator for `redacted_thinking` blocks.
const CONTINUATION_REDACTED_THINKING: &str = "redacted-thinking";

/// Backstop on the accumulated `partial_json` of one streamed tool block.
/// Far above any legitimate argument payload Relay's tool schemas accept; a
/// stream above it fails closed instead of being retained unbounded.
const MAX_TOOL_ARGUMENT_BYTES: usize = 512 * 1024;

/// Options for constructing an [`AnthropicProvider`].
#[derive(Debug, Clone, Default)]
pub struct AnthropicProviderOptions {
    /// API key value; explicit values win over the environment. When
    /// omitted, the adapter reads the variable named by
    /// [`Self::api_key_env`] (default `ANTHROPIC_API_KEY`). Never logged or
    /// echoed.
    pub api_key: Option<String>,
    /// Name of the environment variable that carries the API key when
    /// [`Self::api_key`] is omitted (default `ANTHROPIC_API_KEY`). Only the
    /// variable *name* is configured; the value is resolved here, at the
    /// adapter boundary, and never persisted.
    pub api_key_env: Option<String>,
    /// Base URL override (used by the offline conformance tests).
    pub base_url: Option<String>,
    /// Effective capability profile enforced by this connection. `None`
    /// means the `anthropic` adapter defaults; configuration layers resolve
    /// presets and overrides through
    /// [`crate::contract::resolve_capabilities`] before construction.
    pub capabilities: Option<AdapterCapabilities>,
}

// ---------------------------------------------------------------------------
// Wire types (private to this module)
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct WireRequest {
    model: String,
    max_tokens: u64,
    messages: Vec<WireMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<WireTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
}

#[derive(Serialize)]
#[serde(tag = "role")]
enum WireMessage {
    #[serde(rename = "user")]
    User { content: WireUserContent },
    #[serde(rename = "assistant")]
    Assistant { content: Vec<WireContentBlock> },
}

#[derive(Serialize)]
#[serde(untagged)]
enum WireUserContent {
    Text(String),
    Blocks(Vec<WireContentBlock>),
}

#[derive(Serialize, PartialEq, Debug)]
#[serde(tag = "type")]
enum WireContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: String,
        // The protocol treats `is_error` as optional (absent means success),
        // so only error results carry it.
        #[serde(skip_serializing_if = "Option::is_none")]
        is_error: Option<bool>,
    },
    #[serde(rename = "thinking")]
    Thinking { thinking: String, signature: String },
    #[serde(rename = "redacted_thinking")]
    RedactedThinking { data: String },
}

#[derive(Serialize)]
struct WireTool {
    name: String,
    description: String,
    input_schema: crate::contract::JsonSchemaObject,
}

#[derive(Deserialize)]
struct WireUsage {
    #[serde(default)]
    input_tokens: Option<i64>,
    #[serde(default)]
    output_tokens: Option<i64>,
}

#[derive(Deserialize)]
struct WireMessageBody {
    #[serde(default)]
    usage: Option<WireUsage>,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum WireContentBlockStart {
    #[serde(rename = "tool_use")]
    ToolUse {
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        name: Option<String>,
    },
    #[serde(rename = "thinking")]
    Thinking {
        #[serde(default)]
        thinking: String,
    },
    #[serde(rename = "redacted_thinking")]
    RedactedThinking {
        #[serde(default)]
        data: String,
    },
    // Announced `text` blocks (and any future unknown block kinds) carry no
    // start payload; unknown kinds keep the reference adapter's permissive
    // text-compatible handling.
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum WireDelta {
    #[serde(rename = "text_delta")]
    Text { text: String },
    #[serde(rename = "input_json_delta")]
    InputJson {
        #[serde(default)]
        partial_json: String,
    },
    #[serde(rename = "thinking_delta")]
    Thinking {
        #[serde(default)]
        thinking: String,
    },
    #[serde(rename = "signature_delta")]
    Signature {
        #[serde(default)]
        signature: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
struct WireStopDelta {
    #[serde(default)]
    stop_reason: Option<String>,
}

#[derive(Deserialize)]
struct WireErrorBody {
    #[serde(default)]
    message: Option<String>,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum WireStreamEvent {
    #[serde(rename = "message_start")]
    MessageStart { message: WireMessageBody },
    #[serde(rename = "content_block_start")]
    ContentBlockStart {
        index: i64,
        content_block: WireContentBlockStart,
    },
    #[serde(rename = "content_block_delta")]
    ContentBlockDelta { index: i64, delta: WireDelta },
    #[serde(rename = "content_block_stop")]
    ContentBlockStop { index: i64 },
    #[serde(rename = "message_delta")]
    MessageDelta {
        delta: WireStopDelta,
        #[serde(default)]
        usage: Option<WireUsage>,
    },
    #[serde(rename = "message_stop")]
    MessageStop,
    #[serde(rename = "ping")]
    Ping,
    #[serde(rename = "error")]
    Error { error: WireErrorBody },
    #[serde(other)]
    Unknown,
}

// ---------------------------------------------------------------------------
// Adapter
// ---------------------------------------------------------------------------

/// The Anthropic Messages API provider.
///
/// One instance is one configured connection: its endpoint, resolved
/// credential, and effective capability profile are fixed at construction,
/// and every [`ModelProvider::stream`] call is preflight-checked against
/// that profile before any HTTP request is built.
pub struct AnthropicProvider {
    client: reqwest::blocking::Client,
    api_key: String,
    base_url: String,
    capabilities: AdapterCapabilities,
}

impl AnthropicProvider {
    /// Build a provider. Mirrors the SDK's constructor behavior of failing
    /// when no API key is available: an explicit key wins, then the
    /// configured (or default) credential variable, then construction fails
    /// with the redacted missing-variable auth error.
    pub fn new(options: AnthropicProviderOptions) -> Result<Self, ProviderFailure> {
        let env_name = options.api_key_env.as_deref().unwrap_or(API_KEY_ENV);
        let api_key = crate::errors::resolve_api_key_for(
            env_name,
            options.api_key,
            std::env::var(env_name).ok().as_deref(),
        )?;
        let client = reqwest::blocking::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|error| {
                to_provider_failure(crate::errors::ErrorSignal {
                    name: Some("APIConnectionError".into()),
                    message: error.to_string(),
                    ..crate::errors::ErrorSignal::default()
                })
            })?;
        Ok(AnthropicProvider {
            client,
            api_key,
            base_url: options
                .base_url
                .unwrap_or_else(|| DEFAULT_BASE_URL.to_owned()),
            capabilities: options
                .capabilities
                .unwrap_or_else(|| AdapterCapabilities::defaults_for(AdapterKind::Anthropic)),
        })
    }

    /// The effective capability profile this connection enforces.
    pub fn capabilities(&self) -> &AdapterCapabilities {
        &self.capabilities
    }

    /// Preflight a request against the connection's capability profile.
    /// Returns the failure to emit before any HTTP request is built, or
    /// `None` when every requested feature is supported or translated.
    fn preflight_failure(&self, request: &ModelRequest) -> Option<ProviderFailure> {
        if request
            .tools
            .as_ref()
            .is_some_and(|tools| !tools.is_empty())
            && self.capabilities.tool_calls == FeatureSupport::Unsupported
        {
            return Some(unsupported_capability(
                "tool_calls",
                "the request carries tools, but this connection declares tool calls unsupported",
            ));
        }
        if request.structured_output.is_some() {
            match self.capabilities.structured_output {
                StructuredOutputMode::Unsupported => {
                    return Some(unsupported_capability(
                        "structured_output",
                        "the request asks for structured output, but this connection declares \
                         structured output unsupported",
                    ));
                }
                // The Messages API has no json_schema response format; the
                // native mode cannot be honored, so fail closed rather than
                // silently downgrading to the embedded-schema translation.
                StructuredOutputMode::JsonSchema => {
                    return Some(ProviderFailure::new(
                        ProviderErrorKind::UnsupportedCapability,
                        crate::errors::normalized_message(
                            "structured-output mode json-schema is not implemented by the \
                             anthropic adapter; configure structured-output: json-object on this \
                             anthropic connection (the JSON Schema is embedded in the system \
                             prompt)",
                        ),
                    ));
                }
                StructuredOutputMode::JsonObject => {}
            }
        }
        None
    }
}

/// An actionable preflight failure for a Relay feature the connection's
/// capability profile does not allow.
fn unsupported_capability(capability: &str, detail: &str) -> ProviderFailure {
    ProviderFailure::new(
        ProviderErrorKind::UnsupportedCapability,
        format!(
            "unsupported capability on this anthropic connection: {capability} ({detail}); \
             change the connection's capability configuration before retrying"
        ),
    )
}

fn map_stop_reason(reason: &str) -> FinishReason {
    match reason {
        "tool_use" => FinishReason::ToolCalls,
        "max_tokens" => FinishReason::Length,
        // `end_turn`, `stop_sequence`, `pause_turn`, `refusal`, and future
        // reasons all normalize to the neutral Stop.
        _ => FinishReason::Stop,
    }
}

/// The internal encoding of a `thinking` continuation payload: compact JSON
/// of the signature and reasoning text. Opaque to everything outside this
/// module; round-trips through [`encode_thinking_continuation`] and
/// [`decode_thinking_continuation`] byte-stably.
#[derive(Serialize, Deserialize)]
struct ThinkingContinuationPayload {
    signature: String,
    thinking: String,
}

fn encode_thinking_continuation(signature: &str, thinking: &str) -> Result<String, String> {
    serde_json::to_string(&ThinkingContinuationPayload {
        signature: signature.to_owned(),
        thinking: thinking.to_owned(),
    })
    .map_err(|error| format!("thinking continuation payload could not be encoded: {error}"))
}

fn decode_thinking_continuation(data: &str) -> Result<ThinkingContinuationPayload, String> {
    serde_json::from_str(data).map_err(|error| {
        format!("stored thinking continuation payload could not be decoded: {error}")
    })
}

/// Map one transcript continuation block back onto its wire block for replay
/// to the same compatible endpoint. Unknown discriminators, undecodable
/// payloads, and payloads above the continuation bounds fail closed: the
/// endpoint needs the exact block to continue the conversation, so dropping
/// or truncating it would silently corrupt the turn.
fn continuation_to_wire(
    discriminator: &str,
    data: &str,
) -> Result<WireContentBlock, ProviderFailure> {
    if discriminator.chars().count() > MAX_CONTINUATION_DISCRIMINATOR_CHARS {
        return Err(ProviderFailure::new(
            ProviderErrorKind::InvalidResponse,
            crate::errors::normalized_message(&format!(
                "cannot replay a continuation block whose discriminator exceeds the \
                 {MAX_CONTINUATION_DISCRIMINATOR_CHARS}-character bound"
            )),
        ));
    }
    if data.len() > MAX_CONTINUATION_DATA_BYTES {
        return Err(ProviderFailure::new(
            ProviderErrorKind::InvalidResponse,
            crate::errors::normalized_message(&format!(
                "cannot replay a continuation block above the \
                 {MAX_CONTINUATION_DATA_BYTES}-byte bound"
            )),
        ));
    }
    match discriminator {
        CONTINUATION_THINKING => {
            let payload = decode_thinking_continuation(data).map_err(|detail| {
                ProviderFailure::new(
                    ProviderErrorKind::InvalidResponse,
                    crate::errors::normalized_message(&detail),
                )
            })?;
            Ok(WireContentBlock::Thinking {
                thinking: payload.thinking,
                signature: payload.signature,
            })
        }
        CONTINUATION_REDACTED_THINKING => Ok(WireContentBlock::RedactedThinking {
            data: data.to_owned(),
        }),
        other => Err(ProviderFailure::new(
            ProviderErrorKind::InvalidResponse,
            crate::errors::normalized_message(&format!(
                "cannot replay a continuation block with unknown discriminator \"{other}\""
            )),
        )),
    }
}

fn assistant_blocks(
    content: &[AssistantContent],
) -> Result<Vec<WireContentBlock>, ProviderFailure> {
    let mut blocks = Vec::new();
    for block in content {
        match block {
            AssistantContent::Text { text } => {
                if !text.is_empty() {
                    blocks.push(WireContentBlock::Text { text: text.clone() });
                }
            }
            AssistantContent::ToolCall { call } => {
                blocks.push(WireContentBlock::ToolUse {
                    id: call.id.clone(),
                    name: call.name.clone(),
                    input: if call.arguments.is_null() {
                        json!({})
                    } else {
                        call.arguments.clone()
                    },
                });
            }
            // Opaque continuation blocks replay only to the same compatible
            // route; an un-replayable one fails the request below rather
            // than being silently dropped.
            AssistantContent::Continuation {
                discriminator,
                data,
            } => blocks.push(continuation_to_wire(discriminator, data)?),
        }
    }
    Ok(blocks)
}

fn to_wire_messages(messages: &[ModelMessage]) -> Result<Vec<WireMessage>, ProviderFailure> {
    let mut mapped: Vec<WireMessage> = Vec::new();
    for message in messages {
        match message {
            ModelMessage::User { content } => {
                mapped.push(WireMessage::User {
                    content: WireUserContent::Text(
                        content.iter().map(|block| block.text()).collect(),
                    ),
                });
            }
            ModelMessage::Assistant { content } => {
                mapped.push(WireMessage::Assistant {
                    content: assistant_blocks(content)?,
                });
            }
            ModelMessage::ToolResult {
                tool_call_id,
                content,
                is_error,
            } => {
                let block = WireContentBlock::ToolResult {
                    tool_use_id: tool_call_id.clone(),
                    content: content.clone(),
                    is_error: is_error.then_some(true),
                };
                // Anthropic requires alternating roles: merge consecutive tool
                // results into one user message with multiple tool_result
                // blocks. A plain-text user message never merges.
                let merge_into = match mapped.last_mut() {
                    Some(WireMessage::User {
                        content: WireUserContent::Blocks(blocks),
                    }) => Some(blocks),
                    _ => None,
                };
                match merge_into {
                    Some(blocks) => blocks.push(block),
                    None => mapped.push(WireMessage::User {
                        content: WireUserContent::Blocks(vec![block]),
                    }),
                }
            }
        }
    }
    Ok(mapped)
}

fn to_wire_tools(tools: &[ToolSpec]) -> Vec<WireTool> {
    tools
        .iter()
        .map(|tool| WireTool {
            name: tool.name.clone(),
            description: tool.description.clone(),
            input_schema: tool.parameters.clone(),
        })
        .collect()
}

/// A bounded, redacted invalid-response terminal event.
fn invalid_response(message: String) -> ProviderStreamEvent {
    ProviderStreamEvent::Error {
        error: ProviderFailure::new(
            ProviderErrorKind::InvalidResponse,
            crate::errors::normalized_message(&message),
        ),
    }
}

/// One tracked content block in the incoming stream.
#[derive(Clone)]
enum BlockState {
    /// A text block (or an announced kind the adapter does not know, which
    /// keeps the reference adapter's text-compatible tolerance). `bytes`
    /// accumulates the block's size for the fail-closed bound.
    Text {
        bytes: usize,
    },
    Tool {
        id: String,
        name: String,
        json: String,
    },
    Thinking {
        signature: String,
        data: String,
    },
    RedactedThinking {
        data: String,
    },
}

struct AnthropicStream {
    receiver: StdReceiver<FetchItem>,
    signal: Option<CancelToken>,
    /// All visible text across blocks, kept for structured-output parsing.
    text: String,
    input_tokens: i64,
    output_tokens: i64,
    stop_reason: Option<FinishReason>,
    /// Currently open blocks by announced index; a block leaves the map when
    /// its `content_block_stop` arrives, so deltas for a closed index fail.
    blocks: HashMap<i64, BlockState>,
    structured_output: Option<crate::contract::StructuredOutputRequest>,
    pending: std::collections::VecDeque<ProviderStreamEvent>,
    finished: bool,
}

impl AnthropicStream {
    /// End the stream with a typed invalid-response failure.
    fn fail(&mut self, message: String) -> Vec<ProviderStreamEvent> {
        self.finished = true;
        vec![invalid_response(message)]
    }

    fn absorb_event(&mut self, event: WireStreamEvent) -> Vec<ProviderStreamEvent> {
        match event {
            WireStreamEvent::MessageStart { message } => {
                self.input_tokens = message
                    .usage
                    .and_then(|usage| usage.input_tokens)
                    .unwrap_or(0);
                vec![ProviderStreamEvent::Usage {
                    usage: TokenUsage {
                        input_tokens: self.input_tokens,
                        output_tokens: self.output_tokens,
                    },
                }]
            }
            WireStreamEvent::ContentBlockStart {
                index,
                content_block,
            } => self.absorb_block_start(index, content_block),
            WireStreamEvent::ContentBlockDelta { index, delta } => self.absorb_delta(index, delta),
            WireStreamEvent::ContentBlockStop { index } => self.absorb_block_stop(index),
            WireStreamEvent::MessageDelta { delta, usage } => {
                self.output_tokens = usage
                    .and_then(|usage| usage.output_tokens)
                    .unwrap_or(self.output_tokens);
                if self.stop_reason.is_none()
                    && let Some(reason) = delta.stop_reason.as_deref()
                {
                    self.stop_reason = Some(map_stop_reason(reason));
                }
                Vec::new()
            }
            WireStreamEvent::MessageStop => self.absorb_message_stop(),
            WireStreamEvent::Ping | WireStreamEvent::Unknown => Vec::new(),
            WireStreamEvent::Error { error } => {
                // The SDK throws an APIError for mid-stream error events, which
                // aborts the stream: the normalizer classifies it (no status
                // attached) as unknown, and no further events are emitted.
                self.finished = true;
                let message = error.message.unwrap_or_else(|| "stream error".to_owned());
                vec![ProviderStreamEvent::Error {
                    error: to_provider_failure(crate::errors::ErrorSignal::from_message(message)),
                }]
            }
        }
    }

    fn absorb_block_start(
        &mut self,
        index: i64,
        content_block: WireContentBlockStart,
    ) -> Vec<ProviderStreamEvent> {
        if index < 0 {
            return self.fail(format!(
                "content_block_start announced a negative block index ({index})"
            ));
        }
        if self.blocks.contains_key(&index) {
            return self.fail(format!(
                "content_block_start announced block index {index}, which is already open"
            ));
        }
        let state = match content_block {
            WireContentBlockStart::ToolUse { id, name } => {
                let id = id.filter(|value| !value.is_empty());
                let name = name.filter(|value| !value.is_empty());
                let missing_field = if id.is_none() { "id" } else { "name" };
                let (Some(id), Some(name)) = (id, name) else {
                    return self.fail(format!(
                        "tool_use content block at index {index} is missing its {missing_field}"
                    ));
                };
                BlockState::Tool {
                    id,
                    name,
                    json: String::new(),
                }
            }
            // The protocol sends the reasoning text only through deltas; the
            // start payload's `thinking` field is normally empty.
            WireContentBlockStart::Thinking { thinking } => BlockState::Thinking {
                signature: String::new(),
                data: thinking,
            },
            // A redacted block arrives complete at start; it streams no
            // deltas.
            WireContentBlockStart::RedactedThinking { data } => {
                BlockState::RedactedThinking { data }
            }
            WireContentBlockStart::Other => BlockState::Text { bytes: 0 },
        };
        self.blocks.insert(index, state);
        Vec::new()
    }

    fn absorb_delta(&mut self, index: i64, delta: WireDelta) -> Vec<ProviderStreamEvent> {
        let Some(state) = self.blocks.get_mut(&index) else {
            return self.fail(format!(
                "content_block_delta arrived for block index {index}, which is not open"
            ));
        };
        // Compute the outcome while only the block state is borrowed; the
        // failure path needs `&mut self` afterwards.
        let outcome = match (delta, state) {
            (WireDelta::Text { text }, BlockState::Text { bytes }) => {
                if *bytes + text.len() > MAX_TEXT_BLOCK_BYTES {
                    Err(format!(
                        "text content block at index {index} exceeded the \
                         {MAX_TEXT_BLOCK_BYTES}-byte bound"
                    ))
                } else {
                    *bytes += text.len();
                    self.text.push_str(&text);
                    Ok(vec![ProviderStreamEvent::TextDelta { text }])
                }
            }
            (WireDelta::Text { .. }, _) => Err(format!(
                "text_delta arrived for block index {index}, which is not a text block"
            )),
            (WireDelta::InputJson { partial_json }, BlockState::Tool { json, .. }) => {
                if json.len() + partial_json.len() > MAX_TOOL_ARGUMENT_BYTES {
                    Err(format!(
                        "tool_use input at block index {index} exceeded the \
                         {MAX_TOOL_ARGUMENT_BYTES}-byte bound"
                    ))
                } else {
                    json.push_str(&partial_json);
                    Ok(Vec::new())
                }
            }
            (WireDelta::InputJson { .. }, _) => Err(format!(
                "input_json_delta arrived for block index {index}, which is not a tool_use block"
            )),
            (WireDelta::Thinking { thinking }, BlockState::Thinking { data, .. }) => {
                // Mid-stream guard on the raw payload parts; the exact bound
                // is re-checked on the encoded payload at block stop.
                if data.len() + thinking.len() > MAX_CONTINUATION_DATA_BYTES {
                    Err(format!(
                        "thinking content block at index {index} exceeded the \
                         {MAX_CONTINUATION_DATA_BYTES}-byte continuation bound"
                    ))
                } else {
                    data.push_str(&thinking);
                    Ok(Vec::new())
                }
            }
            (WireDelta::Thinking { .. }, _) => Err(format!(
                "thinking_delta arrived for block index {index}, which is not a thinking block"
            )),
            (
                WireDelta::Signature { signature },
                BlockState::Thinking {
                    signature: accumulated,
                    ..
                },
            ) => {
                if accumulated.len() + signature.len() > MAX_CONTINUATION_DATA_BYTES {
                    Err(format!(
                        "thinking content block at index {index} exceeded the \
                         {MAX_CONTINUATION_DATA_BYTES}-byte continuation bound"
                    ))
                } else {
                    accumulated.push_str(&signature);
                    Ok(Vec::new())
                }
            }
            (WireDelta::Signature { .. }, _) => Err(format!(
                "signature_delta arrived for block index {index}, which is not a thinking block"
            )),
            // Unknown delta kinds stay tolerated, like unknown event types.
            (WireDelta::Other, _) => Ok(Vec::new()),
        };
        match outcome {
            Ok(events) => events,
            Err(message) => self.fail(message),
        }
    }

    fn absorb_block_stop(&mut self, index: i64) -> Vec<ProviderStreamEvent> {
        let Some(state) = self.blocks.remove(&index) else {
            return self.fail(format!(
                "content_block_stop arrived for block index {index}, which is not open"
            ));
        };
        match state {
            BlockState::Text { .. } => Vec::new(),
            BlockState::Tool { id, name, json } => {
                // An empty argument buffer is a call with no arguments; any
                // non-empty buffer must parse, or the turn fails closed
                // instead of handing the executor a raw string.
                let arguments = if json.is_empty() {
                    json!({})
                } else {
                    match serde_json::from_str(&json) {
                        Ok(value) => value,
                        Err(error) => {
                            return self.fail(format!(
                                "tool_use block \"{name}\" (call {id}) accumulated input that \
                                 is not valid JSON: {error}"
                            ));
                        }
                    }
                };
                vec![ProviderStreamEvent::ToolCall {
                    call: ToolCall {
                        id,
                        name,
                        arguments,
                    },
                }]
            }
            BlockState::Thinking { signature, data } => {
                let encoded = match encode_thinking_continuation(&signature, &data) {
                    Ok(encoded) => encoded,
                    Err(detail) => return self.fail(detail),
                };
                if encoded.len() > MAX_CONTINUATION_DATA_BYTES {
                    return self.fail(format!(
                        "thinking content block at index {index} produced a continuation \
                         payload above the {MAX_CONTINUATION_DATA_BYTES}-byte bound"
                    ));
                }
                vec![ProviderStreamEvent::Continuation {
                    discriminator: CONTINUATION_THINKING.to_owned(),
                    data: encoded,
                }]
            }
            BlockState::RedactedThinking { data } => {
                if data.len() > MAX_CONTINUATION_DATA_BYTES {
                    return self.fail(format!(
                        "redacted_thinking content block at index {index} exceeded the \
                         {MAX_CONTINUATION_DATA_BYTES}-byte continuation bound"
                    ));
                }
                vec![ProviderStreamEvent::Continuation {
                    discriminator: CONTINUATION_REDACTED_THINKING.to_owned(),
                    data,
                }]
            }
        }
    }

    /// Terminal events on `message_stop`: structured output (when requested),
    /// then completion with the authoritative usage pair.
    fn absorb_message_stop(&mut self) -> Vec<ProviderStreamEvent> {
        if !self.blocks.is_empty() {
            return self
                .fail("message_stop arrived while content blocks are still open".to_owned());
        }
        self.finished = true;
        let mut events = Vec::new();
        if let Some(requested) = self.structured_output.clone() {
            match crate::errors::parse_jsonish(&self.text) {
                Some(value) => {
                    events.push(ProviderStreamEvent::StructuredOutput {
                        name: requested.name,
                        value,
                    });
                }
                None => {
                    let excerpt: String = self.text.chars().take(200).collect();
                    events.push(invalid_response(format!(
                        "Anthropic structured output was not valid JSON for \"{}\": {excerpt}",
                        requested.name
                    )));
                    return events;
                }
            }
        }
        events.push(ProviderStreamEvent::Completed {
            finish_reason: self.stop_reason.unwrap_or(FinishReason::Stop),
            usage: TokenUsage {
                input_tokens: self.input_tokens,
                output_tokens: self.output_tokens,
            },
        });
        events
    }
}

impl Iterator for AnthropicStream {
    type Item = ProviderStreamEvent;

    fn next(&mut self) -> Option<ProviderStreamEvent> {
        if let Some(event) = self.pending.pop_front() {
            return Some(event);
        }
        if self.finished {
            return None;
        }
        loop {
            match self.receiver.recv_timeout(Duration::from_millis(50)) {
                Ok(FetchItem::Message(message)) => {
                    match serde_json::from_str::<WireStreamEvent>(&message.data) {
                        Ok(event) => {
                            let events = self.absorb_event(event);
                            if events.is_empty() {
                                continue;
                            }
                            self.pending.extend(events);
                            return self.pending.pop_front();
                        }
                        Err(error) => {
                            self.finished = true;
                            return Some(ProviderStreamEvent::Error {
                                error: ProviderFailure::new(
                                    ProviderErrorKind::InvalidResponse,
                                    crate::errors::normalized_message(&format!(
                                        "malformed stream event: {error}"
                                    )),
                                ),
                            });
                        }
                    }
                }
                Ok(FetchItem::Eof) | Err(RecvTimeoutError::Disconnected) => {
                    // EOF without message_stop is an incomplete response:
                    // fail closed instead of synthesizing completion.
                    self.finished = true;
                    return Some(invalid_response(
                        "stream ended without a message_stop event".to_owned(),
                    ));
                }
                Ok(FetchItem::Failed(failure)) => {
                    self.finished = true;
                    if self
                        .signal
                        .as_ref()
                        .is_some_and(|signal| signal.is_cancelled())
                        && failure.kind != ProviderErrorKind::Canceled
                    {
                        return Some(ProviderStreamEvent::Error {
                            error: ProviderFailure::new(
                                ProviderErrorKind::Canceled,
                                "request aborted before completion",
                            ),
                        });
                    }
                    return Some(ProviderStreamEvent::Error { error: failure });
                }
                Err(RecvTimeoutError::Timeout) => {
                    if self
                        .signal
                        .as_ref()
                        .is_some_and(|signal| signal.is_cancelled())
                    {
                        self.finished = true;
                        return Some(ProviderStreamEvent::Error {
                            error: ProviderFailure::new(
                                ProviderErrorKind::Canceled,
                                "request aborted before completion",
                            ),
                        });
                    }
                    continue;
                }
            }
        }
    }
}

impl ModelProvider for AnthropicProvider {
    fn name(&self) -> AdapterKind {
        AdapterKind::Anthropic
    }

    fn models(&self) -> Vec<String> {
        vec![
            "claude-opus-5-5".into(),
            "claude-sonnet-5-5".into(),
            "claude-haiku-4-5".into(),
        ]
    }

    fn stream(&self, request: &ModelRequest) -> ProviderEventStream<'_> {
        // Mirror the SDK: an already-aborted signal fails the call before any
        // network activity.
        if request
            .signal
            .as_ref()
            .is_some_and(CancelToken::is_cancelled)
        {
            let failure =
                ProviderFailure::new(ProviderErrorKind::Canceled, "This operation was aborted");
            return Box::new(std::iter::once(ProviderStreamEvent::Error {
                error: failure,
            }));
        }

        // Capability preflight: a Relay feature this connection cannot carry
        // or translate fails here, before any HTTP request is built.
        if let Some(failure) = self.preflight_failure(request) {
            return Box::new(std::iter::once(ProviderStreamEvent::Error {
                error: failure,
            }));
        }

        let mut system = request.system.clone().unwrap_or_default();
        if let Some(structured) = &request.structured_output {
            // Anthropic has no JSON-schema response mode; instruct the model.
            let schema_json = serde_json::to_string(&structured.schema).unwrap_or_default();
            let instruction =
                format!("Respond with ONLY a JSON object matching this schema: {schema_json}");
            system = if system.is_empty() {
                instruction
            } else {
                format!("{system}\n\n{instruction}")
            };
        }

        // An un-replayable continuation block in the transcript fails here,
        // before any HTTP request: the endpoint needs the exact block.
        let messages = match to_wire_messages(&request.messages) {
            Ok(messages) => messages,
            Err(failure) => {
                return Box::new(std::iter::once(ProviderStreamEvent::Error {
                    error: failure,
                }));
            }
        };

        let tools = request.tools.as_deref().map(to_wire_tools);
        let wire = WireRequest {
            model: request.model.clone(),
            max_tokens: request.max_output_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
            messages,
            system: if system.is_empty() {
                None
            } else {
                Some(system)
            },
            tools: tools.filter(|tools| !tools.is_empty()),
            temperature: request.temperature,
        };

        let body = match serde_json::to_value(&wire) {
            Ok(body) => body,
            Err(error) => {
                let failure = to_provider_failure(crate::errors::ErrorSignal {
                    message: format!("could not encode request: {error}"),
                    ..crate::errors::ErrorSignal::default()
                });
                return Box::new(std::iter::once(ProviderStreamEvent::Error {
                    error: failure,
                }));
            }
        };

        let url = format!("{}/v1/messages", self.base_url.trim_end_matches('/'));
        let request_builder = self
            .client
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", API_VERSION)
            .json(&body);
        let receiver = fetch_sse(request_builder);

        Box::new(AnthropicStream {
            receiver,
            signal: request.signal.clone(),
            text: String::new(),
            input_tokens: 0,
            output_tokens: 0,
            stop_reason: None,
            blocks: HashMap::new(),
            structured_output: request.structured_output.clone(),
            pending: std::collections::VecDeque::new(),
            finished: false,
        })
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn stop_reasons_map_to_the_neutral_finish_reasons() {
        assert_eq!(map_stop_reason("tool_use"), FinishReason::ToolCalls);
        assert_eq!(map_stop_reason("max_tokens"), FinishReason::Length);
        assert_eq!(map_stop_reason("end_turn"), FinishReason::Stop);
        assert_eq!(map_stop_reason("stop_sequence"), FinishReason::Stop);
        assert_eq!(map_stop_reason("pause_turn"), FinishReason::Stop);
        assert_eq!(map_stop_reason("refusal"), FinishReason::Stop);
    }

    #[test]
    fn thinking_continuation_payloads_round_trip_byte_stably() {
        let encoded = encode_thinking_continuation("sig-123", "pondering deeply").expect("encodes");
        assert_eq!(
            encoded,
            "{\"signature\":\"sig-123\",\"thinking\":\"pondering deeply\"}"
        );
        let decoded = decode_thinking_continuation(&encoded).expect("decodes");
        assert_eq!(decoded.signature, "sig-123");
        assert_eq!(decoded.thinking, "pondering deeply");
        // The decoded payload maps back onto the wire block exactly.
        assert_eq!(
            continuation_to_wire(CONTINUATION_THINKING, &encoded).expect("replayable"),
            WireContentBlock::Thinking {
                thinking: "pondering deeply".into(),
                signature: "sig-123".into(),
            }
        );
    }

    #[test]
    fn redacted_continuations_replay_their_bytes_verbatim() {
        let block = continuation_to_wire(CONTINUATION_REDACTED_THINKING, "opaque-blob")
            .expect("replayable");
        assert_eq!(
            block,
            WireContentBlock::RedactedThinking {
                data: "opaque-blob".into()
            }
        );
    }

    #[test]
    fn unknown_or_malformed_continuations_fail_closed_without_echoing_the_payload() {
        let failure =
            continuation_to_wire("vendor-x", "secret-opaque-payload").expect_err("unknown");
        assert_eq!(failure.kind, ProviderErrorKind::InvalidResponse);
        assert!(failure.message.contains("vendor-x"), "{}", failure.message);
        assert!(
            !failure.message.contains("secret-opaque-payload"),
            "{}",
            failure.message
        );

        let failure =
            continuation_to_wire(CONTINUATION_THINKING, "not-json{secret").expect_err("malformed");
        assert_eq!(failure.kind, ProviderErrorKind::InvalidResponse);
        assert!(!failure.message.contains("secret"), "{}", failure.message);

        let oversized = continuation_to_wire(
            CONTINUATION_REDACTED_THINKING,
            &"x".repeat(MAX_CONTINUATION_DATA_BYTES + 1),
        )
        .expect_err("oversized");
        assert_eq!(oversized.kind, ProviderErrorKind::InvalidResponse);
        assert!(oversized.message.contains("bound"), "{}", oversized.message);

        let long_discriminator =
            continuation_to_wire(&"d".repeat(MAX_CONTINUATION_DISCRIMINATOR_CHARS + 1), "x")
                .expect_err("long discriminator");
        assert_eq!(long_discriminator.kind, ProviderErrorKind::InvalidResponse);
    }
}
