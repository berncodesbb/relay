//! Ports of `test/unit/config/load.test.ts`: layering, effective-configuration
//! validation, forceProvider, and dotted lookups.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::{
    ROLE_ROUTING_YAML, USER_LAYER_YAML, config_with, defaults, inline, load_for, model_role,
    model_role_with_fallbacks, role, workspace_with_project_config, write_user_config,
};
use relay_config::load::{
    ConfigError, ConfigOverrides, EFFECTIVE_CONFIG_SOURCE, EffectiveConfigOrigins, LoadOptions,
    load_config, validate_effective_config,
};
use relay_config::normalized::normalize_routing;
use relay_config::schema::{InlineFallback, ProviderName, RelayConfig, RoutingValue};
use relay_config::{ConfigValidationError, MAX_FALLBACKS, inspect};
use std::path::Path;

fn catch_validation(
    action: impl FnOnce() -> Result<RelayConfig, ConfigError>,
) -> ConfigValidationError {
    match action() {
        Ok(_) => panic!("expected loadConfig to reject the configuration"),
        Err(ConfigError::Validation(error)) => error,
        Err(other) => panic!("expected a validation error, got: {other}"),
    }
}

fn load_workspace(
    workspace: &Path,
    user_config: Option<&Path>,
    overrides: Option<ConfigOverrides>,
) -> Result<RelayConfig, ConfigError> {
    load_config(LoadOptions {
        workspace_root: workspace.to_path_buf(),
        user_config_path: user_config.map(|path| path.to_path_buf()),
        overrides,
    })
    .map(|loaded| loaded.config)
}

#[test]
fn defaults_define_the_five_default_roles_and_route_every_mode_plus_intent() {
    let workspace = workspace_with_project_config(None);
    let loaded = load_for(workspace.path());
    assert_eq!(
        loaded
            .layers
            .iter()
            .map(|layer| layer.source)
            .collect::<Vec<_>>(),
        vec![relay_config::load::ConfigLayerSource::Defaults]
    );
    let models = loaded.config.models.as_ref().unwrap();
    let mut names: Vec<&String> = models.keys().collect();
    names.sort();
    assert_eq!(
        names,
        vec!["architect", "coding", "fast", "reasoning", "review"]
    );
    assert_eq!(loaded.config.routing.get("intent"), Some(&role("fast")));
    assert_eq!(
        loaded.config.routing.get("verification"),
        Some(&role("review"))
    );
}

#[test]
fn a_user_layer_replaces_one_complete_role_while_other_roles_and_intent_survive() {
    let workspace = workspace_with_project_config(None);
    let (user_dir, user_path) = write_user_config(USER_LAYER_YAML);
    let config = load_workspace(workspace.path(), Some(&user_path), None).unwrap();
    let models = config.models.as_ref().unwrap();
    assert_eq!(
        models.get("reasoning"),
        Some(&model_role(ProviderName::OpenAi, "gpt-5.2"))
    );
    assert_eq!(
        models.get("fast"),
        defaults().models.as_ref().unwrap().get("fast")
    );
    assert_eq!(
        models.get("review"),
        Some(&model_role_with_fallbacks(
            ProviderName::OpenAi,
            "gpt-5.2",
            vec!["reasoning"]
        ))
    );
    // The user layer did not define intent: the default entry survives.
    assert_eq!(config.routing.get("intent"), Some(&role("fast")));
    drop(user_dir);
}

#[test]
fn a_project_layer_overrides_the_user_layer_and_unrelated_user_roles_survive() {
    let workspace = workspace_with_project_config(Some(
        format!(
            "
version: 1
budgets: {{}}
models:
  reasoning: {{ provider: fake, model: echo-1 }}
{ROLE_ROUTING_YAML}"
        )
        .as_str(),
    ));
    let (user_dir, user_path) = write_user_config(
        r#"
version: 1
budgets: {}
models:
  reasoning: { provider: openai, model: gpt-5.2 }
  product: { provider: fake, model: demo-product }
routing:
  intent: product
  discovery: reasoning
  specification: reasoning
  technical-design: architect
  planning: reasoning
  implementation: coding
  verification: review
  delivery: fast"#,
    );
    let config = load_workspace(workspace.path(), Some(&user_path), None).unwrap();
    let models = config.models.as_ref().unwrap();
    assert_eq!(
        models.get("reasoning"),
        Some(&model_role(ProviderName::Fake, "echo-1"))
    );
    assert_eq!(
        models.get("product"),
        Some(&model_role(ProviderName::Fake, "demo-product"))
    );
    assert_eq!(
        models.get("fast"),
        defaults().models.as_ref().unwrap().get("fast")
    );
    // Routing merges by key: user's intent survives the project layer.
    assert_eq!(config.routing.get("intent"), Some(&role("product")));
    assert_eq!(config.routing.get("discovery"), Some(&role("reasoning")));
    drop(user_dir);
}

