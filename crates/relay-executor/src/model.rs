//! The model-client port: provider access behind executor-owned types.
//!
//! Executors and the orchestrator speak only to [`ModelClient`] with
//! [`ModelTurnRequest`]/[`ModelTurnResponse`]. The adapter
//! [`ProviderModelClient`] implements the port over `relay-providers`'
//! neutral contract internally, so no provider type ever appears in an
//! executor signature. Swapping providers (or fakes) changes the bytes a
//! model produces, never the contract or the policy outcomes built on it.

use std::sync::Arc;

use relay_domain::ModelRoute;
use relay_providers::collect_turn;
use relay_providers::contract::{CancelToken, ModelMessage, ModelProvider, ModelRequest};

/// A model call as an executor sees it: prompts in, text and usage out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelTurnRequest {
    /// System/developer instructions.
    pub system: Option<String>,
    /// The user turn (typically the attempt objective plus input
    /// summaries).
    pub prompt: String,
    /// Output token ceiling.
    pub max_output_tokens: Option<u64>,
}

impl ModelTurnRequest {
    /// A prompt-only request.
    pub fn from_prompt(prompt: impl Into<String>) -> Self {
        ModelTurnRequest {
            system: None,
            prompt: prompt.into(),
            max_output_tokens: None,
        }
    }
}

/// A completed model turn as an executor sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelTurnResponse {
    /// The assistant text.
    pub text: String,
    /// Input tokens consumed.
    pub input_tokens: u64,
    /// Output tokens consumed.
    pub output_tokens: u64,
}

/// Why a model call failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ModelClientError {
    /// The transport or provider failed.
    #[error("model call failed: {0}")]
    Provider(String),
    /// The call was canceled through its signal.
    #[error("model call canceled: {0}")]
    Canceled(String),
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

/// [`ModelClient`] over any `relay-providers` [`ModelProvider`]. The
/// provider's wire types stay inside this adapter.
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
            temperature: None,
            purpose: route.role.clone(),
            signal: Some(signal.clone()),
        };
        let turn = collect_turn(self.provider.stream(&provider_request))
            .map_err(|failure| ModelClientError::Provider(failure.to_string()))?;
        if signal.is_cancelled() {
            return Err(ModelClientError::Canceled(
                signal.reason().unwrap_or_else(|| "canceled".to_owned()),
            ));
        }
        Ok(ModelTurnResponse {
            text: turn.text,
            input_tokens: turn.usage.input_tokens.max(0) as u64,
            output_tokens: turn.usage.output_tokens.max(0) as u64,
        })
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use relay_providers::contract::{CancelToken, ProviderName};
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
        assert_eq!(provider.name(), ProviderName::Fake);
    }
}
