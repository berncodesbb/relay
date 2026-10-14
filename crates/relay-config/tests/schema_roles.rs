//! Ports of the retired `test/unit/config/roles.test.ts` (removed from the
//! tree 2026-10-14): the raw `models` catalog, union routing-value schema
//! surface, and the named provider-connection section.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::{
    NAMED_CONNECTIONS_YAML, PLAN_DEFAULT_YAML, connection, model_role, model_role_with_fallbacks,
    role,
};
use relay_config::load::parse_yaml_layer;
use relay_config::schema::{
    AdapterKind, CapabilityOverrides, ConfigValidationError, ProviderConnection, RoutingValue,
    is_valid_role_name,
};
use relay_providers::contract::{
    FeatureSupport, InstructionStyle, OutputTokenParam, StructuredOutputMode,
};

fn parse_yaml_config(
    text: &str,
) -> Result<relay_config::schema::RelayConfig, ConfigValidationError> {
    parse_yaml_layer("roles.yaml", text).map_err(|error| match error {
        relay_config::load::ConfigError::Validation(validation) => validation,
        other => panic!("expected a validation error, got: {other}"),
    })
}

fn expect_rejection(text: &str) -> ConfigValidationError {
    match parse_yaml_config(text) {
        Ok(_) => panic!("expected parse_yaml_config to reject the configuration"),
        Err(error) => error,
    }
}

#[test]
fn parses_the_plan_default_block_verbatim() {
    let parsed = parse_yaml_config(PLAN_DEFAULT_YAML).unwrap();
    let models = parsed.models.as_ref().unwrap();
    assert_eq!(
        models.get("reasoning"),
        Some(&model_role("anthropic", "claude-sonnet-5-5"))
    );
    assert_eq!(
        models.get("review"),
        Some(&model_role_with_fallbacks(
            "openai",
            "gpt-5.2",
            vec!["reasoning"]
        ))
    );
    assert_eq!(parsed.routing.get("intent"), Some(&role("fast")));
    assert_eq!(parsed.routing.get("verification"), Some(&role("review")));
}

#[test]
fn accepts_user_defined_role_names() {
    let parsed = parse_yaml_config(
        r#"
version: 1
budgets: {}
models:
  product: { provider: openai, model: gpt-5.2 }
  ux-writing: { provider: anthropic, model: claude-haiku-4-5 }
  security-review: { provider: openai, model: gpt-5.2, fallbacks: [product] }
routing:
  discovery: product
  specification: product
  technical-design: product
  planning: product
  implementation: product
  verification: security-review
  delivery: ux-writing
"#,
    )
    .unwrap();
    let models = parsed.models.as_ref().unwrap();
    let mut names: Vec<&String> = models.keys().collect();
    names.sort();
    assert_eq!(
        names,
        vec!["product", "security-review", "ux-writing"],
        "user-defined role names are catalog keys, not an enum"
    );
    assert_eq!(
        models.get("security-review").unwrap().fallbacks,
        Some(vec!["product".to_string()])
    );
}

#[test]
fn parses_per_role_limits() {
    let parsed = parse_yaml_config(
        r#"
version: 1
budgets: {}
models:
  reasoning:
    provider: anthropic
    model: claude-sonnet-5-5
    limits: { maxOutputTokens: 8192, temperature: 0.3 }
routing:
  discovery: reasoning
  specification: reasoning
  technical-design: reasoning
  planning: reasoning
  implementation: reasoning
  verification: reasoning
  delivery: reasoning
"#,
    )
    .unwrap();
    let limits = parsed
        .models
        .as_ref()
        .unwrap()
        .get("reasoning")
        .unwrap()
        .limits
        .clone()
        .unwrap();
    assert_eq!(limits.max_output_tokens, Some(8192));
    assert_eq!(limits.temperature, Some(0.3));
}

#[test]
fn rejects_invalid_role_names_in_the_catalog_with_actionable_messages() {
    let bad_names: Vec<String> = vec![
        String::new(),
        "Reasoning".to_string(),
        format!("{}b", "a".repeat(64)),
        "a/b".to_string(),
        "a_b".to_string(),
    ];
    for name in &bad_names {
        let yaml_key = name.clone();
        let text = format!(
            r#"
version: 1
budgets: {{}}
models:
  "{yaml_key}": {{ provider: anthropic, model: claude-sonnet-5-5 }}
routing:
  discovery: {{ provider: anthropic, model: claude-sonnet-5-5 }}
  specification: {{ provider: anthropic, model: claude-sonnet-5-5 }}
  technical-design: {{ provider: anthropic, model: claude-opus-5-5 }}
  planning: {{ provider: anthropic, model: claude-sonnet-5-5 }}
  implementation: {{ provider: anthropic, model: claude-sonnet-5-5 }}
  verification: {{ provider: openai, model: gpt-5.2 }}
  delivery: {{ provider: anthropic, model: claude-haiku-4-5 }}
"#
        );
        let error = expect_rejection(&text);
        assert!(
            error
                .issues
                .iter()
                .any(|issue| issue.contains("kebab-case")),
            "role name {name:?} should produce the kebab-case diagnostic: {:?}",
            error.issues
        );
        assert!(!is_valid_role_name(name), "role name {name:?}");
    }
}

