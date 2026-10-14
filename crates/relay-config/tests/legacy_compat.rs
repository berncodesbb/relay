//! Ports of the retired `test/unit/config/legacy-compat.test.ts` (removed
//! from the tree 2026-10-14): TODAY's behavior for
//! complete version-1 configurations that use only inline mode routes (no
//! `models` catalog, no role references, no `intent` key). The named-role
//! routing work must keep every one of these passing unchanged.
//!
//! The TypeScript file also drives a `ModelRouter` with fake providers; the
//! router-owned assertions are covered here through the same normalized
//! routing view the router consumes, and live with the runtime crate.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::{LEGACY_YAML, defaults, load_for, workspace_with_project_config};
use relay_config::load::{ConfigOverrides, LoadOptions, load_config};
use relay_config::normalized::{NormalizedRoute, normalize_routing};
use relay_config::schema::{ProviderName, RoutingValue, WORKFLOW_MODES};

fn parse_legacy(text: &str) -> relay_config::schema::RelayConfig {
    relay_config::load::parse_yaml_layer("legacy.yaml", text).expect("legacy layer should parse")
}

#[test]
fn parses_a_complete_version_one_inline_only_configuration_unchanged() {
    let parsed = parse_legacy(LEGACY_YAML);
    let mut keys: Vec<&String> = parsed.routing.keys().collect();
    keys.sort();
    let mut modes: Vec<&str> = WORKFLOW_MODES.to_vec();
    modes.sort();
    assert_eq!(keys, modes);
    assert_eq!(
        parsed.routing.get("verification"),
        Some(&RoutingValue::Inline(relay_config::schema::RouteConfig {
            provider: ProviderName::OpenAi,
            model: "gpt-5.2".to_string(),
            fallbacks: None,
            limits: None,
        }))
    );
    match parsed.routing.get("planning") {
        Some(RoutingValue::Inline(route)) => assert_eq!(
            route.fallbacks,
            Some(vec![relay_config::schema::InlineFallback {
                provider: ProviderName::Anthropic,
                model: "claude-sonnet-5-5".to_string(),
            }])
        ),
        other => panic!("planning should be an inline route: {other:?}"),
    }
}

#[test]
fn resolves_every_workflow_mode_through_inline_routes_without_a_models_catalog() {
    let config = parse_legacy(LEGACY_YAML);
    let routing = normalize_routing(&config);
    let model_of = |key: &str| {
        routing
            .purpose_chains
            .get(key)
            .and_then(|chain| chain.first())
            .map(|route| route.model.clone())
    };
    let provider_of = |key: &str| {
        routing
            .purpose_chains
            .get(key)
            .and_then(|chain| chain.first())
            .map(|route| route.provider)
    };
    assert_eq!(model_of("discovery"), Some("claude-sonnet-5-5".to_string()));
    assert_eq!(
        model_of("technical-design"),
        Some("claude-opus-5-5".to_string())
    );
    assert_eq!(provider_of("verification"), Some(ProviderName::OpenAi));
    assert_eq!(model_of("delivery"), Some("claude-haiku-4-5".to_string()));
    // No role names are fabricated for inline routes.
    assert_eq!(
        routing
            .purpose_chains
            .get("delivery")
            .unwrap()
            .first()
            .unwrap()
            .role,
        None
    );
}

#[test]
fn keeps_the_historical_intent_fallback_first_configured_route_insertion_order() {
    let config = parse_legacy(LEGACY_YAML);
    let routing = normalize_routing(&config);
    let fallback = routing.legacy_fallback.as_ref().unwrap();
    assert_eq!(fallback.len(), 1);
    assert_eq!(fallback[0].provider, ProviderName::Anthropic);
    assert_eq!(fallback[0].model, "claude-sonnet-5-5");
    assert_eq!(
        fallback,
        routing.purpose_chains.get("discovery").unwrap(),
        "the legacy fallback is the first configured route, in insertion order"
    );
}

#[test]
fn preserves_inline_fallback_order_in_the_normalized_chain() {
    let config = parse_legacy(LEGACY_YAML);
    let routing = normalize_routing(&config);
    let chain = routing.purpose_chains.get("planning").unwrap();
    let models: Vec<&str> = chain.iter().map(|route| route.model.as_str()).collect();
    assert_eq!(models, vec!["gpt-5-mini", "claude-sonnet-5-5"]);
    // Provider registration filtering is a provider-time concern and is
    // exercised by the runtime crate with its fake providers.
    assert!(chain.iter().all(|route| route.role.is_none()));
}

