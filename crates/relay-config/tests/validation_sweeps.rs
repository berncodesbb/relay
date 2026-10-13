//! Determinism and property-style sweeps over the configuration surface:
//! parse+normalize twice yields equal results, and arbitrary fallback graphs
//! either validate cleanly and normalize to bounded, duplicate-free chains in
//! discovery order, or fail with one of the expected cross-reference
//! diagnostics.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::{
    LEGACY_YAML, PLAN_DEFAULT_YAML, config_with, defaults, inline_routes, model_role, role,
};
use relay_config::load::{
    ConfigOverrides, EFFECTIVE_CONFIG_SOURCE, LoadOptions, load_config, parse_yaml_layer,
    validate_effective_config,
};
use relay_config::normalized::{expand_role_names, normalize_routing};
use relay_config::schema::{ModelRoleConfig, ProviderName, RelayConfig, RoutingValue};
use std::collections::HashSet;

/// A tiny deterministic LCG so the sweeps never depend on external state.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 16
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }
}

fn build_config(catalog: &[(&str, ModelRoleConfig)], primary: Option<&str>) -> RelayConfig {
    let mut routing: Vec<(&str, RoutingValue)> = inline_routes();
    if let Some(name) = primary {
        routing.push(("discovery", role(name)));
    }
    let mut config = config_with(routing, Some(catalog.to_vec()));
    // Reference every role from routing so excessive-chain validation covers
    // the whole graph unless the case intentionally leaves roles unreferenced.
    if primary.is_none() {
        for (name, _) in catalog {
            config
                .routing
                .insert(name.to_string(), RoutingValue::Role(name.to_string()));
        }
    }
    config
}

fn role_def(fallbacks: Vec<&str>) -> ModelRoleConfig {
    ModelRoleConfig {
        provider: ProviderName::Fake,
        model: "echo".to_string(),
        fallbacks: if fallbacks.is_empty() {
            None
        } else {
            Some(fallbacks.iter().map(|name| name.to_string()).collect())
        },
        limits: None,
    }
}

#[test]
fn parsing_and_normalizing_twice_is_deterministic() {
    let corpus = [PLAN_DEFAULT_YAML, LEGACY_YAML];
    for text in corpus {
        let first = parse_yaml_layer("sweep.yaml", text).unwrap();
        let second = parse_yaml_layer("sweep.yaml", text).unwrap();
        assert_eq!(first, second);
        assert_eq!(normalize_routing(&first), normalize_routing(&second));
        assert_eq!(normalize_routing(&first), normalize_routing(&second));
    }
    let defaults_first = defaults();
    let defaults_second = defaults();
    assert_eq!(defaults_first, defaults_second);
    assert_eq!(
        normalize_routing(&defaults_first),
        normalize_routing(&defaults_second)
    );
}

#[test]
fn loading_twice_from_disk_is_deterministic() {
    let workspace = common::workspace_with_project_config(PLAN_DEFAULT_YAML);
    let options = LoadOptions {
        workspace_root: workspace.path().to_path_buf(),
        user_config_path: None,
        overrides: Some(ConfigOverrides {
            autonomy: Some("until:implementation".to_string()),
            force_provider: Some(ProviderName::Fake),
        }),
    };
    let first = load_config(options.clone()).unwrap();
    let second = load_config(options).unwrap();
    assert_eq!(first.config, second.config);
    assert_eq!(first.layers, second.layers);
    assert_eq!(
        normalize_routing(&first.config),
        normalize_routing(&second.config)
    );
}

#[test]
fn sweeps_over_random_fallback_graphs() {
    let mut rng = Lcg(0x5eed_1234);
    for iteration in 0..300u64 {
        let role_count = 1 + rng.below(7);
        let names: Vec<String> = (0..role_count).map(|index| format!("r{index}")).collect();
        let mut catalog: Vec<(String, ModelRoleConfig)> = Vec::new();
        for (index, name) in names.iter().enumerate() {
            let fallback_count = rng.below(4);
            let mut fallbacks: Vec<String> = Vec::new();
            for _ in 0..fallback_count {
                // Mostly known roles, sometimes an unknown or self reference.
                match rng.below(10) {
                    0 => fallbacks.push("ghost".to_string()),
                    1 => fallbacks.push(name.clone()),
                    _ => {
                        let target = names[rng.below(names.len() as u64) as usize].clone();
                        if !fallbacks.contains(&target) {
                            fallbacks.push(target);
                        }
                    }
                }
                let _ = index;
            }
            let refs: Vec<&str> = fallbacks.iter().map(String::as_str).collect();
            catalog.push((name.clone(), role_def(refs)));
        }
        let borrowed: Vec<(&str, ModelRoleConfig)> = catalog
            .iter()
            .map(|(name, definition)| (name.as_str(), definition.clone()))
            .collect();
        let config = build_config(&borrowed, Some("r0"));

        match validate_effective_config(&config, EFFECTIVE_CONFIG_SOURCE, None) {
            Ok(()) => {
                let routing = normalize_routing(&config);
                for (name, definition) in &catalog {
                    let chain = routing.role_chains.get(name).unwrap();
                    assert!(!chain.is_empty(), "iteration {iteration}: {name}");
                    assert!(
                        chain.len() <= 1 + relay_config::MAX_FALLBACKS,
                        "iteration {iteration}: chain for {name} has {} routes",
                        chain.len()
                    );
                    assert_eq!(chain[0].role.as_deref(), Some(name.as_str()));
                    assert_eq!(chain[0].model, definition.model);
                    // Discovery order, deduplicated without reordering.
                    let mut seen = HashSet::new();
                    for route in chain {
                        assert!(
                            seen.insert(route.role.clone().unwrap_or_default()),
                            "iteration {iteration}: duplicate role in chain for {name}"
                        );
                    }
                    let expanded = expand_role_names(name, &config.models.clone().unwrap(), None);
                    assert_eq!(expanded.len(), chain.len().min(expanded.len()));
                }
            }
            Err(error) => {
                let text = error.issues.join("\n");
                let expected = [
                    "references unknown model role",
                    "cannot fall back to itself",
                    "role fallback cycle detected",
                    "expands to",
                ];
                assert!(
                    expected.iter().any(|needle| text.contains(needle)),
                    "iteration {iteration}: unexpected diagnostic: {text}"
                );
            }
        }
    }
}

