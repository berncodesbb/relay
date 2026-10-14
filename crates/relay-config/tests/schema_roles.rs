//! Ports of the retired `test/unit/config/roles.test.ts` (removed from the
//! tree 2026-10-14): the raw `models` catalog and
//! union routing-value schema surface.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::{PLAN_DEFAULT_YAML, model_role, model_role_with_fallbacks, role};
use relay_config::load::parse_yaml_layer;
use relay_config::schema::{ConfigValidationError, ProviderName, RoutingValue, is_valid_role_name};

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
        Some(&model_role(ProviderName::Anthropic, "claude-sonnet-5-5"))
    );
    assert_eq!(
        models.get("review"),
        Some(&model_role_with_fallbacks(
            ProviderName::OpenAi,
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
        Some(&common::inline(
            ProviderName::Anthropic,
            "claude-sonnet-5-5"
        ))
    );
    assert_eq!(
        parsed.routing.get("technical-design"),
        Some(&common::inline_with_fallbacks(
            ProviderName::Anthropic,
            "claude-opus-5-5",
            vec![relay_config::schema::InlineFallback {
                provider: ProviderName::OpenAi,
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
            label: "provider enum",
            yaml: "version: 1\nbudgets: {}\nmodels:\n  reasoning:\n    provider: openaai\n    model: m1\nrouting:\n  discovery: reasoning\n",
            expected: "models.reasoning.provider: Invalid option: expected one of \
                       \"openai\"|\"anthropic\"|\"fake\"",
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