#[test]
fn exposes_one_canonical_role_name_rule() {
    assert!(is_valid_role_name("reasoning"));
    assert!(is_valid_role_name("ux-writing"));
    assert!(is_valid_role_name("a"));
    assert!(is_valid_role_name(&"a".repeat(64)));
    assert!(!is_valid_role_name(""));
    assert!(!is_valid_role_name("Reasoning"));
    assert!(!is_valid_role_name(&"a".repeat(65)));
    assert!(!is_valid_role_name("a/b"));
    assert!(!is_valid_role_name("a_b"));
}

#[test]
fn validates_fallback_entries_as_role_names_and_caps_their_count() {
    // A catalog role with an invalid fallback name (ported through the same
    // parse path the TypeScript test uses for modelRoleSchema).
    let error = expect_rejection(
        r#"
version: 1
budgets: {}
models:
  review:
    provider: openai
    model: gpt-5.2
    fallbacks: [Reasoning]
routing:
  discovery: reasoning
  specification: reasoning
  technical-design: architect
  planning: reasoning
  implementation: coding
  verification: review
  delivery: fast
"#,
    );
    assert!(
        error
            .issues
            .iter()
            .any(|issue| issue.contains("kebab-case"))
    );

    let error = expect_rejection(
        r#"
version: 1
budgets: {}
models:
  review:
    provider: openai
    model: gpt-5.2
    fallbacks: [a, b, c, d]
routing:
  discovery: reasoning
  specification: reasoning
  technical-design: architect
  planning: reasoning
  implementation: coding
  verification: review
  delivery: fast
"#,
    );
    assert!(
        error
            .issues
            .iter()
            .any(|issue| issue.contains("fallbacks") && issue.contains("<=3")),
        "fallback count cap diagnostic: {:?}",
        error.issues
    );
}

#[test]
fn parses_a_file_mixing_a_role_name_intent_with_inline_mode_objects() {
    let parsed = parse_yaml_config(
        r#"
version: 1
budgets: {}
models:
  fast: { provider: anthropic, model: claude-haiku-4-5 }
routing:
  intent: fast
  discovery: { provider: anthropic, model: claude-sonnet-5-5 }
  specification: { provider: anthropic, model: claude-sonnet-5-5 }
  technical-design: { provider: anthropic, model: claude-opus-5-5, fallbacks: [{ provider: openai, model: gpt-5.2 }] }
  planning: { provider: anthropic, model: claude-sonnet-5-5 }
  implementation: { provider: anthropic, model: claude-sonnet-5-5 }
  verification: { provider: openai, model: gpt-5.2 }
  delivery: { provider: anthropic, model: claude-haiku-4-5 }
"#,
    )
    .unwrap();
    assert_eq!(parsed.routing.get("intent"), Some(&role("fast")));
    assert_eq!(
        parsed.routing.get("discovery"),
        Some(&common::inline("anthropic", "claude-sonnet-5-5"))
    );
    assert_eq!(
        parsed.routing.get("technical-design"),
        Some(&common::inline_with_fallbacks(
            "anthropic",
            "claude-opus-5-5",
            vec![relay_config::schema::InlineFallback {
                provider: "openai".to_string(),
                model: "gpt-5.2".to_string(),
            }],
        ))
    );
}

#[test]
fn accepts_the_intent_key_alongside_the_seven_workflow_modes() {
    let parsed = parse_yaml_config(PLAN_DEFAULT_YAML).unwrap();
    let mut keys: Vec<&String> = parsed.routing.keys().collect();
    keys.sort();
    assert_eq!(
        keys,
        vec![
            "delivery",
            "discovery",
            "implementation",
            "intent",
            "planning",
            "specification",
            "technical-design",
            "verification",
        ]
    );
}

#[test]
fn rejects_routing_keys_that_are_neither_modes_nor_intent() {
    let error = expect_rejection(
        r#"
version: 1
budgets: {}
routing:
  intent: { provider: anthropic, model: claude-haiku-4-5 }
  frobnicate: { provider: openai, model: gpt-5.2 }
  discovery: { provider: anthropic, model: claude-sonnet-5-5 }
  specification: { provider: anthropic, model: claude-sonnet-5-5 }
  technical-design: { provider: anthropic, model: claude-opus-5-5 }
  planning: { provider: anthropic, model: claude-sonnet-5-5 }
  implementation: { provider: anthropic, model: claude-sonnet-5-5 }
  verification: { provider: openai, model: gpt-5.2 }
  delivery: { provider: anthropic, model: claude-haiku-4-5 }
"#,
    );
    assert!(
        error
            .issues
            .iter()
            .any(|issue| issue.contains("unknown modes") && issue.contains("frobnicate"))
    );
    assert_eq!(
        error.issues[0],
        "routing contains unknown modes: frobnicate (allowed keys: discovery, specification, \
         technical-design, planning, implementation, verification, delivery, and intent)"
    );
}

