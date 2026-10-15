//! Anthropic adapter conformance tests.
//!
//! Recorded, sanitized SSE fixtures (the Anthropic Messages stream event
//! sequence) replayed through the loopback mock server, asserting the exact
//! normalized outputs and wire translations originally pinned by the retired
//! TypeScript suite (`test/unit/providers/anthropic.test.ts`, removed from
//! the tree 2026-10-14); those recorded fixtures are preserved here as the
//! frozen goldens. The suite also covers the native tool-loop conformance
//! added with the Anthropic-messages adapter rework: true-order text/tool
//! blocks, the fail-closed stream matrix, `is_error` tool results, and
//! opaque thinking/redacted-thinking continuation round-trips. No network
//! access: everything runs against 127.0.0.1.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use std::time::Duration;

use common::{CannedResponse, MockServer};
use relay_providers::anthropic::AnthropicProvider;
use relay_providers::collect_turn;
use relay_providers::contract::{
    AdapterCapabilities, AdapterKind, AssistantContent, CancelToken, CapabilityOverrides,
    FeatureSupport, FinishReason, MAX_CONTINUATION_DATA_BYTES, MAX_TEXT_BLOCK_BYTES, ModelMessage,
    ModelProvider, ModelRequest, ProviderErrorKind, ProviderFailure, ProviderStreamEvent,
    StructuredOutputMode, StructuredOutputRequest, ToolCall, ToolSpec, resolve_capabilities,
};
use serde_json::{Value, json};

const API_KEY: &str = "test-key-not-a-real-credential";

fn provider(base_url: &str) -> AnthropicProvider {
    AnthropicProvider::new(relay_providers::anthropic::AnthropicProviderOptions {
        api_key: Some(API_KEY.into()),
        api_key_env: None,
        base_url: Some(base_url.into()),
        capabilities: None,
    })
    .expect("provider with a key constructs")
}

/// A provider carrying an explicit capability profile (the way a configured
/// connection would).
fn provider_with(base_url: &str, capabilities: AdapterCapabilities) -> AnthropicProvider {
    AnthropicProvider::new(relay_providers::anthropic::AnthropicProviderOptions {
        api_key: Some(API_KEY.into()),
        api_key_env: None,
        base_url: Some(base_url.into()),
        capabilities: Some(capabilities),
    })
    .expect("provider with a key constructs")
}

/// Override one field of the `anthropic` adapter defaults.
fn with_override(
    mutate: impl FnOnce(&mut CapabilityOverrides),
) -> Result<AdapterCapabilities, relay_providers::contract::CapabilityError> {
    let mut overrides = CapabilityOverrides::default();
    mutate(&mut overrides);
    resolve_capabilities(AdapterKind::Anthropic, None, &overrides)
}

fn sse_body(events: &[Value]) -> String {
    let mut body = String::new();
    for event in events {
        let kind = event["type"].as_str().unwrap_or_default();
        body.push_str(&format!("event: {kind}\ndata: {event}\n\n"));
    }
    body
}

fn base_request() -> ModelRequest {
    ModelRequest::simple("claude-sonnet-5-5", "inspect the repo")
        .with_purpose("mode:technical-design")
}

/// The tool-use fixture from the TypeScript suite.
fn tool_use_events() -> Vec<Value> {
    vec![
        json!({ "type": "message_start", "message": { "id": "m1", "usage": { "input_tokens": 10, "output_tokens": 1 } } }),
        json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "text", "text": "" } }),
        json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "text_delta", "text": "Let me check " } }),
        json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "text_delta", "text": "that file." } }),
        json!({ "type": "content_block_stop", "index": 0 }),
        json!({ "type": "content_block_start", "index": 1, "content_block": { "type": "tool_use", "id": "tu_1", "name": "read-file", "input": {} } }),
        json!({ "type": "content_block_delta", "index": 1, "delta": { "type": "input_json_delta", "partial_json": "{\"path\":\"sr" } }),
        json!({ "type": "content_block_delta", "index": 1, "delta": { "type": "input_json_delta", "partial_json": "c/app.ts\"}" } }),
        json!({ "type": "content_block_stop", "index": 1 }),
        json!({ "type": "message_delta", "delta": { "stop_reason": "tool_use", "stop_sequence": null }, "usage": { "output_tokens": 7 } }),
        json!({ "type": "message_stop" }),
    ]
}

fn plain_end_turn_events(stop_reason: &str) -> Vec<Value> {
    vec![
        json!({ "type": "message_start", "message": { "id": "m", "usage": { "input_tokens": 1, "output_tokens": 1 } } }),
        json!({ "type": "message_delta", "delta": { "stop_reason": stop_reason, "stop_sequence": null }, "usage": { "output_tokens": 1 } }),
        json!({ "type": "message_stop" }),
    ]
}

/// One stable name per event kind, for ordering assertions.
fn event_kinds(events: &[ProviderStreamEvent]) -> Vec<&'static str> {
    events
        .iter()
        .map(|event| match event {
            ProviderStreamEvent::TextDelta { .. } => "text-delta",
            ProviderStreamEvent::ToolCall { .. } => "tool-call",
            ProviderStreamEvent::Continuation { .. } => "continuation",
            ProviderStreamEvent::Usage { .. } => "usage",
            ProviderStreamEvent::StructuredOutput { .. } => "structured-output",
            ProviderStreamEvent::Completed { .. } => "completed",
            ProviderStreamEvent::Error { .. } => "error",
        })
        .collect()
}

/// The first error event in a stream, for fail-closed assertions.
fn error_event(events: &[ProviderStreamEvent]) -> ProviderFailure {
    events
        .iter()
        .find_map(|event| match event {
            ProviderStreamEvent::Error { error } => Some(error.clone()),
            _ => None,
        })
        .expect("an error event")
}

fn assert_no_completed(events: &[ProviderStreamEvent]) {
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, ProviderStreamEvent::Completed { .. })),
        "a fail-closed stream never completes: {:?}",
        event_kinds(events)
    );
}

