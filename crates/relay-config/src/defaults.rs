//! Default configuration: the explicit offline routing profile.
//!
//! Every routing purpose resolves to the deterministic in-process fake
//! connection, so a fresh checkout with no keys and no user or project
//! configuration runs entirely offline — the offline default is effective
//! configuration, not hidden CLI branching. The five well-known role names
//! (`reasoning`, `architect`, `coding`, `review`, `fast`) and the
//! routing-purpose mapping are unchanged, so user and project layers can
//! override individual roles predictably.
//!
//! The `providers` map also defines the `anthropic` connection with its
//! default credential-variable name, ready to be referenced by higher layers
//! without redefining infrastructure. (The implicit `openai` connection was
//! removed with the `openai-chat` adapter; configuration naming it fails
//! schema validation with migration guidance.)

use crate::schema::{
    AdapterKind, BudgetLimits, CapabilityOverrides, ModelRoleConfig, ProviderConnection, Providers,
    RelayConfig, RoutingValue, ToolSettings,
};
use indexmap::{IndexMap, indexmap};

/// The model id the offline fake connection serves in the default profile.
pub const DEFAULT_FAKE_MODEL: &str = "fake-standard";

/// The connection names the default profile defines, in order.
pub const DEFAULT_CONNECTION_NAMES: [&str; 2] = ["fake", "anthropic"];

fn connection(adapter: AdapterKind, api_key_env: Option<&str>) -> ProviderConnection {
    ProviderConnection {
        adapter,
        api_key_env: api_key_env.map(str::to_string),
        base_url: None,
        preset: None,
        capabilities: CapabilityOverrides::default(),
    }
}

/// The built-in default configuration (`DEFAULT_CONFIG` in the TypeScript
/// reference). Catalog, providers, and routing order are part of the
/// contract: they drive the `available roles`/`available connections`
/// diagnostics and the legacy first-route fallback.
pub fn default_config() -> RelayConfig {
    let offline = || ModelRoleConfig {
        provider: "fake".to_string(),
        model: DEFAULT_FAKE_MODEL.to_string(),
        fallbacks: None,
        limits: None,
    };
    let models: IndexMap<String, ModelRoleConfig> = indexmap! {
        "reasoning".to_string() => offline(),
        "architect".to_string() => offline(),
        "coding".to_string() => offline(),
        "review".to_string() => offline(),
        "fast".to_string() => offline(),
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
        providers: Providers(indexmap! {
            "fake".to_string() => connection(AdapterKind::Fake, None),
            "anthropic".to_string() => connection(AdapterKind::Anthropic, Some("ANTHROPIC_API_KEY")),
        }),
        models: Some(models),
        routing,
        budgets: BudgetLimits::default(),
        autonomy: "next-checkpoint".to_string(),
        tools: ToolSettings::default(),
    }
}