#[test]
fn still_requires_all_seven_workflow_modes_with_role_based_routing() {
    let error = expect_rejection(
        r#"
version: 1
budgets: {}
models:
  reasoning: { provider: anthropic, model: claude-sonnet-5-5 }
routing:
  discovery: reasoning
  specification: reasoning
  technical-design: reasoning
  planning: reasoning
  implementation: reasoning
  verification: reasoning
"#,
    );
    assert!(
        error
            .issues
            .iter()
            .any(|issue| issue.contains("missing modes") && issue.contains("delivery"))
    );
    assert_eq!(error.issues[0], "routing is missing modes: delivery");
}

#[test]
fn rejects_routing_values_that_are_neither_role_names_nor_inline_routes() {
    let error = expect_rejection(
        r#"
version: 1
budgets: {}
routing:
  discovery: 42
  specification: { provider: anthropic, model: claude-sonnet-5-5 }
  technical-design: { provider: anthropic, model: claude-opus-5-5 }
  planning: { provider: anthropic, model: claude-sonnet-5-5 }
  implementation: { provider: anthropic, model: claude-sonnet-5-5 }
  verification: { provider: openai, model: gpt-5.2 }
  delivery: { provider: anthropic, model: claude-haiku-4-5 }
"#,
    );
    assert!(error.issues.iter().any(|issue| issue.contains("role name")));
    assert!(
        error
            .issues
            .iter()
            .any(|issue| issue.contains("inline route"))
    );
    assert_eq!(
        error.issues[0],
        "routing.discovery: routing values must be a model-role name (a lowercase kebab-case \
         string referencing a models entry) or an inline route object with provider and model"
    );

    // routingValueSchema.parse('reasoning') / inline object: the union
    // accepts both forms; a role reference parses without a catalog entry
    // (cross-reference validation happens on the effective configuration).
    let ok = parse_yaml_config(
        r#"
version: 1
budgets: {}
routing:
  discovery: reasoning
  specification: { provider: anthropic, model: claude-sonnet-5-5 }
  technical-design: { provider: anthropic, model: claude-opus-5-5 }
  planning: { provider: anthropic, model: claude-sonnet-5-5 }
  implementation: { provider: anthropic, model: claude-sonnet-5-5 }
  verification: { provider: openai, model: gpt-5.2 }
  delivery: { provider: anthropic, model: claude-haiku-4-5 }
"#,
    );
    assert!(ok.is_ok());
    assert_eq!(
        ok.unwrap().routing.get("discovery"),
        Some(&RoutingValue::Role("reasoning".to_string()))
    );
}

#[test]
fn keeps_schema_version_at_one_for_the_backward_compatible_additions() {
    let parsed = parse_yaml_config(PLAN_DEFAULT_YAML).unwrap();
    assert_eq!(parsed.version, 1);
}

#[test]
fn parses_named_provider_connections_with_adapter_endpoint_and_preset() {
    let parsed = parse_yaml_config(NAMED_CONNECTIONS_YAML).unwrap();
    let glm = parsed.providers.get("glm").unwrap();
    assert_eq!(glm.adapter, AdapterKind::OpenaiChat);
    assert_eq!(glm.api_key_env.as_deref(), Some("GLM_API_KEY"));
    assert_eq!(
        glm.base_url.as_deref(),
        Some("https://api.z.ai/api/paas/v4")
    );
    assert_eq!(glm.preset.as_deref(), Some("glm"));
    // A second connection sharing the openai-chat adapter coexists; its
    // apiKeyEnv defaults per adapter and its preset is unset.
    let local = parsed.providers.get("local").unwrap();
    assert_eq!(local.adapter, AdapterKind::OpenaiChat);
    assert_eq!(local.api_key_env.as_deref(), Some("OPENAI_API_KEY"));
    assert_eq!(local.base_url.as_deref(), Some("http://localhost:8080/v1"));
    assert_eq!(local.preset, None);
    // Roles reference the connections by name; validity is deferred to
    // effective validation, but the names parse as strings.
    assert_eq!(
        parsed
            .models
            .as_ref()
            .unwrap()
            .get("reasoning")
            .unwrap()
            .provider,
        "glm"
    );
}