/// Text before AND after a tool_use block: the true-order fixture.
fn text_tool_text_events() -> Vec<Value> {
    vec![
        json!({ "type": "message_start", "message": { "id": "m1", "usage": { "input_tokens": 12, "output_tokens": 1 } } }),
        json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "text", "text": "" } }),
        json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "text_delta", "text": "Checking " } }),
        json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "text_delta", "text": "the file." } }),
        json!({ "type": "content_block_stop", "index": 0 }),
        json!({ "type": "content_block_start", "index": 1, "content_block": { "type": "tool_use", "id": "tu_9", "name": "read-file", "input": {} } }),
        json!({ "type": "content_block_delta", "index": 1, "delta": { "type": "input_json_delta", "partial_json": "{\"path\":\"pkg" } }),
        json!({ "type": "content_block_delta", "index": 1, "delta": { "type": "input_json_delta", "partial_json": ".rs\"}" } }),
        json!({ "type": "content_block_stop", "index": 1 }),
        json!({ "type": "content_block_start", "index": 2, "content_block": { "type": "text", "text": "" } }),
        json!({ "type": "content_block_delta", "index": 2, "delta": { "type": "text_delta", "text": "Now summarizing." } }),
        json!({ "type": "content_block_stop", "index": 2 }),
        json!({ "type": "message_delta", "delta": { "stop_reason": "tool_use", "stop_sequence": null }, "usage": { "output_tokens": 20 } }),
        json!({ "type": "message_stop" }),
    ]
}

/// Two tool_use blocks with no text: the tool-only fixture.
fn two_tool_events() -> Vec<Value> {
    vec![
        json!({ "type": "message_start", "message": { "id": "m2", "usage": { "input_tokens": 8, "output_tokens": 1 } } }),
        json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "tool_use", "id": "tu_a", "name": "read-file", "input": {} } }),
        json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "input_json_delta", "partial_json": "{\"path\":\"a.ts\"}" } }),
        json!({ "type": "content_block_stop", "index": 0 }),
        json!({ "type": "content_block_start", "index": 1, "content_block": { "type": "tool_use", "id": "tu_b", "name": "search", "input": {} } }),
        json!({ "type": "content_block_delta", "index": 1, "delta": { "type": "input_json_delta", "partial_json": "{\"query\":" } }),
        json!({ "type": "content_block_delta", "index": 1, "delta": { "type": "input_json_delta", "partial_json": "\"relay\"}" } }),
        json!({ "type": "content_block_stop", "index": 1 }),
        json!({ "type": "message_delta", "delta": { "stop_reason": "tool_use", "stop_sequence": null }, "usage": { "output_tokens": 30 } }),
        json!({ "type": "message_stop" }),
    ]
}

/// A thinking block (deltas plus signature) followed by visible text.
fn thinking_then_text_events() -> Vec<Value> {
    vec![
        json!({ "type": "message_start", "message": { "id": "m3", "usage": { "input_tokens": 9, "output_tokens": 1 } } }),
        json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "thinking", "thinking": "" } }),
        json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "thinking_delta", "thinking": "Pondering " } }),
        json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "thinking_delta", "thinking": "deeply." } }),
        json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "signature_delta", "signature": "sig-abc123" } }),
        json!({ "type": "content_block_stop", "index": 0 }),
        json!({ "type": "content_block_start", "index": 1, "content_block": { "type": "text", "text": "" } }),
        json!({ "type": "content_block_delta", "index": 1, "delta": { "type": "text_delta", "text": "The answer is 42." } }),
        json!({ "type": "content_block_stop", "index": 1 }),
        json!({ "type": "message_delta", "delta": { "stop_reason": "end_turn", "stop_sequence": null }, "usage": { "output_tokens": 40 } }),
        json!({ "type": "message_stop" }),
    ]
}

/// A redacted_thinking block (complete at start, no deltas) followed by text.
fn redacted_thinking_then_text_events() -> Vec<Value> {
    vec![
        json!({ "type": "message_start", "message": { "id": "m4", "usage": { "input_tokens": 7, "output_tokens": 1 } } }),
        json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "redacted_thinking", "data": "opaque-eNc=" } }),
        json!({ "type": "content_block_stop", "index": 0 }),
        json!({ "type": "content_block_start", "index": 1, "content_block": { "type": "text", "text": "" } }),
        json!({ "type": "content_block_delta", "index": 1, "delta": { "type": "text_delta", "text": "Visible answer." } }),
        json!({ "type": "content_block_stop", "index": 1 }),
        json!({ "type": "message_delta", "delta": { "stop_reason": "end_turn", "stop_sequence": null }, "usage": { "output_tokens": 15 } }),
        json!({ "type": "message_stop" }),
    ]
}

#[test]
fn normalizes_text_tool_use_fragments_usage_and_stop_reason() {
    let server = MockServer::start(CannedResponse::sse(200, &sse_body(&tool_use_events())));
    let provider = provider(&server.base_url);

    let result = collect_turn(provider.stream(&base_request())).expect("turn completes");

    assert_eq!(result.text(), "Let me check that file.");
    assert_eq!(
        result.tool_calls(),
        vec![ToolCall {
            id: "tu_1".into(),
            name: "read-file".into(),
            arguments: json!({ "path": "src/app.ts" }),
        }]
    );
    assert_eq!(result.usage.input_tokens, 10);
    assert_eq!(result.usage.output_tokens, 7);
    assert_eq!(result.finish_reason, FinishReason::ToolCalls);

    let request = &server.recorded()[0];
    assert_eq!(request.method, "POST");
    assert_eq!(request.path, "/v1/messages");
    assert_eq!(request.header("x-api-key"), Some(API_KEY));
    assert_eq!(request.header("anthropic-version"), Some("2023-06-01"));
    assert_eq!(request.body["model"], json!("claude-sonnet-5-5"));
    assert_eq!(request.body["max_tokens"], json!(8192));
}

#[test]
fn maps_stop_reasons_end_turn_and_max_tokens() {
    let end_turn = MockServer::start(CannedResponse::sse(
        200,
        &sse_body(&plain_end_turn_events("end_turn")),
    ));
    let result_a =
        collect_turn(provider(&end_turn.base_url).stream(&base_request())).expect("turn completes");
    assert_eq!(result_a.finish_reason, FinishReason::Stop);

    let max_tokens = MockServer::start(CannedResponse::sse(
        200,
        &sse_body(&plain_end_turn_events("max_tokens")),
    ));
    let result_b = collect_turn(provider(&max_tokens.base_url).stream(&base_request()))
        .expect("turn completes");
    assert_eq!(result_b.finish_reason, FinishReason::Length);
}

