//! Normalized routing contract.
//!
//! Shared seam between configuration and runtime: configuration may describe
//! routes as role references or legacy inline objects, but by the time the
//! model router runs, everything is normalized into the concrete shapes here.
//! The raw string/object union never leaks past this boundary.

use crate::schema::{ModelRoleConfig, RelayConfig, RouteConfig, RouteLimits, RoutingValue};
use indexmap::IndexMap;
use std::collections::{HashSet, VecDeque};

/// Maximum fallbacks after the primary route (existing behavior).
pub const MAX_FALLBACKS: usize = 3;

/// The only non-mode routing purpose with a dedicated configuration key.
pub const INTENT_PURPOSE: &str = "intent";

/// One concrete executable route. `role` is `None` for legacy inline routes;
/// a role name is never fabricated. `provider` is a provider *connection*
/// name (a `providers.<name>` key of the effective configuration), not an
/// adapter kind.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct NormalizedRoute {
    /// The model-role name this route came from, or `None` for inline routes.
    /// Serialized as `null` (not omitted), matching the reference contract
    /// shape where `role` is always present on a normalized route.
    pub role: Option<String>,
    /// The concrete provider connection name.
    pub provider: String,
    /// The concrete model identifier.
    pub model: String,
    /// Optional per-route limits.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limits: Option<RouteLimits>,
}

/// Ordered execution chain: primary first, fallbacks after, deduplicated
/// without reordering, at most one primary plus [`MAX_FALLBACKS`] fallbacks.
pub type NormalizedChain = Vec<NormalizedRoute>;

/// The runtime-facing view of an effective configuration's routing.
///
/// - `role_chains`: every defined model role (user-defined names included),
///   expanded to its full fallback chain.
/// - `purpose_chains`: routing keys (the seven workflow modes plus explicit
///   non-mode purposes such as `intent`), each resolved to a chain. Legacy
///   inline entries appear here with `role: None`.
/// - `legacy_fallback`: the first configured route in configuration order,
///   used only for non-mode purposes that have no explicit routing entry —
///   preserving the historical behavior.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
pub struct NormalizedRouting {
    /// Expanded chains for every catalog role, in catalog order.
    pub role_chains: IndexMap<String, NormalizedChain>,
    /// Resolved chains for every routing key, in configuration order.
    pub purpose_chains: IndexMap<String, NormalizedChain>,
    /// The first configured route's chain, for legacy non-mode purposes.
    pub legacy_fallback: Option<NormalizedChain>,
}

/// Normalizes the effective configuration into the runtime-facing routing
/// contract: every catalog role expanded to its fallback chain, every routing
/// key resolved to a chain (role references through the role graph, legacy
/// inline objects to themselves plus their inline fallbacks with
/// `role: None`), and the legacy first-route fallback.
///
/// Pure and deterministic: no IO, safe to call repeatedly.
pub fn normalize_routing(config: &RelayConfig) -> NormalizedRouting {
    let models = config.models.clone().unwrap_or_default();
    let role_chains: IndexMap<String, NormalizedChain> = models
        .keys()
        .map(|role| (role.clone(), role_chain(role, &models)))
        .collect();
    let purpose_chains: IndexMap<String, NormalizedChain> = config
        .routing
        .iter()
        .map(|(key, value)| {
            let chain = match value {
                RoutingValue::Role(role) => role_chain(role, &models),
                RoutingValue::Inline(route) => inline_chain(route),
            };
            (key.clone(), chain)
        })
        .collect();
    let legacy_fallback = config
        .routing
        .keys()
        .next()
        .and_then(|first| purpose_chains.get(first))
        .cloned();
    NormalizedRouting {
        role_chains,
        purpose_chains,
        legacy_fallback,
    }
}

fn role_chain(primary: &str, models: &IndexMap<String, ModelRoleConfig>) -> NormalizedChain {
    let mut chain: NormalizedChain = Vec::new();
    for name in expand_role_names(primary, models, Some(1 + MAX_FALLBACKS)) {
        if let Some(definition) = models.get(&name) {
            chain.push(NormalizedRoute {
                role: Some(name),
                provider: definition.provider.clone(),
                model: definition.model.clone(),
                limits: definition.limits.clone(),
            });
        }
    }
    dedup_routes(chain)
}

fn inline_chain(route: &RouteConfig) -> NormalizedChain {
    let mut chain: NormalizedChain = vec![NormalizedRoute {
        role: None,
        provider: route.provider.clone(),
        model: route.model.clone(),
        limits: route.limits.clone(),
    }];
    for fallback in route.fallbacks.iter().flatten() {
        chain.push(NormalizedRoute {
            role: None,
            provider: fallback.provider.clone(),
            model: fallback.model.clone(),
            limits: None,
        });
    }
    dedup_routes(chain)
}

/// Drops later routes whose `(provider connection, model)` pair was already
/// seen: a fallback identical to an earlier route cannot succeed where the
/// earlier one was tried, so only the first occurrence is kept. Deduplication
/// never reorders and only ever shrinks a chain, so the
/// one-primary-plus-[`MAX_FALLBACKS`] bound still holds.
fn dedup_routes(chain: NormalizedChain) -> NormalizedChain {
    let mut seen: HashSet<(String, String)> = HashSet::new();
    chain
        .into_iter()
        .filter(|route| seen.insert((route.provider.clone(), route.model.clone())))
        .collect()
}

/// Expands a primary role through the role fallback graph in breadth-first
/// discovery order, deduplicating while preserving first-seen order. `cap`
/// (when given) hard-stops the chain at that many roles.
///
/// A reference to an unknown role expands to an empty chain; effective
/// configuration validation rejects those before normalization runs.
pub fn expand_role_names(
    primary: &str,
    models: &IndexMap<String, ModelRoleConfig>,
    cap: Option<usize>,
) -> Vec<String> {
    let mut order: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    if !models.contains_key(primary) {
        return order;
    }
    order.push(primary.to_string());
    seen.insert(primary.to_string());
    let mut queue: VecDeque<String> = VecDeque::new();
    queue.push_back(primary.to_string());
    while let Some(role) = queue.pop_front() {
        let Some(definition) = models.get(&role) else {
            continue;
        };
        for fallback in definition.fallbacks.iter().flatten() {
            if seen.contains(fallback) || !models.contains_key(fallback) {
                continue;
            }
            if cap.is_some_and(|limit| order.len() >= limit) {
                return order;
            }
            seen.insert(fallback.clone());
            order.push(fallback.clone());
            queue.push_back(fallback.clone());
        }
    }
    order
}