#[test]
fn infers_legacy_adapters_and_preserves_the_version_one_default_quirks() {
    // Absent section: both legacy live keys with the object-level defaults.
    let parsed = parse_yaml_config(PLAN_DEFAULT_YAML).unwrap();
    assert_eq!(
        parsed.providers.names().collect::<Vec<_>>(),
        ["openai", "anthropic"]
    );
    assert_eq!(
        parsed.providers.get("openai"),
        Some(&connection(AdapterKind::OpenaiChat, Some("OPENAI_API_KEY")))
    );
    assert_eq!(
        parsed.providers.get("anthropic"),
        Some(&connection(
            AdapterKind::Anthropic,
            Some("ANTHROPIC_API_KEY")
        ))
    );

    // Present section, present-but-empty anthropic object: the field-level
    // OPENAI_API_KEY quirk; absent openai key: the object-level default.
    let parsed = parse_yaml_config(
        r#"
version: 1
budgets: {}
providers: { anthropic: {} }
routing:
  discovery: { provider: fake, model: m }
  specification: { provider: fake, model: m }
  technical-design: { provider: fake, model: m }
  planning: { provider: fake, model: m }
  implementation: { provider: fake, model: m }
  verification: { provider: fake, model: m }
  delivery: { provider: fake, model: m }
"#,
    )
    .unwrap();
    assert_eq!(
        parsed.providers.get("anthropic"),
        Some(&connection(AdapterKind::Anthropic, Some("OPENAI_API_KEY")))
    );
    assert_eq!(
        parsed.providers.get("openai"),
        Some(&connection(AdapterKind::OpenaiChat, Some("OPENAI_API_KEY")))
    );

    // The fake key infers its adapter and needs no credential.
    let parsed = parse_yaml_config(
        r#"
version: 1
budgets: {}
providers: { fake: {} }
routing:
  discovery: { provider: fake, model: m }
  specification: { provider: fake, model: m }
  technical-design: { provider: fake, model: m }
  planning: { provider: fake, model: m }
  implementation: { provider: fake, model: m }
  verification: { provider: fake, model: m }
  delivery: { provider: fake, model: m }
"#,
    )
    .unwrap();
    assert_eq!(
        parsed.providers.get("fake"),
        Some(&connection(AdapterKind::Fake, None))
    );

    // A present-but-empty apiKeyEnv keeps the soft "Too small" quirk.
    let error = expect_rejection(
        r#"
version: 1
budgets: {}
providers: { openai: { apiKeyEnv: "" } }
routing:
  discovery: { provider: fake, model: m }
  specification: { provider: fake, model: m }
  technical-design: { provider: fake, model: m }
  planning: { provider: fake, model: m }
  implementation: { provider: fake, model: m }
  verification: { provider: fake, model: m }
  delivery: { provider: fake, model: m }
"#,
    );
    assert!(
        error.issues.iter().any(|issue| issue
            == "providers.openai.apiKeyEnv: Too small: expected string to have >=1 characters"),
        "{:?}",
        error.issues
    );
}

#[test]
fn rejects_unknown_adapter_kinds_listing_the_allowed_spellings() {
    for bad in ["openai", "bedrock", "chat"] {
        let error = expect_rejection(&format!(
            r#"
version: 1
budgets: {{}}
providers:
  glm:
    adapter: {bad}
    apiKeyEnv: GLM_API_KEY
routing:
  discovery: {{ provider: fake, model: m }}
  specification: {{ provider: fake, model: m }}
  technical-design: {{ provider: fake, model: m }}
  planning: {{ provider: fake, model: m }}
  implementation: {{ provider: fake, model: m }}
  verification: {{ provider: fake, model: m }}
  delivery: {{ provider: fake, model: m }}
"#
        ));
        assert!(
            error.issues.iter().any(|issue| issue
                == "providers.glm.adapter: Invalid option: expected one of \
                    \"openai-chat\"|\"anthropic\"|\"fake\""),
            "adapter {bad:?}: {:?}",
            error.issues
        );
    }

    let error = expect_rejection(
        r#"
version: 1
budgets: {}
providers:
  glm:
    adapter: 7
routing:
  discovery: { provider: fake, model: m }
  specification: { provider: fake, model: m }
  technical-design: { provider: fake, model: m }
  planning: { provider: fake, model: m }
  implementation: { provider: fake, model: m }
  verification: { provider: fake, model: m }
  delivery: { provider: fake, model: m }
"#,
    );
    assert!(
        error.issues.iter().any(|issue| issue
            == "providers.glm.adapter: Invalid input: expected string, received number"),
        "{:?}",
        error.issues
    );
}