#[test]
fn merges_tool_results_and_replays_assistant_calls_as_tool_use() {
    let server = MockServer::start(CannedResponse::sse(
        200,
        &sse_body(&plain_end_turn_events("end_turn")),
    ));
    let provider = provider(&server.base_url);

    let mut request = ModelRequest::simple("claude-haiku-4-5", "check both");
    request.system = Some("sys prompt".into());
    request.max_output_tokens = Some(1000);
    request.temperature = Some(0.3);
    request.messages = vec![
        ModelMessage::user_text("check both"),
        ModelMessage::Assistant {
            content: vec![
                AssistantContent::ToolCall {
                    call: ToolCall {
                        id: "t1".into(),
                        name: "read-file".into(),
                        arguments: json!({ "path": "a.ts" }),
                    },
                },
                AssistantContent::ToolCall {
                    call: ToolCall {
                        id: "t2".into(),
                        name: "read-file".into(),
                        arguments: json!({ "path": "b.ts" }),
                    },
                },
            ],
        },
        ModelMessage::tool_result("t1", "contents a", false).expect("bounded"),
        ModelMessage::tool_result("t2", "contents b", false).expect("bounded"),
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
    assert_eq!(wire["system"], json!("sys prompt"));
    assert_eq!(wire["max_tokens"], json!(1000));
    assert_eq!(wire["temperature"], json!(0.3));
    assert_eq!(
        wire["messages"],
        json!([
            { "role": "user", "content": "check both" },
            {
                "role": "assistant",
                "content": [
                    { "type": "tool_use", "id": "t1", "name": "read-file", "input": { "path": "a.ts" } },
                    { "type": "tool_use", "id": "t2", "name": "read-file", "input": { "path": "b.ts" } },
                ],
            },
            {
                "role": "user",
                "content": [
                    { "type": "tool_result", "tool_use_id": "t1", "content": "contents a" },
                    { "type": "tool_result", "tool_use_id": "t2", "content": "contents b" },
                ],
            },
        ])
    );
    assert_eq!(
        wire["tools"],
        json!([
            {
                "name": "read-file",
                "description": "Reads a file",
                "input_schema": { "type": "object", "properties": { "path": { "type": "string" } } },
            },
        ])
    );
}

#[test]
fn prepends_a_json_schema_instruction_for_structured_output_and_parses_the_reply() {
    let events = vec![
        json!({ "type": "message_start", "message": { "id": "m", "usage": { "input_tokens": 4, "output_tokens": 2 } } }),
        json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "text", "text": "" } }),
        json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "text_delta", "text": "{\"mode\":\"discovery\"}" } }),
        json!({ "type": "content_block_stop", "index": 0 }),
        json!({ "type": "message_delta", "delta": { "stop_reason": "end_turn", "stop_sequence": null }, "usage": { "output_tokens": 3 } }),
        json!({ "type": "message_stop" }),
    ];
    let server = MockServer::start(CannedResponse::sse(200, &sse_body(&events)));
    let provider = provider(&server.base_url);

    let mut request = ModelRequest::simple("claude-sonnet-5-5", "classify");
    request.system = Some("classifier".into());
    request.structured_output = Some(StructuredOutputRequest {
        name: "intent-proposal".into(),
        schema: serde_json::Map::from_iter([("type".to_owned(), json!("object"))]),
    });

    let result = collect_turn(provider.stream(&request)).expect("turn completes");

    let recorded = server.recorded();
    let system = recorded[0].body["system"]
        .as_str()
        .expect("system")
        .to_owned();
    assert!(system.contains("classifier"), "{system}");
    assert!(
        system.contains("Respond with ONLY a JSON object matching this schema:"),
        "{system}"
    );
    assert!(system.contains("\"type\":\"object\""), "{system}");
    assert_eq!(
        result.structured_output,
        Some(relay_providers::contract::StructuredOutput {
            name: "intent-proposal".into(),
            value: json!({ "mode": "discovery" }),
        })
    );
}

#[test]
fn emits_structured_output_before_completion() {
    let events = vec![
        json!({ "type": "message_start", "message": { "id": "m", "usage": { "input_tokens": 4, "output_tokens": 2 } } }),
        json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "text", "text": "" } }),
        json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "text_delta", "text": "{\"mode\":\"discovery\"}" } }),
        json!({ "type": "content_block_stop", "index": 0 }),
        json!({ "type": "message_delta", "delta": { "stop_reason": "end_turn", "stop_sequence": null }, "usage": { "output_tokens": 3 } }),
        json!({ "type": "message_stop" }),
    ];
    let server = MockServer::start(CannedResponse::sse(200, &sse_body(&events)));
    let provider = provider(&server.base_url);

    let mut request = ModelRequest::simple("claude-sonnet-5-5", "classify");
    request.structured_output = Some(StructuredOutputRequest {
        name: "intent-proposal".into(),
        schema: serde_json::Map::from_iter([("type".to_owned(), json!("object"))]),
    });

    let events: Vec<ProviderStreamEvent> = provider.stream(&request).collect();
    let kinds = event_kinds(&events);
    let structured = kinds
        .iter()
        .position(|kind| *kind == "structured-output")
        .expect("structured output emitted");
    let completed = kinds
        .iter()
        .position(|kind| *kind == "completed")
        .expect("completed emitted");
    assert!(structured < completed, "{kinds:?}");
}

