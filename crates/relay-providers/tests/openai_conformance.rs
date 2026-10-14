//! OpenAI adapter conformance tests.
//!
//! Recorded, sanitized SSE fixtures replayed through the loopback mock
//! server, asserting the exact normalized outputs and wire translations
//! originally pinned by the retired TypeScript suite
//! (`test/unit/providers/openai.test.ts`, removed from the tree 2026-10-14);
//! those recorded fixtures are preserved here as the frozen goldens. The
//! capability-driven cases below layer the GLM-shaped profile (and explicit
//! overrides) over the same fixtures — sanitized shapes only; no test ever
//! contacts GLM or any other external service. No network access:
//! everything runs against 127.0.0.1.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use std::time::Duration;

use common::{CannedResponse, MockServer};
use relay_providers::collect_turn;
use relay_providers::contract::{
    AdapterCapabilities, AdapterKind, AssistantContent, CancelToken, CapabilityOverrides,
    FeatureSupport, FinishReason, InstructionStyle, ModelMessage, ModelProvider, ModelRequest,
    OutputTokenParam, ProviderErrorKind, ProviderStreamEvent, StructuredOutputMode,
    StructuredOutputRequest, ToolCall, ToolSpec, resolve_capabilities,
};
use relay_providers::openai::{OpenAIProvider, OpenAIProviderOptions};
use serde_json::{Value, json};

const API_KEY: &str = "test-key-not-a-real-credential";

fn provider(base_url: &str) -> OpenAIProvider {
    OpenAIProvider::new(OpenAIProviderOptions {
        api_key: Some(API_KEY.into()),
        api_key_env: None,
        base_url: Some(base_url.into()),
        capabilities: None,
    })
    .expect("provider with a key constructs")
}

/// A provider carrying an explicit capability profile (the way a configured
/// connection would).
fn provider_with(base_url: &str, capabilities: AdapterCapabilities) -> OpenAIProvider {
    OpenAIProvider::new(OpenAIProviderOptions {
        api_key: Some(API_KEY.into()),
        api_key_env: None,
        base_url: Some(base_url.into()),
        capabilities: Some(capabilities),
    })
    .expect("provider with a key constructs")
}

/// The effective GLM profile: system instructions, json-object structured
/// output, no streamed usage, plain max_tokens — resolved exactly the way a
/// configured `preset: glm` connection resolves it.
fn glm_capabilities() -> AdapterCapabilities {
    resolve_capabilities(
        AdapterKind::OpenaiChat,
        Some("glm"),
        &CapabilityOverrides::default(),
    )
    .expect("glm preset resolves for the openai-chat adapter")
}

/// Override one field of the `openai-chat` adapter defaults.
fn with_override(
    mutate: impl FnOnce(&mut CapabilityOverrides),
) -> Result<AdapterCapabilities, relay_providers::contract::CapabilityError> {
    let mut overrides = CapabilityOverrides::default();
    mutate(&mut overrides);
    resolve_capabilities(AdapterKind::OpenaiChat, None, &overrides)
}

fn text_chunk(text: &str) -> String {
    json!({
        "id": "c1",
        "choices": [{ "index": 0, "delta": { "role": "assistant", "content": text }, "finish_reason": null }],
    })
    .to_string()
}

fn tool_call_chunk(index: u64, fragment: Value) -> String {
    let mut call = json!({ "index": index });
    if let Some(object) = fragment.as_object() {
        for (key, value) in object {
            call[key.as_str()] = value.clone();
        }
    }
    json!({
        "id": "c1",
        "choices": [{ "index": 0, "delta": { "tool_calls": [call] }, "finish_reason": null }],
    })
    .to_string()
}

fn finish_chunk(reason: &str) -> String {
    json!({
        "id": "c1",
        "choices": [{ "index": 0, "delta": {}, "finish_reason": reason }],
    })
    .to_string()
}

fn usage_chunk(prompt: i64, completion: i64) -> String {
    json!({
        "id": "c1",
        "choices": [],
        "usage": { "prompt_tokens": prompt, "completion_tokens": completion },
    })
    .to_string()
}

fn sse_body(payloads: &[String]) -> String {
    let mut body = String::new();
    for payload in payloads {
        body.push_str(&format!("data: {payload}\n\n"));
    }
    body.push_str("data: [DONE]\n\n");
    body
}

