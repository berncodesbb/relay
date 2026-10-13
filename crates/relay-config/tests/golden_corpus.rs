//! Golden corpus verified against the TypeScript reference implementation
//! (`src/config/**` as of the model-role routing work). Each case records the
//! TypeScript behavior exactly: parsed-layer value equality for valid layers
//! and byte-identical diagnostic strings for invalid ones, plus
//! effective-validation scenarios on merged configurations.
//!
//! Regenerating this corpus requires the TypeScript oracle; the expectations
//! are checked in so the tests stay offline and deterministic.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use relay_config::load::{ConfigError, parse_yaml_layer, validate_effective_config};
use relay_config::schema::RelayConfig;
use serde_json::Value;

/// (name, YAML text, TypeScript outcome) for single-layer parsing.
const PARSE_CASES: &[(&str, &str, TsOutcome)] = &[
    (
        "plan_defaults",
        r#"version: 1
budgets: {}
models:
  reasoning: { provider: anthropic, model: claude-sonnet-5-5 }
  architect: { provider: anthropic, model: claude-opus-5-5 }
  coding: { provider: anthropic, model: claude-sonnet-5-5 }
  review: { provider: openai, model: gpt-5.2, fallbacks: [reasoning] }
  fast: { provider: anthropic, model: claude-haiku-4-5 }
routing:
  intent: fast
  discovery: reasoning
  specification: reasoning
  technical-design: architect
  planning: reasoning
  implementation: coding
  verification: review
  delivery: fast"#,
        TsOutcome::Ok(
            r#"{"version":1,"providers":{"openai":{"apiKeyEnv":"OPENAI_API_KEY"},"anthropic":{"apiKeyEnv":"ANTHROPIC_API_KEY"}},"routing":{"intent":"fast","discovery":"reasoning","specification":"reasoning","technical-design":"architect","planning":"reasoning","implementation":"coding","verification":"review","delivery":"fast"},"models":{"reasoning":{"provider":"anthropic","model":"claude-sonnet-5-5"},"architect":{"provider":"anthropic","model":"claude-opus-5-5"},"coding":{"provider":"anthropic","model":"claude-sonnet-5-5"},"review":{"provider":"openai","model":"gpt-5.2","fallbacks":["reasoning"]},"fast":{"provider":"anthropic","model":"claude-haiku-4-5"}},"budgets":{"maxTurns":24,"maxModelCalls":48,"maxToolCalls":96,"maxOutputTokens":400000,"wallClockMs":1800000,"maxRepeatedActions":3},"autonomy":"next-checkpoint","tools":{"commandTimeoutMs":120000,"maxSearchResults":200,"maxFileBytes":2000000}}"#,
        ),
    ),
    (
        "mixed_union",
        r#"version: 1
budgets: {}
models:
  fast: { provider: anthropic, model: claude-haiku-4-5 }
  limited: { provider: openai, model: gpt-5.2, limits: { maxOutputTokens: 8192, temperature: 0.3 } }
routing:
  intent: fast
  discovery: limited
  specification: reasoning-unknown
  technical-design: { provider: anthropic, model: claude-opus-5-5, fallbacks: [{ provider: openai, model: gpt-5.2 }] }
  planning: { provider: anthropic, model: claude-sonnet-5-5 }
  implementation: { provider: anthropic, model: claude-sonnet-5-5 }
  verification: { provider: openai, model: gpt-5.2 }
  delivery: fast"#,
        TsOutcome::Ok(
            r#"{"version":1,"providers":{"openai":{"apiKeyEnv":"OPENAI_API_KEY"},"anthropic":{"apiKeyEnv":"ANTHROPIC_API_KEY"}},"routing":{"intent":"fast","discovery":"limited","specification":"reasoning-unknown","technical-design":{"provider":"anthropic","model":"claude-opus-5-5","fallbacks":[{"provider":"openai","model":"gpt-5.2"}]},"planning":{"provider":"anthropic","model":"claude-sonnet-5-5"},"implementation":{"provider":"anthropic","model":"claude-sonnet-5-5"},"verification":{"provider":"openai","model":"gpt-5.2"},"delivery":"fast"},"models":{"fast":{"provider":"anthropic","model":"claude-haiku-4-5"},"limited":{"provider":"openai","model":"gpt-5.2","limits":{"maxOutputTokens":8192,"temperature":0.3}}},"budgets":{"maxTurns":24,"maxModelCalls":48,"maxToolCalls":96,"maxOutputTokens":400000,"wallClockMs":1800000,"maxRepeatedActions":3},"autonomy":"next-checkpoint","tools":{"commandTimeoutMs":120000,"maxSearchResults":200,"maxFileBytes":2000000}}"#,
        ),
    ),
    (
        "legacy_inline",
        r#"version: 1
budgets: {}
routing:
  discovery: { provider: anthropic, model: claude-sonnet-5-5 }
  specification: { provider: anthropic, model: claude-sonnet-5-5 }
  technical-design: { provider: anthropic, model: claude-opus-5-5 }
  planning: { provider: openai, model: gpt-5-mini, fallbacks: [{ provider: anthropic, model: claude-sonnet-5-5 }] }
  implementation: { provider: anthropic, model: claude-sonnet-5-5 }
  verification: { provider: openai, model: gpt-5.2 }
  delivery: { provider: anthropic, model: claude-haiku-4-5 }"#,
        TsOutcome::Ok(
            r#"{"version":1,"providers":{"openai":{"apiKeyEnv":"OPENAI_API_KEY"},"anthropic":{"apiKeyEnv":"ANTHROPIC_API_KEY"}},"routing":{"discovery":{"provider":"anthropic","model":"claude-sonnet-5-5"},"specification":{"provider":"anthropic","model":"claude-sonnet-5-5"},"technical-design":{"provider":"anthropic","model":"claude-opus-5-5"},"planning":{"provider":"openai","model":"gpt-5-mini","fallbacks":[{"provider":"anthropic","model":"claude-sonnet-5-5"}]},"implementation":{"provider":"anthropic","model":"claude-sonnet-5-5"},"verification":{"provider":"openai","model":"gpt-5.2"},"delivery":{"provider":"anthropic","model":"claude-haiku-4-5"}},"budgets":{"maxTurns":24,"maxModelCalls":48,"maxToolCalls":96,"maxOutputTokens":400000,"wallClockMs":1800000,"maxRepeatedActions":3},"autonomy":"next-checkpoint","tools":{"commandTimeoutMs":120000,"maxSearchResults":200,"maxFileBytes":2000000}}"#,
        ),
    ),
    (
        "providers_full",
        r#"version: 1
budgets: {}
providers:
  openai: { apiKeyEnv: MY_OPENAI_KEY, baseUrl: "https://gw.example/v1" }
  anthropic: { baseUrl: "https://anthro.example" }
routing:
  discovery: { provider: anthropic, model: m1 }
  specification: { provider: anthropic, model: m1 }
  technical-design: { provider: anthropic, model: m1 }
  planning: { provider: anthropic, model: m1 }
  implementation: { provider: anthropic, model: m1 }
  verification: { provider: openai, model: m2 }
  delivery: { provider: anthropic, model: m1 }"#,
        TsOutcome::Ok(
            r#"{"version":1,"providers":{"openai":{"apiKeyEnv":"MY_OPENAI_KEY","baseUrl":"https://gw.example/v1"},"anthropic":{"apiKeyEnv":"OPENAI_API_KEY","baseUrl":"https://anthro.example"}},"routing":{"discovery":{"provider":"anthropic","model":"m1"},"specification":{"provider":"anthropic","model":"m1"},"technical-design":{"provider":"anthropic","model":"m1"},"planning":{"provider":"anthropic","model":"m1"},"implementation":{"provider":"anthropic","model":"m1"},"verification":{"provider":"openai","model":"m2"},"delivery":{"provider":"anthropic","model":"m1"}},"budgets":{"maxTurns":24,"maxModelCalls":48,"maxToolCalls":96,"maxOutputTokens":400000,"wallClockMs":1800000,"maxRepeatedActions":3},"autonomy":"next-checkpoint","tools":{"commandTimeoutMs":120000,"maxSearchResults":200,"maxFileBytes":2000000}}"#,
        ),
    ),
    (
        "providers_partial_empty_anthropic",
        r#"version: 1
budgets: {}
providers: { anthropic: {} }
routing:
  discovery: { provider: anthropic, model: m1 }
  specification: { provider: anthropic, model: m1 }
  technical-design: { provider: anthropic, model: m1 }
  planning: { provider: anthropic, model: m1 }
  implementation: { provider: anthropic, model: m1 }
  verification: { provider: openai, model: m2 }
  delivery: { provider: anthropic, model: m1 }"#,
        TsOutcome::Ok(
            r#"{"version":1,"providers":{"openai":{"apiKeyEnv":"OPENAI_API_KEY"},"anthropic":{"apiKeyEnv":"OPENAI_API_KEY"}},"routing":{"discovery":{"provider":"anthropic","model":"m1"},"specification":{"provider":"anthropic","model":"m1"},"technical-design":{"provider":"anthropic","model":"m1"},"planning":{"provider":"anthropic","model":"m1"},"implementation":{"provider":"anthropic","model":"m1"},"verification":{"provider":"openai","model":"m2"},"delivery":{"provider":"anthropic","model":"m1"}},"budgets":{"maxTurns":24,"maxModelCalls":48,"maxToolCalls":96,"maxOutputTokens":400000,"wallClockMs":1800000,"maxRepeatedActions":3},"autonomy":"next-checkpoint","tools":{"commandTimeoutMs":120000,"maxSearchResults":200,"maxFileBytes":2000000}}"#,
        ),
    ),
    (
        "autonomy_until",
        r#"version: 1
budgets: {}
autonomy: until:implementation
routing:
  discovery: { provider: anthropic, model: m1 }
  specification: { provider: anthropic, model: m1 }
  technical-design: { provider: anthropic, model: m1 }
  planning: { provider: anthropic, model: m1 }
  implementation: { provider: anthropic, model: m1 }
  verification: { provider: openai, model: m2 }
  delivery: { provider: anthropic, model: m1 }"#,
        TsOutcome::Ok(
            r#"{"version":1,"providers":{"openai":{"apiKeyEnv":"OPENAI_API_KEY"},"anthropic":{"apiKeyEnv":"ANTHROPIC_API_KEY"}},"routing":{"discovery":{"provider":"anthropic","model":"m1"},"specification":{"provider":"anthropic","model":"m1"},"technical-design":{"provider":"anthropic","model":"m1"},"planning":{"provider":"anthropic","model":"m1"},"implementation":{"provider":"anthropic","model":"m1"},"verification":{"provider":"openai","model":"m2"},"delivery":{"provider":"anthropic","model":"m1"}},"budgets":{"maxTurns":24,"maxModelCalls":48,"maxToolCalls":96,"maxOutputTokens":400000,"wallClockMs":1800000,"maxRepeatedActions":3},"autonomy":"until:implementation","tools":{"commandTimeoutMs":120000,"maxSearchResults":200,"maxFileBytes":2000000}}"#,
        ),
    ),
    (
        "budgets_tools_partial",
        r#"version: 1
budgets: { maxTurns: 7, wallClockMs: 600000 }
tools: { maxSearchResults: 5 }
routing:
  discovery: { provider: anthropic, model: m1 }
  specification: { provider: anthropic, model: m1 }
  technical-design: { provider: anthropic, model: m1 }
  planning: { provider: anthropic, model: m1 }
  implementation: { provider: anthropic, model: m1 }
  verification: { provider: openai, model: m2 }
  delivery: { provider: anthropic, model: m1 }"#,
        TsOutcome::Ok(
            r#"{"version":1,"providers":{"openai":{"apiKeyEnv":"OPENAI_API_KEY"},"anthropic":{"apiKeyEnv":"ANTHROPIC_API_KEY"}},"routing":{"discovery":{"provider":"anthropic","model":"m1"},"specification":{"provider":"anthropic","model":"m1"},"technical-design":{"provider":"anthropic","model":"m1"},"planning":{"provider":"anthropic","model":"m1"},"implementation":{"provider":"anthropic","model":"m1"},"verification":{"provider":"openai","model":"m2"},"delivery":{"provider":"anthropic","model":"m1"}},"budgets":{"maxTurns":7,"maxModelCalls":48,"maxToolCalls":96,"maxOutputTokens":400000,"wallClockMs":600000,"maxRepeatedActions":3},"autonomy":"next-checkpoint","tools":{"commandTimeoutMs":120000,"maxSearchResults":5,"maxFileBytes":2000000}}"#,
        ),
    ),
    (
        "unknown_top_and_route_fields",
        r#"version: 1
budgets: {}
extraTop: 42
routing:
  discovery: { provider: anthropic, model: m1, extra: yes }
  specification: { provider: anthropic, model: m1 }
  technical-design: { provider: anthropic, model: m1 }
  planning: { provider: anthropic, model: m1 }
  implementation: { provider: anthropic, model: m1 }
  verification: { provider: openai, model: m2 }
  delivery: { provider: anthropic, model: m1 }"#,
        TsOutcome::Ok(
            r#"{"version":1,"providers":{"openai":{"apiKeyEnv":"OPENAI_API_KEY"},"anthropic":{"apiKeyEnv":"ANTHROPIC_API_KEY"}},"routing":{"discovery":{"provider":"anthropic","model":"m1"},"specification":{"provider":"anthropic","model":"m1"},"technical-design":{"provider":"anthropic","model":"m1"},"planning":{"provider":"anthropic","model":"m1"},"implementation":{"provider":"anthropic","model":"m1"},"verification":{"provider":"openai","model":"m2"},"delivery":{"provider":"anthropic","model":"m1"}},"budgets":{"maxTurns":24,"maxModelCalls":48,"maxToolCalls":96,"maxOutputTokens":400000,"wallClockMs":1800000,"maxRepeatedActions":3},"autonomy":"next-checkpoint","tools":{"commandTimeoutMs":120000,"maxSearchResults":200,"maxFileBytes":2000000}}"#,
        ),
    ),
    (
        "limits_on_inline_route",
        r#"version: 1
budgets: {}
routing:
  discovery: { provider: openai, model: gpt-5.2, fallbacks: [{ provider: anthropic, model: claude-sonnet-5-5 }], limits: { temperature: 0.1 } }
  specification: { provider: anthropic, model: m1 }
  technical-design: { provider: anthropic, model: m1 }
  planning: { provider: anthropic, model: m1 }
  implementation: { provider: anthropic, model: m1 }
  verification: { provider: openai, model: m2 }
  delivery: { provider: anthropic, model: m1 }"#,
        TsOutcome::Ok(
            r#"{"version":1,"providers":{"openai":{"apiKeyEnv":"OPENAI_API_KEY"},"anthropic":{"apiKeyEnv":"ANTHROPIC_API_KEY"}},"routing":{"discovery":{"provider":"openai","model":"gpt-5.2","fallbacks":[{"provider":"anthropic","model":"claude-sonnet-5-5"}],"limits":{"temperature":0.1}},"specification":{"provider":"anthropic","model":"m1"},"technical-design":{"provider":"anthropic","model":"m1"},"planning":{"provider":"anthropic","model":"m1"},"implementation":{"provider":"anthropic","model":"m1"},"verification":{"provider":"openai","model":"m2"},"delivery":{"provider":"anthropic","model":"m1"}},"budgets":{"maxTurns":24,"maxModelCalls":48,"maxToolCalls":96,"maxOutputTokens":400000,"wallClockMs":1800000,"maxRepeatedActions":3},"autonomy":"next-checkpoint","tools":{"commandTimeoutMs":120000,"maxSearchResults":200,"maxFileBytes":2000000}}"#,
        ),
    ),
    (
        "role_name_64",
        r#"version: 1
budgets: {}
models:
  aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa: { provider: anthropic, model: m1 }
routing:
  discovery: { provider: anthropic, model: m1 }
  specification: { provider: anthropic, model: m1 }
  technical-design: { provider: anthropic, model: m1 }
  planning: { provider: anthropic, model: m1 }
  implementation: { provider: anthropic, model: m1 }
  verification: { provider: openai, model: m2 }
  delivery: { provider: anthropic, model: m1 }"#,
        TsOutcome::Ok(
            r#"{"version":1,"providers":{"openai":{"apiKeyEnv":"OPENAI_API_KEY"},"anthropic":{"apiKeyEnv":"ANTHROPIC_API_KEY"}},"routing":{"discovery":{"provider":"anthropic","model":"m1"},"specification":{"provider":"anthropic","model":"m1"},"technical-design":{"provider":"anthropic","model":"m1"},"planning":{"provider":"anthropic","model":"m1"},"implementation":{"provider":"anthropic","model":"m1"},"verification":{"provider":"openai","model":"m2"},"delivery":{"provider":"anthropic","model":"m1"}},"models":{"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa":{"provider":"anthropic","model":"m1"}},"budgets":{"maxTurns":24,"maxModelCalls":48,"maxToolCalls":96,"maxOutputTokens":400000,"wallClockMs":1800000,"maxRepeatedActions":3},"autonomy":"next-checkpoint","tools":{"commandTimeoutMs":120000,"maxSearchResults":200,"maxFileBytes":2000000}}"#,
        ),
    ),
    (
        "bad_routing_number",
        r#"version: 1
budgets: {}
routing:
  discovery: 42
  specification: { provider: anthropic, model: m1 }
  technical-design: { provider: anthropic, model: m1 }
  planning: { provider: anthropic, model: m1 }
  implementation: { provider: anthropic, model: m1 }
  verification: { provider: openai, model: m2 }
  delivery: { provider: anthropic, model: m1 }"#,
        TsOutcome::Invalid(&[
            "routing.discovery: routing values must be a model-role name (a lowercase kebab-case string referencing a models entry) or an inline route object with provider and model",
        ]),
    ),
    (
        "bad_routing_bool",
        r#"version: 1
budgets: {}
routing:
  discovery: true
  specification: { provider: anthropic, model: m1 }
  technical-design: { provider: anthropic, model: m1 }
  planning: { provider: anthropic, model: m1 }
  implementation: { provider: anthropic, model: m1 }
  verification: { provider: openai, model: m2 }
  delivery: { provider: anthropic, model: m1 }"#,
        TsOutcome::Invalid(&[
            "routing.discovery: routing values must be a model-role name (a lowercase kebab-case string referencing a models entry) or an inline route object with provider and model",
        ]),
    ),
    (
        "bad_routing_null",
        r#"version: 1
budgets: {}
routing:
  discovery:
  specification: { provider: anthropic, model: m1 }
  technical-design: { provider: anthropic, model: m1 }
  planning: { provider: anthropic, model: m1 }
  implementation: { provider: anthropic, model: m1 }
  verification: { provider: openai, model: m2 }
  delivery: { provider: anthropic, model: m1 }"#,
        TsOutcome::Invalid(&[
            "routing.discovery: routing values must be a model-role name (a lowercase kebab-case string referencing a models entry) or an inline route object with provider and model",
        ]),
    ),
    (
        "bad_routing_list",
        r#"version: 1
budgets: {}
routing:
  discovery: [a]
  specification: { provider: anthropic, model: m1 }
  technical-design: { provider: anthropic, model: m1 }
  planning: { provider: anthropic, model: m1 }
  implementation: { provider: anthropic, model: m1 }
  verification: { provider: openai, model: m2 }
  delivery: { provider: anthropic, model: m1 }"#,
        TsOutcome::Invalid(&[
            "routing.discovery: routing values must be a model-role name (a lowercase kebab-case string referencing a models entry) or an inline route object with provider and model",
        ]),
    ),
    (
        "bad_routing_role_string",
        r#"version: 1
budgets: {}
routing:
  discovery: Not-A-Role
  specification: { provider: anthropic, model: m1 }
  technical-design: { provider: anthropic, model: m1 }
  planning: { provider: anthropic, model: m1 }
  implementation: { provider: anthropic, model: m1 }
  verification: { provider: openai, model: m2 }
  delivery: { provider: anthropic, model: m1 }"#,
        TsOutcome::Invalid(&[
            "routing.discovery: role names are lowercase kebab-case, 1-64 characters, e.g. reasoning, ux-writing",
        ]),
    ),
    (
        "bad_provider_typo",
        r#"version: 1
budgets: {}
routing:
  discovery: { provider: openaai, model: m1 }
  specification: { provider: anthropic, model: m1 }
  technical-design: { provider: anthropic, model: m1 }
  planning: { provider: anthropic, model: m1 }
  implementation: { provider: anthropic, model: m1 }
  verification: { provider: openai, model: m2 }
  delivery: { provider: anthropic, model: m1 }"#,
        TsOutcome::Invalid(&[
            "routing.discovery: routing values must be a model-role name (a lowercase kebab-case string referencing a models entry) or an inline route object with provider and model",
        ]),
    ),
    (
        "bad_model_empty",
        r#"version: 1
budgets: {}
routing:
  discovery: { provider: anthropic, model: "" }
  specification: { provider: anthropic, model: m1 }
  technical-design: { provider: anthropic, model: m1 }
  planning: { provider: anthropic, model: m1 }
  implementation: { provider: anthropic, model: m1 }
  verification: { provider: openai, model: m2 }
  delivery: { provider: anthropic, model: m1 }"#,
        TsOutcome::Invalid(&[
            "routing.discovery.model: Too small: expected string to have >=1 characters",
        ]),
    ),
    (
        "bad_model_number",
        r#"version: 1
budgets: {}
routing:
  discovery: { provider: anthropic, model: 5 }
  specification: { provider: anthropic, model: m1 }
  technical-design: { provider: anthropic, model: m1 }
  planning: { provider: anthropic, model: m1 }
  implementation: { provider: anthropic, model: m1 }
  verification: { provider: openai, model: m2 }
  delivery: { provider: anthropic, model: m1 }"#,
        TsOutcome::Invalid(&[
            "routing.discovery: routing values must be a model-role name (a lowercase kebab-case string referencing a models entry) or an inline route object with provider and model",
        ]),
    ),
    (
        "bad_fallbacks_four",
        r#"version: 1
budgets: {}
routing:
  discovery: { provider: anthropic, model: m1, fallbacks: [{ provider: openai, model: a }, { provider: openai, model: b }, { provider: openai, model: c }, { provider: openai, model: d }] }
  specification: { provider: anthropic, model: m1 }
  technical-design: { provider: anthropic, model: m1 }
  planning: { provider: anthropic, model: m1 }
  implementation: { provider: anthropic, model: m1 }
  verification: { provider: openai, model: m2 }
  delivery: { provider: anthropic, model: m1 }"#,
        TsOutcome::Invalid(&[
            "routing.discovery: routing values must be a model-role name (a lowercase kebab-case string referencing a models entry) or an inline route object with provider and model",
        ]),
    ),
    (
        "bad_fallback_element",
        r#"version: 1
budgets: {}
routing:
  discovery: { provider: anthropic, model: m1, fallbacks: [{ provider: openaai, model: a }] }
  specification: { provider: anthropic, model: m1 }
  technical-design: { provider: anthropic, model: m1 }
  planning: { provider: anthropic, model: m1 }
  implementation: { provider: anthropic, model: m1 }
  verification: { provider: openai, model: m2 }
  delivery: { provider: anthropic, model: m1 }"#,
        TsOutcome::Invalid(&[
            "routing.discovery: routing values must be a model-role name (a lowercase kebab-case string referencing a models entry) or an inline route object with provider and model",
        ]),
    ),
    (
        "soft_plus_hard_route",
        r#"version: 1
budgets: {}
routing:
  discovery: { provider: openaai, model: "", limits: { temperature: 9 } }
  specification: { provider: anthropic, model: m1 }
  technical-design: { provider: anthropic, model: m1 }
  planning: { provider: anthropic, model: m1 }
  implementation: { provider: anthropic, model: m1 }
  verification: { provider: openai, model: m2 }
  delivery: { provider: anthropic, model: m1 }"#,
        TsOutcome::Invalid(&[
            "routing.discovery: routing values must be a model-role name (a lowercase kebab-case string referencing a models entry) or an inline route object with provider and model",
        ]),
    ),
    (
        "soft_only_route",
        r#"version: 1
budgets: {}
routing:
  discovery: { provider: openai, model: "", limits: { temperature: 9, maxOutputTokens: 10 } }
  specification: { provider: anthropic, model: m1 }
  technical-design: { provider: anthropic, model: m1 }
  planning: { provider: anthropic, model: m1 }
  implementation: { provider: anthropic, model: m1 }
  verification: { provider: openai, model: m2 }
  delivery: { provider: anthropic, model: m1 }"#,
        TsOutcome::Invalid(&[
            "routing.discovery.model: Too small: expected string to have >=1 characters",
            "routing.discovery.limits.maxOutputTokens: Too small: expected number to be >=256",
            "routing.discovery.limits.temperature: Too big: expected number to be <=2",
        ]),
    ),
    (
        "limits_float_int_field",
        r#"version: 1
budgets: {}
models:
  r: { provider: openai, model: m, limits: { maxOutputTokens: 4096.5 } }
routing:
  discovery: r
  specification: { provider: anthropic, model: m1 }
  technical-design: { provider: anthropic, model: m1 }
  planning: { provider: anthropic, model: m1 }
  implementation: { provider: anthropic, model: m1 }
  verification: { provider: openai, model: m2 }
  delivery: { provider: anthropic, model: m1 }"#,
        TsOutcome::Invalid(&[
            "models.r.limits.maxOutputTokens: Invalid input: expected int, received number",
        ]),
    ),
    (
        "limits_null",
        r#"version: 1
budgets: {}
models:
  r: { provider: openai, model: m, limits: null }
routing:
  discovery: r
  specification: { provider: anthropic, model: m1 }
  technical-design: { provider: anthropic, model: m1 }
  planning: { provider: anthropic, model: m1 }
  implementation: { provider: anthropic, model: m1 }
  verification: { provider: openai, model: m2 }
  delivery: { provider: anthropic, model: m1 }"#,
        TsOutcome::Invalid(&["models.r.limits: Invalid input: expected object, received null"]),
    ),
    (
        "fallbacks_map",
        r#"version: 1
budgets: {}
models:
  r: { provider: openai, model: m, fallbacks: { a: b } }
routing:
  discovery: r
  specification: { provider: anthropic, model: m1 }
  technical-design: { provider: anthropic, model: m1 }
  planning: { provider: anthropic, model: m1 }
  implementation: { provider: anthropic, model: m1 }
  verification: { provider: openai, model: m2 }
  delivery: { provider: anthropic, model: m1 }"#,
        TsOutcome::Invalid(&["models.r.fallbacks: Invalid input: expected array, received object"]),
    ),
    (
        "role_bad_name_upper",
        r#"version: 1
budgets: {}
models:
  Reasoning: { provider: anthropic, model: m1 }
routing:
  discovery: { provider: anthropic, model: m1 }
  specification: { provider: anthropic, model: m1 }
  technical-design: { provider: anthropic, model: m1 }
  planning: { provider: anthropic, model: m1 }
  implementation: { provider: anthropic, model: m1 }
  verification: { provider: openai, model: m2 }
  delivery: { provider: anthropic, model: m1 }"#,
        TsOutcome::Invalid(&[
            "models.Reasoning: role names are lowercase kebab-case, 1-64 characters, e.g. reasoning, ux-writing",
        ]),
    ),
    (
        "role_bad_name_empty",
        r#"version: 1
budgets: {}
models:
  "": { provider: anthropic, model: m1 }
routing:
  discovery: { provider: anthropic, model: m1 }
  specification: { provider: anthropic, model: m1 }
  technical-design: { provider: anthropic, model: m1 }
  planning: { provider: anthropic, model: m1 }
  implementation: { provider: anthropic, model: m1 }
  verification: { provider: openai, model: m2 }
  delivery: { provider: anthropic, model: m1 }"#,
        TsOutcome::Invalid(&[
            "models.: role names are lowercase kebab-case, 1-64 characters, e.g. reasoning, ux-writing",
        ]),
    ),
    (
        "role_bad_name_numeric",
        r#"version: 1
budgets: {}
models:
  1: { provider: anthropic, model: m1 }
routing:
  discovery: { provider: anthropic, model: m1 }
  specification: { provider: anthropic, model: m1 }
  technical-design: { provider: anthropic, model: m1 }
  planning: { provider: anthropic, model: m1 }
  implementation: { provider: anthropic, model: m1 }
  verification: { provider: openai, model: m2 }
  delivery: { provider: anthropic, model: m1 }"#,
        TsOutcome::Invalid(&[
            "models.1: role names are lowercase kebab-case, 1-64 characters, e.g. reasoning, ux-writing",
        ]),
    ),
    (
        "role_bad_name_65",
        r#"version: 1
budgets: {}
models:
  "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaab": { provider: anthropic, model: m1 }
routing:
  discovery: { provider: anthropic, model: m1 }
  specification: { provider: anthropic, model: m1 }
  technical-design: { provider: anthropic, model: m1 }
  planning: { provider: anthropic, model: m1 }
  implementation: { provider: anthropic, model: m1 }
  verification: { provider: openai, model: m2 }
  delivery: { provider: anthropic, model: m1 }"#,
        TsOutcome::Invalid(&[
            "models.aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaab: role names are lowercase kebab-case, 1-64 characters, e.g. reasoning, ux-writing",
        ]),
    ),
    (
        "role_fallback_bad_name",
        r#"version: 1
budgets: {}
models:
  r: { provider: openai, model: m, fallbacks: [Bad] }
routing:
  discovery: r
  specification: { provider: anthropic, model: m1 }
  technical-design: { provider: anthropic, model: m1 }
  planning: { provider: anthropic, model: m1 }
  implementation: { provider: anthropic, model: m1 }
  verification: { provider: openai, model: m2 }
  delivery: { provider: anthropic, model: m1 }"#,
        TsOutcome::Invalid(&[
            "models.r.fallbacks.0: role names are lowercase kebab-case, 1-64 characters, e.g. reasoning, ux-writing",
        ]),
    ),
    (
        "role_fallback_number",
        r#"version: 1
budgets: {}
models:
  r: { provider: openai, model: m, fallbacks: [3] }
routing:
  discovery: r
  specification: { provider: anthropic, model: m1 }
  technical-design: { provider: anthropic, model: m1 }
  planning: { provider: anthropic, model: m1 }
  implementation: { provider: anthropic, model: m1 }
  verification: { provider: openai, model: m2 }
  delivery: { provider: anthropic, model: m1 }"#,
        TsOutcome::Invalid(&[
            "models.r.fallbacks.0: Invalid input: expected string, received number",
        ]),
    ),
    (
        "models_null",
        r#"version: 1
budgets: {}
models: null
routing:
  discovery: { provider: anthropic, model: m1 }
  specification: { provider: anthropic, model: m1 }
  technical-design: { provider: anthropic, model: m1 }
  planning: { provider: anthropic, model: m1 }
  implementation: { provider: anthropic, model: m1 }
  verification: { provider: openai, model: m2 }
  delivery: { provider: anthropic, model: m1 }"#,
        TsOutcome::Invalid(&["models: Invalid input: expected record, received null"]),
    ),
    (
        "models_list",
        r#"version: 1
budgets: {}
models: [a]
routing:
  discovery: { provider: anthropic, model: m1 }
  specification: { provider: anthropic, model: m1 }
  technical-design: { provider: anthropic, model: m1 }
  planning: { provider: anthropic, model: m1 }
  implementation: { provider: anthropic, model: m1 }
  verification: { provider: openai, model: m2 }
  delivery: { provider: anthropic, model: m1 }"#,
        TsOutcome::Invalid(&["models: Invalid input: expected record, received array"]),
    ),
    (
        "budgets_missing",
        r#"version: 1
routing:
  discovery: { provider: anthropic, model: m1 }
  specification: { provider: anthropic, model: m1 }
  technical-design: { provider: anthropic, model: m1 }
  planning: { provider: anthropic, model: m1 }
  implementation: { provider: anthropic, model: m1 }
  verification: { provider: openai, model: m2 }
  delivery: { provider: anthropic, model: m1 }"#,
        TsOutcome::Invalid(&["budgets: Invalid input: expected object, received undefined"]),
    ),
    (
        "budgets_null",
        r#"version: 1
budgets: null
routing:
  discovery: { provider: anthropic, model: m1 }
  specification: { provider: anthropic, model: m1 }
  technical-design: { provider: anthropic, model: m1 }
  planning: { provider: anthropic, model: m1 }
  implementation: { provider: anthropic, model: m1 }
  verification: { provider: openai, model: m2 }
  delivery: { provider: anthropic, model: m1 }"#,
        TsOutcome::Invalid(&["budgets: Invalid input: expected object, received null"]),
    ),
    (
        "budgets_range",
        r#"version: 1
budgets: { maxTurns: 0, maxModelCalls: 999, wallClockMs: 5000 }
routing:
  discovery: { provider: anthropic, model: m1 }
  specification: { provider: anthropic, model: m1 }
  technical-design: { provider: anthropic, model: m1 }
  planning: { provider: anthropic, model: m1 }
  implementation: { provider: anthropic, model: m1 }
  verification: { provider: openai, model: m2 }
  delivery: { provider: anthropic, model: m1 }"#,
        TsOutcome::Invalid(&[
            "budgets.maxTurns: Too small: expected number to be >=1",
            "budgets.wallClockMs: Too small: expected number to be >=10000",
        ]),
    ),
    (
        "budgets_float",
        r#"version: 1
budgets: { maxTurns: 1.5, maxOutputTokens: 24.0, maxRepeatedActions: 2.5 }
routing:
  discovery: { provider: anthropic, model: m1 }
  specification: { provider: anthropic, model: m1 }
  technical-design: { provider: anthropic, model: m1 }
  planning: { provider: anthropic, model: m1 }
  implementation: { provider: anthropic, model: m1 }
  verification: { provider: openai, model: m2 }
  delivery: { provider: anthropic, model: m1 }"#,
        TsOutcome::Invalid(&[
            "budgets.maxTurns: Invalid input: expected int, received number",
            "budgets.maxOutputTokens: Too small: expected number to be >=1024",
            "budgets.maxRepeatedActions: Invalid input: expected int, received number",
        ]),
    ),
    (
        "budgets_string",
        r#"version: 1
budgets: { maxTurns: "24" }
routing:
  discovery: { provider: anthropic, model: m1 }
  specification: { provider: anthropic, model: m1 }
  technical-design: { provider: anthropic, model: m1 }
  planning: { provider: anthropic, model: m1 }
  implementation: { provider: anthropic, model: m1 }
  verification: { provider: openai, model: m2 }
  delivery: { provider: anthropic, model: m1 }"#,
        TsOutcome::Invalid(&["budgets.maxTurns: Invalid input: expected number, received string"]),
    ),
    (
        "tools_bad",
        r#"version: 1
budgets: {}
tools: { commandTimeoutMs: 1, maxFileBytes: 1.5 }
routing:
  discovery: { provider: anthropic, model: m1 }
  specification: { provider: anthropic, model: m1 }
  technical-design: { provider: anthropic, model: m1 }
  planning: { provider: anthropic, model: m1 }
  implementation: { provider: anthropic, model: m1 }
  verification: { provider: openai, model: m2 }
  delivery: { provider: anthropic, model: m1 }"#,
        TsOutcome::Invalid(&[
            "tools.commandTimeoutMs: Too small: expected number to be >=1000",
            "tools.maxFileBytes: Invalid input: expected int, received number",
        ]),
    ),
    (
        "autonomy_bad",
        r#"version: 1
budgets: {}
autonomy: sometimes
routing:
  discovery: { provider: anthropic, model: m1 }
  specification: { provider: anthropic, model: m1 }
  technical-design: { provider: anthropic, model: m1 }
  planning: { provider: anthropic, model: m1 }
  implementation: { provider: anthropic, model: m1 }
  verification: { provider: openai, model: m2 }
  delivery: { provider: anthropic, model: m1 }"#,
        TsOutcome::Invalid(&[
            "autonomy: autonomy must be off, next-checkpoint, workflow, or until:<mode>",
        ]),
    ),
    (
        "autonomy_until_bad",
        r#"version: 1
budgets: {}
autonomy: until:frobnicate
routing:
  discovery: { provider: anthropic, model: m1 }
  specification: { provider: anthropic, model: m1 }
  technical-design: { provider: anthropic, model: m1 }
  planning: { provider: anthropic, model: m1 }
  implementation: { provider: anthropic, model: m1 }
  verification: { provider: openai, model: m2 }
  delivery: { provider: anthropic, model: m1 }"#,
        TsOutcome::Invalid(&[
            "autonomy: autonomy must be off, next-checkpoint, workflow, or until:<mode>",
        ]),
    ),
    (
        "version_two",
        r#"version: 2
budgets: {}
routing:
  discovery: { provider: anthropic, model: m1 }
  specification: { provider: anthropic, model: m1 }
  technical-design: { provider: anthropic, model: m1 }
  planning: { provider: anthropic, model: m1 }
  implementation: { provider: anthropic, model: m1 }
  verification: { provider: openai, model: m2 }
  delivery: { provider: anthropic, model: m1 }"#,
        TsOutcome::Invalid(&["version: Invalid input: expected 1"]),
    ),
    (
        "version_string",
        r#"version: "1"
budgets: {}
routing:
  discovery: { provider: anthropic, model: m1 }
  specification: { provider: anthropic, model: m1 }
  technical-design: { provider: anthropic, model: m1 }
  planning: { provider: anthropic, model: m1 }
  implementation: { provider: anthropic, model: m1 }
  verification: { provider: openai, model: m2 }
  delivery: { provider: anthropic, model: m1 }"#,
        TsOutcome::Invalid(&["version: Invalid input: expected 1"]),
    ),
    (
        "version_float",
        r#"version: 1.0
budgets: {}
routing:
  discovery: { provider: anthropic, model: m1 }
  specification: { provider: anthropic, model: m1 }
  technical-design: { provider: anthropic, model: m1 }
  planning: { provider: anthropic, model: m1 }
  implementation: { provider: anthropic, model: m1 }
  verification: { provider: openai, model: m2 }
  delivery: { provider: anthropic, model: m1 }"#,
        TsOutcome::Ok(
            r#"{"version":1,"providers":{"openai":{"apiKeyEnv":"OPENAI_API_KEY"},"anthropic":{"apiKeyEnv":"ANTHROPIC_API_KEY"}},"routing":{"discovery":{"provider":"anthropic","model":"m1"},"specification":{"provider":"anthropic","model":"m1"},"technical-design":{"provider":"anthropic","model":"m1"},"planning":{"provider":"anthropic","model":"m1"},"implementation":{"provider":"anthropic","model":"m1"},"verification":{"provider":"openai","model":"m2"},"delivery":{"provider":"anthropic","model":"m1"}},"budgets":{"maxTurns":24,"maxModelCalls":48,"maxToolCalls":96,"maxOutputTokens":400000,"wallClockMs":1800000,"maxRepeatedActions":3},"autonomy":"next-checkpoint","tools":{"commandTimeoutMs":120000,"maxSearchResults":200,"maxFileBytes":2000000}}"#,
        ),
    ),
    (
        "baseurl_bad",
        r#"version: 1
budgets: {}
providers: { openai: { baseUrl: notaurl } }
routing:
  discovery: { provider: anthropic, model: m1 }
  specification: { provider: anthropic, model: m1 }
  technical-design: { provider: anthropic, model: m1 }
  planning: { provider: anthropic, model: m1 }
  implementation: { provider: anthropic, model: m1 }
  verification: { provider: openai, model: m2 }
  delivery: { provider: anthropic, model: m1 }"#,
        TsOutcome::Invalid(&["providers.openai.baseUrl: Invalid URL"]),
    ),
    (
        "providers_null_entry",
        r#"version: 1
budgets: {}
providers: { anthropic: null }
routing:
  discovery: { provider: anthropic, model: m1 }
  specification: { provider: anthropic, model: m1 }
  technical-design: { provider: anthropic, model: m1 }
  planning: { provider: anthropic, model: m1 }
  implementation: { provider: anthropic, model: m1 }
  verification: { provider: openai, model: m2 }
  delivery: { provider: anthropic, model: m1 }"#,
        TsOutcome::Invalid(&["providers.anthropic: Invalid input: expected object, received null"]),
    ),
    (
        "root_list",
        r#"- a
- b"#,
        TsOutcome::Invalid(&["(root): Invalid input: expected object, received array"]),
    ),
    (
        "root_string",
        r#"just a string"#,
        TsOutcome::Invalid(&["(root): Invalid input: expected object, received string"]),
    ),
    (
        "root_null",
        r#"null"#,
        TsOutcome::Invalid(&["(root): Invalid input: expected object, received null"]),
    ),
    (
        "empty_text",
        r#"   "#,
        TsOutcome::Invalid(&[
            "routing: Invalid input: expected record, received undefined",
            "budgets: Invalid input: expected object, received undefined",
        ]),
    ),
    (
        "comment_only",
        r#"# nothing here
"#,
        TsOutcome::Invalid(&["(root): Invalid input: expected object, received null"]),
    ),
    (
        "unknown_routing_key",
        r#"version: 1
budgets: {}
routing:
  intent: { provider: anthropic, model: claude-haiku-4-5 }
  frobnicate: { provider: openai, model: gpt-5.2 }
  discovery: { provider: anthropic, model: m1 }
  specification: { provider: anthropic, model: m1 }
  technical-design: { provider: anthropic, model: m1 }
  planning: { provider: anthropic, model: m1 }
  implementation: { provider: anthropic, model: m1 }
  verification: { provider: openai, model: m2 }
  delivery: { provider: anthropic, model: m1 }"#,
        TsOutcome::Invalid(&[
            "routing contains unknown modes: frobnicate (allowed keys: discovery, specification, technical-design, planning, implementation, verification, delivery, and intent)",
        ]),
    ),
    (
        "missing_modes",
        r#"version: 1
budgets: {}
routing:
  discovery: { provider: anthropic, model: m1 }
  specification: { provider: anthropic, model: m1 }"#,
        TsOutcome::Invalid(&[
            "routing is missing modes: technical-design, planning, implementation, verification, delivery",
        ]),
    ),
    (
        "routing_missing",
        r#"version: 1
budgets: {}"#,
        TsOutcome::Invalid(&["routing: Invalid input: expected record, received undefined"]),
    ),
];