#[test]
fn emits_invalid_response_when_the_structured_reply_is_not_json() {
    let events = vec![
        json!({ "type": "message_start", "message": { "id": "m", "usage": { "input_tokens": 1, "output_tokens": 1 } } }),
        json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "text", "text": "" } }),
        json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "text_delta", "text": "definitely { not json" } }),
        json!({ "type": "content_block_stop", "index": 0 }),
        json!({ "type": "message_delta", "delta": { "stop_reason": "end_turn", "stop_sequence": null }, "usage": { "output_tokens": 2 } }),
        json!({ "type": "message_stop" }),
    ];
    let server = MockServer::start(CannedResponse::sse(200, &sse_body(&events)));
    let provider = provider(&server.base_url);

    let mut request = ModelRequest::simple("claude-sonnet-5-5", "classify");
    request.structured_output = Some(StructuredOutputRequest {
        name: "intent-proposal".into(),
        schema: serde_json::Map::from_iter([("type".to_owned(), json!("object"))]),
    });

    let error = collect_turn(provider.stream(&request)).expect_err("invalid JSON fails");
    assert_eq!(error.kind, ProviderErrorKind::InvalidResponse);
    assert!(!error.retryable);
}

#[test]
fn fails_closed_when_tool_input_cannot_be_parsed() {
    // The TypeScript suite replaced the first input delta with '{broken' and
    // emptied the second, letting the raw string through; the native
    // tool-loop adapter fails closed instead of handing the executor a
    // raw-string argument.
    let mut replaced_first = false;
    let broken: Vec<Value> = tool_use_events()
        .into_iter()
        .map(|mut event| {
            if event["type"] == "content_block_delta" && event["index"] == 1 {
                if replaced_first {
                    event["delta"] = json!({ "type": "input_json_delta", "partial_json": "" });
                } else {
                    replaced_first = true;
                    event["delta"] =
                        json!({ "type": "input_json_delta", "partial_json": "{broken" });
                }
            }
            event
        })
        .collect();
    let server = MockServer::start(CannedResponse::sse(200, &sse_body(&broken)));
    let provider = provider(&server.base_url);

    let events: Vec<ProviderStreamEvent> = provider.stream(&base_request()).collect();
    let error = error_event(&events);
    assert_eq!(error.kind, ProviderErrorKind::InvalidResponse);
    assert!(!error.retryable);
    assert!(
        error.message.contains("is not valid JSON"),
        "{}",
        error.message
    );
    assert_no_completed(&events);
    let error = collect_turn(events.into_iter()).expect_err("fails the turn");
    assert_eq!(error.kind, ProviderErrorKind::InvalidResponse);
}

#[test]
fn maps_a_429_response_to_a_retryable_rate_limit_failure() {
    let server = MockServer::start(
        CannedResponse::json_error(
            429,
            r#"{"type":"error","error":{"type":"rate_limit_error","message":"rate limited"}}"#,
        )
        .with_header("retry-after", "3"),
    );
    let provider = provider(&server.base_url);

    let error = collect_turn(provider.stream(&base_request())).expect_err("429 fails the turn");
    assert_eq!(error.kind, ProviderErrorKind::RateLimit);
    assert!(error.retryable);
    assert_eq!(error.status, Some(429));
    assert!(
        error.message.starts_with("rate limited") && error.message.contains("retry-after: 3s"),
        "{}",
        error.message
    );
}

#[test]
fn maps_a_401_response_to_a_non_retryable_auth_failure() {
    let server = MockServer::start(CannedResponse::json_error(
        401,
        r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#,
    ));
    let provider = provider(&server.base_url);

    let error = collect_turn(provider.stream(&base_request())).expect_err("401 fails the turn");
    assert_eq!(error.kind, ProviderErrorKind::Auth);
    assert!(!error.retryable);
    assert_eq!(error.status, Some(401));
}

#[test]
fn reports_cancellation_when_the_signal_aborts_mid_stream() {
    let first = format!(
        "event: message_start\ndata: {}\n\n",
        json!({ "type": "message_start", "message": { "id": "m", "usage": { "input_tokens": 1, "output_tokens": 1 } } })
    );
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
    let mut request = base_request();
    request.signal = Some(token);

    let error = collect_turn(provider.stream(&request)).expect_err("canceled mid-stream");
    assert_eq!(error.kind, ProviderErrorKind::Canceled);
    assert!(!error.retryable);
}

#[test]
fn fails_fast_when_the_signal_is_already_aborted() {
    let server = MockServer::start(CannedResponse::sse(
        200,
        &sse_body(&plain_end_turn_events("end_turn")),
    ));
    let provider = provider(&server.base_url);

    let token = CancelToken::new();
    token.cancel("user interrupt");
    let mut request = base_request();
    request.signal = Some(token);

    let error = collect_turn(provider.stream(&request)).expect_err("aborted signal fails");
    assert_eq!(error.kind, ProviderErrorKind::Canceled);
    assert!(!error.retryable);
    assert!(server.recorded().is_empty(), "no request is sent");
}

#[test]
fn malformed_stream_events_fail_with_invalid_response_and_no_completion() {
    let body = "event: message_start\ndata: {not a json event}\n\n";
    let server = MockServer::start(CannedResponse::sse(200, body));
    let provider = provider(&server.base_url);

    let events: Vec<ProviderStreamEvent> = provider.stream(&base_request()).collect();
    let error = events
        .iter()
        .find_map(|event| match event {
            ProviderStreamEvent::Error { error } => Some(error.clone()),
            _ => None,
        })
        .expect("malformed event surfaces as an error event");
    assert_eq!(error.kind, ProviderErrorKind::InvalidResponse);
    assert!(!error.retryable);
    assert!(
        error.message.contains("malformed stream event"),
        "{}",
        error.message
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, ProviderStreamEvent::Completed { .. })),
        "no completed event after a malformed event"
    );
}

#[test]
fn reports_an_error_event_from_mid_stream_without_completing() {
    let events = vec![
        json!({ "type": "message_start", "message": { "id": "m", "usage": { "input_tokens": 2, "output_tokens": 1 } } }),
        json!({ "type": "error", "error": { "type": "overloaded_error", "message": "Overloaded" } }),
    ];
    let server = MockServer::start(CannedResponse::sse(200, &sse_body(&events)));
    let provider = provider(&server.base_url);

    let collected: Vec<ProviderStreamEvent> = provider.stream(&base_request()).collect();
    let error = collected
        .iter()
        .find_map(|event| match event {
            ProviderStreamEvent::Error { error } => Some(error.clone()),
            _ => None,
        })
        .expect("error event surfaced");
    assert_eq!(error.kind, ProviderErrorKind::Unknown);
    assert_eq!(error.message, "Overloaded");
    assert!(
        !collected
            .iter()
            .any(|event| matches!(event, ProviderStreamEvent::Completed { .. })),
        "no completed event after a mid-stream error"
    );
}