fn base_request(model: &str) -> ModelRequest {
    ModelRequest::simple(model, "list the files").with_purpose("mode:planning")
}

#[test]
fn normalizes_text_deltas_split_tool_call_fragments_usage_and_finish_reason() {
    let body = sse_body(&[
        text_chunk("Sure, "),
        text_chunk("checking."),
        tool_call_chunk(
            0,
            json!({ "id": "call_a", "function": { "name": "list-files", "arguments": "{\"path\":\"sr" } }),
        ),
        tool_call_chunk(0, json!({ "function": { "arguments": "c\"}" } })),
        finish_chunk("tool_calls"),
        usage_chunk(120, 45),
    ]);
    let server = MockServer::start(CannedResponse::sse(200, &body));
    let provider = provider(&server.base_url);

    let result = collect_turn(provider.stream(&base_request("gpt-5.2"))).expect("turn completes");

    assert_eq!(result.text, "Sure, checking.");
    assert_eq!(
        result.tool_calls,
        vec![ToolCall {
            id: "call_a".into(),
            name: "list-files".into(),
            arguments: json!({ "path": "src" }),
        }]
    );
    assert_eq!(result.usage.input_tokens, 120);
    assert_eq!(result.usage.output_tokens, 45);
    assert_eq!(result.finish_reason, FinishReason::ToolCalls);

    let request = &server.recorded()[0];
    assert_eq!(request.method, "POST");
    assert_eq!(request.path, "/chat/completions");
    assert_eq!(
        request.header("authorization"),
        Some(&format!("Bearer {API_KEY}")[..])
    );
    assert_eq!(request.body["stream"], json!(true));
    assert_eq!(
        request.body["stream_options"],
        json!({ "include_usage": true })
    );
}

#[test]
fn replays_assistant_tool_calls_and_tool_results_in_request_messages() {
    let body = sse_body(&[finish_chunk("stop"), usage_chunk(1, 1)]);
    let server = MockServer::start(CannedResponse::sse(200, &body));
    let provider = provider(&server.base_url);

    let mut request = ModelRequest::simple("gpt-4o", "read app.ts");
    request.system = Some("be brief".into());
    request.messages = vec![
        ModelMessage::User {
            content: "read app.ts".into(),
        },
        ModelMessage::Assistant {
            content: vec![
                AssistantContent::Text {
                    text: "reading".into(),
                },
                AssistantContent::ToolCall {
                    call: ToolCall {
                        id: "call_1".into(),
                        name: "read-file".into(),
                        arguments: json!({ "path": "app.ts" }),
                    },
                },
            ],
        },
        ModelMessage::ToolResult {
            tool_call_id: "call_1".into(),
            content: "file body".into(),
        },
    ];
    request.tools = Some(vec![ToolSpec {
        name: "read-file".into(),
        description: "Reads a file".into(),
        parameters: serde_json::Map::from_iter([
            ("type".to_owned(), json!("object")),
            (
                "properties".to_owned(),
                json!({ "path": { "type": "string" } }),
            ),
        ]),
    }]);

    collect_turn(provider.stream(&request)).expect("turn completes");

    let wire = &server.recorded()[0].body;
    assert_eq!(
        wire["messages"],
        json!([
            { "role": "developer", "content": "be brief" },
            { "role": "user", "content": "read app.ts" },
            {
                "role": "assistant",
                "content": "reading",
                "tool_calls": [
                    {
                        "id": "call_1",
                        "type": "function",
                        "function": { "name": "read-file", "arguments": "{\"path\":\"app.ts\"}" },
                    },
                ],
            },
            { "role": "tool", "tool_call_id": "call_1", "content": "file body" },
        ])
    );
    assert_eq!(
        wire["tools"],
        json!([
            {
                "type": "function",
                "function": {
                    "name": "read-file",
                    "description": "Reads a file",
                    "parameters": { "type": "object", "properties": { "path": { "type": "string" } } },
                },
            },
        ])
    );
}

