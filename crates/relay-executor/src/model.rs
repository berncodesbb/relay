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
    CancelToken, ModelMessage, ModelProvider, ModelRequest, ProviderErrorKind,
};

/// Bound on any failure message carried in executor diagnostics; provider
/// failures are already bounded by `relay-providers`, this is defense in
/// depth for locally synthesized messages.
const MAX_FAILURE_MESSAGE_CHARS: usize = 300;

/// A model call as an executor sees it: prompts in, text and usage out.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelTurnRequest {
    /// System/developer instructions.
    pub system: Option<String>,
    /// The user turn (typically the attempt objective plus input
    /// summaries).
    pub prompt: String,
    /// Output token ceiling. When `None`, the routed implementation falls
    /// back to the planned route's limit.
    pub max_output_tokens: Option<u64>,
    /// Sampling temperature. When `None`, the routed implementation falls
    /// back to the planned route's limit. Not `Eq`: temperatures are
    /// floating point.
    pub temperature: Option<f64>,
}

impl ModelTurnRequest {
    /// A prompt-only request.
    pub fn from_prompt(prompt: impl Into<String>) -> Self {
        ModelTurnRequest {
            system: None,
            prompt: prompt.into(),
            max_output_tokens: None,
            temperature: None,
        }
    }
}

/// A completed model turn as an executor sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelTurnResponse {
    /// The assistant text.
    pub text: String,
    /// Input tokens consumed, summed across every attempted route.
    pub input_tokens: u64,
    /// Output tokens consumed, summed across every attempted route.
    pub output_tokens: u64,
    /// Every route that received a model call, in attempt order (the
    /// successful route last). Routes skipped before a call (for example an
    /// unavailable fallback connection) are not listed.
    pub routes_attempted: Vec<ModelRoute>,
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
/// `glm/glm-4.6: rate-limit (throttled); fake/fake-standard: server (down)`.
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
            messages: vec![ModelMessage::User {
                content: request.prompt.clone(),
            }],
            tools: None,
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
            text: turn.text,
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
    use relay_providers::fake::{FakeProvider, FakeResponse};

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
        assert_eq!(response.text, "hello from the model");
        assert_eq!(response.routes_attempted, vec![route.clone()]);
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
            provider: "glm".to_owned(),
            model: "glm-4.6".to_owned(),
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
            "model call failed: glm/glm-4.6: rate-limit (throttled)"
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
