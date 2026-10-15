//! Ports of the retired `test/unit/config/normalize.test.ts` (removed from
//! the tree 2026-10-14): the seam between raw
//! configuration and the runtime routing contract.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::{config_with, defaults, inline, inline_routes, limits, model_role, role};
use relay_config::normalized::{INTENT_PURPOSE, MAX_FALLBACKS, NormalizedRoute, normalize_routing};
use relay_config::schema::{ModelRoleConfig, RelayConfig, RouteConfig, RoutingValue};

fn inline_route(provider: &str, model: &str, fallbacks: Vec<(&str, &str)>) -> RoutingValue {
    RoutingValue::Inline(RouteConfig {
        provider: provider.to_string(),
        model: model.to_string(),
        fallbacks: Some(
            fallbacks
                .into_iter()
                .map(|(provider, model)| relay_config::schema::InlineFallback {
                    provider: provider.to_string(),
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
            provider: inner.provider.clone(),
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

fn route(role: Option<&str>, provider: &str, model: &str) -> NormalizedRoute {
    NormalizedRoute {
        role: role.map(str::to_string),
        provider: provider.to_string(),
        model: model.to_string(),
        limits: None,
    }
}

#[test]
fn routes_intent_through_the_fast_role_on_the_offline_default() {
    let routing = normalize_routing(&defaults());
    assert_eq!(
        routing.purpose_chains.get(INTENT_PURPOSE).unwrap(),
        &vec![route(Some("fast"), "fake", "fake-standard")]
    );
}

#[test]
fn resolves_every_workflow_mode_to_the_offline_fake_connection() {
    let routing = normalize_routing(&defaults());
    let expected: Vec<(&str, NormalizedRoute)> = vec![
        (
            "discovery",
            route(Some("reasoning"), "fake", "fake-standard"),
        ),
        (
            "specification",
            route(Some("reasoning"), "fake", "fake-standard"),
        ),
        (
            "planning",
            route(Some("reasoning"), "fake", "fake-standard"),
        ),
        (
            "technical-design",
            route(Some("architect"), "fake", "fake-standard"),
        ),
        (
            "implementation",
            route(Some("coding"), "fake", "fake-standard"),
        ),
        (
            "verification",
            route(Some("review"), "fake", "fake-standard"),
        ),
        ("delivery", route(Some("fast"), "fake", "fake-standard")),
    ];
    for (mode, first_route) in expected {
        assert_eq!(
            routing.purpose_chains.get(mode),
            Some(&vec![first_route]),
            "{mode}"
        );
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
                    provider: "anthropic".to_string(),
                    model: "claude-sonnet-5-5".to_string(),
                    fallbacks: Some(vec!["b".to_string()]),
                    limits: limits(Some(4096), Some(0.2)),
                },
            ),
            (
                "b",
                ModelRoleConfig {
                    provider: "claude-proxy".to_string(),
                    model: "claude-haiku-4-5".to_string(),
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
                provider: "anthropic".to_string(),
                model: "claude-sonnet-5-5".to_string(),
                limits: limits(Some(4096), Some(0.2)),
            },
            NormalizedRoute {
                role: Some("b".to_string()),
                provider: "claude-proxy".to_string(),
                model: "claude-haiku-4-5".to_string(),
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
                    provider: "fake".to_string(),
                    model: "echo-a".to_string(),
                    fallbacks: Some(vec!["b".to_string(), "c".to_string()]),
                    limits: None,
                },
            ),
            (
                "b",
                ModelRoleConfig {
                    provider: "fake".to_string(),
                    model: "echo-b".to_string(),
                    fallbacks: Some(vec!["c".to_string(), "d".to_string()]),
                    limits: None,
                },
            ),
            ("c", model_role("fake", "echo-c")),
            ("d", model_role("fake", "echo-d")),
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
                    provider: "fake".to_string(),
                    model: "echo-a".to_string(),
                    fallbacks: Some(vec!["b".to_string(), "c".to_string()]),
                    limits: None,
                },
            ),
            (
                "b",
                ModelRoleConfig {
                    provider: "fake".to_string(),
                    model: "echo-b".to_string(),
                    fallbacks: Some(vec!["d".to_string()]),
                    limits: None,
                },
            ),
            (
                "c",
                ModelRoleConfig {
                    provider: "fake".to_string(),
                    model: "echo-c".to_string(),
                    fallbacks: Some(vec!["e".to_string()]),
                    limits: None,
                },
            ),
            ("d", model_role("fake", "echo-d")),
            ("e", model_role("fake", "echo-e")),
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
            model_role("claude-proxy", "claude-haiku-4-5"),
        )]),
    );
    assert_eq!(
        chain_of(&config, "verification"),
        vec![route(Some("review"), "claude-proxy", "claude-haiku-4-5")]
    );
}