#[test]
fn uses_max_completion_tokens_for_gpt5_models_and_max_tokens_for_older_ones() {
    let server_a = MockServer::start(CannedResponse::sse(200, &sse_body(&[finish_chunk("stop")])));
    let mut request = ModelRequest::simple("gpt-5-mini", "hi");
    request.max_output_tokens = Some(512);
    collect_turn(provider(&server_a.base_url).stream(&request)).expect("turn completes");
    let wire_a = &server_a.recorded()[0].body;
    assert_eq!(wire_a["max_completion_tokens"], json!(512));
    assert_eq!(wire_a["max_tokens"], Value::Null);

    let server_b = MockServer::start(CannedResponse::sse(200, &sse_body(&[finish_chunk("stop")])));
    let mut request = ModelRequest::simple("gpt-4o", "hi");
    request.max_output_tokens = Some(256);
    request.temperature = Some(0.2);
    collect_turn(provider(&server_b.base_url).stream(&request)).expect("turn completes");
    let wire_b = &server_b.recorded()[0].body;
    assert_eq!(wire_b["max_tokens"], json!(256));
    assert_eq!(wire_b["temperature"], json!(0.2));
}

#[test]
fn requests_json_schema_response_format_and_emits_parsed_structured_output() {
    let body = sse_body(&[
        text_chunk("{\"kind\":\"intent\",\"mode\":\"discovery\"}"),
        finish_chunk("stop"),
        usage_chunk(9, 9),
    ]);
    let server = MockServer::start(CannedResponse::sse(200, &body));
    let provider = provider(&server.base_url);

    let mut request = ModelRequest::simple("gpt-4.1", "classify");
    request.structured_output = Some(StructuredOutputRequest {
        name: "intent-proposal".into(),
        schema: serde_json::Map::from_iter([("type".to_owned(), json!("object"))]),
    });

    let result = collect_turn(provider.stream(&request)).expect("turn completes");

    let response_format = &server.recorded()[0].body["response_format"];
    assert_eq!(response_format["type"], json!("json_schema"));
    assert_eq!(
        response_format["json_schema"],
        json!({
            "name": "intent-proposal",
            "schema": { "type": "object" },
            "strict": false,
        })
    );
    assert_eq!(
        result.structured_output.as_ref().map(|s| s.name.as_str()),
        Some("intent-proposal")
    );
    assert_eq!(
        result.structured_output.map(|s| s.value),
        Some(json!({ "kind": "intent", "mode": "discovery" }))
    );
}

#[test]
fn emits_invalid_response_error_when_structured_text_is_not_json() {
    let body = sse_body(&[text_chunk("not json at all"), finish_chunk("stop")]);
    let server = MockServer::start(CannedResponse::sse(200, &body));
    let provider = provider(&server.base_url);

    let mut request = ModelRequest::simple("gpt-4.1", "classify");
    request.structured_output = Some(StructuredOutputRequest {
        name: "intent-proposal".into(),
        schema: serde_json::Map::from_iter([("type".to_owned(), json!("object"))]),
    });

    let events: Vec<ProviderStreamEvent> = provider.stream(&request).collect();
    let error = events
        .iter()
        .find_map(|event| match event {
            ProviderStreamEvent::Error { error } => Some(error.clone()),
            _ => None,
        })
        .expect("error event present");
    assert_eq!(error.kind, ProviderErrorKind::InvalidResponse);
    assert!(!error.retryable);
    assert!(
        error.message.contains("not json at all"),
        "{}",
        error.message
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, ProviderStreamEvent::Completed { .. })),
        "no completed event after an invalid structured response"
    );
}

#[test]
fn falls_back_to_the_raw_string_when_tool_arguments_are_not_valid_json() {
    let body = sse_body(&[
        tool_call_chunk(
            0,
            json!({ "id": "call_b", "function": { "name": "odd", "arguments": "{oops" } }),
        ),
        finish_chunk("tool_calls"),
    ]);
    let server = MockServer::start(CannedResponse::sse(200, &body));
    let provider = provider(&server.base_url);

    let result = collect_turn(provider.stream(&base_request("gpt-5.2"))).expect("turn completes");
    assert_eq!(result.tool_calls[0].arguments, json!("{oops"));
}

#[test]
fn maps_http_401_to_a_non_retryable_auth_failure_event() {
    let server = MockServer::start(CannedResponse::json_error(
        401,
        r#"{"error":{"message":"Incorrect API key provided","type":"invalid_request_error"}}"#,
    ));
    let provider = provider(&server.base_url);

    let error =
        collect_turn(provider.stream(&base_request("gpt-5.2"))).expect_err("401 fails the turn");
    assert_eq!(error.kind, ProviderErrorKind::Auth);
    assert!(!error.retryable);
    assert_eq!(error.status, Some(401));
    assert_eq!(error.message, "Incorrect API key provided");
}

