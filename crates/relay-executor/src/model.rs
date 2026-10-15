//! The model-client port: provider access behind executor-owned types.
//!
//! Executors and the orchestrator speak only to [`ModelClient`] with
//! [`ModelTurnRequest`]/[`ModelTurnResponse`]. Adapters implement the port
//! over `relay-providers`' neutral contract internally, so no provider type
//! ever appears in an executor signature. Swapping providers (or fakes)
//! changes the bytes a model produces, never the contract or the policy
//! outcomes built on it.
//!
//! Two implementations exist:
//!
//! - [`RoutedModelClient`](crate::routed_model::RoutedModelClient) is the
//!   production implementation: it resolves the attempt's granted route
//!   through a registry of named provider connections and a resolved route
//!   plan, with ordered, bounded fallback.
//! - [`ProviderModelClient`] is the single-provider test shim: it forwards
//!   every call to one wrapped provider and never falls back. Scheduling and
//!   concurrency tests inject it (or their own `ModelClient`) to stay
//!   deterministic.

use std::fmt;
use std::sync::Arc;

use relay_domain::ModelRoute;
use relay_providers::collect_turn;
use relay_providers::contract::{
    AssistantContent, CancelToken, ModelMessage, ModelProvider, ModelRequest, ProviderErrorKind,
    ToolCall, ToolSpec,
};

/// Bound on any failure message carried in executor diagnostics; provider
/// failures are already bounded by `relay-providers`, this is defense in
/// depth for locally synthesized messages.
const MAX_FAILURE_MESSAGE_CHARS: usize = 300;

/// A model call as an executor sees it: a Relay-owned transcript in, ordered
/// assistant blocks and usage out.
///
/// The transcript (`system` plus `messages`) is executor property: the model
/// client forwards it faithfully and never reconstructs or collapses it.
/// Tool definitions ([`tools`](Self::tools)) are supplied separately from
/// prose, already filtered to the attempt's capability grant.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelTurnRequest {
    /// System/developer instructions.
    pub system: Option<String>,
    /// The Relay-owned conversation transcript, in order. A text-only call
    /// carries a single user message (see [`ModelTurnRequest::from_prompt`]);
    /// a tool-loop session appends assistant tool-use turns and tool results.
    pub messages: Vec<ModelMessage>,
    /// Tool definitions the model may call (grant-filtered). `None` means
    /// the call is tool-free.
    pub tools: Option<Vec<ToolSpec>>,
    /// Output token ceiling. When `None`, the routed implementation falls
    /// back to the planned route's limit.
    pub max_output_tokens: Option<u64>,
    /// Sampling temperature. When `None`, the routed implementation falls
    /// back to the planned route's limit. Not `Eq`: temperatures are
    /// floating point.
    pub temperature: Option<f64>,
}

impl ModelTurnRequest {
    /// A prompt-only request: one user text message, no system instruction,
    /// no tools, no limits. Text-only call sites (chat flows, model-only
    /// executors, scripted tests) start here.
    pub fn from_prompt(prompt: impl Into<String>) -> Self {
        ModelTurnRequest {
            system: None,
            messages: vec![ModelMessage::user_text(prompt)],
            tools: None,
            max_output_tokens: None,
            temperature: None,
        }
    }

    /// The visible text of the last user message in the transcript (the
    /// prompt, for prompt-shaped requests). Empty string when the transcript
    /// has no user turn. This is a convenience for diagnostics and test
    /// doubles, not a substitute for forwarding the transcript.
    pub fn last_user_text(&self) -> String {
        self.messages
            .iter()
            .rev()
            .find_map(|message| match message {
                ModelMessage::User { .. } => Some(message.text()),
                _ => None,
            })
            .unwrap_or_default()
    }
}

/// A completed model turn as an executor sees it: the assistant's ordered
/// content blocks plus summed usage and route diagnostics.
///
/// [`blocks`](Self::blocks) is authoritative; [`text`](Self::text) and
/// [`tool_calls`](Self::tool_calls) are ordered convenience views. Not
/// `Eq`: tool-call arguments are JSON values.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelTurnResponse {
    /// Ordered assistant content blocks (text, tool calls, opaque
    /// continuation data in true order).
    pub blocks: Vec<AssistantContent>,
    /// Input tokens consumed, summed across every attempted route.
    pub input_tokens: u64,
    /// Output tokens consumed, summed across every attempted route.
    pub output_tokens: u64,
    /// Every route that received a model call, in attempt order (the
    /// successful route last). Routes skipped before a call (for example an
    /// unavailable fallback connection) are not listed.
    pub routes_attempted: Vec<ModelRoute>,
}