#[test]
fn exposes_anthropic_metadata_without_leaking_wire_types() {
    let server = MockServer::start(CannedResponse::sse(
        200,
        &sse_body(&plain_end_turn_events("end_turn")),
    ));
    let provider = provider(&server.base_url);
    assert_eq!(provider.name().as_str(), "anthropic");
    assert_eq!(
        provider.models(),
        vec!["claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-4-5"]
    );
}

#[test]
fn ignores_unknown_stream_event_types_like_the_reference_default_branch() {
    let events = vec![
        json!({ "type": "message_start", "message": { "id": "m", "usage": { "input_tokens": 6, "output_tokens": 1 } } }),
        json!({ "type": "ping" }),
        json!({ "type": "future_event_kind", "payload": { "whatever": true } }),
        json!({ "type": "message_delta", "delta": { "stop_reason": "end_turn", "stop_sequence": null }, "usage": { "output_tokens": 2 } }),
        json!({ "type": "message_stop" }),
    ];
    let server = MockServer::start(CannedResponse::sse(200, &sse_body(&events)));
    let provider = provider(&server.base_url);

    let result = collect_turn(provider.stream(&base_request())).expect("turn completes");
    assert_eq!(result.finish_reason, FinishReason::Stop);
    assert_eq!(result.usage.input_tokens, 6);
    assert_eq!(result.usage.output_tokens, 2);
}

// ---------------------------------------------------------------------------
// Native tool-loop conformance: true-order blocks, fail-closed streams,
// error tool results, and opaque thinking continuations (offline, loopback)
// ---------------------------------------------------------------------------

#[test]
fn normalizes_a_text_only_turn() {
    let events = vec![
        json!({ "type": "message_start", "message": { "id": "m", "usage": { "input_tokens": 3, "output_tokens": 1 } } }),
        json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "text", "text": "" } }),
        json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "text_delta", "text": "Hello " } }),
        json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "text_delta", "text": "there." } }),
        json!({ "type": "content_block_stop", "index": 0 }),
        json!({ "type": "message_delta", "delta": { "stop_reason": "end_turn", "stop_sequence": null }, "usage": { "output_tokens": 4 } }),
        json!({ "type": "message_stop" }),
    ];
    let server = MockServer::start(CannedResponse::sse(200, &sse_body(&events)));
    let provider = provider(&server.base_url);

    let result = collect_turn(provider.stream(&base_request())).expect("turn completes");
    assert_eq!(
        result.blocks,
        vec![AssistantContent::Text {
            text: "Hello there.".into()
        }]
    );
    assert_eq!(result.text(), "Hello there.");
    assert!(result.tool_calls().is_empty());
    assert_eq!(result.finish_reason, FinishReason::Stop);
    assert_eq!(result.usage.input_tokens, 3);
    assert_eq!(result.usage.output_tokens, 4);
}

#[test]
fn emits_tool_only_turns_as_ordered_complete_calls() {
    let server = MockServer::start(CannedResponse::sse(200, &sse_body(&two_tool_events())));
    let provider = provider(&server.base_url);

    let events: Vec<ProviderStreamEvent> = provider.stream(&base_request()).collect();
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, ProviderStreamEvent::TextDelta { .. })),
        "no text in a tool-only turn: {:?}",
        event_kinds(&events)
    );

    let result = collect_turn(events.into_iter()).expect("turn completes");
    // Two calls, in arrival order, with distinct ids and validated inputs.
    assert_eq!(
        result.blocks,
        vec![
            AssistantContent::ToolCall {
                call: ToolCall {
                    id: "tu_a".into(),
                    name: "read-file".into(),
                    arguments: json!({ "path": "a.ts" }),
                },
            },
            AssistantContent::ToolCall {
                call: ToolCall {
                    id: "tu_b".into(),
                    name: "search".into(),
                    arguments: json!({ "query": "relay" }),
                },
            },
        ]
    );
    assert_eq!(result.finish_reason, FinishReason::ToolCalls);
}

#[test]
fn emits_blocks_in_true_order_text_tool_text() {
    let server = MockServer::start(CannedResponse::sse(
        200,
        &sse_body(&text_tool_text_events()),
    ));
    let provider = provider(&server.base_url);

    let events: Vec<ProviderStreamEvent> = provider.stream(&base_request()).collect();
    // The raw event stream interleaves: the tool call is emitted at its own
    // content_block_stop, between the two text runs.
    let kinds = event_kinds(&events);
    let first_text = kinds.iter().position(|kind| *kind == "text-delta");
    let last_text = kinds.iter().rposition(|kind| *kind == "text-delta");
    let tool = kinds.iter().position(|kind| *kind == "tool-call");
    assert!(
        first_text.is_some_and(|first| tool.is_some_and(|tool| first < tool))
            && tool.is_some_and(|tool| last_text.is_some_and(|last| tool < last)),
        "{kinds:?}"
    );

    // The authoritative block list keeps the interleaving.
    let result = collect_turn(events.into_iter()).expect("turn completes");
    assert_eq!(
        result.blocks,
        vec![
            AssistantContent::Text {
                text: "Checking the file.".into()
            },
            AssistantContent::ToolCall {
                call: ToolCall {
                    id: "tu_9".into(),
                    name: "read-file".into(),
                    arguments: json!({ "path": "pkg.rs" }),
                },
            },
            AssistantContent::Text {
                text: "Now summarizing.".into()
            },
        ]
    );
    // Visible text joins around the call without the call inside it.
    assert_eq!(result.text(), "Checking the file.Now summarizing.");
}

