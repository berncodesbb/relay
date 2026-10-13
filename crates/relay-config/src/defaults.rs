//! Default configuration.
//!
//! The seven workflow modes and the `intent` purpose route through named model
//! roles defined in the `models` catalog. The defaults resolve to exactly the
//! providers and models the mode-based defaults always used: Anthropic Sonnet
//! for the reasoning-heavy stages, Anthropic Opus for technical design, OpenAI
//! with a reasoning fallback for verification, and the lightweight Anthropic
//! Haiku role for delivery and intent classification. The fake provider is a
//! valid target for demos and deterministic tests; it never contacts the
//! network.

use crate::schema::{
    BudgetLimits, ModelRoleConfig, ProviderName, ProviderSettings, Providers, RelayConfig,
    RoutingValue, ToolSettings,
};
use indexmap::{IndexMap, indexmap};

/// The built-in default configuration (`DEFAULT_CONFIG` in the TypeScript
/// reference). Catalog and routing order are part of the contract: they drive
/// the `available roles` diagnostics and the legacy first-route fallback.
pub fn default_config() -> RelayConfig {
    let models: IndexMap<String, ModelRoleConfig> = indexmap! {
        "reasoning".to_string() => ModelRoleConfig {
            provider: ProviderName::Anthropic,
            model: "claude-sonnet-5-5".to_string(),
            fallbacks: None,
            limits: None,
        },
        "architect".to_string() => ModelRoleConfig {
            provider: ProviderName::Anthropic,
            model: "claude-opus-5-5".to_string(),
            fallbacks: None,
            limits: None,
        },
        "coding".to_string() => ModelRoleConfig {
            provider: ProviderName::Anthropic,
            model: "claude-sonnet-5-5".to_string(),
            fallbacks: None,
            limits: None,
        },
        "review".to_string() => ModelRoleConfig {
            provider: ProviderName::OpenAi,
            model: "gpt-5.2".to_string(),
            fallbacks: Some(vec!["reasoning".to_string()]),
            limits: None,
        },
        "fast".to_string() => ModelRoleConfig {
            provider: ProviderName::Anthropic,
            model: "claude-haiku-4-5".to_string(),
            fallbacks: None,
            limits: None,
        },
    };
    let routing: IndexMap<String, RoutingValue> = indexmap! {
        "intent".to_string() => RoutingValue::Role("fast".to_string()),
        "discovery".to_string() => RoutingValue::Role("reasoning".to_string()),
        "specification".to_string() => RoutingValue::Role("reasoning".to_string()),
        "technical-design".to_string() => RoutingValue::Role("architect".to_string()),
        "planning".to_string() => RoutingValue::Role("reasoning".to_string()),
        "implementation".to_string() => RoutingValue::Role("coding".to_string()),
        "verification".to_string() => RoutingValue::Role("review".to_string()),
        "delivery".to_string() => RoutingValue::Role("fast".to_string()),
    };
    RelayConfig {
        version: crate::schema::CONFIG_SCHEMA_VERSION,
        providers: Providers {
            openai: ProviderSettings {
                api_key_env: "OPENAI_API_KEY".to_string(),
                base_url: None,
            },
            anthropic: ProviderSettings {
                api_key_env: "ANTHROPIC_API_KEY".to_string(),
                base_url: None,
            },
        },
        models: Some(models),
        routing,
        budgets: BudgetLimits::default(),
        autonomy: "next-checkpoint".to_string(),
        tools: ToolSettings::default(),
    }
}