#[test]
fn routing_merges_by_key_mixing_role_references_and_inline_objects() {
    let workspace = workspace_with_project_config(
        r#"
version: 1
budgets: {}
routing:
  intent: reasoning
  discovery: reasoning
  specification: reasoning
  technical-design: architect
  planning: reasoning
  implementation: coding
  verification: { provider: fake, model: echo-v }
  delivery: fast
"#,
    );
    let config = load_workspace(workspace.path(), None, None).unwrap();
    assert_eq!(config.routing.get("intent"), Some(&role("reasoning")));
    assert_eq!(
        config.routing.get("verification"),
        Some(&inline(ProviderName::Fake, "echo-v"))
    );
    assert_eq!(config.routing.get("delivery"), Some(&role("fast")));
}

#[test]
fn a_legacy_layer_without_models_leaves_the_merged_catalog_untouched_but_defined() {
    let workspace = workspace_with_project_config(
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
"#,
    );
    let config = load_workspace(workspace.path(), None, None).unwrap();
    assert_eq!(config.models, defaults().models);
    assert_eq!(
        config.routing.get("verification"),
        Some(&inline(ProviderName::OpenAi, "gpt-5.2"))
    );
    // The default intent entry still routes through the role catalog.
    assert_eq!(config.routing.get("intent"), Some(&role("fast")));
}

#[test]
fn force_provider_rewrites_providers_across_the_catalog_and_inline_routes() {
    let workspace = workspace_with_project_config(
        r#"
version: 1
budgets: {}
models:
  reasoning:
    provider: openai
    model: gpt-5.2
    limits: { maxOutputTokens: 4096 }
routing:
  intent: fast
  discovery: reasoning
  specification: reasoning
  technical-design: architect
  planning: { provider: anthropic, model: claude-sonnet-5-5, fallbacks: [{ provider: openai, model: gpt-5.2 }] }
  implementation: coding
  verification: review
  delivery: fast
"#,
    );
    let config = load_workspace(
        workspace.path(),
        None,
        Some(ConfigOverrides {
            autonomy: None,
            force_provider: Some(ProviderName::Fake),
        }),
    )
    .unwrap();

    for (role_name, definition) in config.models.as_ref().unwrap() {
        assert_eq!(definition.provider, ProviderName::Fake, "role {role_name}");
    }
    assert_eq!(
        config.models.as_ref().unwrap().get("reasoning"),
        Some(&relay_config::schema::ModelRoleConfig {
            provider: ProviderName::Fake,
            model: "gpt-5.2".to_string(),
            fallbacks: None,
            limits: common::limits(Some(4096), None),
        })
    );
    // Role-name fallback references are untouched.
    assert_eq!(
        config
            .models
            .as_ref()
            .unwrap()
            .get("review")
            .unwrap()
            .fallbacks,
        Some(vec!["reasoning".to_string()])
    );
    assert_eq!(
        config.models.as_ref().unwrap().get("fast").unwrap().model,
        "claude-haiku-4-5"
    );
    // Role-reference routing values stay role references.
    assert_eq!(config.routing.get("intent"), Some(&role("fast")));
    assert_eq!(config.routing.get("discovery"), Some(&role("reasoning")));
    // Inline routes and inline fallback objects are rewritten, models kept.
    assert_eq!(
        config.routing.get("planning"),
        Some(&RoutingValue::Inline(relay_config::schema::RouteConfig {
            provider: ProviderName::Fake,
            model: "claude-sonnet-5-5".to_string(),
            fallbacks: Some(vec![InlineFallback {
                provider: ProviderName::Fake,
                model: "gpt-5.2".to_string(),
            }]),
            limits: None,
        }))
    );
}