/// (name, merged-configuration YAML, TypeScript validation issues). `None`
/// means the TypeScript reference accepted the configuration.
const EFFECTIVE_CASES: &[(&str, &str, Option<&'static [&'static str]>)] = &[
    (
        "unknown_role",
        r#"version: 1
providers:
  openai:
    apiKeyEnv: OPENAI_API_KEY
  anthropic:
    apiKeyEnv: ANTHROPIC_API_KEY
routing:
  discovery: ghost
  specification: a
  technical-design: a
  planning: a
  implementation: a
  verification: a
  delivery: a
models:
  a:
    provider: fake
    model: x
budgets:
  maxTurns: 24
  maxModelCalls: 48
  maxToolCalls: 96
  maxOutputTokens: 400000
  wallClockMs: 1800000
  maxRepeatedActions: 3
autonomy: next-checkpoint
tools:
  commandTimeoutMs: 120000
  maxSearchResults: 200
  maxFileBytes: 2000000
"#,
        Some(&[
            "routing.discovery: references unknown model role \"ghost\"; available roles: a. Define a models.ghost entry or route \"discovery\" with an inline provider/model object.",
        ]),
    ),
    (
        "unknown_fallback",
        r#"version: 1
providers:
  openai:
    apiKeyEnv: OPENAI_API_KEY
  anthropic:
    apiKeyEnv: ANTHROPIC_API_KEY
routing:
  discovery: a
  specification: a
  technical-design: a
  planning: a
  implementation: a
  verification: a
  delivery: a
models:
  a:
    provider: fake
    model: x
    fallbacks:
      - ghost
budgets:
  maxTurns: 24
  maxModelCalls: 48
  maxToolCalls: 96
  maxOutputTokens: 400000
  wallClockMs: 1800000
  maxRepeatedActions: 3
autonomy: next-checkpoint
tools:
  commandTimeoutMs: 120000
  maxSearchResults: 200
  maxFileBytes: 2000000
"#,
        Some(&[
            "models.a.fallbacks[0]: references unknown model role \"ghost\"; available roles: a",
        ]),
    ),
    (
        "self_reference",
        r#"version: 1
providers:
  openai:
    apiKeyEnv: OPENAI_API_KEY
  anthropic:
    apiKeyEnv: ANTHROPIC_API_KEY
routing:
  discovery: a
  specification: a
  technical-design: a
  planning: a
  implementation: a
  verification: a
  delivery: a
models:
  a:
    provider: fake
    model: x
    fallbacks:
      - a
budgets:
  maxTurns: 24
  maxModelCalls: 48
  maxToolCalls: 96
  maxOutputTokens: 400000
  wallClockMs: 1800000
  maxRepeatedActions: 3
autonomy: next-checkpoint
tools:
  commandTimeoutMs: 120000
  maxSearchResults: 200
  maxFileBytes: 2000000
"#,
        Some(&["models.a.fallbacks[0]: role \"a\" cannot fall back to itself"]),
    ),
    (
        "cycle_three",
        r#"version: 1
providers:
  openai:
    apiKeyEnv: OPENAI_API_KEY
  anthropic:
    apiKeyEnv: ANTHROPIC_API_KEY
routing:
  discovery: a
  specification: b
  technical-design: b
  planning: b
  implementation: b
  verification: b
  delivery: b
models:
  a:
    provider: fake
    model: x
    fallbacks:
      - b
  b:
    provider: fake
    model: y
    fallbacks:
      - c
  c:
    provider: fake
    model: z
    fallbacks:
      - a
budgets:
  maxTurns: 24
  maxModelCalls: 48
  maxToolCalls: 96
  maxOutputTokens: 400000
  wallClockMs: 1800000
  maxRepeatedActions: 3
autonomy: next-checkpoint
tools:
  commandTimeoutMs: 120000
  maxSearchResults: 200
  maxFileBytes: 2000000
"#,
        Some(&[
            "models.a.fallbacks: role fallback cycle detected (a -> b -> c -> a); remove the cycle in the models catalog",
        ]),
    ),
    (
        "chain_five",
        r#"version: 1
providers:
  openai:
    apiKeyEnv: OPENAI_API_KEY
  anthropic:
    apiKeyEnv: ANTHROPIC_API_KEY
routing:
  discovery: a
  specification: z
  technical-design: z
  planning: z
  implementation: z
  verification: z
  delivery: z
models:
  a:
    provider: fake
    model: x
    fallbacks:
      - b
      - c
  b:
    provider: fake
    model: x
    fallbacks:
      - d
      - e
  c:
    provider: fake
    model: x
  d:
    provider: fake
    model: x
  e:
    provider: fake
    model: x
  z:
    provider: fake
    model: x
budgets:
  maxTurns: 24
  maxModelCalls: 48
  maxToolCalls: 96
  maxOutputTokens: 400000
  wallClockMs: 1800000
  maxRepeatedActions: 3
autonomy: next-checkpoint
tools:
  commandTimeoutMs: 120000
  maxSearchResults: 200
  maxFileBytes: 2000000
"#,
        Some(&[
            "routing.discovery: role \"a\" expands to 5 routes (a -> b -> c -> d -> e); the maximum is 1 primary plus 3 fallbacks",
        ]),
    ),
    (
        "empty_catalog",
        r#"version: 1
providers:
  openai:
    apiKeyEnv: OPENAI_API_KEY
  anthropic:
    apiKeyEnv: ANTHROPIC_API_KEY
routing:
  discovery: a
  specification: a
  technical-design: a
  planning: a
  implementation: a
  verification: a
  delivery: a
models: {}
budgets:
  maxTurns: 24
  maxModelCalls: 48
  maxToolCalls: 96
  maxOutputTokens: 400000
  wallClockMs: 1800000
  maxRepeatedActions: 3
autonomy: next-checkpoint
tools:
  commandTimeoutMs: 120000
  maxSearchResults: 200
  maxFileBytes: 2000000
"#,
        Some(&[
            "routing.discovery: references unknown model role \"a\"; available roles: (no roles are defined in the models catalog). Define a models.a entry or route \"discovery\" with an inline provider/model object.",
        ]),
    ),
    (
        "defaults_ok",
        r#"version: 1
providers:
  openai:
    apiKeyEnv: OPENAI_API_KEY
  anthropic:
    apiKeyEnv: ANTHROPIC_API_KEY
models:
  reasoning:
    provider: anthropic
    model: claude-sonnet-5-5
  architect:
    provider: anthropic
    model: claude-opus-5-5
  coding:
    provider: anthropic
    model: claude-sonnet-5-5
  review:
    provider: openai
    model: gpt-5.2
    fallbacks:
      - reasoning
  fast:
    provider: anthropic
    model: claude-haiku-4-5
routing:
  intent: fast
  discovery: reasoning
  specification: reasoning
  technical-design: architect
  planning: reasoning
  implementation: coding
  verification: review
  delivery: fast
budgets:
  maxTurns: 24
  maxModelCalls: 48
  maxToolCalls: 96
  maxOutputTokens: 400000
  wallClockMs: 1800000
  maxRepeatedActions: 3
autonomy: next-checkpoint
tools:
  commandTimeoutMs: 120000
  maxSearchResults: 200
  maxFileBytes: 2000000
"#,
        None,
    ),
    (
        "unreferenced_deep",
        r#"version: 1
providers:
  openai:
    apiKeyEnv: OPENAI_API_KEY
  anthropic:
    apiKeyEnv: ANTHROPIC_API_KEY
models:
  reasoning:
    provider: anthropic
    model: claude-sonnet-5-5
  architect:
    provider: anthropic
    model: claude-opus-5-5
  coding:
    provider: anthropic
    model: claude-sonnet-5-5
  review:
    provider: openai
    model: gpt-5.2
    fallbacks:
      - reasoning
  fast:
    provider: anthropic
    model: claude-haiku-4-5
  deep:
    provider: fake
    model: x
    fallbacks:
      - d1
      - d2
  d1:
    provider: fake
    model: x
    fallbacks:
      - d3
      - d4
  d2:
    provider: fake
    model: x
  d3:
    provider: fake
    model: x
  d4:
    provider: fake
    model: x
routing:
  intent: fast
  discovery: reasoning
  specification: reasoning
  technical-design: architect
  planning: reasoning
  implementation: coding
  verification: review
  delivery: fast
budgets:
  maxTurns: 24
  maxModelCalls: 48
  maxToolCalls: 96
  maxOutputTokens: 400000
  wallClockMs: 1800000
  maxRepeatedActions: 3
autonomy: next-checkpoint
tools:
  commandTimeoutMs: 120000
  maxSearchResults: 200
  maxFileBytes: 2000000
"#,
        Some(&[
            "models.deep: role \"deep\" expands to 5 routes (deep -> d1 -> d2 -> d3 -> d4); the maximum is 1 primary plus 3 fallbacks",
        ]),
    ),
    (
        "diamond",
        r#"version: 1
providers:
  openai:
    apiKeyEnv: OPENAI_API_KEY
  anthropic:
    apiKeyEnv: ANTHROPIC_API_KEY
routing:
  discovery: a
  specification: a
  technical-design: a
  planning: a
  implementation: a
  verification: a
  delivery: a
models:
  a:
    provider: fake
    model: x
    fallbacks:
      - b
      - c
  b:
    provider: fake
    model: y
    fallbacks:
      - d
  c:
    provider: fake
    model: z
    fallbacks:
      - d
  d:
    provider: fake
    model: w
budgets:
  maxTurns: 24
  maxModelCalls: 48
  maxToolCalls: 96
  maxOutputTokens: 400000
  wallClockMs: 1800000
  maxRepeatedActions: 3
autonomy: next-checkpoint
tools:
  commandTimeoutMs: 120000
  maxSearchResults: 200
  maxFileBytes: 2000000
"#,
        None,
    ),
];