#[test]
fn requires_an_adapter_for_new_connection_names() {
    let error = expect_rejection(
        r#"
version: 1
budgets: {}
providers:
  glm: { apiKeyEnv: GLM_API_KEY }
routing:
  discovery: { provider: fake, model: m }
  specification: { provider: fake, model: m }
  technical-design: { provider: fake, model: m }
  planning: { provider: fake, model: m }
  implementation: { provider: fake, model: m }
  verification: { provider: fake, model: m }
  delivery: { provider: fake, model: m }
"#,
    );
    assert_eq!(
        error.issues[0],
        "providers.glm.adapter: provider connections require an adapter \
         (\"openai-chat\"|\"anthropic\"|\"fake\"); only the legacy keys openai, anthropic, and \
         fake may omit it"
    );
}

#[test]
fn rejects_malformed_connection_names_with_a_connection_phrased_diagnostic() {
    let error = expect_rejection(
        r#"
version: 1
budgets: {}
providers:
  GLM: { adapter: openai-chat }
  local_gateway: { adapter: openai-chat }
routing:
  discovery: { provider: fake, model: m }
  specification: { provider: fake, model: m }
  technical-design: { provider: fake, model: m }
  planning: { provider: fake, model: m }
  implementation: { provider: fake, model: m }
  verification: { provider: fake, model: m }
  delivery: { provider: fake, model: m }
"#,
    );
    assert_eq!(
        error.issues[0],
        "providers.GLM: provider connection names are lowercase kebab-case, 1-64 characters, \
         e.g. openai, glm, local-gateway"
    );
    assert!(
        error
            .issues
            .iter()
            .any(|issue| issue.starts_with("providers.local_gateway:"))
    );
}

#[test]
fn rejects_literal_secrets_and_invalid_names_in_api_key_env() {
    for bad in [
        "sk-1234567890abcdef", // literal credential value
        "MY KEY",              // whitespace
        "OPENAI-KEY",          // dash
        "GLM=KEY",             // assignment
        "1GLM_KEY",            // leading digit
    ] {
        let error = expect_rejection(&format!(
            r#"
version: 1
budgets: {{}}
providers:
  glm:
    adapter: openai-chat
    apiKeyEnv: "{bad}"
routing:
  discovery: {{ provider: fake, model: m }}
  specification: {{ provider: fake, model: m }}
  technical-design: {{ provider: fake, model: m }}
  planning: {{ provider: fake, model: m }}
  implementation: {{ provider: fake, model: m }}
  verification: {{ provider: fake, model: m }}
  delivery: {{ provider: fake, model: m }}
"#
        ));
        assert!(
            error.issues.iter().any(|issue| issue
                == "providers.glm.apiKeyEnv: apiKeyEnv must name an environment variable \
                     (letters, digits, underscores; e.g. GLM_API_KEY), never hold a credential \
                     value"),
            "apiKeyEnv {bad:?}: {:?}",
            error.issues
        );
    }

    // The full valid shape still parses, including underscore-leading names.
    let parsed = parse_yaml_config(
        r#"
version: 1
budgets: {}
providers:
  glm:
    adapter: openai-chat
    apiKeyEnv: _GLM_API_KEY
routing:
  discovery: { provider: fake, model: m }
  specification: { provider: fake, model: m }
  technical-design: { provider: fake, model: m }
  planning: { provider: fake, model: m }
  implementation: { provider: fake, model: m }
  verification: { provider: fake, model: m }
  delivery: { provider: fake, model: m }
"#,
    )
    .unwrap();
    assert_eq!(
        parsed.providers.get("glm").unwrap().api_key_env.as_deref(),
        Some("_GLM_API_KEY")
    );
}

#[test]
fn parses_capability_overrides_with_the_contract_wire_spellings() {
    let parsed = parse_yaml_config(
        r#"
version: 1
budgets: {}
providers:
  glm:
    adapter: openai-chat
    preset: glm
    capabilities:
      instructions: system-instruction
      structuredOutput: json-schema
      streamedUsage: supported
      outputTokenParam: max-tokens
      imageInput: unsupported
routing:
  discovery: { provider: fake, model: m }
  specification: { provider: fake, model: m }
  technical-design: { provider: fake, model: m }
  planning: { provider: fake, model: m }
  implementation: { provider: fake, model: m }
  verification: { provider: fake, model: m }
  delivery: { provider: fake, model: m }
"#,
    )
    .unwrap();
    let capabilities = &parsed.providers.get("glm").unwrap().capabilities;
    assert_eq!(
        capabilities.instructions,
        Some(InstructionStyle::SystemInstruction)
    );
    assert_eq!(
        capabilities.structured_output,
        Some(StructuredOutputMode::JsonSchema)
    );
    assert_eq!(capabilities.streamed_usage, Some(FeatureSupport::Supported));
    assert_eq!(
        capabilities.output_token_param,
        Some(OutputTokenParam::MaxTokens)
    );
    assert_eq!(capabilities.image_input, Some(FeatureSupport::Unsupported));
    // Unset fields stay `None` (they inherit preset/adapter defaults at
    // resolution time), and unknown capability keys are ignored.
    assert_eq!(capabilities.tool_calls, None);
    assert!(!capabilities.is_empty());
}