#[test]
fn sweep_configs_always_normalize_to_bounded_deduplicated_chains() {
    // Even catalogs that would fail effective validation (cycles, deep
    // chains) normalize defensively: bounded length, no duplicates, and a
    // stable result on repeat calls.
    let mut rng = Lcg(0xfeed_beef);
    for iteration in 0..200u64 {
        let role_count = 1 + rng.below(8);
        let names: Vec<String> = (0..role_count).map(|index| format!("c{index}")).collect();
        let mut catalog: Vec<(String, ModelRoleConfig)> = Vec::new();
        for name in &names {
            let mut fallbacks: Vec<String> = Vec::new();
            for _ in 0..rng.below(4) {
                let target = names[rng.below(names.len() as u64) as usize].clone();
                if !fallbacks.contains(&target) {
                    fallbacks.push(target);
                }
            }
            let refs: Vec<&str> = fallbacks.iter().map(String::as_str).collect();
            catalog.push((name.clone(), role_def(refs)));
        }
        let owned: Vec<(&str, ModelRoleConfig)> = catalog
            .iter()
            .map(|(name, definition)| (name.as_str(), definition.clone()))
            .collect();
        let mut routing: Vec<(&str, RoutingValue)> = inline_routes();
        routing.push(("discovery", role("c0")));
        let config = config_with(routing, Some(owned));
        let first = normalize_routing(&config);
        let second = normalize_routing(&config);
        assert_eq!(first, second, "iteration {iteration}");
        for chain in first.role_chains.values() {
            assert!(chain.len() <= 1 + relay_config::MAX_FALLBACKS);
            let mut seen = HashSet::new();
            for route in chain {
                assert!(seen.insert(route.role.clone().unwrap_or_default()));
            }
        }
    }
}

#[test]
fn chain_expansion_is_breadth_first_discovery_order_with_dedup() {
    // Deterministic hand-checks of the expansion rule itself.
    let mut config = config_with(inline_routes(), None);
    let mut catalog = config.models.take().unwrap_or_default();
    // a -> [b, c], b -> [d], c -> [d], d -> []
    for (name, fallbacks) in [
        ("a", vec!["b", "c"]),
        ("b", vec!["d"]),
        ("c", vec!["d"]),
        ("d", vec![] as Vec<&str>),
    ] {
        catalog.insert(name.to_string(), role_def(fallbacks));
    }
    config.models = Some(catalog);
    assert_eq!(
        expand_role_names("a", config.models.as_ref().unwrap(), None),
        vec!["a", "b", "c", "d"]
    );
    // Unknown primary expands to nothing.
    assert_eq!(
        expand_role_names("ghost", config.models.as_ref().unwrap(), None),
        Vec::<String>::new()
    );
    // The cap stops before exceeding one primary plus three fallbacks.
    let wide: Vec<(&str, ModelRoleConfig)> = vec![
        ("w", role_def(vec!["x", "y"])),
        ("x", role_def(vec!["z1", "z2"])),
        ("y", role_def(vec!["z3"])),
        ("z1", model_role(ProviderName::Fake, "z1")),
        ("z2", model_role(ProviderName::Fake, "z2")),
        ("z3", model_role(ProviderName::Fake, "z3")),
    ];
    let wide_catalog: std::collections::HashMap<String, ModelRoleConfig> = wide
        .into_iter()
        .map(|(name, definition)| (name.to_string(), definition))
        .collect();
    let mut ordered = indexmap::IndexMap::new();
    for key in ["w", "x", "y", "z1", "z2", "z3"] {
        ordered.insert(key.to_string(), wide_catalog[key].clone());
    }
    assert_eq!(
        expand_role_names("w", &ordered, Some(4)),
        vec!["w", "x", "y", "z1"]
    );
    assert_eq!(expand_role_names("w", &ordered, None).len(), 6);
}