#[test]
fn rejects_a_routing_reference_to_an_unknown_role_with_dotted_path_and_available_roles() {
    let workspace = workspace_with_project_config(Some(
        "
version: 1
budgets: {}
routing:
  discovery: ghost
  specification: reasoning
  technical-design: architect
  planning: reasoning
  implementation: coding
  verification: review
  delivery: fast"
            .to_string()
            .as_str(),
    ));
    let error = catch_validation(|| load_workspace(workspace.path(), None, None));
    assert_eq!(
        error.source,
        workspace
            .path()
            .join("relay.config.yaml")
            .display()
            .to_string()
    );
    assert!(
        error
            .issues
            .iter()
            .all(|issue| issue.contains("routing.discovery")),
        "{:?}",
        error.issues
    );
    assert!(error.issues[0].contains("\"ghost\""));
    assert!(
        error.issues[0].contains("available roles: reasoning, architect, coding, review, fast")
    );
    assert_eq!(
        error.issues[0],
        "routing.discovery: references unknown model role \"ghost\"; available roles: \
         reasoning, architect, coding, review, fast. Define a models.ghost entry or route \
         \"discovery\" with an inline provider/model object."
    );
}

#[test]
fn rejects_an_unknown_fallback_role_with_the_fallback_index() {
    let workspace = workspace_with_project_config(Some(
        format!(
            "
version: 1
budgets: {{}}
models:
  review:
    provider: openai
    model: gpt-5.2
    fallbacks: [ghost]
{ROLE_ROUTING_YAML}"
        )
        .as_str(),
    ));
    let error = catch_validation(|| load_workspace(workspace.path(), None, None));
    assert!(
        error.issues[0].contains("models.review.fallbacks[0]"),
        "{:?}",
        error.issues
    );
    assert!(error.issues[0].contains("\"ghost\""));
}

#[test]
fn rejects_self_referencing_roles() {
    let workspace = workspace_with_project_config(Some(
        format!(
            "
version: 1
budgets: {{}}
models:
  review:
    provider: openai
    model: gpt-5.2
    fallbacks: [review]
{ROLE_ROUTING_YAML}"
        )
        .as_str(),
    ));
    let error = catch_validation(|| load_workspace(workspace.path(), None, None));
    assert!(error.issues[0].contains("models.review.fallbacks[0]"));
    assert!(error.issues[0].contains("cannot fall back to itself"));
}

#[test]
fn rejects_cycles_in_the_role_fallback_graph() {
    let workspace = workspace_with_project_config(
        r#"
version: 1
budgets: {}
models:
  a: { provider: fake, model: echo-a, fallbacks: [b] }
  b: { provider: fake, model: echo-b, fallbacks: [a] }
routing:
  discovery: reasoning
  specification: reasoning
  technical-design: architect
  planning: reasoning
  implementation: coding
  verification: a
  delivery: fast
"#,
    );
    let error = catch_validation(|| load_workspace(workspace.path(), None, None));
    assert!(error.issues[0].contains("cycle"), "{:?}", error.issues);
    assert!(error.issues[0].contains("a -> b -> a"));
    assert_eq!(
        error.issues[0],
        "models.a.fallbacks: role fallback cycle detected (a -> b -> a); remove the cycle in \
         the models catalog"
    );
}

#[test]
fn rejects_expanded_chains_longer_than_one_primary_plus_three_fallbacks() {
    let workspace = workspace_with_project_config(
        r#"
version: 1
budgets: {}
models:
  a: { provider: fake, model: echo-a, fallbacks: [b, c] }
  b: { provider: fake, model: echo-b, fallbacks: [d, e] }
  c: { provider: fake, model: echo-c }
  d: { provider: fake, model: echo-d }
  e: { provider: fake, model: echo-e }
routing:
  discovery: a
  specification: reasoning
  technical-design: architect
  planning: reasoning
  implementation: coding
  verification: review
  delivery: fast
"#,
    );
    let error = catch_validation(|| load_workspace(workspace.path(), None, None));
    assert!(
        error.issues[0].contains("routing.discovery"),
        "{:?}",
        error.issues
    );
    assert!(error.issues[0].contains("\"a\" expands to 5"));
    assert!(error.issues[0].contains("a -> b -> c -> d -> e"));
    assert!(error.issues[0].contains("1 primary plus 3 fallbacks"));
    assert_eq!(MAX_FALLBACKS, 3);
}

