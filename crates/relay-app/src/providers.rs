//! Provider selection for runs.
//!
//! The application resolves a provider *name* to a provider adapter plus a
//! default model id, and builds the [`ModelRoute`] granted to model work.
//! Preview wiring: the route is provider-wide (config-driven purpose/role
//! routing arrives with the runtime owner's routing work); swapping the
//! provider never changes engine or policy semantics.

use std::sync::Arc;

use relay_domain::ModelRoute;
use relay_providers::anthropic::AnthropicProvider;
use relay_providers::anthropic::AnthropicProviderOptions;
use relay_providers::contract::ModelProvider;
use relay_providers::fake::FakeProvider;
use relay_providers::openai::OpenAIProvider;
use relay_providers::openai::OpenAIProviderOptions;

use crate::error::AppError;

/// The provider names a run accepts.
pub const PROVIDER_NAMES: [&str; 3] = ["fake", "openai", "anthropic"];

/// The default provider for the preview: the offline fake, so `relay run`
/// works without credentials. Live runs select `--provider openai` or
/// `--provider anthropic` (keys come from the environment, never state).
pub const DEFAULT_PROVIDER: &str = "fake";

/// Resolve a provider name to its adapter and default model id. API keys
/// are read from the environment inside the adapter, never stored.
pub fn provider_by_name(name: &str) -> Result<(Arc<dyn ModelProvider>, String), AppError> {
    match name {
        "fake" => Ok((Arc::new(FakeProvider::echo()), "fake-standard".to_string())),
        "openai" => {
            let provider = OpenAIProvider::new(OpenAIProviderOptions {
                api_key: None,
                base_url: None,
            })
            .map_err(|failure| AppError::Rejected(failure.to_string()))?;
            Ok((Arc::new(provider), "gpt-5.2".to_string()))
        }
        "anthropic" => {
            let provider = AnthropicProvider::new(AnthropicProviderOptions {
                api_key: None,
                base_url: None,
            })
            .map_err(|failure| AppError::Rejected(failure.to_string()))?;
            Ok((Arc::new(provider), "claude-sonnet-5-5".to_string()))
        }
        other => Err(AppError::Rejected(format!(
            "unknown provider \"{other}\" (expected one of: {})",
            PROVIDER_NAMES.join(", ")
        ))),
    }
}

/// The model route a run grants to model-attempt work.
pub fn route_for(provider_name: &str) -> Result<ModelRoute, AppError> {
    let (_, model) = provider_by_name(provider_name)?;
    Ok(ModelRoute {
        provider: provider_name.to_string(),
        model,
        role: None,
    })
}