#[test]
fn marks_inline_routes_and_their_fallbacks_with_role_null_keeping_limits() {
    let mut routing_table = inline_routes();
    let discovery = inline_route(
        "claude-proxy",
        "claude-haiku-4-5",
        vec![("anthropic", "claude-sonnet-5-5")],
    );
    let discovery = with_limits(&discovery, None, Some(0.1));
    routing_table.push(("discovery", discovery));
    let config = config_with(routing_table, None);
    assert_eq!(
        chain_of(&config, "discovery"),
        vec![
            NormalizedRoute {
                role: None,
                provider: "claude-proxy".to_string(),
                model: "claude-haiku-4-5".to_string(),
                limits: limits(None, Some(0.1)),
            },
            route(None, "anthropic", "claude-sonnet-5-5"),
        ]
    );
}

#[test]
fn deduplicates_identical_provider_model_pairs_keeping_first_occurrence() {
    // Role chains: two roles with the same (connection, model) pair collapse
    // to the first occurrence (the primary, with its limits).
    let mut routing_table = inline_routes();
    routing_table.push(("discovery", role("a")));
    let config = config_with(
        routing_table,
        Some(vec![
            (
                "a",
                ModelRoleConfig {
                    provider: "fake".to_string(),
                    model: "echo".to_string(),
                    fallbacks: Some(vec!["b".to_string()]),
                    limits: limits(Some(4096), None),
                },
            ),
            (
                "b",
                ModelRoleConfig {
                    provider: "fake".to_string(),
                    model: "echo".to_string(),
                    fallbacks: None,
                    limits: limits(Some(2048), None),
                },
            ),
        ]),
    );
    assert_eq!(
        chain_of(&config, "discovery"),
        vec![NormalizedRoute {
            role: Some("a".to_string()),
            provider: "fake".to_string(),
            model: "echo".to_string(),
            limits: limits(Some(4096), None),
        }]
    );

    // Inline chains: a fallback identical to the primary (or an earlier
    // fallback) is dropped; distinct pairs keep their order.
    let mut routing_table = inline_routes();
    routing_table.push((
        "discovery",
        inline_route(
            "claude-proxy",
            "m1",
            vec![
                ("claude-proxy", "m1"),
                ("anthropic", "m2"),
                ("anthropic", "m2"),
            ],
        ),
    ));
    let config = config_with(routing_table, None);
    assert_eq!(
        chain_of(&config, "discovery"),
        vec![
            route(None, "claude-proxy", "m1"),
            route(None, "anthropic", "m2")
        ]
    );
}

