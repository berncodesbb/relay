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

use crate::contract::TokenUsage;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::contract::{
    AdapterCapabilities, AdapterKind, AssistantContent, CancelToken, FeatureSupport, FinishReason,
    ModelMessage, ModelProvider, ModelRequest, ProviderErrorKind, ProviderEventStream,
    ProviderFailure, ProviderStreamEvent, StructuredOutputMode, ToolCall, ToolSpec,
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

#[derive(Serialize)]
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
    },
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
    ToolUse { id: String, name: String },
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
        let api_key = crate::openai::resolve_api_key_for(
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
/// capability profile does not allow, in the same shape the OpenAI-compatible
/// adapter emits.
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
        _ => FinishReason::Stop,
    }
}

fn assistant_blocks(content: &[AssistantContent]) -> Vec<WireContentBlock> {
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
        }
    }
    blocks
}

fn to_wire_messages(messages: &[ModelMessage]) -> Vec<WireMessage> {
    let mut mapped: Vec<WireMessage> = Vec::new();
    for message in messages {
        match message {
            ModelMessage::User { content } => {
                mapped.push(WireMessage::User {
                    content: WireUserContent::Text(content.clone()),
                });
            }
            ModelMessage::Assistant { content } => {
                mapped.push(WireMessage::Assistant {
                    content: assistant_blocks(content),
                });
            }
            ModelMessage::ToolResult {
                tool_call_id,
                content,
            } => {
                let block = WireContentBlock::ToolResult {
                    tool_use_id: tool_call_id.clone(),
                    content: content.clone(),
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
    mapped
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

/// One tracked content block in the incoming stream.
#[derive(Clone)]
enum BlockState {
    Text,
    Tool {
        id: String,
        name: String,
        json: String,
    },
}

struct AnthropicStream {
    receiver: StdReceiver<FetchItem>,
    signal: Option<CancelToken>,
    text: String,
    input_tokens: i64,
    output_tokens: i64,
    stop_reason: Option<FinishReason>,
    blocks: HashMap<i64, BlockState>,
    /// Tool blocks in stream order; emitted when each block closes.
    closed_tool_blocks: Vec<(String, String, String)>,
    structured_output: Option<crate::contract::StructuredOutputRequest>,
    pending: std::collections::VecDeque<ProviderStreamEvent>,
    finished: bool,
}

impl AnthropicStream {
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
            } => {
                let state = match content_block {
                    WireContentBlockStart::ToolUse { id, name } => BlockState::Tool {
                        id,
                        name,
                        json: String::new(),
                    },
                    WireContentBlockStart::Other => BlockState::Text,
                };
                self.blocks.insert(index, state);
                Vec::new()
            }
            WireStreamEvent::ContentBlockDelta { index, delta } => match delta {
                WireDelta::Text { text } => {
                    self.text.push_str(&text);
                    vec![ProviderStreamEvent::TextDelta { text }]
                }
                WireDelta::InputJson { partial_json } => {
                    if let Some(BlockState::Tool { json, .. }) = self.blocks.get_mut(&index) {
                        json.push_str(&partial_json);
                    }
                    Vec::new()
                }
                WireDelta::Other => Vec::new(),
            },
            WireStreamEvent::ContentBlockStop { index } => {
                if let Some(BlockState::Tool { id, name, json }) = self.blocks.remove(&index) {
                    self.closed_tool_blocks.push((id, name, json));
                }
                Vec::new()
            }
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
            WireStreamEvent::MessageStop | WireStreamEvent::Ping | WireStreamEvent::Unknown => {
                Vec::new()
            }
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

    /// Terminal events: closed tool blocks, structured output, completion.
    fn closing_events(&mut self) -> Vec<ProviderStreamEvent> {
        let mut events = Vec::new();
        for (id, name, json_text) in self.closed_tool_blocks.clone() {
            let input = if json_text.is_empty() {
                json!({})
            } else {
                serde_json::from_str(&json_text).unwrap_or(Value::String(json_text.clone()))
            };
            events.push(ProviderStreamEvent::ToolCall {
                call: ToolCall {
                    id,
                    name,
                    arguments: input,
                },
            });
        }

        if let Some(requested) = self.structured_output.clone() {
            match crate::openai::parse_jsonish(&self.text) {
                Some(value) => {
                    events.push(ProviderStreamEvent::StructuredOutput {
                        name: requested.name,
                        value,
                    });
                }
                None => {
                    let excerpt: String = self.text.chars().take(200).collect();
                    events.push(ProviderStreamEvent::Error {
                        error: ProviderFailure::new(
                            ProviderErrorKind::InvalidResponse,
                            format!(
                                "Anthropic structured output was not valid JSON for \"{}\": {excerpt}",
                                requested.name
                            ),
                        ),
                    });
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
                    let closing = self.closing_events();
                    self.pending.extend(closing);
                    self.finished = true;
                    return self.pending.pop_front();
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

        let tools = request.tools.as_deref().map(to_wire_tools);
        let wire = WireRequest {
            model: request.model.clone(),
            max_tokens: request.max_output_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
            messages: to_wire_messages(&request.messages),
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
            closed_tool_blocks: Vec::new(),
            structured_output: request.structured_output.clone(),
            pending: std::collections::VecDeque::new(),
            finished: false,
        })
    }
}
