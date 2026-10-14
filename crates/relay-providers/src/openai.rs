//! OpenAI adapter.
//!
//! Ported from `src/providers/openai.ts` (retired TypeScript tree, removed
//! from the repository 2026-10-14). Translates Relay-owned requests
//! into OpenAI chat-completion wire calls and normalizes streamed chunks back
//! into Relay-owned provider events. No wire type appears in this module's
//! public API; errors pass through the shared normalizer so headers and
//! bodies never leak. SDK-level retries are disabled so Relay's own retry
//! policy owns backoff.

use std::collections::HashMap;
use std::sync::mpsc::{Receiver as StdReceiver, RecvTimeoutError};
use std::time::Duration;

use crate::contract::TokenUsage;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::contract::{
    CancelToken, FinishReason, ModelMessage, ModelProvider, ModelRequest, ProviderErrorKind,
    ProviderEventStream, ProviderFailure, ProviderName, ProviderStreamEvent, ToolCall, ToolSpec,
};
use crate::errors::to_provider_failure;
use crate::sse::{FetchItem, fetch_sse};

/// Default OpenAI API base (the SDK's default).
const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

/// Total request timeout, matching the OpenAI SDK's 10-minute default.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(600);

/// Environment variable read when no API key is passed in.
const API_KEY_ENV: &str = "OPENAI_API_KEY";

/// Options for constructing an [`OpenAIProvider`].
#[derive(Debug, Clone, Default)]
pub struct OpenAIProviderOptions {
    /// API key; when omitted the adapter reads `API_KEY_ENV` from the
    /// environment. Never logged or echoed.
    pub api_key: Option<String>,
    /// Base URL override (used by the offline conformance tests).
    pub base_url: Option<String>,
}

// ---------------------------------------------------------------------------
// Wire types (private to this module)
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct WireRequest {
    model: String,
    messages: Vec<WireMessage>,
    stream: bool,
    stream_options: WireStreamOptions,
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
    json_schema: WireJsonSchema,
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

/// The OpenAI chat-completions provider.
pub struct OpenAIProvider {
    client: reqwest::blocking::Client,
    api_key: String,
    base_url: String,
}

impl OpenAIProvider {
    /// Build a provider. Mirrors the SDK's constructor behavior of failing
    /// when no API key is available.
    pub fn new(options: OpenAIProviderOptions) -> Result<Self, ProviderFailure> {
        let api_key = resolve_api_key(options.api_key, std::env::var(API_KEY_ENV).ok().as_deref())?;
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
        })
    }
}

/// Resolve the API key: explicit value first, environment second, mirroring
/// the SDK's constructor failure when neither is available.
pub(crate) fn resolve_api_key(
    explicit: Option<String>,
    from_env: Option<&str>,
) -> Result<String, ProviderFailure> {
    resolve_api_key_for(API_KEY_ENV, explicit, from_env)
}

/// Key resolution parameterized by environment variable name, shared with the
/// Anthropic adapter.
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

fn to_wire_messages(messages: &[ModelMessage], system: Option<&str>) -> Vec<WireMessage> {
    let mut mapped: Vec<WireMessage> = Vec::new();
    // The system prompt becomes a leading developer message.
    if let Some(system) = system {
        mapped.push(WireMessage::Developer {
            content: system.to_owned(),
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
                                error: to_provider_failure(crate::errors::ErrorSignal {
                                    message: format!("malformed stream chunk: {error}"),
                                    ..crate::errors::ErrorSignal::default()
                                }),
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
    fn name(&self) -> ProviderName {
        ProviderName::Openai
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

        let tools = request.tools.as_deref().map(to_wire_tools);
        let wire = WireRequest {
            model: request.model.clone(),
            messages: to_wire_messages(&request.messages, request.system.as_deref()),
            stream: true,
            stream_options: WireStreamOptions {
                include_usage: true,
            },
            tools: tools.filter(|tools| !tools.is_empty()),
            temperature: request.temperature,
            max_tokens: None,
            max_completion_tokens: None,
            response_format: request.structured_output.as_ref().map(|structured| {
                WireResponseFormat {
                    kind: "json_schema",
                    json_schema: WireJsonSchema {
                        name: structured.name.clone(),
                        schema: structured.schema.clone(),
                        strict: false,
                    },
                }
            }),
        };
        let (max_tokens, max_completion_tokens) = match request.max_output_tokens {
            Some(limit) if uses_completion_tokens(&request.model) => (None, Some(limit)),
            Some(limit) => (Some(limit), None),
            None => (None, None),
        };
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
    fn resolves_explicit_keys_before_environment_keys() {
        assert_eq!(
            resolve_api_key(Some("explicit".into()), Some("from-env")).as_deref(),
            Ok("explicit")
        );
        assert_eq!(
            resolve_api_key(None, Some("from-env")).as_deref(),
            Ok("from-env")
        );
        let error = resolve_api_key(None, None).expect_err("missing key");
        assert_eq!(error.kind, ProviderErrorKind::Auth);
        assert!(
            error.message.contains("OPENAI_API_KEY"),
            "{}",
            error.message
        );
        let error = resolve_api_key(Some(String::new()), None).expect_err("empty key");
        assert_eq!(error.kind, ProviderErrorKind::Auth);
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