impl ModelTurnResponse {
    /// A text-only response with one text block, zero usage, and no
    /// attempted routes. Test doubles and scripted models migrate to this
    /// and override the fields they care about.
    pub fn from_text(text: impl Into<String>) -> Self {
        ModelTurnResponse {
            blocks: vec![AssistantContent::Text { text: text.into() }],
            input_tokens: 0,
            output_tokens: 0,
            routes_attempted: Vec::new(),
        }
    }

    /// Concatenated visible text across the text blocks (tool calls and
    /// opaque continuation blocks excluded).
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

/// One attempted route and the bounded, redacted reason it failed. Provider
/// messages arrive pre-bounded and pre-redacted from `relay-providers`;
/// locally synthesized messages are bounded at construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteAttemptFailure {
    /// The route that was attempted (or, for registry-level rejections, the
    /// route that could not be attempted; `provider` empty means no route
    /// context, e.g. a not-configured client).
    pub route: ModelRoute,
    /// The normalized failure classification.
    pub kind: ProviderErrorKind,
    /// The bounded failure message.
    pub message: String,
}

impl RouteAttemptFailure {
    /// An attempt summary from a provider failure, message bounded here as
    /// defense in depth.
    pub fn from_failure(
        route: ModelRoute,
        failure: &relay_providers::contract::ProviderFailure,
    ) -> Self {
        RouteAttemptFailure {
            route,
            kind: failure.kind,
            message: bound_message(&failure.message),
        }
    }

    /// An attempt summary synthesized without a provider failure.
    pub fn from_message(
        route: ModelRoute,
        kind: ProviderErrorKind,
        message: impl Into<String>,
    ) -> Self {
        RouteAttemptFailure {
            route,
            kind,
            message: bound_message(&message.into()),
        }
    }
}

/// The typed payload of a [`ModelClientError::Provider`]: every route
/// attempted before the terminal failure, in order. Display is bounded,
/// deterministic, and redacted, e.g.
/// `claude-proxy/claude-sonnet-5-5: rate-limit (throttled); fake/fake-standard: server (down)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelCallFailure {
    attempts: Vec<RouteAttemptFailure>,
}

impl ModelCallFailure {
    /// A failure report from ordered attempt summaries; the last entry is
    /// the terminal failure.
    pub fn new(attempts: Vec<RouteAttemptFailure>) -> Self {
        ModelCallFailure { attempts }
    }

    /// A single-failure report for callers with no route context (test
    /// doubles, not-configured clients).
    pub fn from_message(message: impl Into<String>) -> Self {
        ModelCallFailure {
            attempts: vec![RouteAttemptFailure::from_message(
                ModelRoute {
                    provider: String::new(),
                    model: String::new(),
                    role: None,
                },
                ProviderErrorKind::Unknown,
                message,
            )],
        }
    }

    /// Every attempt, in order; the last is the terminal failure.
    pub fn attempts(&self) -> &[RouteAttemptFailure] {
        &self.attempts
    }

    /// The terminal (last) attempt, when any.
    pub fn terminal(&self) -> Option<&RouteAttemptFailure> {
        self.attempts.last()
    }
}

impl fmt::Display for ModelCallFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.attempts.is_empty() {
            return f.write_str("provider call failed");
        }
        let mut first = true;
        for attempt in &self.attempts {
            if !first {
                f.write_str("; ")?;
            }
            first = false;
            if attempt.route.provider.is_empty() {
                write!(f, "{}", attempt.message)?;
            } else {
                write!(
                    f,
                    "{}/{}: {} ({})",
                    attempt.route.provider,
                    attempt.route.model,
                    attempt.kind.as_str(),
                    attempt.message
                )?;
            }
        }
        Ok(())
    }
}

/// Why a model call failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ModelClientError {
    /// The transport or provider failed on every attempted route. Carries
    /// the typed per-route attempt summaries (see [`ModelCallFailure`]).
    #[error("model call failed: {0}")]
    Provider(ModelCallFailure),
    /// The call was canceled through its signal.
    #[error("model call canceled: {0}")]
    Canceled(String),
}