#[test]
fn maps_http_429_to_retryable_rate_limit_with_retry_after() {
    let server = MockServer::start(
        CannedResponse::json_error(429, r#"{"error":{"message":"slow down"}}"#)
            .with_header("retry-after", "7"),
    );
    let provider = provider(&server.base_url);

    let error =
        collect_turn(provider.stream(&base_request("gpt-5.2"))).expect_err("429 fails the turn");
    assert_eq!(error.kind, ProviderErrorKind::RateLimit);
    assert!(error.retryable);
    assert_eq!(error.status, Some(429));
    assert!(
        error.message.contains("retry-after: 7s"),
        "{}",
        error.message
    );
}

#[test]
fn maps_http_500_to_retryable_server_failure() {
    let server = MockServer::start(CannedResponse::json_error(
        500,
        r#"{"error":{"message":"upstream exploded"}}"#,
    ));
    let provider = provider(&server.base_url);

    let error =
        collect_turn(provider.stream(&base_request("gpt-5.2"))).expect_err("500 fails the turn");
    assert_eq!(error.kind, ProviderErrorKind::Server);
    assert!(error.retryable);
    assert_eq!(error.status, Some(500));
}

#[test]
fn reports_canceled_when_the_signal_is_already_aborted() {
    // The request never leaves the process: an aborted signal fails fast,
    // mirroring the SDK's AbortError before any I/O.
    let server = MockServer::start(CannedResponse::sse(200, &sse_body(&[finish_chunk("stop")])));
    let provider = provider(&server.base_url);

    let token = CancelToken::new();
    token.cancel("user pressed ctrl-c");
    let mut request = base_request("gpt-5.2");
    request.signal = Some(token);

    let error = collect_turn(provider.stream(&request)).expect_err("aborted signal fails");
    assert_eq!(error.kind, ProviderErrorKind::Canceled);
    assert!(!error.retryable);
    assert!(server.recorded().is_empty(), "no request is sent");
}

#[test]
fn cancels_mid_stream_when_the_signal_aborts_while_waiting_for_events() {
    let first = format!("data: {}\n\n", text_chunk("partial"));
    let server = MockServer::start(CannedResponse {
        status: 200,
        headers: Vec::new(),
        body_chunks: vec![first.into_bytes()],
        hang_after_body: true,
        chunk_delay: Duration::from_millis(0),
    });
    let provider = provider(&server.base_url);

    let token = CancelToken::new();
    let watcher = token.clone();
    std::thread::Builder::new()
        .spawn(move || {
            std::thread::sleep(Duration::from_millis(120));
            watcher.cancel("user interrupt");
        })
        .expect("spawn canceller");
    let mut request = base_request("gpt-5.2");
    request.signal = Some(token);

    let events: Vec<ProviderStreamEvent> = provider.stream(&request).collect();
    match events.last() {
        Some(ProviderStreamEvent::Error { error }) => {
            assert_eq!(error.kind, ProviderErrorKind::Canceled);
            assert!(!error.retryable);
        }
        other => panic!("expected canceled error event, got {other:?}"),
    }
}

#[test]
fn reports_name_and_supported_models() {
    let server = MockServer::start(CannedResponse::sse(200, &sse_body(&[finish_chunk("stop")])));
    let provider = provider(&server.base_url);
    assert_eq!(provider.name().as_str(), "openai-chat");
    assert!(provider.models().contains(&"gpt-5.2".to_owned()));
    assert!(provider.models().contains(&"gpt-4o".to_owned()));
}

#[test]
fn parses_fenced_json_for_structured_output() {
    let fenced = "```json\n{\"mode\":\"planning\"}\n```";
    let body = sse_body(&[text_chunk(fenced), finish_chunk("stop")]);
    let server = MockServer::start(CannedResponse::sse(200, &body));
    let provider = provider(&server.base_url);

    let mut request = ModelRequest::simple("gpt-4.1", "classify");
    request.structured_output = Some(StructuredOutputRequest {
        name: "intent-proposal".into(),
        schema: serde_json::Map::from_iter([("type".to_owned(), json!("object"))]),
    });
    let result = collect_turn(provider.stream(&request)).expect("turn completes");
    assert_eq!(
        result.structured_output.map(|output| output.value),
        Some(json!({ "mode": "planning" }))
    );
}

// ---------------------------------------------------------------------------
// Capability-driven conformance (GLM-shaped, sanitized fixtures only)
// ---------------------------------------------------------------------------

#[test]
fn glm_preset_profile_sends_system_role_max_tokens_and_omits_stream_options() {
    let server = MockServer::start(CannedResponse::sse(200, &sse_body(&[finish_chunk("stop")])));
    let provider = provider_with(&server.base_url, glm_capabilities());

    let mut request = ModelRequest::simple("glm-4.6", "hello");
    request.system = Some("be terse".into());
    request.max_output_tokens = Some(333);

    let result = collect_turn(provider.stream(&request)).expect("turn completes");
    assert_eq!(result.finish_reason, FinishReason::Stop);

    let wire = &server.recorded()[0].body;
    assert_eq!(wire["model"], json!("glm-4.6"));
    assert_eq!(wire["messages"][0]["role"], json!("system"));
    assert_eq!(wire["messages"][0]["content"], json!("be terse"));
    assert_eq!(wire["max_tokens"], json!(333));
    assert_eq!(wire["max_completion_tokens"], Value::Null);
    assert!(
        wire.get("stream_options").is_none(),
        "stream_options must be omitted when streamed_usage is unsupported: {wire}"
    );
}

#[test]
fn glm_shaped_chunks_accumulate_tool_arguments_split_across_deltas() {
    // GLM-shaped stream: an assistant delta carrying a reasoning_content
    // field (ignored by the adapter), then tool-call deltas whose id, name,
    // and arguments each arrive split across multiple chunks.
    let reasoning_chunk = json!({
        "id": "chatcmpl-glm-sanitized",
        "model": "glm-4.6",
        "choices": [{ "index": 0, "delta": { "role": "assistant", "reasoning_content": "thinking" }, "finish_reason": null }],
    })
    .to_string();
    let body = sse_body(&[
        reasoning_chunk,
        tool_call_chunk(
            0,
            json!({ "id": "call_glm_1", "function": { "name": "list-fi", "arguments": "{\"pa" } }),
        ),
        tool_call_chunk(
            0,
            json!({ "function": { "name": "les", "arguments": "th\":\"src\"}" } }),
        ),
        finish_chunk("tool_calls"),
        usage_chunk(41, 17),
    ]);
    let server = MockServer::start(CannedResponse::sse(200, &body));
    let provider = provider_with(&server.base_url, glm_capabilities());

    let result = collect_turn(provider.stream(&base_request("glm-4.6"))).expect("turn completes");

    assert_eq!(result.text, "", "reasoning content is not assistant text");
    assert_eq!(
        result.tool_calls,
        vec![ToolCall {
            id: "call_glm_1".into(),
            name: "list-files".into(),
            arguments: json!({ "path": "src" }),
        }]
    );
    assert_eq!(result.usage.input_tokens, 41);
    assert_eq!(result.usage.output_tokens, 17);
    assert_eq!(result.finish_reason, FinishReason::ToolCalls);
}

#[test]
fn streams_without_usage_complete_with_zero_usage_when_streamed_usage_is_unsupported() {
    let body = sse_body(&[text_chunk("hi there"), finish_chunk("stop")]);
    let server = MockServer::start(CannedResponse::sse(200, &body));
    let provider = provider_with(&server.base_url, glm_capabilities());

    let result = collect_turn(provider.stream(&base_request("glm-4.6"))).expect("turn completes");
    assert_eq!(result.text, "hi there");
    assert_eq!(result.usage.input_tokens, 0);
    assert_eq!(result.usage.output_tokens, 0);
    assert!(
        server.recorded()[0].body.get("stream_options").is_none(),
        "a connection whose streams never report usage must not request it"
    );
}

#[test]
fn malformed_stream_chunks_fail_with_invalid_response_and_no_completion() {
    let body = "data: {this is not json\n\ndata: [DONE]\n\n";
    let server = MockServer::start(CannedResponse::sse(200, body));
    let provider = provider(&server.base_url);

    let events: Vec<ProviderStreamEvent> = provider.stream(&base_request("gpt-4o")).collect();
    let error = events
        .iter()
        .find_map(|event| match event {
            ProviderStreamEvent::Error { error } => Some(error.clone()),
            _ => None,
        })
        .expect("malformed chunk surfaces as an error event");
    assert_eq!(error.kind, ProviderErrorKind::InvalidResponse);
    assert!(!error.retryable);
    assert!(
        error.message.contains("malformed stream chunk"),
        "{}",
        error.message
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, ProviderStreamEvent::Completed { .. })),
        "no completed event after a malformed chunk"
    );
}

