//! OpenAI adapter conformance tests.
//!
//! Recorded, sanitized SSE fixtures replayed through the loopback mock
//! server, asserting the exact normalized outputs and wire translations
//! originally pinned by the retired TypeScript suite
//! (`test/unit/providers/openai.test.ts`, removed from the tree 2026-10-14);
//! those recorded fixtures are preserved here as the frozen goldens. No
//! network access: everything runs against 127.0.0.1.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use std::time::Duration;

use common::{CannedResponse, MockServer};
use relay_providers::collect_turn;
use relay_providers::contract::{
    AssistantContent, CancelToken, FinishReason, ModelMessage, ModelProvider, ModelRequest,
    ProviderErrorKind, ProviderStreamEvent, StructuredOutputRequest, ToolCall, ToolSpec,
};
use serde_json::{Value, json};

const API_KEY: &str = "test-key-not-a-real-credential";

fn provider(base_url: &str) -> relay_providers::openai::OpenAIProvider {
    relay_providers::openai::OpenAIProvider::new(relay_providers::openai::OpenAIProviderOptions {
        api_key: Some(API_KEY.into()),
        base_url: Some(base_url.into()),
    })
    .expect("provider with a key constructs")
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
    assert_eq!(provider.name().as_str(), "openai");
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