#[test]
fn error_tool_results_carry_is_error_on_the_wire() {
    let server = MockServer::start(CannedResponse::sse(
        200,
        &sse_body(&plain_end_turn_events("end_turn")),
    ));
    let provider = provider(&server.base_url);

    let mut request = base_request();
    request.messages = vec![
        ModelMessage::user_text("read the config"),
        ModelMessage::Assistant {
            content: vec![AssistantContent::ToolCall {
                call: ToolCall {
                    id: "t1".into(),
                    name: "read-file".into(),
                    arguments: json!({ "path": "relay.config.yaml" }),
                },
            }],
        },
        ModelMessage::tool_result("t1", "denied: the path is outside the grant", true)
            .expect("bounded"),
    ];

    collect_turn(provider.stream(&request)).expect("turn completes");

    let wire = &server.recorded()[0].body;
    assert_eq!(
        wire["messages"][2],
        json!({
            "role": "user",
            "content": [
                {
                    "type": "tool_result",
                    "tool_use_id": "t1",
                    "content": "denied: the path is outside the grant",
                    "is_error": true,
                },
            ],
        })
    );
}

#[test]
fn fails_closed_when_the_stream_ends_without_message_stop() {
    let mut incomplete = tool_use_events();
    assert_eq!(incomplete.pop().unwrap()["type"], "message_stop");
    let server = MockServer::start(CannedResponse::sse(200, &sse_body(&incomplete)));
    let provider = provider(&server.base_url);

    let events: Vec<ProviderStreamEvent> = provider.stream(&base_request()).collect();
    let error = error_event(&events);
    assert_eq!(error.kind, ProviderErrorKind::InvalidResponse);
    assert!(!error.retryable);
    assert!(
        error.message.contains("without a message_stop"),
        "{}",
        error.message
    );
    assert_no_completed(&events);
}

#[test]
fn fails_closed_on_a_duplicate_content_block_start_index() {
    let events = vec![
        json!({ "type": "message_start", "message": { "id": "m", "usage": { "input_tokens": 1, "output_tokens": 1 } } }),
        json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "text", "text": "" } }),
        json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "text", "text": "" } }),
        json!({ "type": "message_stop" }),
    ];
    let server = MockServer::start(CannedResponse::sse(200, &sse_body(&events)));
    let provider = provider(&server.base_url);

    let events: Vec<ProviderStreamEvent> = provider.stream(&base_request()).collect();
    let error = error_event(&events);
    assert_eq!(error.kind, ProviderErrorKind::InvalidResponse);
    assert!(error.message.contains("already open"), "{}", error.message);
    assert_no_completed(&events);
}

#[test]
fn fails_closed_on_a_delta_for_an_unannounced_block_index() {
    let events = vec![
        json!({ "type": "message_start", "message": { "id": "m", "usage": { "input_tokens": 1, "output_tokens": 1 } } }),
        json!({ "type": "content_block_delta", "index": 3, "delta": { "type": "text_delta", "text": "orphan" } }),
        json!({ "type": "message_stop" }),
    ];
    let server = MockServer::start(CannedResponse::sse(200, &sse_body(&events)));
    let provider = provider(&server.base_url);

    let events: Vec<ProviderStreamEvent> = provider.stream(&base_request()).collect();
    let error = error_event(&events);
    assert_eq!(error.kind, ProviderErrorKind::InvalidResponse);
    assert!(error.message.contains("is not open"), "{}", error.message);
    assert_no_completed(&events);
}

#[test]
fn fails_closed_on_a_delta_or_stop_for_a_closed_block_index() {
    let base = vec![
        json!({ "type": "message_start", "message": { "id": "m", "usage": { "input_tokens": 1, "output_tokens": 1 } } }),
        json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "text", "text": "" } }),
        json!({ "type": "content_block_stop", "index": 0 }),
    ];
    for tail in [
        vec![
            json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "text_delta", "text": "late" } }),
        ],
        vec![json!({ "type": "content_block_stop", "index": 0 })],
    ] {
        let mut events = base.clone();
        events.extend(tail);
        events.push(json!({ "type": "message_stop" }));
        let server = MockServer::start(CannedResponse::sse(200, &sse_body(&events)));
        let provider = provider(&server.base_url);

        let collected: Vec<ProviderStreamEvent> = provider.stream(&base_request()).collect();
        let error = error_event(&collected);
        assert_eq!(error.kind, ProviderErrorKind::InvalidResponse);
        assert!(error.message.contains("is not open"), "{}", error.message);
        assert_no_completed(&collected);
    }
}

#[test]
fn fails_closed_when_a_tool_use_block_is_missing_its_id_or_name() {
    for (content_block, missing) in [
        (
            json!({ "type": "tool_use", "name": "read-file", "input": {} }),
            "id",
        ),
        (
            json!({ "type": "tool_use", "id": "tu_1", "input": {} }),
            "name",
        ),
        (
            json!({ "type": "tool_use", "id": "", "name": "read-file", "input": {} }),
            "id",
        ),
        (
            json!({ "type": "tool_use", "id": "tu_1", "name": "", "input": {} }),
            "name",
        ),
    ] {
        let events = vec![
            json!({ "type": "message_start", "message": { "id": "m", "usage": { "input_tokens": 1, "output_tokens": 1 } } }),
            json!({ "type": "content_block_start", "index": 0, "content_block": content_block }),
            json!({ "type": "message_stop" }),
        ];
        let server = MockServer::start(CannedResponse::sse(200, &sse_body(&events)));
        let provider = provider(&server.base_url);

        let collected: Vec<ProviderStreamEvent> = provider.stream(&base_request()).collect();
        let error = error_event(&collected);
        assert_eq!(error.kind, ProviderErrorKind::InvalidResponse);
        assert!(
            error.message.contains(&format!("missing its {missing}")),
            "{}",
            error.message
        );
        assert_no_completed(&collected);
    }
}