#[test]
fn builtin_defaults_still_route_every_mode_to_todays_providers_and_models() {
    assert_eq!(defaults().version, 1);
    let routing = normalize_routing(&defaults());
    let expected: Vec<(&str, ProviderName, &str)> = vec![
        ("discovery", ProviderName::Anthropic, "claude-sonnet-5-5"),
        (
            "specification",
            ProviderName::Anthropic,
            "claude-sonnet-5-5",
        ),
        (
            "technical-design",
            ProviderName::Anthropic,
            "claude-opus-5-5",
        ),
        ("planning", ProviderName::Anthropic, "claude-sonnet-5-5"),
        (
            "implementation",
            ProviderName::Anthropic,
            "claude-sonnet-5-5",
        ),
        ("verification", ProviderName::OpenAi, "gpt-5.2"),
        ("delivery", ProviderName::Anthropic, "claude-haiku-4-5"),
    ];
    for (mode, provider, model) in expected {
        let chain = routing.purpose_chains.get(mode).unwrap();
        let first: &NormalizedRoute = chain.first().unwrap();
        assert_eq!(first.provider, provider, "{mode}");
        assert_eq!(first.model, model, "{mode}");
    }
}

#[test]
fn applies_a_legacy_project_layer_over_defaults_without_edits() {
    let workspace = workspace_with_project_config(
        r#"
version: 1
budgets: {}
routing:
  discovery: { provider: openai, model: gpt-5-mini }
  specification: { provider: openai, model: gpt-5-mini }
  technical-design: { provider: openai, model: gpt-5.2 }
  planning: { provider: openai, model: gpt-5-mini }
  implementation: { provider: openai, model: gpt-5-mini }
  verification: { provider: openai, model: gpt-5.2 }
  delivery: { provider: openai, model: gpt-5-mini }
"#,
    );
    let loaded = load_for(workspace.path());
    assert!(
        loaded
            .layers
            .iter()
            .any(|layer| layer.source == relay_config::load::ConfigLayerSource::Project)
    );
    let routing = normalize_routing(&loaded.config);
    assert_eq!(
        routing
            .purpose_chains
            .get("discovery")
            .unwrap()
            .first()
            .unwrap()
            .model,
        "gpt-5-mini"
    );
    // Untouched layer values survive: budgets keep defaults.
    assert_eq!(
        loaded.config.budgets.max_turns,
        defaults().budgets.max_turns
    );
}

#[test]
fn force_provider_rewrites_providers_while_keeping_model_identifiers() {
    let workspace = workspace_with_project_config(LEGACY_YAML);
    let loaded = load_config(LoadOptions {
        workspace_root: workspace.path().to_path_buf(),
        user_config_path: None,
        overrides: Some(ConfigOverrides {
            autonomy: None,
            force_provider: Some(ProviderName::Fake),
        }),
    })
    .unwrap();
    let routing = normalize_routing(&loaded.config);
    for mode in WORKFLOW_MODES {
        let chain = routing.purpose_chains.get(mode).unwrap();
        assert!(
            chain
                .iter()
                .all(|route| route.provider == ProviderName::Fake),
            "{mode}"
        );
    }
    assert_eq!(
        routing
            .purpose_chains
            .get("verification")
            .unwrap()
            .first()
            .unwrap()
            .model,
        "gpt-5.2"
    );
}

#[test]
fn rejects_legacy_routing_keys_that_are_not_workflow_modes() {
    let error = relay_config::load::parse_yaml_layer(
        "legacy.yaml",
        r#"
version: 1
budgets: {}
routing:
  discovery: { provider: anthropic, model: claude-sonnet-5-5 }
  specification: { provider: anthropic, model: claude-sonnet-5-5 }
  technical-design: { provider: anthropic, model: claude-opus-5-5 }
  planning: { provider: anthropic, model: claude-sonnet-5-5 }
  implementation: { provider: anthropic, model: claude-sonnet-5-5 }
  verification: { provider: openai, model: gpt-5.2 }
  delivery: { provider: anthropic, model: claude-haiku-4-5 }
  frobnicate: { provider: openai, model: gpt-5.2 }
"#,
    )
    .unwrap_err();
    let message = error.to_string();
    assert!(
        message.contains("unknown modes") && message.contains("frobnicate"),
        "{message}"
    );
}

#[test]
fn still_requires_all_seven_workflow_modes() {
    let error = relay_config::load::parse_yaml_layer(
        "legacy.yaml",
        "version: 1\nbudgets: {}\nrouting:\n  discovery: { provider: anthropic, model: claude-sonnet-5-5 }\n",
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("missing modes"),
        "{}",
        error.to_string()
    );
}

#[test]
fn the_full_legacy_shape_remains_representable() {
    // Compile-time lock in the TypeScript test; here, the parsed legacy
    // configuration carries at least the seven mode routes.
    let config = parse_legacy(LEGACY_YAML);
    assert!(config.routing.len() >= WORKFLOW_MODES.len());
}