/// Slice a message to at most [`MAX_FAILURE_MESSAGE_CHARS`] characters on a
/// char boundary.
fn bound_message(message: &str) -> String {
    message.chars().take(MAX_FAILURE_MESSAGE_CHARS).collect()
}

/// Provider access for executors. `route` comes from the attempt's
/// capability grant; implementations resolve it to a concrete provider.
pub trait ModelClient: Send + Sync {
    /// Perform one model turn. Cancellation through `signal` must be
    /// observed between stream events, never by blocking indefinitely.
    fn complete(
        &self,
        route: &ModelRoute,
        request: &ModelTurnRequest,
        signal: &CancelToken,
    ) -> Result<ModelTurnResponse, ModelClientError>;
}

/// [`ModelClient`] over any single `relay-providers` [`ModelProvider`]. The
/// provider's wire types stay inside this adapter.
///
/// This is the single-provider test shim: every call goes to the wrapped
/// provider regardless of the granted route's connection, and failures never
/// fall back. The production implementation is
/// [`RoutedModelClient`](crate::routed_model::RoutedModelClient).
#[derive(Clone)]
pub struct ProviderModelClient {
    provider: Arc<dyn ModelProvider>,
}

impl ProviderModelClient {
    /// Wrap a provider.
    pub fn new(provider: Arc<dyn ModelProvider>) -> Self {
        ProviderModelClient { provider }
    }
}

impl ModelClient for ProviderModelClient {
    fn complete(
        &self,
        route: &ModelRoute,
        request: &ModelTurnRequest,
        signal: &CancelToken,
    ) -> Result<ModelTurnResponse, ModelClientError> {
        let provider_request = ModelRequest {
            model: route.model.clone(),
            system: request.system.clone(),
            messages: request.messages.clone(),
            tools: request.tools.clone(),
            structured_output: None,
            max_output_tokens: request.max_output_tokens,
            temperature: request.temperature,
            purpose: route.role.clone(),
            signal: Some(signal.clone()),
        };
        let turn = collect_turn(self.provider.stream(&provider_request)).map_err(|failure| {
            ModelClientError::Provider(ModelCallFailure::new(vec![
                RouteAttemptFailure::from_failure(route.clone(), &failure),
            ]))
        })?;
        if signal.is_cancelled() {
            return Err(ModelClientError::Canceled(
                signal.reason().unwrap_or_else(|| "canceled".to_owned()),
            ));
        }
        Ok(ModelTurnResponse {
            blocks: turn.blocks,
            input_tokens: turn.usage.input_tokens.max(0) as u64,
            output_tokens: turn.usage.output_tokens.max(0) as u64,
            routes_attempted: vec![route.clone()],
        })
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use relay_providers::contract::{AdapterKind, CancelToken};
    use relay_providers::fake::{FakeProvider, FakeResponse, FakeToolCallPlan};

    fn provider_with_text(text: &str) -> Arc<dyn ModelProvider> {
        Arc::new(FakeProvider::with_fallback(
            Vec::new(),
            FakeResponse {
                text: Some(text.to_owned()),
                ..FakeResponse::default()
            },
        ))
    }

    #[test]
    fn provider_adapters_speak_only_executor_types() {
        let client = ProviderModelClient::new(provider_with_text("hello from the model"));
        let route = ModelRoute {
            provider: "fake".to_owned(),
            model: "fake-standard".to_owned(),
            role: None,
        };
        let response = client
            .complete(
                &route,
                &ModelTurnRequest::from_prompt("say something"),
                &CancelToken::new(),
            )
            .expect("completes");
        assert_eq!(response.text(), "hello from the model");
        assert_eq!(response.routes_attempted, vec![route.clone()]);
    }

    #[test]
    fn from_prompt_builds_a_single_user_transcript_and_reads_back() {
        let request = ModelTurnRequest::from_prompt("do the thing");
        assert_eq!(request.system, None);
        assert_eq!(request.tools, None);
        assert_eq!(request.messages.len(), 1);
        assert_eq!(request.messages[0].text(), "do the thing");
        assert_eq!(request.last_user_text(), "do the thing");

        // With a longer transcript, the newest user turn is the one read.
        let request = ModelTurnRequest {
            system: Some("be brief".into()),
            messages: vec![
                ModelMessage::user_text("first"),
                ModelMessage::Assistant {
                    content: vec![AssistantContent::Text { text: "ack".into() }],
                },
                ModelMessage::user_text("second"),
            ],
            tools: None,
            max_output_tokens: None,
            temperature: None,
        };
        assert_eq!(request.last_user_text(), "second");
        // A transcript without a user turn reads as empty, not as assistant
        // text.
        let request = ModelTurnRequest {
            system: None,
            messages: vec![ModelMessage::Assistant {
                content: vec![AssistantContent::Text { text: "hi".into() }],
            }],
            tools: None,
            max_output_tokens: None,
            temperature: None,
        };
        assert_eq!(request.last_user_text(), "");
    }

    #[test]
    fn from_text_builds_a_text_block_response_that_reads_back() {
        let response = ModelTurnResponse::from_text("{\"outputs\":[]}");
        assert_eq!(response.text(), "{\"outputs\":[]}");
        assert!(response.tool_calls().is_empty());
        assert_eq!(response.input_tokens, 0);
        assert_eq!(response.output_tokens, 0);
        assert!(response.routes_attempted.is_empty());
        assert_eq!(
            response.blocks,
            vec![AssistantContent::Text {
                text: "{\"outputs\":[]}".into()
            }]
        );
    }

    #[test]
    fn forwards_the_transcript_tools_and_limits_faithfully() {
        /// Records the exact request it served, then completes.
        struct RecordingProvider {
            requests: std::sync::Mutex<Vec<relay_providers::contract::ModelRequest>>,
        }
        impl ModelProvider for RecordingProvider {
            fn name(&self) -> relay_providers::contract::AdapterKind {
                relay_providers::contract::AdapterKind::Fake
            }
            fn models(&self) -> Vec<String> {
                vec!["fake-standard".to_owned()]
            }
            fn stream(
                &self,
                request: &relay_providers::contract::ModelRequest,
            ) -> relay_providers::contract::ProviderEventStream<'_> {
                self.requests
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner())
                    .push(request.clone());
                Box::new(
                    vec![
                        relay_providers::contract::ProviderStreamEvent::TextDelta {
                            text: "ok".to_owned(),
                        },
                        relay_providers::contract::ProviderStreamEvent::Completed {
                            finish_reason: relay_providers::contract::FinishReason::Stop,
                            usage: relay_providers::contract::TokenUsage {
                                input_tokens: 3,
                                output_tokens: 4,
                            },
                        },
                    ]
                    .into_iter(),
                )
            }
        }