/// The TypeScript outcome for a parse case.
enum TsOutcome {
    /// The layer parsed; the value is the reference's JSON form.
    Ok(&'static str),
    /// The layer failed schema or key validation; the value is the exact
    /// issue list.
    Invalid(&'static [&'static str]),
}

fn parsed_json(config: &RelayConfig) -> Value {
    serde_json::to_value(config).unwrap()
}

#[test]
fn single_layers_match_the_typescript_reference() {
    for (name, text, outcome) in PARSE_CASES {
        match (outcome, parse_yaml_layer("corpus.yaml", text)) {
            (TsOutcome::Ok(want_json), Ok(config)) => {
                let want: Value = serde_json::from_str(want_json).unwrap();
                let got = parsed_json(&config);
                assert_eq!(want, got, "{name}: parsed configuration differs");
            }
            (TsOutcome::Invalid(want_issues), Err(ConfigError::Validation(error))) => {
                // The reference reports every issue in the same order.
                assert_eq!(error.issues.len(), want_issues.len(), "{name}");
                for (index, issue) in want_issues.iter().enumerate() {
                    assert!(
                        error.issues[index].contains(issue),
                        "{name}[{index}]: {:?} does not contain {issue:?}",
                        error.issues[index]
                    );
                }
            }
            (TsOutcome::Ok(_), Err(error)) => panic!("{name}: unexpectedly rejected: {error}"),
            (TsOutcome::Invalid(_), Ok(config)) => panic!(
                "{name}: unexpectedly accepted: {}",
                serde_json::to_string(&config).unwrap()
            ),
            (_, Err(other)) => panic!("{name}: unexpected error: {other}"),
        }
    }
}

#[test]
fn effective_validation_matches_the_typescript_reference() {
    for (name, yaml, want_issues) in EFFECTIVE_CASES {
        let config = parse_yaml_layer("corpus.yaml", yaml).unwrap();
        let result = validate_effective_config(&config, "the effective configuration", None);
        match want_issues {
            None => assert!(result.is_ok(), "{name}: unexpectedly rejected"),
            Some(want) => match result {
                Ok(()) => panic!("{name}: unexpectedly accepted"),
                Err(error) => {
                    let got: Vec<&str> = error.issues.iter().map(String::as_str).collect();
                    assert_eq!(got, *want, "{name}");
                }
            },
        }
    }
}
