//! OpenAI-compatible chat-completions adapter.
//!
//! Ported from `src/providers/openai.ts` (retired TypeScript tree, removed
//! from the repository 2026-10-14). Translates Relay-owned requests
//! into OpenAI Chat Completions wire calls and normalizes streamed chunks back
//! into Relay-owned provider events. No wire type appears in this module's
//! public API; errors pass through the shared normalizer so headers and
//! bodies never leak. SDK-level retries are disabled so Relay's own retry
//! policy owns backoff.
//!
//! This adapter speaks the *protocol*, not a vendor: any OpenAI-compatible
//! `/chat/completions` endpoint (OpenAI itself, GLM, other gateways, local
//! servers) is a connection built on it, distinguished by its configured
//! base URL, credential variable name, and effective capability profile.
//!
//! # Capability enforcement
//!
//! The connection's [`AdapterCapabilities`] (resolved by the caller from
//! adapter defaults, an optional preset, then explicit overrides — see
//! [`crate::contract::resolve_capabilities`]) is enforced as a preflight
//! inside [`ModelProvider::stream`], before any HTTP request is built:
//!
//! - `instructions: SystemInstruction` — the documented safe fallback that
//!   carries the Relay system prompt as a `system`-role message instead of
//!   `developer` (used by the GLM preset).
//! - `structured_output: JsonSchema` — the native
//!   `response_format: json_schema` request (default).
//! - `structured_output: JsonObject` — `response_format: {"type":
//!   "json_object"}` plus the JSON Schema embedded in a bounded instruction
//!   block so the model knows the required shape; the reply text is parsed
//!   with the shared `parse_jsonish` helper into a `StructuredOutput` event.
//! - `structured_output: Unsupported` — a request carrying structured output
//!   fails with an actionable error before any HTTP call.
//! - `tool_calls: Unsupported` — a request carrying tools fails the same way.
//! - `streamed_usage: Unsupported` — `stream_options` is omitted entirely
//!   and streams that never report usage complete with zero usage.
//! - `output_token_param` — `MaxTokens` always sends `max_tokens`,
//!   `MaxCompletionTokens` always sends `max_completion_tokens`, and
//!   `ModelDefault` keeps the per-model heuristic (`gpt-5*`/`o<digit>*`
//!   models use `max_completion_tokens`).
//!
//! `parallel_tool_calls` and `image_input` are recorded capabilities only:
//! Relay does not request either yet, so nothing is enforced, and the profile
//! exists so the gap fails early when Relay starts requesting them.
//! `streamed_tool_arguments` is likewise recorded only — the accumulator
//! already tolerates arguments arriving whole or split across any number of
//! deltas.

use std::collections::HashMap;
use std::sync::mpsc::{Receiver as StdReceiver, RecvTimeoutError};
use std::time::Duration;

use crate::contract::TokenUsage;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::contract::{
    AdapterCapabilities, AdapterKind, CancelToken, FeatureSupport, FinishReason, InstructionStyle,
    ModelMessage, ModelProvider, ModelRequest, OutputTokenParam, ProviderErrorKind,
    ProviderEventStream, ProviderFailure, ProviderStreamEvent, StructuredOutputMode,
    StructuredOutputRequest, ToolCall, ToolSpec,
};
use crate::errors::to_provider_failure;
use crate::sse::{FetchItem, fetch_sse};

/// Default OpenAI API base (the SDK's default).
const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

/// Total request timeout, matching the OpenAI SDK's 10-minute default.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(600);

/// Environment variable read when no API key is passed in and no variable
/// name is configured.
const API_KEY_ENV: &str = "OPENAI_API_KEY";

/// Upper bound on a JSON Schema embedded into instruction text for the
/// `json_object` structured-output translation, so a pathological schema
/// cannot silently balloon the prompt.
const MAX_EMBEDDED_SCHEMA_CHARS: usize = 16 * 1024;