#[test]
fn rejects_excessive_chains_on_roles_that_routing_never_references() {
    let workspace = workspace_with_project_config(Some(
        format!(
            "
version: 1
budgets: {{}}
models:
  deep: {{ provider: fake, model: echo-deep, fallbacks: [d1, d2] }}
  d1: {{ provider: fake, model: echo-d1, fallbacks: [d3, d4] }}
  d2: {{ provider: fake, model: echo-d2 }}
  d3: {{ provider: fake, model: echo-d3 }}
  d4: {{ provider: fake, model: echo-d4 }}
{ROLE_ROUTING_YAML}"
        )
        .as_str(),
    ));
    let error = catch_validation(|| load_workspace(workspace.path(), None, None));
    assert!(
        error.issues[0].contains("models.deep"),
        "{:?}",
        error.issues
    );
    assert!(error.issues[0].contains("\"deep\" expands to 5"));
    assert!(error.issues[0].contains("1 primary plus 3 fallbacks"));
    assert_eq!(
        error.issues[0],
        "models.deep: role \"deep\" expands to 5 routes (deep -> d1 -> d2 -> d3 -> d4); the \
         maximum is 1 primary plus 3 fallbacks"
    );
}

#[test]
fn accepts_a_valid_four_deep_expanded_chain() {
    let workspace = workspace_with_project_config(
        r#"
version: 1
budgets: {}
models:
  a: { provider: fake, model: echo-a, fallbacks: [b, c] }
  b: { provider: fake, model: echo-b, fallbacks: [d] }
  c: { provider: fake, model: echo-c }
  d: { provider: fake, model: echo-d }
routing:
  discovery: a
  specification: reasoning
  technical-design: architect
  planning: reasoning
  implementation: coding
  verification: review
  delivery: fast
"#,
    );
    let config = load_workspace(workspace.path(), None, None).unwrap();
    assert_eq!(config.routing.get("discovery"), Some(&role("a")));
}

#[test]
fn attributes_errors_to_the_effective_configuration_when_no_layer_file_is_known() {
    let config = config_with(
        vec![
            (
                "discovery",
                inline(ProviderName::Anthropic, "claude-sonnet-5-5"),
            ),
            (
                "specification",
                inline(ProviderName::Anthropic, "claude-sonnet-5-5"),
            ),
            (
                "technical-design",
                inline(ProviderName::Anthropic, "claude-opus-5-5"),
            ),
            (
                "planning",
                inline(ProviderName::Anthropic, "claude-sonnet-5-5"),
            ),
            (
                "implementation",
                inline(ProviderName::Anthropic, "claude-sonnet-5-5"),
            ),
            ("verification", role("review")),
            (
                "delivery",
                inline(ProviderName::Anthropic, "claude-haiku-4-5"),
            ),
        ],
        None,
    );
    let error = match validate_effective_config(&config, EFFECTIVE_CONFIG_SOURCE, None) {
        Ok(()) => panic!("expected validateEffectiveConfig to reject"),
        Err(error) => error,
    };
    assert_eq!(error.source, "the effective configuration");
    assert!(
        error.issues[0].contains("routing.verification: references unknown model role \"review\""),
        "{:?}",
        error.issues
    );
}

#[test]
fn prefers_the_originating_layer_file_when_origins_are_provided() {
    let config = config_with(
        vec![
            (
                "discovery",
                inline(ProviderName::Anthropic, "claude-sonnet-5-5"),
            ),
            (
                "specification",
                inline(ProviderName::Anthropic, "claude-sonnet-5-5"),
            ),
            (
                "technical-design",
                inline(ProviderName::Anthropic, "claude-opus-5-5"),
            ),
            (
                "planning",
                inline(ProviderName::Anthropic, "claude-sonnet-5-5"),
            ),
            (
                "implementation",
                inline(ProviderName::Anthropic, "claude-sonnet-5-5"),
            ),
            ("verification", role("ghost")),
            (
                "delivery",
                inline(ProviderName::Anthropic, "claude-haiku-4-5"),
            ),
        ],
        None,
    );
    let origins = EffectiveConfigOrigins {
        routing: [(
            "verification".to_string(),
            "/layers/project.yaml".to_string(),
        )]
        .into_iter()
        .collect(),
        models: Default::default(),
    };
    let error = match validate_effective_config(&config, EFFECTIVE_CONFIG_SOURCE, Some(&origins)) {
        Ok(()) => panic!("expected validateEffectiveConfig to reject"),
        Err(error) => error,
    };
    assert_eq!(error.source, "/layers/project.yaml");
}