        let provider = Arc::new(RecordingProvider {
            requests: std::sync::Mutex::new(Vec::new()),
        });
        let client = ProviderModelClient::new(provider.clone() as Arc<dyn ModelProvider>);
        let route = ModelRoute {
            provider: "fake".to_owned(),
            model: "fake-standard".to_owned(),
            role: Some("fast".to_owned()),
        };
        let request = ModelTurnRequest {
            system: Some("system instruction".into()),
            messages: vec![
                ModelMessage::user_text("first"),
                ModelMessage::Assistant {
                    content: vec![AssistantContent::Text { text: "mid".into() }],
                },
                ModelMessage::user_text("the prompt"),
            ],
            tools: Some(vec![ToolSpec {
                name: "read-file".into(),
                description: "Reads a file".into(),
                parameters: serde_json::Map::from_iter([(
                    "type".to_owned(),
                    serde_json::json!("object"),
                )]),
            }]),
            max_output_tokens: Some(1_024),
            temperature: Some(0.2),
        };
        let response = client
            .complete(&route, &request, &CancelToken::new())
            .expect("completes");

        // The provider saw the exact transcript, tools, and limits.
        let seen = provider
            .requests
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .last()
            .cloned()
            .expect("a call was recorded");
        assert_eq!(seen.system.as_deref(), Some("system instruction"));
        assert_eq!(seen.messages.len(), 3);
        assert_eq!(seen.messages[2].text(), "the prompt");
        assert_eq!(
            seen.tools.as_ref().map(|tools| tools.len()),
            Some(1),
            "tools are forwarded, not dropped"
        );
        assert_eq!(seen.max_output_tokens, Some(1_024));
        assert_eq!(seen.temperature, Some(0.2));
        assert_eq!(seen.purpose.as_deref(), Some("fast"));