#[test]
fn rejects_capability_values_that_are_not_wire_spellings() {
    let error = expect_rejection(
        r#"
version: 1
budgets: {}
providers:
  glm:
    adapter: openai-chat
    capabilities:
      instructions: system
      structuredOutput: JSON-Schema
      toolCalls: maybe
      outputTokenParam: max_tokens
routing:
  discovery: { provider: fake, model: m }
  specification: { provider: fake, model: m }
  technical-design: { provider: fake, model: m }
  planning: { provider: fake, model: m }
  implementation: { provider: fake, model: m }
  verification: { provider: fake, model: m }
  delivery: { provider: fake, model: m }
"#,
    );
    let expected = [
        "providers.glm.capabilities.instructions: Invalid option: expected one of \
         \"developer\"|\"system-instruction\"",
        "providers.glm.capabilities.structuredOutput: Invalid option: expected one of \
         \"json-schema\"|\"json-object\"|\"unsupported\"",
        "providers.glm.capabilities.toolCalls: Invalid option: expected one of \
         \"supported\"|\"unsupported\"",
        "providers.glm.capabilities.outputTokenParam: Invalid option: expected one of \
         \"max-tokens\"|\"max-completion-tokens\"|\"model-default\"",
    ];
    for issue in expected {
        assert!(
            error.issues.contains(&issue.to_string()),
            "{issue}: {:?}",
            error.issues
        );
    }

    let error = expect_rejection(
        r#"
version: 1
budgets: {}
providers:
  glm:
    adapter: openai-chat
    capabilities: { streamedUsage: true }
routing:
  discovery: { provider: fake, model: m }
  specification: { provider: fake, model: m }
  technical-design: { provider: fake, model: m }
  planning: { provider: fake, model: m }
  implementation: { provider: fake, model: m }
  verification: { provider: fake, model: m }
  delivery: { provider: fake, model: m }
"#,
    );
    assert!(
        error.issues.iter().any(|issue| issue
            == "providers.glm.capabilities.streamedUsage: Invalid input: expected string, \
                     received boolean"),
        "{:?}",
        error.issues
    );
}

#[test]
fn presets_parse_as_nonempty_strings_and_defer_validity_to_effective_validation() {
    // An unknown preset is a per-layer parse success: whether it exists (and
    // matches the adapter) is only knowable against the merged providers map,
    // so validation belongs to the effective pass.
    let parsed = parse_yaml_config(
        r#"
version: 1
budgets: {}
providers:
  glm:
    adapter: openai-chat
    preset: grok
routing:
  discovery: { provider: fake, model: m }
  specification: { provider: fake, model: m }
  technical-design: { provider: fake, model: m }
  planning: { provider: fake, model: m }
  implementation: { provider: fake, model: m }
  verification: { provider: fake, model: m }
  delivery: { provider: fake, model: m }
"#,
    );
    assert_eq!(
        parsed
            .unwrap()
            .providers
            .get("glm")
            .unwrap()
            .preset
            .as_deref(),
        Some("grok")
    );

    let error = expect_rejection(
        r#"
version: 1
budgets: {}
providers:
  glm:
    adapter: openai-chat
    preset: ""
routing:
  discovery: { provider: fake, model: m }
  specification: { provider: fake, model: m }
  technical-design: { provider: fake, model: m }
  planning: { provider: fake, model: m }
  implementation: { provider: fake, model: m }
  verification: { provider: fake, model: m }
  delivery: { provider: fake, model: m }
"#,
    );
    assert!(
        error.issues.iter().any(|issue| issue
            == "providers.glm.preset: Too small: expected string to have >=1 characters"),
        "{:?}",
        error.issues
    );
}

#[test]
fn rejects_duplicate_connection_keys_at_yaml_parse() {
    let message = match parse_yaml_layer(
        "roles.yaml",
        r#"
version: 1
budgets: {}
providers:
  glm: { adapter: openai-chat, apiKeyEnv: GLM_API_KEY }
  glm: { adapter: anthropic }
routing:
  discovery: { provider: fake, model: m }
  specification: { provider: fake, model: m }
  technical-design: { provider: fake, model: m }
  planning: { provider: fake, model: m }
  implementation: { provider: fake, model: m }
  verification: { provider: fake, model: m }
  delivery: { provider: fake, model: m }
"#,
    ) {
        Err(relay_config::load::ConfigError::Yaml { message, .. }) => message,
        other => panic!("expected a YAML duplicate-key error, got: {other:?}"),
    };
    assert!(
        message.contains("Map keys must be unique"),
        "duplicate-key diagnostic: {message}"
    );
}

