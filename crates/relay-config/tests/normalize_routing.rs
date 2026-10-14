//! Ports of the retired `test/unit/config/normalize.test.ts` (removed from
//! the tree 2026-10-14): the seam between raw
//! configuration and the runtime routing contract.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::{config_with, defaults, inline, inline_routes, limits, model_role, role};
use relay_config::normalized::{INTENT_PURPOSE, MAX_FALLBACKS, NormalizedRoute, normalize_routing};
use relay_config::schema::{ModelRoleConfig, ProviderName, RelayConfig, RouteConfig, RoutingValue};

fn inline_route(
    provider: ProviderName,
    model: &str,
    fallbacks: Vec<(ProviderName, &str)>,
) -> RoutingValue {
    RoutingValue::Inline(RouteConfig {
        provider,
        model: model.to_string(),
        fallbacks: Some(
            fallbacks
                .into_iter()
                .map(|(provider, model)| relay_config::schema::InlineFallback {
                    provider,
                    model: model.to_string(),
                })
                .collect(),
        ),
        limits: None,
    })
}

fn with_limits(
    route: &RoutingValue,
    max_output_tokens: Option<i64>,
    temperature: Option<f64>,
) -> RoutingValue {
    match route {
        RoutingValue::Inline(inner) => RoutingValue::Inline(RouteConfig {
            provider: inner.provider,
            model: inner.model.clone(),
            fallbacks: inner.fallbacks.clone(),
            limits: limits(max_output_tokens, temperature),
        }),
        other => other.clone(),
    }
}

fn chain_of(config: &RelayConfig, key: &str) -> Vec<NormalizedRoute> {
    normalize_routing(config)
        .purpose_chains
        .get(key)
        .cloned()
        .unwrap_or_default()
}

#[test]
fn routes_intent_to_the_lightweight_fast_role() {
    let routing = normalize_routing(&defaults());
    assert_eq!(
        routing.purpose_chains.get(INTENT_PURPOSE).unwrap(),
        &vec![NormalizedRoute {
            role: Some("fast".to_string()),
            provider: ProviderName::Anthropic,
            model: "claude-haiku-4-5".to_string(),
            limits: None,
        }]
    );
}

#[test]
fn resolves_every_workflow_mode_to_todays_provider_and_model_with_role_names() {
    let routing = normalize_routing(&defaults());
    let reasoning = NormalizedRoute {
        role: Some("reasoning".to_string()),
        provider: ProviderName::Anthropic,
        model: "claude-sonnet-5-5".to_string(),
        limits: None,
    };
    let expected: Vec<(&str, Vec<NormalizedRoute>)> = vec![
        ("discovery", vec![reasoning.clone()]),
        ("specification", vec![reasoning.clone()]),
        ("planning", vec![reasoning.clone()]),
        (
            "technical-design",
            vec![NormalizedRoute {
                role: Some("architect".to_string()),
                provider: ProviderName::Anthropic,
                model: "claude-opus-5-5".to_string(),
                limits: None,
            }],
        ),
        (
            "implementation",
            vec![NormalizedRoute {
                role: Some("coding".to_string()),
                provider: ProviderName::Anthropic,
                model: "claude-sonnet-5-5".to_string(),
                limits: None,
            }],
        ),
        (
            "verification",
            vec![
                NormalizedRoute {
                    role: Some("review".to_string()),
                    provider: ProviderName::OpenAi,
                    model: "gpt-5.2".to_string(),
                    limits: None,
                },
                reasoning,
            ],
        ),
        (
            "delivery",
            vec![NormalizedRoute {
                role: Some("fast".to_string()),
                provider: ProviderName::Anthropic,
                model: "claude-haiku-4-5".to_string(),
                limits: None,
            }],
        ),
    ];
    for (mode, chain) in expected {
        assert_eq!(routing.purpose_chains.get(mode), Some(&chain), "{mode}");
    }
}

#[test]
fn expands_every_catalog_role_and_uses_the_first_routing_key_as_legacy_fallback() {
    let routing = normalize_routing(&defaults());
    let mut names: Vec<&String> = routing.role_chains.keys().collect();
    names.sort();
    assert_eq!(
        names,
        vec!["architect", "coding", "fast", "reasoning", "review"]
    );
    assert_eq!(
        routing.role_chains.get("review"),
        routing.purpose_chains.get("verification")
    );
    // First key of the default routing is `intent`.
    assert_eq!(
        routing.purpose_chains.keys().next(),
        Some(&INTENT_PURPOSE.to_string())
    );
    assert_eq!(
        routing.legacy_fallback,
        routing.purpose_chains.get(INTENT_PURPOSE).cloned()
    );
}