#[test]
fn resolves_dotted_lookups_on_the_effective_object() {
    let workspace = workspace_with_project_config(None);
    let config = load_workspace(workspace.path(), None, None).unwrap();

    assert_eq!(
        inspect::lookup_dotted(&config, "models.fast.model"),
        Some(serde_yaml::Value::String("claude-haiku-4-5".to_string()))
    );
    assert_eq!(
        inspect::lookup_dotted(&config, "models.review.fallbacks.0"),
        Some(serde_yaml::Value::String("reasoning".to_string()))
    );
    assert_eq!(
        inspect::lookup_dotted(&config, "routing.intent"),
        Some(serde_yaml::Value::String("fast".to_string()))
    );
    assert_eq!(inspect::lookup_dotted(&config, "models.ghost"), None);
    assert_eq!(inspect::lookup_dotted(&config, "models.ghost.model"), None);
    assert_eq!(
        inspect::lookup_dotted(&config, "budgets.maxTurns"),
        Some(serde_yaml::Value::Number(24.into()))
    );
}

#[test]
fn the_effective_dump_renders_catalog_and_routing_coherently() {
    let workspace = workspace_with_project_config(None);
    let config = load_workspace(workspace.path(), None, None).unwrap();
    let value = inspect::effective_value(&config).unwrap();
    let mapping = value.as_mapping().unwrap();
    // Merged-configuration key order is part of the show/get contract.
    let keys: Vec<String> = mapping
        .keys()
        .map(|key| match key {
            serde_yaml::Value::String(name) => name.clone(),
            other => format!("{other:?}"),
        })
        .collect();
    assert_eq!(
        keys,
        vec![
            "version".to_string(),
            "providers".to_string(),
            "models".to_string(),
            "routing".to_string(),
            "budgets".to_string(),
            "autonomy".to_string(),
            "tools".to_string(),
        ]
    );
    let catalog = mapping
        .get(serde_yaml::Value::String("models".to_string()))
        .unwrap()
        .as_mapping()
        .unwrap();
    assert_eq!(catalog.len(), 5);
    let routing = mapping
        .get(serde_yaml::Value::String("routing".to_string()))
        .unwrap()
        .as_mapping()
        .unwrap();
    assert_eq!(routing.len(), 8);
    // A role reference renders as the bare role-name string, not a tagged
    // value, and omitted optional fields stay absent.
    assert_eq!(
        routing
            .get(serde_yaml::Value::String("intent".to_string()))
            .unwrap(),
        &serde_yaml::Value::String("fast".to_string())
    );
    let reasoning = catalog
        .get(serde_yaml::Value::String("reasoning".to_string()))
        .unwrap()
        .as_mapping()
        .unwrap();
    assert!(
        reasoning
            .get(serde_yaml::Value::String("fallbacks".to_string()))
            .is_none()
    );
}

#[test]
fn normalization_and_overrides_stay_consistent_after_loading() {
    // Sanity: loading plus normalization of the defaults yields the intent
    // chain, and an unknown --role lists the available roles.
    let workspace = workspace_with_project_config(None);
    let config = load_workspace(workspace.path(), None, None).unwrap();
    let routing = normalize_routing(&config);
    let error = relay_config::apply_invocation_overrides(
        &routing,
        "intent",
        None,
        &relay_config::InvocationOverrides {
            role: Some("ghost".to_string()),
            provider: None,
            model: None,
        },
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("No usable model route: unknown model role \"ghost\""),
        "{error}"
    );
    assert!(
        error
            .to_string()
            .contains("available roles: reasoning, architect, coding, review, fast"),
        "{error}"
    );
}