#[test]
fn fails_closed_on_a_delta_kind_that_does_not_match_the_block_kind() {
    for (block, delta, expected) in [
        (
            json!({ "type": "text", "text": "" }),
            json!({ "type": "input_json_delta", "partial_json": "{\"a\":" }),
            "input_json_delta arrived for block index 0, which is not a tool_use block",
        ),
        (
            json!({ "type": "tool_use", "id": "tu_1", "name": "read-file", "input": {} }),
            json!({ "type": "text_delta", "text": "sneaky" }),
            "text_delta arrived for block index 0, which is not a text block",
        ),
        (
            json!({ "type": "text", "text": "" }),
            json!({ "type": "signature_delta", "signature": "sig" }),
            "signature_delta arrived for block index 0, which is not a thinking block",
        ),
    ] {
        let events = vec![
            json!({ "type": "message_start", "message": { "id": "m", "usage": { "input_tokens": 1, "output_tokens": 1 } } }),
            json!({ "type": "content_block_start", "index": 0, "content_block": block }),
            json!({ "type": "content_block_delta", "index": 0, "delta": delta }),
            json!({ "type": "message_stop" }),
        ];
        let server = MockServer::start(CannedResponse::sse(200, &sse_body(&events)));
        let provider = provider(&server.base_url);

        let collected: Vec<ProviderStreamEvent> = provider.stream(&base_request()).collect();
        let error = error_event(&collected);
        assert_eq!(error.kind, ProviderErrorKind::InvalidResponse);
        assert_eq!(error.message, expected);
        assert_no_completed(&collected);
    }
}

#[test]
fn captures_thinking_as_an_opaque_continuation_and_replays_it_byte_for_byte() {
    let server = MockServer::start(CannedResponse::sse(
        200,
        &sse_body(&thinking_then_text_events()),
    ));
    let capture_provider = provider(&server.base_url);

    let result = collect_turn(capture_provider.stream(&base_request())).expect("turn completes");

    // Captured in true order as a bounded opaque continuation block.
    assert_eq!(result.blocks.len(), 2);
    match &result.blocks[0] {
        AssistantContent::Continuation {
            discriminator,
            data,
        } => {
            assert_eq!(discriminator, "thinking");
            assert_eq!(
                data,
                "{\"signature\":\"sig-abc123\",\"thinking\":\"Pondering deeply.\"}"
            );
        }
        other => panic!("expected a continuation block, got {other:?}"),
    }
    // Hidden reasoning never reaches any visible surface: not the turn text,
    // not structured output (and receipts serialize only those).
    assert_eq!(result.text(), "The answer is 42.");
    assert!(!result.text().contains("Pondering"), "{}", result.text());
    assert!(result.structured_output.is_none());

    // Replay: the same blocks in the next request transcript map back onto
    // the identical thinking wire block, bytes preserved.
    let replay = MockServer::start(CannedResponse::sse(
        200,
        &sse_body(&plain_end_turn_events("end_turn")),
    ));
    let replay_provider = provider(&replay.base_url);
    let mut request = base_request();
    request.messages = vec![
        ModelMessage::user_text("the original question"),
        ModelMessage::Assistant {
            content: result.blocks.clone(),
        },
    ];
    collect_turn(replay_provider.stream(&request)).expect("replay turn completes");

    let wire = &replay.recorded()[0].body;
    assert_eq!(
        wire["messages"][1],
        json!({
            "role": "assistant",
            "content": [
                { "type": "thinking", "thinking": "Pondering deeply.", "signature": "sig-abc123" },
                { "type": "text", "text": "The answer is 42." },
            ],
        })
    );
}

#[test]
fn captures_redacted_thinking_and_replays_it_verbatim() {
    let server = MockServer::start(CannedResponse::sse(
        200,
        &sse_body(&redacted_thinking_then_text_events()),
    ));
    let capture_provider = provider(&server.base_url);

    let result = collect_turn(capture_provider.stream(&base_request())).expect("turn completes");
    assert_eq!(result.blocks.len(), 2);
    assert_eq!(
        result.blocks[0],
        AssistantContent::Continuation {
            discriminator: "redacted-thinking".into(),
            data: "opaque-eNc=".into(),
        }
    );
    // The opaque payload is not visible text.
    assert_eq!(result.text(), "Visible answer.");

    let replay = MockServer::start(CannedResponse::sse(
        200,
        &sse_body(&plain_end_turn_events("end_turn")),
    ));
    let replay_provider = provider(&replay.base_url);
    let mut request = base_request();
    request.messages = vec![
        ModelMessage::user_text("again"),
        ModelMessage::Assistant {
            content: result.blocks.clone(),
        },
    ];
    collect_turn(replay_provider.stream(&request)).expect("replay turn completes");

    let wire = &replay.recorded()[0].body;
    assert_eq!(
        wire["messages"][1],
        json!({
            "role": "assistant",
            "content": [
                { "type": "redacted_thinking", "data": "opaque-eNc=" },
                { "type": "text", "text": "Visible answer." },
            ],
        })
    );
}

#[test]
fn un_replayable_continuation_blocks_fail_before_any_http_request() {
    let server = MockServer::start(CannedResponse::sse(
        200,
        &sse_body(&plain_end_turn_events("end_turn")),
    ));
    let provider = provider(&server.base_url);

    let mut request = base_request();
    request.messages = vec![
        ModelMessage::user_text("continue"),
        ModelMessage::Assistant {
            content: vec![AssistantContent::Continuation {
                discriminator: "vendor-x".into(),
                data: "secret-opaque-payload".into(),
            }],
        },
    ];

    let error =
        collect_turn(provider.stream(&request)).expect_err("unknown discriminator fails closed");
    assert_eq!(error.kind, ProviderErrorKind::InvalidResponse);
    assert!(error.message.contains("vendor-x"), "{}", error.message);
    // The opaque payload is never echoed in the failure.
    assert!(
        !error.message.contains("secret-opaque-payload"),
        "{}",
        error.message
    );
    assert!(
        server.recorded().is_empty(),
        "no HTTP request may reach the server for an un-replayable continuation"
    );
}

#[test]
fn fails_closed_when_a_text_block_exceeds_the_bound() {
    let events = vec![
        json!({ "type": "message_start", "message": { "id": "m", "usage": { "input_tokens": 1, "output_tokens": 1 } } }),
        json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "text", "text": "" } }),
        json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "text_delta", "text": "x".repeat(MAX_TEXT_BLOCK_BYTES + 1) } }),
        json!({ "type": "content_block_stop", "index": 0 }),
        json!({ "type": "message_delta", "delta": { "stop_reason": "end_turn", "stop_sequence": null }, "usage": { "output_tokens": 2 } }),
        json!({ "type": "message_stop" }),
    ];
    let server = MockServer::start(CannedResponse::sse(200, &sse_body(&events)));
    let provider = provider(&server.base_url);

    let collected: Vec<ProviderStreamEvent> = provider.stream(&base_request()).collect();
    let error = error_event(&collected);
    assert_eq!(error.kind, ProviderErrorKind::InvalidResponse);
    assert!(
        error.message.contains("exceeded the") && error.message.contains("bound"),
        "{}",
        error.message
    );
    assert_no_completed(&collected);
}