#[test]
fn validates_provider_references_as_connection_names_at_layer_parse() {
    // Invalid name syntax fails at layer parse with its own path (soft, so
    // it survives the routing union without collapsing).
    let error = expect_rejection(
        r#"
version: 1
budgets: {}
routing:
  discovery: { provider: OpenAI, model: m1 }
  specification: { provider: anthropic, model: m1 }
  technical-design: { provider: anthropic, model: m1 }
  planning: { provider: anthropic, model: m1 }
  implementation: { provider: anthropic, model: m1 }
  verification: { provider: openai, model: m2 }
  delivery: { provider: anthropic, model: m1 }
"#,
    );
    assert_eq!(
        error.issues[0],
        "routing.discovery.provider: provider must be a provider connection name (lowercase \
         kebab-case, e.g. openai, glm)"
    );

    // A non-string provider stays a hard union issue.
    let error = expect_rejection(
        r#"
version: 1
budgets: {}
routing:
  discovery: { provider: 5, model: m1 }
  specification: { provider: anthropic, model: m1 }
  technical-design: { provider: anthropic, model: m1 }
  planning: { provider: anthropic, model: m1 }
  implementation: { provider: anthropic, model: m1 }
  verification: { provider: openai, model: m2 }
  delivery: { provider: anthropic, model: m1 }
"#,
    );
    assert_eq!(
        error.issues[0],
        "routing.discovery: routing values must be a model-role name (a lowercase kebab-case \
         string referencing a models entry) or an inline route object with provider and model"
    );

    // A syntactically valid but unknown reference (`openaai`) parses; the
    // existence check runs on the effective configuration.
    let parsed = parse_yaml_config(
        r#"
version: 1
budgets: {}
routing:
  discovery: { provider: openaai, model: m1 }
  specification: { provider: anthropic, model: m1 }
  technical-design: { provider: anthropic, model: m1 }
  planning: { provider: anthropic, model: m1 }
  implementation: { provider: anthropic, model: m1 }
  verification: { provider: openai, model: m2 }
  delivery: { provider: anthropic, model: m1 }
"#,
    )
    .unwrap();
    assert_eq!(
        parsed.routing.get("discovery"),
        Some(&common::inline("openaai", "m1"))
    );

    // Catalog roles validate the same way, with the models.<role> path.
    let error = expect_rejection(
        r#"
version: 1
budgets: {}
models:
  reasoning: { provider: no_dot, model: m1 }
routing:
  discovery: reasoning
  specification: reasoning
  technical-design: reasoning
  planning: reasoning
  implementation: reasoning
  verification: reasoning
  delivery: reasoning
"#,
    );
    assert_eq!(
        error.issues[0],
        "models.reasoning.provider: provider must be a provider connection name (lowercase \
         kebab-case, e.g. openai, glm)"
    );
}

#[test]
fn empty_capability_overrides_are_equivalent_to_absent_ones() {
    let parsed = parse_yaml_config(
        r#"
version: 1
budgets: {}
providers:
  glm:
    adapter: openai-chat
    capabilities: {}
routing:
  discovery: { provider: fake, model: m }
  specification: { provider: fake, model: m }
  technical-design: { provider: fake, model: m }
  planning: { provider: fake, model: m }
  implementation: { provider: fake, model: m }
  verification: { provider: fake, model: m }
  delivery: { provider: fake, model: m }
"#,
    )
    .unwrap();
    assert_eq!(
        parsed.providers.get("glm").unwrap().capabilities,
        CapabilityOverrides::default()
    );
    assert_eq!(
        *parsed.providers.get("glm").unwrap(),
        connection(AdapterKind::OpenaiChat, Some("OPENAI_API_KEY"))
    );
}