#[test]
fn json_object_structured_output_embeds_the_schema_and_parses_the_reply() {
    let body = sse_body(&[text_chunk("{\"mode\":\"planning\"}"), finish_chunk("stop")]);
    let server = MockServer::start(CannedResponse::sse(200, &body));
    let capabilities = with_override(|overrides| {
        overrides.structured_output = Some(StructuredOutputMode::JsonObject);
    })
    .expect("json-object override resolves");
    let provider = provider_with(&server.base_url, capabilities);

    let mut request = ModelRequest::simple("glm-4.6", "classify");
    request.system = Some("classifier".into());
    request.structured_output = Some(StructuredOutputRequest {
        name: "intent-proposal".into(),
        schema: serde_json::Map::from_iter([
            ("type".to_owned(), json!("object")),
            (
                "properties".to_owned(),
                json!({ "mode": { "type": "string" } }),
            ),
        ]),
    });

    let result = collect_turn(provider.stream(&request)).expect("turn completes");

    let wire = &server.recorded()[0].body;
    assert_eq!(
        wire["response_format"],
        json!({ "type": "json_object" }),
        "json-object mode sends exactly the compatibility response format: {wire}"
    );
    let instruction = wire["messages"][0]["content"]
        .as_str()
        .expect("instruction message");
    assert!(instruction.contains("classifier"), "{instruction}");
    assert!(
        instruction.contains("Structured output \"intent-proposal\" is requested."),
        "{instruction}"
    );
    assert!(
        instruction.contains("\"type\":\"object\"")
            && instruction.contains("\"properties\":{\"mode\":{\"type\":\"string\"}}"),
        "the JSON Schema must be embedded in the instruction: {instruction}"
    );
    assert_eq!(
        result.structured_output.map(|output| output.value),
        Some(json!({ "mode": "planning" }))
    );
}