        // The response reads back through the ordered-block views with the
        // route and usage accounted.
        assert_eq!(response.text(), "ok");
        assert_eq!(response.input_tokens, 3);
        assert_eq!(response.output_tokens, 4);
        assert_eq!(response.routes_attempted, vec![route.clone()]);
    }

    #[test]
    fn returns_ordered_blocks_without_discarding_tool_calls() {
        let provider = Arc::new(FakeProvider::with_fallback(
            Vec::new(),
            FakeResponse {
                text: Some("let me check".into()),
                tool_calls: Some(vec![
                    FakeToolCallPlan {
                        name: "read-file".into(),
                        args: serde_json::json!({ "path": "a" }),
                    },
                    FakeToolCallPlan {
                        name: "list-files".into(),
                        args: serde_json::json!({ "path": "." }),
                    },
                ]),
                ..FakeResponse::default()
            },
        ));
        let client = ProviderModelClient::new(provider);
        let route = ModelRoute {
            provider: "fake".to_owned(),
            model: "fake-standard".to_owned(),
            role: None,
        };
        let response = client
            .complete(
                &route,
                &ModelTurnRequest::from_prompt("x"),
                &CancelToken::new(),
            )
            .expect("completes");
        // Authoritative order: text first, then both calls.
        let kinds: Vec<&str> = response
            .blocks
            .iter()
            .map(|block| match block {
                AssistantContent::Text { .. } => "text",
                AssistantContent::ToolCall { .. } => "tool-call",
                AssistantContent::Continuation { .. } => "continuation",
            })
            .collect();
        assert_eq!(kinds, ["text", "tool-call", "tool-call"]);
        assert_eq!(response.text(), "let me check");
        assert_eq!(
            response
                .tool_calls()
                .iter()
                .map(|call| call.name.as_str())
                .collect::<Vec<_>>(),
            ["read-file", "list-files"]
        );
    }

    #[test]
    fn cancellation_surfaces_as_a_canceled_error() {
        let client = ProviderModelClient::new(provider_with_text("late"));
        let route = ModelRoute {
            provider: "fake".to_owned(),
            model: "fake-standard".to_owned(),
            role: None,
        };
        let signal = CancelToken::new();
        signal.cancel("test abort");
        let error = client
            .complete(&route, &ModelTurnRequest::from_prompt("x"), &signal)
            .expect_err("canceled");
        assert!(matches!(error, ModelClientError::Canceled(_)), "{error}");
    }

    #[test]
    fn the_adapter_name_matches_the_fake_provider() {
        let provider = provider_with_text("x");
        assert_eq!(provider.name(), AdapterKind::Fake);
    }

    #[test]
    fn provider_failures_carry_typed_attempt_summaries() {
        let provider = Arc::new(FakeProvider::with_fallback(
            Vec::new(),
            FakeResponse {
                error: Some(relay_providers::fake::FakeErrorPlan {
                    kind: ProviderErrorKind::RateLimit,
                    retryable: true,
                    message: "throttled".to_owned(),
                    fail_times: None,
                }),
                ..FakeResponse::default()
            },
        )) as Arc<dyn ModelProvider>;
        let client = ProviderModelClient::new(provider);
        let route = ModelRoute {
            provider: "claude-proxy".to_owned(),
            model: "claude-sonnet-5-5".to_owned(),
            role: Some("fast".to_owned()),
        };
        let error = client
            .complete(
                &route,
                &ModelTurnRequest::from_prompt("x"),
                &CancelToken::new(),
            )
            .expect_err("fails");
        let ModelClientError::Provider(failure) = &error else {
            panic!("expected a provider failure, got {error}");
        };
        assert_eq!(failure.attempts().len(), 1);
        let attempt = failure.terminal().unwrap();
        assert_eq!(attempt.kind, ProviderErrorKind::RateLimit);
        assert_eq!(attempt.route, route);
        assert_eq!(
            error.to_string(),
            "model call failed: claude-proxy/claude-sonnet-5-5: rate-limit (throttled)"
        );
    }

    #[test]
    fn routeless_failure_messages_display_without_a_route_prefix() {
        let failure = ModelCallFailure::from_message("no model client configured");
        assert_eq!(failure.to_string(), "no model client configured");
        assert_eq!(
            ModelCallFailure::new(Vec::new()).to_string(),
            "provider call failed"
        );
    }
}