/// Options for constructing an [`OpenAIProvider`].
///
/// Each configured provider connection builds its own provider from its own
/// options, so any number of `openai-chat` connections (OpenAI, GLM, local
/// gateways) can coexist with distinct endpoints, credential variables, and
/// capability profiles.
#[derive(Debug, Clone, Default)]
pub struct OpenAIProviderOptions {
    /// API key value; explicit values win over the environment. When
    /// omitted, the adapter reads the variable named by
    /// [`Self::api_key_env`] (default `OPENAI_API_KEY`). Never logged or
    /// echoed.
    pub api_key: Option<String>,
    /// Name of the environment variable that carries the API key when
    /// [`Self::api_key`] is omitted (default `OPENAI_API_KEY`). Only the
    /// variable *name* is configured; the value is resolved here, at the
    /// adapter boundary, and never persisted.
    pub api_key_env: Option<String>,
    /// Base URL override (used by the offline conformance tests). The
    /// adapter appends `/chat/completions` after trimming trailing slashes,
    /// so bases with and without a trailing slash (and with a path prefix
    /// such as `/v4`) join the same way.
    pub base_url: Option<String>,
    /// Effective capability profile enforced by this connection. `None`
    /// means the `openai-chat` adapter defaults; configuration layers
    /// resolve presets and overrides through
    /// [`crate::contract::resolve_capabilities`] before construction.
    pub capabilities: Option<AdapterCapabilities>,
}

// ---------------------------------------------------------------------------
// Wire types (private to this module)
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct WireRequest {
    model: String,
    messages: Vec<WireMessage>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<WireStreamOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<WireTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_completion_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<WireResponseFormat>,
}

#[derive(Serialize)]
struct WireStreamOptions {
    include_usage: bool,
}

#[derive(Serialize)]
#[serde(tag = "role")]
enum WireMessage {
    #[serde(rename = "developer")]
    Developer { content: String },
    #[serde(rename = "system")]
    System { content: String },
    #[serde(rename = "user")]
    User { content: String },
    #[serde(rename = "assistant")]
    Assistant {
        // Serialized as null when the turn carried no text, like the SDK.
        content: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        tool_calls: Option<Vec<WireToolCall>>,
    },
    #[serde(rename = "tool")]
    Tool {
        tool_call_id: String,
        content: String,
    },
}

#[derive(Serialize)]
struct WireToolCall {
    id: String,
    #[serde(rename = "type")]
    kind: &'static str,
    function: WireFunction,
}

#[derive(Serialize)]
struct WireFunction {
    name: String,
    arguments: String,
}

#[derive(Serialize)]
struct WireTool {
    #[serde(rename = "type")]
    kind: &'static str,
    function: WireToolDefinition,
}

#[derive(Serialize)]
struct WireToolDefinition {
    name: String,
    description: String,
    parameters: crate::contract::JsonSchemaObject,
}

#[derive(Serialize)]
struct WireResponseFormat {
    #[serde(rename = "type")]
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    json_schema: Option<WireJsonSchema>,
}

#[derive(Serialize)]
struct WireJsonSchema {
    name: String,
    schema: crate::contract::JsonSchemaObject,
    strict: bool,
}

#[derive(Deserialize)]
struct WireChunk {
    #[serde(default)]
    choices: Vec<WireChoice>,
    #[serde(default)]
    usage: Option<WireUsage>,
}

#[derive(Deserialize)]
struct WireChoice {
    #[serde(default)]
    delta: Option<WireDelta>,
    finish_reason: Option<String>,
}

#[derive(Deserialize, Default)]
struct WireDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<WireToolFragment>,
}

#[derive(Deserialize)]
struct WireToolFragment {
    index: i64,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<WireFunctionFragment>,
}