#[test]
fn unsupported_structured_output_fails_before_any_http_request() {
    let server = MockServer::start(CannedResponse::sse(200, &sse_body(&[finish_chunk("stop")])));
    let capabilities = with_override(|overrides| {
        overrides.structured_output = Some(StructuredOutputMode::Unsupported);
    })
    .expect("unsupported override resolves");
    let provider = provider_with(&server.base_url, capabilities);

    let mut request = ModelRequest::simple("glm-4.6", "classify");
    request.structured_output = Some(StructuredOutputRequest {
        name: "intent-proposal".into(),
        schema: serde_json::Map::from_iter([("type".to_owned(), json!("object"))]),
    });

    let error = collect_turn(provider.stream(&request)).expect_err("preflight fails");
    assert_eq!(error.kind, ProviderErrorKind::UnsupportedCapability);
    assert!(!error.retryable);
    assert!(
        error.message.contains("structured_output")
            && error.message.contains("openai-chat")
            && error.message.contains("capability configuration"),
        "{}",
        error.message
    );
    assert!(
        server.recorded().is_empty(),
        "no HTTP request may reach the server on an unsupported capability"
    );
}

#[test]
fn unsupported_tool_calls_fail_before_any_http_request() {
    let server = MockServer::start(CannedResponse::sse(200, &sse_body(&[finish_chunk("stop")])));
    let capabilities = with_override(|overrides| {
        overrides.tool_calls = Some(FeatureSupport::Unsupported);
    })
    .expect("unsupported override resolves");
    let provider = provider_with(&server.base_url, capabilities);

    let mut request = ModelRequest::simple("glm-4.6", "read app.ts");
    request.tools = Some(vec![ToolSpec {
        name: "read-file".into(),
        description: "Reads a file".into(),
        parameters: serde_json::Map::from_iter([("type".to_owned(), json!("object"))]),
    }]);

    let error = collect_turn(provider.stream(&request)).expect_err("preflight fails");
    assert_eq!(error.kind, ProviderErrorKind::UnsupportedCapability);
    assert!(!error.retryable);
    assert!(
        error.message.contains("tool_calls")
            && error.message.contains("openai-chat")
            && error.message.contains("capability configuration"),
        "{}",
        error.message
    );
    assert!(
        server.recorded().is_empty(),
        "no HTTP request may reach the server on an unsupported capability"
    );
}