#[test]
fn carries_per_role_limits_through_the_chain() {
    let mut routing_table = inline_routes();
    routing_table.push(("discovery", role("a")));
    let config = config_with(
        routing_table,
        Some(vec![
            (
                "a",
                ModelRoleConfig {
                    provider: ProviderName::Anthropic,
                    model: "claude-sonnet-5-5".to_string(),
                    fallbacks: Some(vec!["b".to_string()]),
                    limits: limits(Some(4096), Some(0.2)),
                },
            ),
            (
                "b",
                ModelRoleConfig {
                    provider: ProviderName::OpenAi,
                    model: "gpt-5.2".to_string(),
                    fallbacks: None,
                    limits: limits(Some(2048), None),
                },
            ),
        ]),
    );
    let routing = normalize_routing(&config);
    assert_eq!(
        routing.purpose_chains.get("discovery").unwrap(),
        &vec![
            NormalizedRoute {
                role: Some("a".to_string()),
                provider: ProviderName::Anthropic,
                model: "claude-sonnet-5-5".to_string(),
                limits: limits(Some(4096), Some(0.2)),
            },
            NormalizedRoute {
                role: Some("b".to_string()),
                provider: ProviderName::OpenAi,
                model: "gpt-5.2".to_string(),
                limits: limits(Some(2048), None),
            },
        ]
    );
    assert_eq!(
        routing.role_chains.get("a"),
        routing.purpose_chains.get("discovery")
    );
}

#[test]
fn expands_fallbacks_breadth_first_deduplicating_without_reordering() {
    let mut routing_table = inline_routes();
    routing_table.push(("discovery", role("a")));
    let config = config_with(
        routing_table,
        Some(vec![
            (
                "a",
                ModelRoleConfig {
                    provider: ProviderName::Fake,
                    model: "echo-a".to_string(),
                    fallbacks: Some(vec!["b".to_string(), "c".to_string()]),
                    limits: None,
                },
            ),
            (
                "b",
                ModelRoleConfig {
                    provider: ProviderName::Fake,
                    model: "echo-b".to_string(),
                    fallbacks: Some(vec!["c".to_string(), "d".to_string()]),
                    limits: None,
                },
            ),
            ("c", model_role(ProviderName::Fake, "echo-c")),
            ("d", model_role(ProviderName::Fake, "echo-d")),
        ]),
    );
    let roles: Vec<Option<String>> = chain_of(&config, "discovery")
        .into_iter()
        .map(|route| route.role)
        .collect();
    assert_eq!(
        roles,
        vec![
            Some("a".to_string()),
            Some("b".to_string()),
            Some("c".to_string()),
            Some("d".to_string()),
        ]
    );
}

#[test]
fn hard_caps_expanded_chains_at_one_primary_plus_max_fallbacks() {
    let mut routing_table = inline_routes();
    routing_table.push(("discovery", role("a")));
    let config = config_with(
        routing_table,
        Some(vec![
            (
                "a",
                ModelRoleConfig {
                    provider: ProviderName::Fake,
                    model: "echo-a".to_string(),
                    fallbacks: Some(vec!["b".to_string(), "c".to_string()]),
                    limits: None,
                },
            ),
            (
                "b",
                ModelRoleConfig {
                    provider: ProviderName::Fake,
                    model: "echo-b".to_string(),
                    fallbacks: Some(vec!["d".to_string()]),
                    limits: None,
                },
            ),
            (
                "c",
                ModelRoleConfig {
                    provider: ProviderName::Fake,
                    model: "echo-c".to_string(),
                    fallbacks: Some(vec!["e".to_string()]),
                    limits: None,
                },
            ),
            ("d", model_role(ProviderName::Fake, "echo-d")),
            ("e", model_role(ProviderName::Fake, "echo-e")),
        ]),
    );
    // This catalog would fail effective validation; normalization still
    // behaves defensively and stops at four routes.
    let roles: Vec<Option<String>> = chain_of(&config, "discovery")
        .into_iter()
        .map(|route| route.role)
        .collect();
    assert_eq!(
        roles,
        vec![
            Some("a".to_string()),
            Some("b".to_string()),
            Some("c".to_string()),
            Some("d".to_string()),
        ]
    );
    assert_eq!(MAX_FALLBACKS, 3);
}