#[derive(Deserialize, Default)]
struct WireFunctionFragment {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

#[derive(Deserialize)]
struct WireUsage {
    prompt_tokens: i64,
    completion_tokens: i64,
}

// ---------------------------------------------------------------------------
// Adapter
// ---------------------------------------------------------------------------

/// The OpenAI-compatible chat-completions provider.
///
/// One instance is one configured connection: its endpoint, resolved
/// credential, and effective capability profile are fixed at construction,
/// and every [`ModelProvider::stream`] call is preflight-checked against
/// that profile before any HTTP request is built.
pub struct OpenAIProvider {
    client: reqwest::blocking::Client,
    api_key: String,
    base_url: String,
    capabilities: AdapterCapabilities,
}

impl OpenAIProvider {
    /// Build a provider. Mirrors the SDK's constructor behavior of failing
    /// when no API key is available: an explicit key wins, then the
    /// configured (or default) credential variable, then construction fails
    /// with the redacted missing-variable auth error.
    pub fn new(options: OpenAIProviderOptions) -> Result<Self, ProviderFailure> {
        let env_name = options.api_key_env.as_deref().unwrap_or(API_KEY_ENV);
        let api_key = resolve_api_key_for(
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
        Ok(OpenAIProvider {
            client,
            api_key,
            base_url: options
                .base_url
                .unwrap_or_else(|| DEFAULT_BASE_URL.to_owned()),
            capabilities: options
                .capabilities
                .unwrap_or_else(|| AdapterCapabilities::defaults_for(AdapterKind::OpenaiChat)),
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
        if request.structured_output.is_some()
            && self.capabilities.structured_output == StructuredOutputMode::Unsupported
        {
            return Some(unsupported_capability(
                "structured_output",
                "the request asks for structured output, but this connection declares \
                 structured output unsupported",
            ));
        }
        None
    }
}

/// An actionable preflight failure for a Relay feature the connection's
/// capability profile does not allow. Emitted before any HTTP call so a
/// misconfigured connection fails fast and loudly instead of halfway through
/// an attempt.
fn unsupported_capability(capability: &str, detail: &str) -> ProviderFailure {
    ProviderFailure::new(
        ProviderErrorKind::UnsupportedCapability,
        format!(
            "unsupported capability on this openai-chat connection: {capability} ({detail}); \
             change the connection's capability configuration before retrying"
        ),
    )
}

/// Key resolution parameterized by environment variable name, shared with the
/// Anthropic adapter: an explicit value wins, then the named variable's
/// value, mirroring the SDK's constructor failure when neither is available.
pub(crate) fn resolve_api_key_for(
    env_name: &str,
    explicit: Option<String>,
    from_env: Option<&str>,
) -> Result<String, ProviderFailure> {
    if let Some(explicit) = explicit.filter(|key| !key.is_empty()) {
        return Ok(explicit);
    }
    if let Some(from_env) = from_env.filter(|key| !key.is_empty()) {
        return Ok(from_env.to_owned());
    }
    Err(ProviderFailure::new(
        ProviderErrorKind::Auth,
        format!("The {env_name} environment variable is missing"),
    ))
}

/// Models that only accept `max_completion_tokens` (newer reasoning family):
/// the TypeScript pattern `/^(gpt-5|o\d)/`.
fn uses_completion_tokens(model: &str) -> bool {
    model.starts_with("gpt-5")
        || (model.starts_with('o') && model[1..].starts_with(|c: char| c.is_ascii_digit()))
}

fn map_finish_reason(reason: &str) -> FinishReason {
    match reason {
        "tool_calls" | "function_call" => FinishReason::ToolCalls,
        "length" => FinishReason::Length,
        _ => FinishReason::Stop,
    }
}

fn to_wire_messages(
    messages: &[ModelMessage],
    instruction: Option<&str>,
    style: InstructionStyle,
) -> Vec<WireMessage> {
    let mut mapped: Vec<WireMessage> = Vec::new();
    // The Relay system prompt becomes a leading developer message, or a
    // system message under the SystemInstruction compatibility fallback.
    if let Some(instruction) = instruction {
        mapped.push(match style {
            InstructionStyle::Developer => WireMessage::Developer {
                content: instruction.to_owned(),
            },
            InstructionStyle::SystemInstruction => WireMessage::System {
                content: instruction.to_owned(),
            },
        });
    }
    for message in messages {
        match message {
            ModelMessage::User { content } => {
                mapped.push(WireMessage::User {
                    content: content.clone(),
                });
            }
            ModelMessage::Assistant { content } => {
                let mut text = String::new();
                let mut tool_calls: Vec<WireToolCall> = Vec::new();
                for block in content {
                    match block {
                        crate::contract::AssistantContent::Text { text: part } => {
                            text.push_str(part);
                        }
                        crate::contract::AssistantContent::ToolCall { call } => {
                            tool_calls.push(WireToolCall {
                                id: call.id.clone(),
                                kind: "function",
                                function: WireFunction {
                                    name: call.name.clone(),
                                    arguments: serialize_arguments(&call.arguments),
                                },
                            });
                        }
                    }
                }
                mapped.push(WireMessage::Assistant {
                    content: if text.is_empty() { None } else { Some(text) },
                    tool_calls: if tool_calls.is_empty() {
                        None
                    } else {
                        Some(tool_calls)
                    },
                });
            }
            ModelMessage::ToolResult {
                tool_call_id,
                content,
            } => {
                mapped.push(WireMessage::Tool {
                    tool_call_id: tool_call_id.clone(),
                    content: content.clone(),
                });
            }
        }
    }
    mapped
}

fn serialize_arguments(arguments: &Value) -> String {
    if arguments.is_null() {
        "null".to_owned()
    } else {
        serde_json::to_string(arguments).unwrap_or_else(|_| "null".to_owned())
    }
}

fn to_wire_tools(tools: &[ToolSpec]) -> Vec<WireTool> {
    tools
        .iter()
        .map(|tool| WireTool {
            kind: "function",
            function: WireToolDefinition {
                name: tool.name.clone(),
                description: tool.description.clone(),
                parameters: tool.parameters.clone(),
            },
        })
        .collect()
}

/// Compose the leading instruction message content: the Relay system prompt,
/// plus (only in `json_object` mode) a bounded block embedding the requested
/// JSON Schema so the model knows the required shape. `json_schema` mode
/// carries the schema natively in `response_format`, so the instruction is
/// the system prompt unchanged.
fn compose_instruction(
    system: Option<&str>,
    structured: Option<&StructuredOutputRequest>,
    mode: StructuredOutputMode,
) -> Result<Option<String>, ProviderFailure> {
    let Some(structured) = structured.filter(|_| mode == StructuredOutputMode::JsonObject) else {
        return Ok(system.map(str::to_owned));
    };
    let block = embedded_schema_block(structured)?;
    Ok(Some(match system.filter(|system| !system.is_empty()) {
        Some(system) => format!("{system}\n\n{block}"),
        None => block,
    }))
}

/// The bounded instruction block used when the connection's structured-output
/// mode is `json_object`: it names the requested output and embeds the JSON
/// Schema verbatim. A schema larger than [`MAX_EMBEDDED_SCHEMA_CHARS`] fails
/// here rather than silently truncating the schema the model must satisfy.
fn embedded_schema_block(structured: &StructuredOutputRequest) -> Result<String, ProviderFailure> {
    let schema = serde_json::to_string(&structured.schema).unwrap_or_default();
    if schema.chars().count() > MAX_EMBEDDED_SCHEMA_CHARS {
        return Err(ProviderFailure::new(
            ProviderErrorKind::UnsupportedCapability,
            format!(
                "structured-output schema for \"{}\" is too large to embed for the json-object \
                 translation ({} characters, limit {MAX_EMBEDDED_SCHEMA_CHARS}); use a connection \
                 with native json-schema structured output or shrink the schema",
                structured.name,
                schema.chars().count()
            ),
        ));
    }
    Ok(format!(
        "Structured output \"{}\" is requested. Respond with ONLY a JSON object (no prose, no \
         code fences) conforming to this JSON Schema:\n{schema}",
        structured.name
    ))
}

/// Choose the output-token wire parameters for a limit under the
/// connection's parameter convention: `MaxTokens` always sends `max_tokens`,
/// `MaxCompletionTokens` always sends `max_completion_tokens`, and
/// `ModelDefault` keeps the per-model heuristic.
fn select_output_token_params(
    limit: Option<u64>,
    model: &str,
    param: OutputTokenParam,
) -> (Option<u64>, Option<u64>) {
    let Some(limit) = limit else {
        return (None, None);
    };
    let completion = match param {
        OutputTokenParam::MaxCompletionTokens => true,
        OutputTokenParam::MaxTokens => false,
        OutputTokenParam::ModelDefault => uses_completion_tokens(model),
    };
    if completion {
        (None, Some(limit))
    } else {
        (Some(limit), None)
    }
}

/// The `response_format` wire value for a structured-output request under the
/// connection's mode: native `json_schema` (with the schema and name), the
/// compatibility `json_object` spelling (schema embedded in the instruction
/// instead), or `None`. `Unsupported` never reaches here — preflight rejects
/// the request first — so it maps to `None` for totality.
fn wire_response_format(
    structured: Option<&StructuredOutputRequest>,
    mode: StructuredOutputMode,
) -> Option<WireResponseFormat> {
    let structured = structured?;
    match mode {
        StructuredOutputMode::JsonSchema => Some(WireResponseFormat {
            kind: "json_schema",
            json_schema: Some(WireJsonSchema {
                name: structured.name.clone(),
                schema: structured.schema.clone(),
                strict: false,
            }),
        }),
        StructuredOutputMode::JsonObject => Some(WireResponseFormat {
            kind: "json_object",
            json_schema: None,
        }),
        StructuredOutputMode::Unsupported => None,
    }
}

/// Parse JSON that models sometimes wrap in markdown fences.
pub(crate) fn parse_jsonish(text: &str) -> Option<Value> {
    let trimmed = text.trim();
    if let Ok(value) = serde_json::from_str(trimmed) {
        return Some(value);
    }
    let unfenced = strip_code_fence(trimmed);
    serde_json::from_str(&unfenced).ok()
}

/// Remove a leading ```` ```json ````/```` ``` ```` fence and trailing fence,
/// mirroring the TypeScript `^```(?:json)?\s*` / `\s*```$` (case-insensitive)
/// replacement pair.
fn strip_code_fence(trimmed: &str) -> String {
    let Some(after_open) = trimmed.strip_prefix("```") else {
        return trimmed.to_owned();
    };
    let after_tag = if after_open.len() >= 4 && after_open[..4].eq_ignore_ascii_case("json") {
        &after_open[4..]
    } else {
        after_open
    };
    let stripped_start = after_tag.trim_start();
    let trimmed_end = stripped_start.trim_end();
    let without_close = trimmed_end.strip_suffix("```").unwrap_or(trimmed_end);
    without_close.trim_end().to_owned()
}

/// Accumulated tool-call fragments for one streamed call.
#[derive(Default)]
struct AccumulatedToolCall {
    id: String,
    name: String,
    args: String,
}

/// Parse tool arguments that stream as JSON text; fall back to the raw string.
fn parse_tool_arguments(args: &str) -> Value {
    if args.is_empty() {
        return json!({});
    }
    serde_json::from_str(args).unwrap_or(Value::String(args.to_owned()))
}

struct OpenAiStream {
    receiver: StdReceiver<FetchItem>,
    signal: Option<CancelToken>,
    text: String,
    last_usage: Option<TokenUsage>,
    finish_reason: Option<FinishReason>,
    accumulators: Vec<AccumulatedToolCall>,
    by_index: HashMap<i64, usize>,
    structured_output: Option<crate::contract::StructuredOutputRequest>,
    pending: std::collections::VecDeque<ProviderStreamEvent>,
    finished: bool,
}

impl OpenAiStream {
    /// Fold one wire chunk into adapter state, returning the events to emit.
    fn absorb_chunk(&mut self, chunk: WireChunk) -> Vec<ProviderStreamEvent> {
        let mut events = Vec::new();
        if let Some(usage) = chunk.usage {
            let normalized = TokenUsage {
                input_tokens: usage.prompt_tokens,
                output_tokens: usage.completion_tokens,
            };
            self.last_usage = Some(normalized);
            events.push(ProviderStreamEvent::Usage { usage: normalized });
        }
        let Some(choice) = chunk.choices.into_iter().next() else {
            return events;
        };
        if let Some(delta) = choice.delta {
            if let Some(content) = delta.content
                && !content.is_empty()
            {
                self.text.push_str(&content);
                events.push(ProviderStreamEvent::TextDelta { text: content });
            }
            for fragment in delta.tool_calls {
                let position = match self.by_index.get(&fragment.index).copied() {
                    Some(position) => position,
                    None => {
                        self.accumulators.push(AccumulatedToolCall::default());
                        let position = self.accumulators.len() - 1;
                        self.by_index.insert(fragment.index, position);
                        position
                    }
                };
                let acc = &mut self.accumulators[position];
                if let Some(id) = fragment.id
                    && !id.is_empty()
                {
                    acc.id = id;
                }
                if let Some(function) = fragment.function {
                    if let Some(name) = function.name {
                        acc.name.push_str(&name);
                    }
                    if let Some(args) = function.arguments {
                        acc.args.push_str(&args);
                    }
                }
            }
        }
        if choice.finish_reason.is_some() && self.finish_reason.is_none() {
            self.finish_reason = choice.finish_reason.as_deref().map(map_finish_reason);
        }
        events
    }

    /// Terminal events: accumulated tool calls, structured output, completion.
    fn closing_events(&mut self) -> Vec<ProviderStreamEvent> {
        let mut events = Vec::new();
        for (position, acc) in self.accumulators.iter().enumerate() {
            events.push(ProviderStreamEvent::ToolCall {
                call: ToolCall {
                    id: if acc.id.is_empty() {
                        format!("call_{position}")
                    } else {
                        acc.id.clone()
                    },
                    name: acc.name.clone(),
                    arguments: parse_tool_arguments(&acc.args),
                },
            });
        }

        if let Some(requested) = self.structured_output.clone() {
            match parse_jsonish(&self.text) {
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
                                "OpenAI structured output was not valid JSON for \"{}\": {excerpt}",
                                requested.name
                            ),
                        ),
                    });
                    return events;
                }
            }
        }