#[test]
fn system_instruction_style_carries_the_prompt_as_a_system_role_message() {
    let server = MockServer::start(CannedResponse::sse(200, &sse_body(&[finish_chunk("stop")])));
    let capabilities = with_override(|overrides| {
        overrides.instructions = Some(InstructionStyle::SystemInstruction);
    })
    .expect("system-instruction override resolves");
    let provider = provider_with(&server.base_url, capabilities);

    let mut request = ModelRequest::simple("glm-4.6", "hello");
    request.system = Some("be brief".into());
    collect_turn(provider.stream(&request)).expect("turn completes");

    let wire = &server.recorded()[0].body;
    assert_eq!(wire["messages"][0]["role"], json!("system"));
    assert_eq!(wire["messages"][0]["content"], json!("be brief"));
    assert_eq!(wire["messages"][1]["role"], json!("user"));
}

#[test]
fn output_token_param_overrides_pin_the_wire_parameter() {
    // MaxTokens pins max_tokens even for a model the default heuristic would
    // route to max_completion_tokens.
    let server_a = MockServer::start(CannedResponse::sse(200, &sse_body(&[finish_chunk("stop")])));
    let pinned_max = with_override(|overrides| {
        overrides.output_token_param = Some(OutputTokenParam::MaxTokens);
    })
    .expect("max-tokens override resolves");
    let mut request = ModelRequest::simple("gpt-5.2", "hi");
    request.max_output_tokens = Some(128);
    collect_turn(provider_with(&server_a.base_url, pinned_max).stream(&request))
        .expect("turn completes");
    let wire_a = &server_a.recorded()[0].body;
    assert_eq!(wire_a["max_tokens"], json!(128));
    assert_eq!(wire_a["max_completion_tokens"], Value::Null);

    // MaxCompletionTokens pins max_completion_tokens even for an older model.
    let server_b = MockServer::start(CannedResponse::sse(200, &sse_body(&[finish_chunk("stop")])));
    let pinned_completion = with_override(|overrides| {
        overrides.output_token_param = Some(OutputTokenParam::MaxCompletionTokens);
    })
    .expect("max-completion-tokens override resolves");
    let mut request = ModelRequest::simple("gpt-4o", "hi");
    request.max_output_tokens = Some(64);
    collect_turn(provider_with(&server_b.base_url, pinned_completion).stream(&request))
        .expect("turn completes");
    let wire_b = &server_b.recorded()[0].body;
    assert_eq!(wire_b["max_completion_tokens"], json!(64));
    assert_eq!(wire_b["max_tokens"], Value::Null);
}

#[test]
fn base_url_joining_tolerates_trailing_slashes_and_path_prefixes() {
    let server = MockServer::start(CannedResponse::sse(200, &sse_body(&[finish_chunk("stop")])));
    for base in [
        server.base_url.clone(),
        format!("{}/", server.base_url),
        format!("{}/v4", server.base_url),
    ] {
        let provider = provider(&base);
        collect_turn(provider.stream(&base_request("gpt-4o"))).expect("turn completes");
    }
    let recorded = server.recorded();
    let paths: Vec<&str> = recorded
        .iter()
        .map(|request| request.path.as_str())
        .collect();
    assert_eq!(
        paths,
        [
            "/chat/completions",
            "/chat/completions",
            "/v4/chat/completions"
        ]
    );
}

#[test]
fn api_key_env_names_the_variable_and_missing_named_variables_fail_closed() {
    // A named variable that is not set fails construction with the redacted
    // missing-variable auth error naming the configured variable (unique,
    // test-only name; never set anywhere). Matched by hand: the provider
    // deliberately does not implement Debug (it holds the resolved key).
    let error = match OpenAIProvider::new(OpenAIProviderOptions {
        api_key: None,
        api_key_env: Some("RELAY_PROVIDERS_TEST_ABSENT_KEY_2026_10".into()),
        base_url: None,
        capabilities: None,
    }) {
        Err(error) => error,
        Ok(_) => panic!("missing named variable must fail construction"),
    };
    assert_eq!(error.kind, ProviderErrorKind::Auth);
    assert!(!error.retryable);
    assert!(
        error.message.contains(
            "The RELAY_PROVIDERS_TEST_ABSENT_KEY_2026_10 environment variable is missing"
        ),
        "{}",
        error.message
    );

    // An explicit key wins over the named variable.
    let provider = OpenAIProvider::new(OpenAIProviderOptions {
        api_key: Some(API_KEY.into()),
        api_key_env: Some("RELAY_PROVIDERS_TEST_ABSENT_KEY_2026_10".into()),
        base_url: None,
        capabilities: None,
    })
    .expect("explicit key wins over the named variable");
    assert_eq!(
        provider.capabilities(),
        &AdapterCapabilities::defaults_for(AdapterKind::OpenaiChat)
    );
}