#[test]
fn resolves_a_role_reference_with_no_fallbacks_to_a_single_route_chain() {
    let mut routing_table = inline_routes();
    routing_table.push(("verification", role("review")));
    let config = config_with(
        routing_table,
        Some(vec![(
            "review",
            model_role(ProviderName::OpenAi, "gpt-5.2"),
        )]),
    );
    assert_eq!(
        chain_of(&config, "verification"),
        vec![NormalizedRoute {
            role: Some("review".to_string()),
            provider: ProviderName::OpenAi,
            model: "gpt-5.2".to_string(),
            limits: None,
        }]
    );
}

#[test]
fn marks_inline_routes_and_their_fallbacks_with_role_null_keeping_limits() {
    let mut routing_table = inline_routes();
    let discovery = inline_route(
        ProviderName::OpenAi,
        "gpt-5.2",
        vec![(ProviderName::Anthropic, "claude-sonnet-5-5")],
    );
    let discovery = with_limits(&discovery, None, Some(0.1));
    routing_table.push(("discovery", discovery));
    let config = config_with(routing_table, None);
    assert_eq!(
        chain_of(&config, "discovery"),
        vec![
            NormalizedRoute {
                role: None,
                provider: ProviderName::OpenAi,
                model: "gpt-5.2".to_string(),
                limits: limits(None, Some(0.1)),
            },
            NormalizedRoute {
                role: None,
                provider: ProviderName::Anthropic,
                model: "claude-sonnet-5-5".to_string(),
                limits: None,
            },
        ]
    );
}

#[test]
fn produces_an_empty_role_catalog_and_the_first_route_as_legacy_fallback() {
    let config = config_with(inline_routes(), None);
    let routing = normalize_routing(&config);
    assert_eq!(routing.role_chains.len(), 0);
    assert_eq!(routing.purpose_chains.len(), 7);
    assert!(!routing.purpose_chains.contains_key(INTENT_PURPOSE));
    assert_eq!(
        routing.legacy_fallback,
        Some(vec![NormalizedRoute {
            role: None,
            provider: ProviderName::Anthropic,
            model: "claude-sonnet-5-5".to_string(),
            limits: None,
        }])
    );
}

#[test]
fn is_deterministic_and_does_not_mutate_the_configuration() {
    let mut routing_table = inline_routes();
    routing_table.push(("intent", role("a")));
    routing_table.push(("discovery", role("a")));
    let config = config_with(
        routing_table,
        Some(vec![
            (
                "a",
                ModelRoleConfig {
                    provider: ProviderName::Fake,
                    model: "echo-a".to_string(),
                    fallbacks: Some(vec!["b".to_string()]),
                    limits: None,
                },
            ),
            ("b", model_role(ProviderName::Fake, "echo-b")),
        ]),
    );
    let before = config.clone();
    let first = normalize_routing(&config);
    let second = normalize_routing(&config);
    assert_eq!(first, second);
    assert_eq!(config, before);
    let roles: Vec<Option<String>> = first
        .purpose_chains
        .get(INTENT_PURPOSE)
        .unwrap()
        .iter()
        .map(|route| route.role.clone())
        .collect();
    assert_eq!(roles, vec![Some("a".to_string()), Some("b".to_string())]);
}

#[test]
fn yields_an_empty_chain_for_references_to_unknown_roles() {
    let mut routing_table = inline_routes();
    routing_table.push(("discovery", role("ghost")));
    let config = config_with(
        routing_table,
        Some(vec![("a", model_role(ProviderName::Fake, "echo-a"))]),
    );
    assert_eq!(
        chain_of(&config, "discovery"),
        Vec::<NormalizedRoute>::new()
    );
    // The first routing key is still the legacy fallback, and — matching the
    // reference's `purposeChains.get(firstKey) ?? null` — an empty (but
    // present) first chain yields an empty legacy fallback, not null.
    assert_eq!(normalize_routing(&config).legacy_fallback, Some(Vec::new()));
    let _ = inline(ProviderName::Fake, "unused");
}
