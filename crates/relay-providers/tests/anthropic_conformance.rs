//! Anthropic adapter conformance tests.
//!
//! Recorded, sanitized SSE fixtures (the Anthropic Messages stream event
//! sequence) replayed through the loopback mock server, asserting the exact
//! normalized outputs and wire translations originally pinned by the retired
//! TypeScript suite (`test/unit/providers/anthropic.test.ts`, removed from
//! the tree 2026-10-14); those recorded fixtures are preserved here as the
//! frozen goldens. No network access: everything runs against 127.0.0.1.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

mod common;

use std::time::Duration;

use common::{CannedResponse, MockServer};
use relay_providers::anthropic::AnthropicProvider;
use relay_providers::collect_turn;
use relay_providers::contract::{
    AdapterCapabilities, AdapterKind, AssistantContent, CancelToken, CapabilityOverrides,
    FeatureSupport, FinishReason, ModelMessage, ModelProvider, ModelRequest, ProviderErrorKind,
    ProviderStreamEvent, StructuredOutputMode, StructuredOutputRequest, ToolCall, ToolSpec,
    resolve_capabilities,
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

#[test]
fn normalizes_text_tool_use_fragments_usage_and_stop_reason() {
    let server = MockServer::start(CannedResponse::sse(200, &sse_body(&tool_use_events())));
    let provider = provider(&server.base_url);

    let result = collect_turn(provider.stream(&base_request())).expect("turn completes");

    assert_eq!(result.text, "Let me check that file.");
    assert_eq!(
        result.tool_calls,
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
        ModelMessage::User {
            content: "check both".into(),
        },
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
        ModelMessage::ToolResult {
            tool_call_id: "t1".into(),
            content: "contents a".into(),
        },
        ModelMessage::ToolResult {
            tool_call_id: "t2".into(),
            content: "contents b".into(),
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
    let kinds: Vec<&str> = events
        .iter()
        .map(|event| match event {
            ProviderStreamEvent::TextDelta { .. } => "text-delta",
            ProviderStreamEvent::ToolCall { .. } => "tool-call",
            ProviderStreamEvent::Usage { .. } => "usage",
            ProviderStreamEvent::StructuredOutput { .. } => "structured-output",
            ProviderStreamEvent::Completed { .. } => "completed",
            ProviderStreamEvent::Error { .. } => "error",
        })
        .collect();
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
fn falls_back_to_the_raw_json_string_when_tool_input_cannot_be_parsed() {
    // The TypeScript suite replaces the first input delta with '{broken' and
    // empties the second, so the accumulated JSON text is invalid.
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

    let result = collect_turn(provider.stream(&base_request())).expect("turn completes");
    assert_eq!(result.tool_calls[0].arguments, json!("{broken"));
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