/// Unique, test-only credential variable names for the two-connection
/// coexistence test. The values set through them exist only inside the
/// spawned child process below; they are fake tokens, never real
/// credentials, and are never logged.
const CONN_A_KEY_ENV: &str = "RELAY_PROVIDERS_TEST_CONN_A_KEY_2026_10";
const CONN_B_KEY_ENV: &str = "RELAY_PROVIDERS_TEST_CONN_B_KEY_2026_10";
const CONN_A_KEY: &str = "test-conn-a-key-not-a-real-credential";
const CONN_B_KEY: &str = "test-conn-b-key-not-a-real-credential";

#[test]
fn two_named_openai_chat_connections_coexist_with_their_own_endpoints_keys_and_models() {
    // `std::env::set_var` is unsafe in edition 2024 and unsafe code is
    // forbidden workspace-wide, so the env-bearing half of this test
    // re-executes the test binary as a child with the two variables set
    // through `Command::env` (spawn-style, argument array, no shell). The
    // child then exercises the real `std::env::var` lookup in the
    // constructor.
    if std::env::var(CONN_A_KEY_ENV).is_err() {
        let child = std::process::Command::new(std::env::current_exe().expect("test executable"))
            .args([
                "--exact",
                "two_named_openai_chat_connections_coexist_with_their_own_endpoints_keys_and_models",
            ])
            .env(CONN_A_KEY_ENV, CONN_A_KEY)
            .env(CONN_B_KEY_ENV, CONN_B_KEY)
            .output()
            .expect("spawn the env-bearing child run");
        assert!(
            child.status.success(),
            "env-bearing child run failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&child.stdout),
            String::from_utf8_lossy(&child.stderr),
        );
        return;
    }

    // Child mode: the variables above are set. Two connections built from
    // different options must coexist and stay independent.
    let server_a = MockServer::start(CannedResponse::sse(200, &sse_body(&[finish_chunk("stop")])));
    let server_b = MockServer::start(CannedResponse::sse(200, &sse_body(&[finish_chunk("stop")])));

    let conn_a = OpenAIProvider::new(OpenAIProviderOptions {
        api_key: None,
        api_key_env: Some(CONN_A_KEY_ENV.into()),
        base_url: Some(server_a.base_url.clone()),
        capabilities: Some(glm_capabilities()),
    })
    .expect("connection a constructs from its named variable");
    let conn_b = OpenAIProvider::new(OpenAIProviderOptions {
        api_key: None,
        api_key_env: Some(CONN_B_KEY_ENV.into()),
        base_url: Some(server_b.base_url.clone()),
        capabilities: None,
    })
    .expect("connection b constructs from its named variable");
    assert_eq!(
        conn_b.capabilities(),
        &AdapterCapabilities::defaults_for(AdapterKind::OpenaiChat)
    );

    collect_turn(conn_a.stream(&ModelRequest::simple("glm-4.6", "hello from a")))
        .expect("connection a completes");
    collect_turn(conn_b.stream(&ModelRequest::simple("gpt-4o", "hello from b")))
        .expect("connection b completes");

    let recorded_a = server_a.recorded();
    assert_eq!(recorded_a.len(), 1, "connection a hit its own endpoint");
    assert_eq!(recorded_a[0].path, "/chat/completions");
    assert_eq!(
        recorded_a[0].header("authorization"),
        Some(&format!("Bearer {CONN_A_KEY}")[..])
    );
    assert_eq!(recorded_a[0].body["model"], json!("glm-4.6"));

    let recorded_b = server_b.recorded();
    assert_eq!(recorded_b.len(), 1, "connection b hit its own endpoint");
    assert_eq!(recorded_b[0].path, "/chat/completions");
    assert_eq!(
        recorded_b[0].header("authorization"),
        Some(&format!("Bearer {CONN_B_KEY}")[..])
    );
    assert_eq!(recorded_b[0].body["model"], json!("gpt-4o"));
}