        events.push(ProviderStreamEvent::Completed {
            finish_reason: self.finish_reason.unwrap_or(FinishReason::Stop),
            usage: self.last_usage.unwrap_or(TokenUsage {
                input_tokens: 0,
                output_tokens: 0,
            }),
        });
        events
    }

    fn canceled_failure() -> ProviderFailure {
        ProviderFailure::new(
            ProviderErrorKind::Canceled,
            "request aborted before completion",
        )
    }
}

impl Iterator for OpenAiStream {
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
                    if message.data == "[DONE]" {
                        let closing = self.closing_events();
                        self.pending.extend(closing);
                        self.finished = true;
                        return self.pending.pop_front();
                    }
                    match serde_json::from_str::<WireChunk>(&message.data) {
                        Ok(chunk) => {
                            let events = self.absorb_chunk(chunk);
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
                                        "malformed stream chunk: {error}"
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
                            error: Self::canceled_failure(),
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
                            error: Self::canceled_failure(),
                        });
                    }
                    continue;
                }
            }
        }
    }
}

impl ModelProvider for OpenAIProvider {
    fn name(&self) -> AdapterKind {
        AdapterKind::OpenaiChat
    }

    fn models(&self) -> Vec<String> {
        vec![
            "gpt-5.2".into(),
            "gpt-5-mini".into(),
            "gpt-4.1".into(),
            "gpt-4o".into(),
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

        let instruction = match compose_instruction(
            request.system.as_deref(),
            request.structured_output.as_ref(),
            self.capabilities.structured_output,
        ) {
            Ok(instruction) => instruction,
            Err(failure) => {
                return Box::new(std::iter::once(ProviderStreamEvent::Error {
                    error: failure,
                }));
            }
        };

        let tools = request.tools.as_deref().map(to_wire_tools);
        let wire = WireRequest {
            model: request.model.clone(),
            messages: to_wire_messages(
                &request.messages,
                instruction.as_deref(),
                self.capabilities.instructions,
            ),
            stream: true,
            // Connections whose streams never report usage must not ask for
            // them: stream_options is omitted entirely.
            stream_options: (self.capabilities.streamed_usage == FeatureSupport::Supported)
                .then_some(WireStreamOptions {
                    include_usage: true,
                }),
            tools: tools.filter(|tools| !tools.is_empty()),
            temperature: request.temperature,
            max_tokens: None,
            max_completion_tokens: None,
            response_format: wire_response_format(
                request.structured_output.as_ref(),
                self.capabilities.structured_output,
            ),
        };
        let (max_tokens, max_completion_tokens) = select_output_token_params(
            request.max_output_tokens,
            &request.model,
            self.capabilities.output_token_param,
        );
        let wire = WireRequest {
            max_tokens,
            max_completion_tokens,
            ..wire
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

        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let request_builder = self
            .client
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&body);
        let receiver = fetch_sse(request_builder);

        Box::new(OpenAiStream {
            receiver,
            signal: request.signal.clone(),
            text: String::new(),
            last_usage: None,
            finish_reason: None,
            accumulators: Vec::new(),
            by_index: HashMap::new(),
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
    fn resolves_explicit_keys_before_named_environment_keys() {
        assert_eq!(
            resolve_api_key_for("GLM_API_KEY", Some("explicit".into()), Some("from-env"))
                .as_deref(),
            Ok("explicit")
        );
        assert_eq!(
            resolve_api_key_for("GLM_API_KEY", None, Some("from-env")).as_deref(),
            Ok("from-env")
        );
        let error = resolve_api_key_for("GLM_API_KEY", None, None).expect_err("missing key");
        assert_eq!(error.kind, ProviderErrorKind::Auth);
        assert!(
            error
                .message
                .contains("The GLM_API_KEY environment variable is missing"),
            "{}",
            error.message
        );
        let error =
            resolve_api_key_for("GLM_API_KEY", Some(String::new()), None).expect_err("empty key");
        assert_eq!(error.kind, ProviderErrorKind::Auth);
    }

    #[test]
    fn defaults_are_the_openai_chat_adapter_defaults() {
        let provider = OpenAIProvider::new(OpenAIProviderOptions {
            api_key: Some("explicit-test-key".into()),
            ..OpenAIProviderOptions::default()
        })
        .expect("provider constructs offline");
        assert_eq!(
            provider.capabilities(),
            &AdapterCapabilities::defaults_for(AdapterKind::OpenaiChat)
        );
    }

    #[test]
    fn preflight_rejects_unsupported_tools_and_structured_output() {
        let provider = |capabilities: AdapterCapabilities| {
            OpenAIProvider::new(OpenAIProviderOptions {
                api_key: Some("explicit-test-key".into()),
                capabilities: Some(capabilities),
                ..OpenAIProviderOptions::default()
            })
            .expect("provider constructs offline")
        };

        let no_tools = provider(AdapterCapabilities {
            tool_calls: FeatureSupport::Unsupported,
            ..AdapterCapabilities::defaults_for(AdapterKind::OpenaiChat)
        });
        let mut with_tools = ModelRequest::simple("glm-4.6", "hi");
        with_tools.tools = Some(vec![ToolSpec {
            name: "read-file".into(),
            description: "reads".into(),
            parameters: serde_json::Map::new(),
        }]);
        let failure = no_tools
            .preflight_failure(&with_tools)
            .expect("tools rejected");
        assert_eq!(failure.kind, ProviderErrorKind::UnsupportedCapability);
        assert!(!failure.retryable);
        assert!(
            failure.message.contains("tool_calls")
                && failure.message.contains("openai-chat")
                && failure.message.contains("capability configuration"),
            "{failure}"
        );
        // A request without tools is unaffected by the tools capability.
        assert!(
            no_tools
                .preflight_failure(&ModelRequest::simple("glm-4.6", "hi"))
                .is_none()
        );

        let no_structured = provider(AdapterCapabilities {
            structured_output: StructuredOutputMode::Unsupported,
            ..AdapterCapabilities::defaults_for(AdapterKind::OpenaiChat)
        });
        let mut with_structured = ModelRequest::simple("glm-4.6", "hi");
        with_structured.structured_output = Some(StructuredOutputRequest {
            name: "intent-proposal".into(),
            schema: serde_json::Map::new(),
        });
        let failure = no_structured
            .preflight_failure(&with_structured)
            .expect("structured output rejected");
        assert!(
            failure.message.contains("structured_output")
                && failure.message.contains("openai-chat"),
            "{failure}"
        );
        assert!(
            no_structured
                .preflight_failure(&ModelRequest::simple("glm-4.6", "hi"))
                .is_none()
        );
    }

    #[test]
    fn json_object_instructions_embed_the_schema_json_schema_does_not() {
        let schema = serde_json::Map::from_iter([
            ("type".to_owned(), json!("object")),
            (
                "properties".to_owned(),
                json!({ "mode": { "type": "string" } }),
            ),
        ]);
        let structured = StructuredOutputRequest {
            name: "intent-proposal".into(),
            schema: schema.clone(),
        };

        let embedded = compose_instruction(
            Some("classifier"),
            Some(&structured),
            StructuredOutputMode::JsonObject,
        )
        .expect("embeds")
        .expect("some instruction");
        assert!(embedded.starts_with("classifier\n\n"), "{embedded}");
        assert!(
            embedded.contains("Structured output \"intent-proposal\" is requested."),
            "{embedded}"
        );
        // Map keys serialize in sorted order; the schema text is embedded
        // verbatim either way.
        assert!(
            embedded.contains("\"properties\":{\"mode\":{\"type\":\"string\"}}")
                && embedded.contains("\"type\":\"object\""),
            "{embedded}"
        );

        let without_system =
            compose_instruction(None, Some(&structured), StructuredOutputMode::JsonObject)
                .expect("embeds");
        assert!(
            without_system
                .as_deref()
                .is_some_and(|text| text.starts_with("Structured output")),
            "{without_system:?}"
        );

        let native = compose_instruction(
            Some("classifier"),
            Some(&structured),
            StructuredOutputMode::JsonSchema,
        )
        .expect("native mode passes through");
        assert_eq!(native.as_deref(), Some("classifier"));

        let no_structured =
            compose_instruction(Some("classifier"), None, StructuredOutputMode::JsonSchema)
                .expect("plain system passes through");
        assert_eq!(no_structured.as_deref(), Some("classifier"));
    }

    #[test]
    fn oversized_schemas_fail_the_json_object_translation() {
        let mut schema = serde_json::Map::new();
        schema.insert(
            "padding".to_owned(),
            Value::String("x".repeat(MAX_EMBEDDED_SCHEMA_CHARS)),
        );
        let structured = StructuredOutputRequest {
            name: "huge".into(),
            schema,
        };
        let failure = embedded_schema_block(&structured).expect_err("oversized rejected");
        assert_eq!(failure.kind, ProviderErrorKind::UnsupportedCapability);
        assert!(
            failure.message.contains("too large to embed") && failure.message.contains("\"huge\""),
            "{failure}"
        );
    }

    #[test]
    fn output_token_params_follow_the_configured_convention() {
        // MaxTokens pins max_tokens even for models the heuristic would
        // route to max_completion_tokens.
        assert_eq!(
            select_output_token_params(Some(128), "gpt-5.2", OutputTokenParam::MaxTokens),
            (Some(128), None)
        );
        // MaxCompletionTokens pins max_completion_tokens even for older
        // models.
        assert_eq!(
            select_output_token_params(Some(64), "gpt-4o", OutputTokenParam::MaxCompletionTokens),
            (None, Some(64))
        );
        // ModelDefault keeps the per-model heuristic.
        assert_eq!(
            select_output_token_params(Some(32), "gpt-5.2", OutputTokenParam::ModelDefault),
            (None, Some(32))
        );
        assert_eq!(
            select_output_token_params(Some(32), "glm-4.6", OutputTokenParam::ModelDefault),
            (Some(32), None)
        );
        // Without a limit, neither parameter is sent.
        assert_eq!(
            select_output_token_params(None, "gpt-4o", OutputTokenParam::MaxCompletionTokens),
            (None, None)
        );
    }

    #[test]
    fn completion_token_models_follow_the_reference_pattern() {
        assert!(uses_completion_tokens("gpt-5.2"));
        assert!(uses_completion_tokens("gpt-5-mini"));
        assert!(uses_completion_tokens("o3"));
        assert!(uses_completion_tokens("o1-mini"));
        assert!(!uses_completion_tokens("gpt-4.1"));
        assert!(!uses_completion_tokens("gpt-4o"));
        assert!(!uses_completion_tokens("ollama"));
    }

    #[test]
    fn finish_reason_mapping_matches_the_reference() {
        assert_eq!(map_finish_reason("tool_calls"), FinishReason::ToolCalls);
        assert_eq!(map_finish_reason("function_call"), FinishReason::ToolCalls);
        assert_eq!(map_finish_reason("length"), FinishReason::Length);
        assert_eq!(map_finish_reason("stop"), FinishReason::Stop);
        assert_eq!(map_finish_reason("content_filter"), FinishReason::Stop);
    }

    #[test]
    fn code_fences_are_stripped_before_parsing() {
        assert_eq!(strip_code_fence("```json\n{}\n```"), "{}");
        assert_eq!(strip_code_fence("```JSON {\"a\":1} ```"), "{\"a\":1}");
        assert_eq!(strip_code_fence("```\n[1,2]\n```"), "[1,2]");
        assert_eq!(strip_code_fence("{\"a\":1}"), "{\"a\":1}");
        assert_eq!(
            parse_jsonish("```json\n{\"mode\":\"planning\"}\n```"),
            Some(json!({ "mode": "planning" }))
        );
        assert_eq!(parse_jsonish("plain"), None);
    }

    #[test]
    fn tool_arguments_fall_back_to_the_raw_string() {
        assert_eq!(parse_tool_arguments(""), json!({}));
        assert_eq!(parse_tool_arguments("{\"a\":1}"), json!({ "a": 1 }));
        assert_eq!(parse_tool_arguments("{oops"), json!("{oops"));
    }
}