#[test]
fn dedup_across_two_connections_sharing_one_adapter_keeps_both_routes() {
    // `claude-pro` and `claude-local` are different connections on the same
    // adapter: their routes are distinct even with the same model id.
    let mut routing_table = inline_routes();
    routing_table.push(("discovery", role("a")));
    let config = config_with(
        routing_table,
        Some(vec![
            (
                "a",
                ModelRoleConfig {
                    provider: "claude-pro".to_string(),
                    model: "shared-model".to_string(),
                    fallbacks: Some(vec!["b".to_string()]),
                    limits: None,
                },
            ),
            (
                "b",
                ModelRoleConfig {
                    provider: "claude-local".to_string(),
                    model: "shared-model".to_string(),
                    fallbacks: None,
                    limits: None,
                },
            ),
        ]),
    );
    assert_eq!(
        chain_of(&config, "discovery"),
        vec![
            route(Some("a"), "claude-pro", "shared-model"),
            route(Some("b"), "claude-local", "shared-model"),
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
        Some(vec![route(None, "anthropic", "claude-sonnet-5-5")])
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
                    provider: "fake".to_string(),
                    model: "echo-a".to_string(),
                    fallbacks: Some(vec!["b".to_string()]),
                    limits: None,
                },
            ),
            ("b", model_role("fake", "echo-b")),
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
        Some(vec![("a", model_role("fake", "echo-a"))]),
    );
    assert_eq!(
        chain_of(&config, "discovery"),
        Vec::<NormalizedRoute>::new()
    );
    // The first routing key is still the legacy fallback, and — matching the
    // reference's `purposeChains.get(firstKey) ?? null` — an empty (but
    // present) first chain yields an empty legacy fallback, not null.
    assert_eq!(normalize_routing(&config).legacy_fallback, Some(Vec::new()));
    let _ = inline("fake", "unused");
}

#[test]
fn invocation_provider_override_filters_the_chain_by_connection_name() {
    let mut routing_table = inline_routes();
    routing_table.push(("verification", role("review")));
    let config = config_with(
        routing_table,
        Some(vec![
            (
                "review",
                ModelRoleConfig {
                    provider: "claude-proxy".to_string(),
                    model: "claude-haiku-4-5".to_string(),
                    fallbacks: Some(vec!["backup".to_string()]),
                    limits: None,
                },
            ),
            ("backup", model_role("anthropic", "claude-sonnet-5-5")),
        ]),
    );
    let routing = normalize_routing(&config);

    // A configured connection name keeps only routes on that connection.
    let chain = relay_config::apply_invocation_overrides(
        &routing,
        "verification",
        None,
        &relay_config::InvocationOverrides {
            role: None,
            provider: Some("anthropic".to_string()),
            model: None,
        },
    )
    .unwrap();
    assert_eq!(
        chain,
        vec![route(Some("backup"), "anthropic", "claude-sonnet-5-5")]
    );

    // The legacy spellings work because they are the default connection
    // names; user-defined names work identically.
    let config = config_with(
        vec![
            ("discovery", role("a")),
            ("specification", inline("anthropic", "m")),
            ("technical-design", inline("anthropic", "m")),
            ("planning", inline("anthropic", "m")),
            ("implementation", inline("anthropic", "m")),
            ("verification", inline("claude-proxy", "claude-haiku-4-5")),
            ("delivery", inline("anthropic", "m")),
        ],
        Some(vec![("a", model_role("claude-proxy", "claude-haiku-4-5"))]),
    );
    let routing = normalize_routing(&config);
    let chain = relay_config::apply_invocation_overrides(
        &routing,
        "discovery",
        None,
        &relay_config::InvocationOverrides {
            role: None,
            provider: Some("claude-proxy".to_string()),
            model: None,
        },
    )
    .unwrap();
    assert_eq!(
        chain,
        vec![route(Some("a"), "claude-proxy", "claude-haiku-4-5")]
    );
}

#[test]
fn invocation_provider_override_names_the_connection_in_its_error() {
    let workspace_config = config_with(inline_routes(), None);
    let routing = normalize_routing(&workspace_config);
    let error = relay_config::apply_invocation_overrides(
        &routing,
        "discovery",
        None,
        &relay_config::InvocationOverrides {
            role: None,
            provider: Some("grok".to_string()),
            model: None,
        },
    )
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        "No usable model route: provider connection override \"grok\" matched no route in the \
         chain for purpose \"discovery\""
    );
}