#[test]
fn surfaces_zod_shaped_diagnostics_for_schema_failures() {
    // A representative sweep of layer-schema diagnostics, matching the
    // TypeScript reference's Zod wording.
    struct Case {
        label: &'static str,
        yaml: &'static str,
        expected: &'static str,
    }
    let cases = [
        Case {
            label: "missing budgets",
            yaml: r#"
version: 1
routing:
  discovery: { provider: anthropic, model: m1 }
  specification: { provider: anthropic, model: m1 }
  technical-design: { provider: anthropic, model: m1 }
  planning: { provider: anthropic, model: m1 }
  implementation: { provider: anthropic, model: m1 }
  verification: { provider: openai, model: m2 }
  delivery: { provider: anthropic, model: m1 }
"#,
            expected: "budgets: Invalid input: expected object, received undefined",
        },
        Case {
            label: "empty routing table",
            yaml: "version: 1\nbudgets: {}\nrouting: {}\n",
            expected: "routing is missing modes: discovery, specification, technical-design, \
                       planning, implementation, verification, delivery",
        },
        Case {
            label: "wrong version",
            yaml: "version: 2\nbudgets: {}\nrouting:\n  discovery: r\n",
            expected: "version: Invalid input: expected 1",
        },
        Case {
            label: "budget range",
            yaml: "version: 1\nbudgets: { maxTurns: 0 }\nrouting:\n  discovery: r\n",
            expected: "budgets.maxTurns: Too small: expected number to be >=1",
        },
        Case {
            label: "budget float",
            yaml: "version: 1\nbudgets: { maxTurns: 1.5 }\nrouting:\n  discovery: r\n",
            expected: "budgets.maxTurns: Invalid input: expected int, received number",
        },
        Case {
            label: "budget string",
            yaml: "version: 1\nbudgets: { maxTurns: \"24\" }\nrouting:\n  discovery: r\n",
            expected: "budgets.maxTurns: Invalid input: expected number, received string",
        },
        Case {
            label: "empty model id",
            yaml: "version: 1\nbudgets: {}\nrouting:\n  discovery: { provider: anthropic, model: \"\" }\n",
            expected: "routing.discovery.model: Too small: expected string to have >=1 characters",
        },
        Case {
            label: "limits range",
            yaml: "version: 1\nbudgets: {}\nmodels:\n  reasoning:\n    provider: anthropic\n    model: m1\n    limits: { maxOutputTokens: 10, temperature: 5 }\nrouting:\n  discovery: reasoning\n",
            expected: "models.reasoning.limits.maxOutputTokens: Too small: expected number to be >=256",
        },
        Case {
            label: "limits temperature too big",
            yaml: "version: 1\nbudgets: {}\nmodels:\n  reasoning:\n    provider: anthropic\n    model: m1\n    limits: { maxOutputTokens: 10, temperature: 5 }\nrouting:\n  discovery: reasoning\n",
            expected: "models.reasoning.limits.temperature: Too big: expected number to be <=2",
        },
        Case {
            label: "provider name syntax",
            yaml: "version: 1\nbudgets: {}\nmodels:\n  reasoning:\n    provider: OpenAI\n    model: m1\nrouting:\n  discovery: reasoning\n",
            expected: "models.reasoning.provider: provider must be a provider connection name \
                       (lowercase kebab-case, e.g. openai, glm)",
        },
        Case {
            label: "autonomy",
            yaml: "version: 1\nbudgets: {}\nautonomy: sometimes\nrouting:\n  discovery: { provider: fake, model: m }\n",
            expected: "autonomy: autonomy must be off, next-checkpoint, workflow, or until:<mode>",
        },
        Case {
            label: "until with unknown mode",
            yaml: "version: 1\nbudgets: {}\nautonomy: until:frobnicate\nrouting:\n  discovery: { provider: fake, model: m }\n",
            expected: "autonomy: autonomy must be off, next-checkpoint, workflow, or until:<mode>",
        },
        Case {
            label: "baseUrl",
            yaml: "version: 1\nbudgets: {}\nproviders:\n  openai: { baseUrl: notaurl }\nrouting:\n  discovery: { provider: fake, model: m }\n",
            expected: "providers.openai.baseUrl: Invalid URL",
        },
        Case {
            label: "root not an object",
            yaml: "- a\n- b\n",
            expected: "(root): Invalid input: expected object, received array",
        },
    ];
    for case in cases {
        let error = expect_rejection(case.yaml);
        assert!(
            error.issues.iter().any(|issue| issue == case.expected),
            "{}: expected {:?} in {:?}",
            case.label,
            case.expected,
            error.issues
        );
    }
}

#[test]
fn formats_multi_issue_reports_like_the_typescript_reference() {
    let error = expect_rejection(concat!(
        "version: 3\n",
        "budgets: { maxTurns: 0 }\n",
        "routing:\n",
        "  discovery: { provider: fake, model: m }\n",
    ));
    assert_eq!(error.issues.len(), 2);
    assert_eq!(
        error.to_string(),
        "Invalid Relay configuration in roles.yaml:\n  - version: Invalid input: expected 1\n  \
         - budgets.maxTurns: Too small: expected number to be >=1"
    );
}

#[test]
fn provider_connections_expose_lookup_helpers() {
    let parsed = parse_yaml_config(NAMED_CONNECTIONS_YAML).unwrap();
    assert_eq!(
        parsed.providers.names().collect::<Vec<_>>(),
        ["glm", "local", "openai", "anthropic"]
    );
    assert!(parsed.providers.contains("glm"));
    assert!(!parsed.providers.contains("groq"));
    let pairs: Vec<(&str, AdapterKind)> = parsed
        .providers
        .iter()
        .map(|(name, connection): (&str, &ProviderConnection)| (name, connection.adapter))
        .collect();
    assert_eq!(
        pairs,
        vec![
            ("glm", AdapterKind::OpenaiChat),
            ("local", AdapterKind::OpenaiChat),
            ("openai", AdapterKind::OpenaiChat),
            ("anthropic", AdapterKind::Anthropic),
        ]
    );
}