#[test]
fn fails_closed_when_a_thinking_block_exceeds_the_continuation_bound() {
    let events = vec![
        json!({ "type": "message_start", "message": { "id": "m", "usage": { "input_tokens": 1, "output_tokens": 1 } } }),
        json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "thinking", "thinking": "" } }),
        json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "thinking_delta", "thinking": "x".repeat(MAX_CONTINUATION_DATA_BYTES + 1) } }),
        json!({ "type": "content_block_stop", "index": 0 }),
        json!({ "type": "message_stop" }),
    ];
    let server = MockServer::start(CannedResponse::sse(200, &sse_body(&events)));
    let provider = provider(&server.base_url);

    let collected: Vec<ProviderStreamEvent> = provider.stream(&base_request()).collect();
    let error = error_event(&collected);
    assert_eq!(error.kind, ProviderErrorKind::InvalidResponse);
    assert!(
        error.message.contains("continuation") && error.message.contains("bound"),
        "{}",
        error.message
    );
    assert_no_completed(&collected);
}

// ---------------------------------------------------------------------------
// Capability enforcement (offline, loopback-only)
// ---------------------------------------------------------------------------

#[test]
fn reports_the_anthropic_adapter_default_capabilities() {
    let server = MockServer::start(CannedResponse::sse(
        200,
        &sse_body(&plain_end_turn_events("end_turn")),
    ));
    let provider = provider(&server.base_url);
    assert_eq!(
        provider.capabilities(),
        &AdapterCapabilities::defaults_for(AdapterKind::Anthropic)
    );
    assert_eq!(
        provider.capabilities().structured_output,
        StructuredOutputMode::JsonObject
    );
}

#[test]
fn api_key_env_names_the_variable_and_missing_named_variables_fail_closed() {
    // A named variable that is not set fails construction with the redacted
    // missing-variable auth error naming the configured variable (unique,
    // test-only name; never set anywhere). Matched by hand: the provider
    // deliberately does not implement Debug (it holds the resolved key).
    let error = match AnthropicProvider::new(relay_providers::anthropic::AnthropicProviderOptions {
        api_key: None,
        api_key_env: Some("RELAY_PROVIDERS_TEST_ABSENT_ANTHROPIC_KEY_2026_10".into()),
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
            "The RELAY_PROVIDERS_TEST_ABSENT_ANTHROPIC_KEY_2026_10 environment variable is missing"
        ),
        "{}",
        error.message
    );

    // An explicit key wins over the named variable.
    let provider = AnthropicProvider::new(relay_providers::anthropic::AnthropicProviderOptions {
        api_key: Some(API_KEY.into()),
        api_key_env: Some("RELAY_PROVIDERS_TEST_ABSENT_ANTHROPIC_KEY_2026_10".into()),
        base_url: None,
        capabilities: None,
    })
    .expect("explicit key wins over the named variable");
    assert_eq!(
        provider.capabilities(),
        &AdapterCapabilities::defaults_for(AdapterKind::Anthropic)
    );
}

#[test]
fn unsupported_structured_output_fails_before_any_http_request() {
    let server = MockServer::start(CannedResponse::sse(
        200,
        &sse_body(&plain_end_turn_events("end_turn")),
    ));
    let capabilities = with_override(|overrides| {
        overrides.structured_output = Some(StructuredOutputMode::Unsupported);
    })
    .expect("unsupported override resolves");
    let provider = provider_with(&server.base_url, capabilities);

    let mut request = base_request();
    request.structured_output = Some(StructuredOutputRequest {
        name: "intent-proposal".into(),
        schema: serde_json::Map::from_iter([("type".to_owned(), json!("object"))]),
    });

    let error = collect_turn(provider.stream(&request)).expect_err("preflight fails");
    assert_eq!(error.kind, ProviderErrorKind::UnsupportedCapability);
    assert!(!error.retryable);
    assert!(
        error.message.contains("structured_output")
            && error.message.contains("anthropic")
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
fn json_schema_structured_output_mode_is_rejected_as_unimplemented() {
    let server = MockServer::start(CannedResponse::sse(
        200,
        &sse_body(&plain_end_turn_events("end_turn")),
    ));
    let capabilities = with_override(|overrides| {
        overrides.structured_output = Some(StructuredOutputMode::JsonSchema);
    })
    .expect("json-schema override resolves");
    let provider = provider_with(&server.base_url, capabilities);

    let mut request = base_request();
    request.structured_output = Some(StructuredOutputRequest {
        name: "intent-proposal".into(),
        schema: serde_json::Map::from_iter([("type".to_owned(), json!("object"))]),
    });

    let error = collect_turn(provider.stream(&request)).expect_err("preflight fails");
    assert_eq!(error.kind, ProviderErrorKind::UnsupportedCapability);
    assert!(!error.retryable);
    assert!(
        error.message.contains("json-schema")
            && error.message.contains("json-object")
            && error.message.contains("anthropic"),
        "{}",
        error.message
    );
    assert!(
        server.recorded().is_empty(),
        "no HTTP request may reach the server for an unimplemented mode"
    );
}

#[test]
fn unsupported_tool_calls_fail_before_any_http_request() {
    let server = MockServer::start(CannedResponse::sse(
        200,
        &sse_body(&plain_end_turn_events("end_turn")),
    ));
    let capabilities = with_override(|overrides| {
        overrides.tool_calls = Some(FeatureSupport::Unsupported);
    })
    .expect("unsupported override resolves");
    let provider = provider_with(&server.base_url, capabilities);

    let mut request = base_request();
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
            && error.message.contains("anthropic")
            && error.message.contains("capability configuration"),
        "{}",
        error.message
    );
    assert!(
        server.recorded().is_empty(),
        "no HTTP request may reach the server on an unsupported capability"
    );
}
