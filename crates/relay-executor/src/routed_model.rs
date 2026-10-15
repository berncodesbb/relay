//! Connection registry and the routed model client.
//!
//! The production [`ModelClient`] implementation:
//! named provider connections built once from effective configuration, a
//! purpose-keyed route plan resolved once per invocation (after invocation
//! overrides), and deterministic per-attempt routing from each grant's
//! immutable [`ModelRoute`].
//!
//! Determinism and safety properties:
//!
//! - The registry performs no IO beyond environment-variable reads at
//!   construction (credential *values* resolve inside the adapters and are
//!   never stored, logged, or echoed; only the bounded missing-variable auth
//!   message survives).
//! - Neither the registry, the plan, nor the client holds or acquires any
//!   storage lock; fallback loops and provider calls never run under the
//!   project writer lock.
//! - Models never choose their provider, role, or fallback: the granted
//!   route selects the chain, the chain order is configuration order, and
//!   every attempted route is recorded in the response diagnostics.
//!
//! Fallback is ordered, bounded (by the configured chain length, at most one
//! primary plus [`MAX_FALLBACKS`](relay_config::MAX_FALLBACKS) fallbacks),
//! deduplicated by `(connection, model)`, and advanced only for provider
//! *availability* failures — see the table on
//! [`RoutedModelClient`]. A transcript carrying opaque provider continuation
//! blocks is pinned to its primary route and never falls back (see
//! [`RoutedModelClient`] for the pinned-failure contract).

use std::sync::Arc;

use relay_config::{
    InvocationOverrides, NormalizedChain, NormalizedRoute, NormalizedRouting, ProviderConnection,
    RelayConfig, RouteLimits, RouteResolutionError, apply_invocation_overrides,
};
use relay_domain::ModelRoute;
use relay_providers::anthropic::AnthropicProvider;
use relay_providers::collect_turn;
use relay_providers::contract::{
    AdapterKind, AssistantContent, CancelToken, ModelMessage, ModelProvider, ModelRequest,
    ProviderErrorKind, resolve_capabilities,
};
use relay_providers::fake::FakeProvider;

use crate::model::{
    ModelCallFailure, ModelClient, ModelClientError, ModelTurnRequest, ModelTurnResponse,
    RouteAttemptFailure,
};

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

/// The state of one named provider connection in the registry.
pub enum ConnectionState {
    /// The adapter constructed successfully and is ready to serve calls.
    Ready {
        /// The wire-protocol adapter serving this connection.
        adapter: AdapterKind,
        /// The constructed provider.
        provider: Arc<dyn ModelProvider>,
    },
    /// The connection is configured but cannot serve calls (for example its
    /// credential environment variable is missing, or its capability
    /// preset/override combination cannot resolve). `reason` is the
    /// adapter's bounded, redacted message.
    Unavailable {
        /// The wire-protocol adapter the connection would speak.
        adapter: AdapterKind,
        /// Bounded, redacted explanation (never contains a credential).
        reason: String,
    },
}

/// Named provider connections, built once from the effective configuration.
///
/// Construction never fails: a connection whose adapter cannot construct
/// (missing credential variable, unresolvable capability profile) is recorded
/// as [`ConnectionState::Unavailable`] with the adapter's bounded message, so
/// the offline default (the `fake` connection) stays usable regardless of
/// which live credentials exist. Effective-configuration validation normally
/// rejects invalid capability settings first; recording them here is defense
/// in depth.
pub struct ProviderRegistry {
    /// Named connections in configuration order.
    connections: Vec<(String, ConnectionState)>,
}

impl ProviderRegistry {
    /// Build the registry from the effective configuration: one connection
    /// per `providers.<name>` entry, in configuration order. Performs no IO
    /// beyond credential environment-variable reads inside the adapters, and
    /// never holds or acquires storage locks.
    pub fn from_effective_config(config: &RelayConfig) -> Self {
        ProviderRegistry {
            connections: config
                .providers
                .iter()
                .map(|(name, connection)| (name.to_owned(), build_connection(connection)))
                .collect(),
        }
    }

    /// Build a registry directly from named connection states, in the given
    /// order. The injection point for deterministic tests and for callers
    /// that construct adapters themselves; [`Self::from_effective_config`]
    /// is the production path.
    pub fn from_connections<I, S>(connections: I) -> Self
    where
        I: IntoIterator<Item = (S, ConnectionState)>,
        S: Into<String>,
    {
        ProviderRegistry {
            connections: connections
                .into_iter()
                .map(|(name, state)| (name.into(), state))
                .collect(),
        }
    }

    /// The state of one named connection.
    pub fn connection(&self, name: &str) -> Option<&ConnectionState> {
        self.connections
            .iter()
            .find(|(connection, _)| connection == name)
            .map(|(_, state)| state)
    }

    /// The defined connection names, in configuration (registry) order.
    pub fn names(&self) -> Vec<String> {
        self.connections
            .iter()
            .map(|(name, _)| name.clone())
            .collect()
    }

    /// The ready provider for one named connection, when it is ready.
    pub fn ready(&self, name: &str) -> Option<Arc<dyn ModelProvider>> {
        match self.connection(name) {
            Some(ConnectionState::Ready { provider, .. }) => Some(Arc::clone(provider)),
            _ => None,
        }
    }

    /// The connection names joined for actionable diagnostics.
    fn names_listing(&self) -> String {
        let names = self.names();
        if names.is_empty() {
            "(no provider connections are configured)".to_owned()
        } else {
            names.join(", ")
        }
    }
}

/// Construct one connection from its configuration, mapping every failure to
/// [`ConnectionState::Unavailable`] with the bounded, redacted reason.
fn build_connection(connection: &ProviderConnection) -> ConnectionState {
    match connection.adapter {
        AdapterKind::Anthropic => {
            let adapter = AdapterKind::Anthropic;
            match resolve_capabilities(
                adapter,
                connection.preset.as_deref(),
                &connection.capabilities,
            ) {
                Err(error) => unavailable(adapter, error.to_string()),
                Ok(capabilities) => {
                    match AnthropicProvider::new(
                        relay_providers::anthropic::AnthropicProviderOptions {
                            api_key: None,
                            api_key_env: connection.api_key_env.clone(),
                            base_url: connection.base_url.clone(),
                            capabilities: Some(capabilities),
                        },
                    ) {
                        Ok(provider) => ConnectionState::Ready {
                            adapter,
                            provider: Arc::new(provider),
                        },
                        Err(failure) => unavailable(adapter, failure.to_string()),
                    }
                }
            }
        }
        AdapterKind::Fake => ConnectionState::Ready {
            adapter: AdapterKind::Fake,
            provider: Arc::new(FakeProvider::echo()),
        },
    }
}

/// A [`ConnectionState::Unavailable`] from a bounded reason message.
fn unavailable(adapter: AdapterKind, reason: String) -> ConnectionState {
    ConnectionState::Unavailable {
        adapter,
        reason: reason.chars().take(300).collect(),
    }
}

// ---------------------------------------------------------------------------
// Route plan
// ---------------------------------------------------------------------------

/// One concrete executable route with its configured limits: the
/// [`ModelRoute`] that goes into an attempt's immutable grant, plus the
/// per-route limits the routed client applies when the request itself does
/// not carry values.
#[derive(Debug, Clone, PartialEq)]
pub struct PlannedRoute {
    /// The grant-facing route (provider is the *connection name*).
    pub route: ModelRoute,
    /// Optional per-route limits (output-token ceiling, temperature).
    pub limits: Option<RouteLimits>,
}

/// Purpose-keyed and role-keyed route chains, resolved once per invocation
/// after invocation overrides (`--role`, `--provider`, `--model`).
///
/// A purpose (or role) whose configured chain plus overrides resolve to
/// nothing usable is simply absent from the plan — for example a provider
/// override that filters every route out of one purpose's chain.
/// [`Self::primary`] turns that absence into an actionable error.
pub struct ResolvedRoutePlan {
    /// Post-override chains by routing purpose, in configuration order.
    purposes: Vec<(String, Vec<PlannedRoute>)>,
    /// Post-override chains by model-role name, in catalog order.
    roles: Vec<(String, Vec<PlannedRoute>)>,
}

impl ResolvedRoutePlan {
    /// Resolve every configured purpose and role through the shared
    /// invocation-override semantics. A resolution error for one key leaves
    /// that key absent (its diagnostics surface through [`Self::primary`]).
    pub fn from_normalized(routing: &NormalizedRouting, overrides: &InvocationOverrides) -> Self {
        let purposes = routing
            .purpose_chains
            .iter()
            .filter_map(|(key, _)| {
                apply_invocation_overrides(routing, key, None, overrides)
                    .ok()
                    .map(|chain| (key.clone(), planned_chain(&chain)))
            })
            .collect();
        let roles = routing
            .role_chains
            .iter()
            .filter_map(|(role, _)| {
                role_chain_with_overrides(routing, role, overrides)
                    .ok()
                    .map(|chain| (role.clone(), planned_chain(&chain)))
            })
            .collect();
        ResolvedRoutePlan { purposes, roles }
    }

    /// The post-override chain for one routing purpose, when it resolved to
    /// a usable (non-empty) chain.
    pub fn chain(&self, purpose: &str) -> Option<&[PlannedRoute]> {
        self.purposes
            .iter()
            .find(|(key, _)| key == purpose)
            .map(|(_, chain)| chain.as_slice())
    }

    /// The primary (first) route for one routing purpose, or an actionable
    /// error when the purpose is absent from the plan (its chain resolved to
    /// nothing after overrides, or it was never configured).
    pub fn primary(&self, purpose: &str) -> Result<&PlannedRoute, RouteResolutionError> {
        self.chain(purpose)
            .and_then(|chain| chain.first())
            .ok_or_else(|| {
                RouteResolutionError::new(format!(
                    "no usable route remains for purpose \"{purpose}\" after invocation \
                     overrides; adjust the invocation overrides or add a routing entry for \
                     the purpose in relay configuration"
                ))
            })
    }

    /// Every purpose that resolved to a usable chain, in configuration
    /// order.
    pub fn purposes(&self) -> impl Iterator<Item = &str> {
        self.purposes.iter().map(|(key, _)| key.as_str())
    }

    /// The post-override chain keyed by model-role name (the granted route's
    /// `role`), when it resolved to a usable chain. Used so fallback can key
    /// on the granted route's role rather than re-deriving a purpose.
    pub fn role_chain(&self, role: &str) -> Option<&[PlannedRoute]> {
        self.roles
            .iter()
            .find(|(key, _)| key == role)
            .map(|(_, chain)| chain.as_slice())
    }
}

/// Convert a normalized chain into planned routes.
fn planned_chain(chain: &NormalizedChain) -> Vec<PlannedRoute> {
    chain
        .iter()
        .map(|route: &NormalizedRoute| PlannedRoute {
            route: ModelRoute {
                provider: route.provider.clone(),
                model: route.model.clone(),
                role: route.role.clone(),
            },
            limits: route.limits.clone(),
        })
        .collect()
}

/// Resolve one role's chain through the shared override semantics: a scoped
/// copy of the routing view promotes the role's own chain to the purpose
/// entry (so role-override replacement, provider filtering, and model
/// rewriting apply unchanged through `apply_invocation_overrides`), with the
/// legacy first-route fallback cleared so a missing role can never leak to
/// an unrelated route.
fn role_chain_with_overrides(
    routing: &NormalizedRouting,
    role: &str,
    overrides: &InvocationOverrides,
) -> Result<NormalizedChain, RouteResolutionError> {
    let mut scoped = routing.clone();
    scoped.purpose_chains.insert(
        role.to_owned(),
        routing.role_chains.get(role).cloned().unwrap_or_default(),
    );
    scoped.legacy_fallback = None;
    apply_invocation_overrides(&scoped, role, None, overrides)
}

// ---------------------------------------------------------------------------
// Routed client
// ---------------------------------------------------------------------------

/// The production [`ModelClient`]: routes each granted [`ModelRoute`] through
/// a [`ProviderRegistry`] of named connections and a [`ResolvedRoutePlan`].
///
/// ## Fallback policy (deterministic)
///
/// The fallback chain for a granted route is resolved as: (a) the granted
/// role's post-override chain, when its head equals the granted route; else
/// (b) the first purpose chain whose head equals the granted route; else
/// (c) the granted route alone (injected and test wirings). The chain is then
/// deduplicated by `(connection, model)` preserving order — the granted route
/// first.
///
/// The next route is attempted **only** for availability failures; everything
/// else fails the call on the spot:
///
/// | Provider failure | Action |
/// | --- | --- |
/// | `RateLimit`, `Server`, `Network`, `Timeout` | advance to the next route |
/// | `Canceled` | fail `ModelClientError::Canceled` (never fall back) |
/// | `UnsupportedCapability`, `Auth`, `InvalidResponse`, `Unknown` | fail `ModelClientError::Provider` (never fall back) |
/// | successful response | return it (never fall back) |
///
/// Registry-level outcomes: an unknown connection name fails hard with the
/// available names; an `Unavailable` connection on the primary (granted)
/// route fails hard naming the bounded reason; an `Unavailable` connection in
/// a fallback position is recorded and skipped.
///
/// ## Continuation route pinning
///
/// A transcript carrying any opaque
/// [`AssistantContent::Continuation`] block is provider-bound state: it is
/// pinned to the granted (primary) route and never falls back, because
/// another connection cannot resume it.
/// If that primary is unavailable or fails, the call fails with an
/// actionable error explaining the pinning (restart from a fresh turn
/// boundary, or make the primary connection available). A transcript with no
/// continuation blocks is fully Relay-owned and portable: fallback replays
/// the complete transcript on the next route unchanged.
///
/// Token usage is summed across every attempted route; every route that
/// received a call is recorded in
/// [`ModelTurnResponse::routes_attempted`](crate::model::ModelTurnResponse).
/// No storage lock is held anywhere in this path.
pub struct RoutedModelClient {
    registry: ProviderRegistry,
    plan: ResolvedRoutePlan,
}

impl RoutedModelClient {
    /// Combine a registry and a route plan into the production model client.
    pub fn new(registry: ProviderRegistry, plan: ResolvedRoutePlan) -> Self {
        RoutedModelClient { registry, plan }
    }

    /// The connection registry calls resolve through.
    pub fn registry(&self) -> &ProviderRegistry {
        &self.registry
    }

    /// The resolved route plan calls select chains from.
    pub fn plan(&self) -> &ResolvedRoutePlan {
        &self.plan
    }

    /// The deterministic fallback chain for a granted route: role chain by
    /// head match, else the first purpose chain by head match, else the
    /// granted route alone; deduplicated by `(connection, model)`.
    fn fallback_chain_for(&self, granted: &ModelRoute) -> Vec<PlannedRoute> {
        let matched: Option<&[PlannedRoute]> = if let Some(role) = granted.role.as_deref()
            && let Some(chain) = self.plan.role_chain(role)
            && chain.first().is_some_and(|head| &head.route == granted)
        {
            Some(chain)
        } else {
            self.plan.purposes.iter().find_map(|(_, chain)| {
                chain
                    .first()
                    .is_some_and(|head| &head.route == granted)
                    .then_some(chain.as_slice())
            })
        };
        let Some(chain) = matched else {
            // Injected/test wiring: the granted route carries no plan
            // context, so it stands alone (without limits).
            return vec![PlannedRoute {
                route: granted.clone(),
                limits: None,
            }];
        };

        let mut seen: Vec<(&str, &str)> = Vec::new();
        chain
            .iter()
            .filter(|planned| {
                let key = (
                    planned.route.provider.as_str(),
                    planned.route.model.as_str(),
                );
                if seen.contains(&key) {
                    false
                } else {
                    seen.push(key);
                    true
                }
            })
            .cloned()
            .collect()
    }
}

impl ModelClient for RoutedModelClient {
    fn complete(
        &self,
        route: &ModelRoute,
        request: &ModelTurnRequest,
        signal: &CancelToken,
    ) -> Result<ModelTurnResponse, ModelClientError> {
        // Route pinning: opaque provider continuation blocks in the
        // transcript belong to the connection that produced them and must
        // never cross routes, so the call is pinned to the granted (primary)
        // route alone. A transcript without continuation blocks is fully
        // Relay-owned and portable, and fallback replays it unchanged.
        let pinned = transcript_carries_continuation(&request.messages);
        let chain = if pinned {
            self.fallback_chain_for(route)
                .into_iter()
                .take(1)
                .collect::<Vec<_>>()
        } else {
            self.fallback_chain_for(route)
        };
        let mut attempts: Vec<RouteAttemptFailure> = Vec::new();
        let mut routes_attempted: Vec<ModelRoute> = Vec::new();
        let mut input_tokens: u64 = 0;
        let mut output_tokens: u64 = 0;

        for (index, planned) in chain.iter().enumerate() {
            // Between attempts, honor cancellation before any further call.
            if index > 0 && signal.is_cancelled() {
                return Err(ModelClientError::Canceled(canceled_reason(signal)));
            }

            let provider = match self.registry.connection(&planned.route.provider) {
                None => {
                    attempts.push(RouteAttemptFailure::from_message(
                        planned.route.clone(),
                        ProviderErrorKind::Unknown,
                        format!(
                            "unknown provider connection \"{}\" (available connections: {})",
                            planned.route.provider,
                            self.registry.names_listing()
                        ),
                    ));
                    return Err(provider_failure(&planned.route, pinned, attempts));
                }
                Some(ConnectionState::Unavailable { reason, .. }) => {
                    attempts.push(RouteAttemptFailure::from_message(
                        planned.route.clone(),
                        ProviderErrorKind::Unknown,
                        format!("connection unavailable: {reason}"),
                    ));
                    if index == 0 {
                        // An unavailable primary fails the call outright;
                        // only fallback positions may be skipped.
                        return Err(provider_failure(&planned.route, pinned, attempts));
                    }
                    continue;
                }
                Some(ConnectionState::Ready { provider, .. }) => Arc::clone(provider),
            };

            routes_attempted.push(planned.route.clone());

            let provider_request = ModelRequest {
                model: planned.route.model.clone(),
                system: request.system.clone(),
                messages: request.messages.clone(),
                tools: request.tools.clone(),
                structured_output: None,
                max_output_tokens: request
                    .max_output_tokens
                    .or_else(|| route_limit_max_output_tokens(planned)),
                temperature: request
                    .temperature
                    .or_else(|| route_limit_temperature(planned)),
                purpose: route.role.clone(),
                signal: Some(signal.clone()),
            };

            match collect_turn(provider.stream(&provider_request)) {
                Ok(turn) => {
                    if signal.is_cancelled() {
                        return Err(ModelClientError::Canceled(canceled_reason(signal)));
                    }
                    input_tokens =
                        input_tokens.saturating_add(turn.usage.input_tokens.max(0) as u64);
                    output_tokens =
                        output_tokens.saturating_add(turn.usage.output_tokens.max(0) as u64);
                    return Ok(ModelTurnResponse {
                        blocks: turn.blocks,
                        input_tokens,
                        output_tokens,
                        routes_attempted,
                    });
                }
                Err(failure) => {
                    let kind = failure.kind;
                    attempts.push(RouteAttemptFailure::from_failure(
                        planned.route.clone(),
                        &failure,
                    ));
                    match kind {
                        ProviderErrorKind::RateLimit
                        | ProviderErrorKind::Server
                        | ProviderErrorKind::Network
                        | ProviderErrorKind::Timeout => continue,
                        ProviderErrorKind::Canceled => {
                            return Err(ModelClientError::Canceled(failure.message));
                        }
                        ProviderErrorKind::UnsupportedCapability
                        | ProviderErrorKind::Auth
                        | ProviderErrorKind::InvalidResponse
                        | ProviderErrorKind::Unknown => {
                            return Err(provider_failure(&planned.route, pinned, attempts));
                        }
                    }
                }
            }
        }

        // Every route failed with an availability failure (or was skipped).
        let failed_on = chain
            .last()
            .map(|planned| planned.route.clone())
            .unwrap_or_else(|| route.clone());
        Err(provider_failure(&failed_on, pinned, attempts))
    }
}

/// Whether the transcript carries any opaque provider continuation block.
/// Continuation state is bound to the connection that produced it; a
/// transcript carrying any is not portable across routes.
fn transcript_carries_continuation(messages: &[ModelMessage]) -> bool {
    messages.iter().any(|message| match message {
        ModelMessage::Assistant { content } => content
            .iter()
            .any(|block| matches!(block, AssistantContent::Continuation { .. })),
        _ => false,
    })
}

/// The terminal provider failure for a call. When the call was pinned to its
/// primary route by continuation state, an actionable note explaining the
/// pinning is appended so the failure says why no fallback happened and what
/// to do about it.
fn provider_failure(
    route: &ModelRoute,
    pinned: bool,
    attempts: Vec<RouteAttemptFailure>,
) -> ModelClientError {
    let mut attempts = attempts;
    if pinned {
        attempts.push(RouteAttemptFailure::from_message(
            route.clone(),
            ProviderErrorKind::Unknown,
            format!(
                "the conversation carries provider continuation state that only connection \
                 \"{}\" can resume, so the call is route-pinned and did not fall back; restart \
                 the attempt from a fresh turn boundary or make connection \"{}\" available",
                route.provider, route.provider
            ),
        ));
    }
    ModelClientError::Provider(ModelCallFailure::new(attempts))
}

/// The route's output-token ceiling as `u64` (negative limits are ignored).
fn route_limit_max_output_tokens(planned: &PlannedRoute) -> Option<u64> {
    planned
        .limits
        .as_ref()
        .and_then(|limits| limits.max_output_tokens)
        .filter(|limit| *limit > 0)
        .map(|limit| limit as u64)
}

/// The route's temperature limit.
fn route_limit_temperature(planned: &PlannedRoute) -> Option<f64> {
    planned
        .limits
        .as_ref()
        .and_then(|limits| limits.temperature)
}

/// The recorded cancellation reason, with a stable fallback.
fn canceled_reason(signal: &CancelToken) -> String {
    signal.reason().unwrap_or_else(|| "canceled".to_owned())
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use relay_config::{NormalizedRouting, RouteLimits, normalize_routing};
    use relay_providers::contract::{
        AdapterKind, AssistantContent, FinishReason, ProviderEventStream, ProviderFailure,
        ProviderStreamEvent, TokenUsage, ToolCall,
    };
    use std::collections::VecDeque;
    use std::sync::Mutex;

    // ------------------------------------------------------------------
    // A tiny scripted provider: pops one scripted turn per call and records
    // every request it received.
    // ------------------------------------------------------------------

    enum ScriptedTurn {
        Respond {
            text: String,
            usage: TokenUsage,
        },
        RespondBlocks {
            blocks: Vec<AssistantContent>,
            usage: TokenUsage,
        },
        Fail(ProviderErrorKind, &'static str),
    }

    struct ScriptedProvider {
        turns: Mutex<VecDeque<ScriptedTurn>>,
        requests: Mutex<Vec<ModelRequest>>,
    }

    impl ScriptedProvider {
        fn new(turns: Vec<ScriptedTurn>) -> Arc<Self> {
            Arc::new(ScriptedProvider {
                turns: Mutex::new(turns.into()),
                requests: Mutex::new(Vec::new()),
            })
        }

        fn requests(&self) -> Vec<ModelRequest> {
            self.requests
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .clone()
        }
    }

    impl ModelProvider for ScriptedProvider {
        fn name(&self) -> AdapterKind {
            AdapterKind::Fake
        }

        fn models(&self) -> Vec<String> {
            vec!["scripted".to_owned()]
        }

        fn stream(&self, request: &ModelRequest) -> ProviderEventStream<'_> {
            let turn = {
                let mut turns = self
                    .turns
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner());
                turns.pop_front()
            };
            self.requests
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .push(request.clone());
            let events: Vec<ProviderStreamEvent> = match turn {
                Some(ScriptedTurn::Respond { text, usage }) => vec![
                    ProviderStreamEvent::TextDelta { text },
                    ProviderStreamEvent::Usage { usage },
                    ProviderStreamEvent::Completed {
                        finish_reason: FinishReason::Stop,
                        usage,
                    },
                ],
                Some(ScriptedTurn::RespondBlocks { blocks, usage }) => {
                    let mut events = Vec::new();
                    for block in blocks {
                        match block {
                            AssistantContent::Text { text } => {
                                events.push(ProviderStreamEvent::TextDelta { text });
                            }
                            AssistantContent::ToolCall { call } => {
                                events.push(ProviderStreamEvent::ToolCall { call });
                            }
                            AssistantContent::Continuation {
                                discriminator,
                                data,
                            } => {
                                events.push(ProviderStreamEvent::Continuation {
                                    discriminator,
                                    data,
                                });
                            }
                        }
                    }
                    events.push(ProviderStreamEvent::Usage { usage });
                    events.push(ProviderStreamEvent::Completed {
                        finish_reason: FinishReason::ToolCalls,
                        usage,
                    });
                    events
                }
                Some(ScriptedTurn::Fail(kind, message)) => {
                    vec![ProviderStreamEvent::Error {
                        error: ProviderFailure::new(kind, message),
                    }]
                }
                None => vec![ProviderStreamEvent::Completed {
                    finish_reason: FinishReason::Stop,
                    usage: TokenUsage::default(),
                }],
            };
            Box::new(events.into_iter())
        }
    }

    fn respond(text: &str, input: i64, output: i64) -> ScriptedTurn {
        ScriptedTurn::Respond {
            text: text.to_owned(),
            usage: TokenUsage {
                input_tokens: input,
                output_tokens: output,
            },
        }
    }

    fn respond_blocks(blocks: Vec<AssistantContent>, input: i64, output: i64) -> ScriptedTurn {
        ScriptedTurn::RespondBlocks {
            blocks,
            usage: TokenUsage {
                input_tokens: input,
                output_tokens: output,
            },
        }
    }

    fn tool_call(id: &str, name: &str, path: &str) -> AssistantContent {
        AssistantContent::ToolCall {
            call: ToolCall {
                id: id.to_owned(),
                name: name.to_owned(),
                arguments: serde_json::json!({ "path": path }),
            },
        }
    }

    // ------------------------------------------------------------------
    // Builders
    // ------------------------------------------------------------------

    fn ready(provider: Arc<dyn ModelProvider>) -> ConnectionState {
        ConnectionState::Ready {
            adapter: AdapterKind::Fake,
            provider,
        }
    }

    fn down(reason: &str) -> ConnectionState {
        ConnectionState::Unavailable {
            adapter: AdapterKind::Fake,
            reason: reason.to_owned(),
        }
    }

    fn model_route(provider: &str, model: &str, role: Option<&str>) -> ModelRoute {
        ModelRoute {
            provider: provider.to_owned(),
            model: model.to_owned(),
            role: role.map(str::to_owned),
        }
    }

    fn normalized_route(provider: &str, model: &str, role: Option<&str>) -> NormalizedRoute {
        NormalizedRoute {
            provider: provider.to_owned(),
            model: model.to_owned(),
            role: role.map(str::to_owned),
            limits: None,
        }
    }

    /// A routing view with exactly the given purpose and role chains.
    fn routing(
        purposes: Vec<(&str, NormalizedChain)>,
        roles: Vec<(&str, NormalizedChain)>,
    ) -> NormalizedRouting {
        let mut routing = NormalizedRouting::default();
        for (key, chain) in purposes {
            routing.purpose_chains.insert(key.to_owned(), chain);
        }
        for (role, chain) in roles {
            routing.role_chains.insert(role.to_owned(), chain);
        }
        routing
    }

    fn no_overrides() -> InvocationOverrides {
        InvocationOverrides::default()
    }

    fn client(registry: ProviderRegistry, plan: ResolvedRoutePlan) -> RoutedModelClient {
        RoutedModelClient::new(registry, plan)
    }

    fn prompt(text: &str) -> ModelTurnRequest {
        ModelTurnRequest::from_prompt(text)
    }

    // ------------------------------------------------------------------
    // Per-route provider selection
    // ------------------------------------------------------------------

    #[test]
    fn grants_select_their_own_connection_and_model() {
        let provider_a = ScriptedProvider::new(vec![respond("from-a", 3, 5)]);
        let provider_b = ScriptedProvider::new(vec![respond("from-b", 7, 11)]);
        let registry = ProviderRegistry::from_connections([
            ("conn-a", ready(provider_a.clone())),
            ("conn-b", ready(provider_b.clone())),
        ]);
        let plan = ResolvedRoutePlan::from_normalized(
            &routing(
                vec![
                    (
                        "intent",
                        vec![normalized_route("conn-a", "model-a", Some("fast"))],
                    ),
                    (
                        "delivery",
                        vec![normalized_route("conn-b", "model-b", Some("fast"))],
                    ),
                ],
                Vec::new(),
            ),
            &no_overrides(),
        );
        let client = client(registry, plan);

        let response = client
            .complete(
                &model_route("conn-a", "model-a", Some("fast")),
                &prompt("to a"),
                &CancelToken::new(),
            )
            .expect("a completes");
        assert_eq!(response.text(), "from-a");
        assert_eq!(response.input_tokens, 3);
        assert_eq!(response.output_tokens, 5);
        assert_eq!(
            response.routes_attempted,
            vec![model_route("conn-a", "model-a", Some("fast"))]
        );

        let response = client
            .complete(
                &model_route("conn-b", "model-b", Some("fast")),
                &prompt("to b"),
                &CancelToken::new(),
            )
            .expect("b completes");
        assert_eq!(response.text(), "from-b");

        // Each connection saw exactly its own call, with its own model id.
        assert_eq!(
            provider_a
                .requests()
                .iter()
                .map(|request| request.model.as_str())
                .collect::<Vec<_>>(),
            ["model-a"]
        );
        assert_eq!(
            provider_b
                .requests()
                .iter()
                .map(|request| request.model.as_str())
                .collect::<Vec<_>>(),
            ["model-b"]
        );
    }

    // ------------------------------------------------------------------
    // Ordered, bounded fallback
    // ------------------------------------------------------------------

    #[test]
    fn retryable_failures_advance_to_the_next_route_in_order() {
        for kind in [
            ProviderErrorKind::RateLimit,
            ProviderErrorKind::Server,
            ProviderErrorKind::Network,
            ProviderErrorKind::Timeout,
        ] {
            let flaky = ScriptedProvider::new(vec![ScriptedTurn::Fail(kind, "availability")]);
            let backup = ScriptedProvider::new(vec![respond("from-backup", 4, 6)]);
            let registry = ProviderRegistry::from_connections([
                ("flaky", ready(flaky)),
                ("backup", ready(backup.clone())),
            ]);
            let plan = ResolvedRoutePlan::from_normalized(
                &routing(
                    vec![(
                        "intent",
                        vec![
                            normalized_route("flaky", "m1", Some("fast")),
                            normalized_route("backup", "m2", Some("fast")),
                        ],
                    )],
                    Vec::new(),
                ),
                &no_overrides(),
            );
            let client = client(registry, plan);

            let response = client
                .complete(
                    &model_route("flaky", "m1", Some("fast")),
                    &prompt("x"),
                    &CancelToken::new(),
                )
                .expect("fallback completes");
            assert_eq!(response.text(), "from-backup", "kind {kind:?}");
            assert_eq!(
                response.routes_attempted,
                vec![
                    model_route("flaky", "m1", Some("fast")),
                    model_route("backup", "m2", Some("fast")),
                ]
            );
            assert_eq!(response.input_tokens, 4);
            assert_eq!(response.output_tokens, 6);
        }
    }

    #[test]
    fn exhausted_chains_fail_with_every_attempt_listed_in_order() {
        let first = ScriptedProvider::new(vec![ScriptedTurn::Fail(
            ProviderErrorKind::RateLimit,
            "throttled",
        )]);
        let second =
            ScriptedProvider::new(vec![ScriptedTurn::Fail(ProviderErrorKind::Server, "down")]);
        let registry = ProviderRegistry::from_connections([
            ("primary", ready(first)),
            ("secondary", ready(second)),
        ]);
        let plan = ResolvedRoutePlan::from_normalized(
            &routing(
                vec![(
                    "intent",
                    vec![
                        normalized_route("primary", "m1", Some("fast")),
                        normalized_route("secondary", "m2", Some("fast")),
                        // Duplicate of the primary: deduplicated away.
                        normalized_route("primary", "m1", Some("fast")),
                    ],
                )],
                Vec::new(),
            ),
            &no_overrides(),
        );
        let client = client(registry, plan);

        let error = client
            .complete(
                &model_route("primary", "m1", Some("fast")),
                &prompt("x"),
                &CancelToken::new(),
            )
            .expect_err("chain exhausted");
        let ModelClientError::Provider(failure) = &error else {
            panic!("expected a provider failure, got {error}");
        };
        assert_eq!(failure.attempts().len(), 2, "dedup removed the third route");
        assert_eq!(
            error.to_string(),
            "model call failed: primary/m1: rate-limit (throttled); secondary/m2: server (down)"
        );
    }

    #[test]
    fn success_never_falls_back() {
        let primary = ScriptedProvider::new(vec![respond("primary answer", 1, 1)]);
        let backup = ScriptedProvider::new(vec![respond("backup answer", 1, 1)]);
        let registry = ProviderRegistry::from_connections([
            ("primary", ready(primary.clone())),
            ("backup", ready(backup.clone())),
        ]);
        let plan = ResolvedRoutePlan::from_normalized(
            &routing(
                vec![(
                    "intent",
                    vec![
                        normalized_route("primary", "m1", None),
                        normalized_route("backup", "m2", None),
                    ],
                )],
                Vec::new(),
            ),
            &no_overrides(),
        );
        let client = client(registry, plan);

        let response = client
            .complete(
                &model_route("primary", "m1", None),
                &prompt("x"),
                &CancelToken::new(),
            )
            .expect("completes");
        assert_eq!(response.text(), "primary answer");
        assert!(backup.requests().is_empty(), "backup must not be called");
    }

    // ------------------------------------------------------------------
    // No fallback classifications
    // ------------------------------------------------------------------

    #[test]
    fn cancellation_never_falls_back() {
        let canceled = ScriptedProvider::new(vec![ScriptedTurn::Fail(
            ProviderErrorKind::Canceled,
            "request aborted before completion",
        )]);
        let backup = ScriptedProvider::new(vec![respond("backup", 1, 1)]);
        let registry = ProviderRegistry::from_connections([
            ("c", ready(canceled)),
            ("b", ready(backup.clone())),
        ]);
        let plan = ResolvedRoutePlan::from_normalized(
            &routing(
                vec![(
                    "intent",
                    vec![
                        normalized_route("c", "m1", None),
                        normalized_route("b", "m2", None),
                    ],
                )],
                Vec::new(),
            ),
            &no_overrides(),
        );
        let client = client(registry, plan);

        let error = client
            .complete(
                &model_route("c", "m1", None),
                &prompt("x"),
                &CancelToken::new(),
            )
            .expect_err("canceled");
        assert!(matches!(error, ModelClientError::Canceled(_)), "{error}");
        assert!(
            backup.requests().is_empty(),
            "cancellation must not fall back"
        );
    }

    #[test]
    fn non_availability_failures_never_fall_back() {
        for kind in [
            ProviderErrorKind::UnsupportedCapability,
            ProviderErrorKind::Auth,
            ProviderErrorKind::InvalidResponse,
            ProviderErrorKind::Unknown,
        ] {
            let failing = ScriptedProvider::new(vec![ScriptedTurn::Fail(kind, "no retry")]);
            let backup = ScriptedProvider::new(vec![respond("backup", 1, 1)]);
            let registry = ProviderRegistry::from_connections([
                ("f", ready(failing)),
                ("b", ready(backup.clone())),
            ]);
            let plan = ResolvedRoutePlan::from_normalized(
                &routing(
                    vec![(
                        "intent",
                        vec![
                            normalized_route("f", "m1", None),
                            normalized_route("b", "m2", None),
                        ],
                    )],
                    Vec::new(),
                ),
                &no_overrides(),
            );
            let client = client(registry, plan);

            let error = client
                .complete(
                    &model_route("f", "m1", None),
                    &prompt("x"),
                    &CancelToken::new(),
                )
                .expect_err("fails without fallback");
            let ModelClientError::Provider(failure) = &error else {
                panic!("expected a provider failure for {kind:?}, got {error}");
            };
            assert_eq!(failure.attempts().len(), 1, "one attempt for {kind:?}");
            assert_eq!(failure.terminal().unwrap().kind, kind);
            assert!(backup.requests().is_empty(), "{kind:?} must not fall back");
        }
    }

    #[test]
    fn a_pre_canceled_signal_discards_the_turn_and_reports_cancellation() {
        // The routed client does not pre-check the signal before the first
        // attempt: real providers fail fast on canceled signals themselves,
        // and a provider that ignores the signal is caught by the post-turn
        // check, which discards the turn.
        let provider = ScriptedProvider::new(vec![respond("late", 1, 1)]);
        let registry = ProviderRegistry::from_connections([("c", ready(provider.clone()))]);
        let plan = ResolvedRoutePlan::from_normalized(
            &routing(
                vec![("intent", vec![normalized_route("c", "m1", None)])],
                Vec::new(),
            ),
            &no_overrides(),
        );
        let client = client(registry, plan);

        let signal = CancelToken::new();
        signal.cancel("user interrupt");
        let error = client
            .complete(&model_route("c", "m1", None), &prompt("x"), &signal)
            .expect_err("canceled");
        let ModelClientError::Canceled(reason) = &error else {
            panic!("expected cancellation, got {error}");
        };
        assert_eq!(reason, "user interrupt");
        assert_eq!(provider.requests().len(), 1);
    }

    // ------------------------------------------------------------------
    // Native tool turns through the routed client
    // ------------------------------------------------------------------

    #[test]
    fn tool_use_turns_return_ordered_blocks_and_native_calls() {
        // A successful route answers with interleaved text and two tool
        // calls; the response keeps the authoritative order and exposes the
        // native calls through `ModelClient` without text-encoding them.
        let provider = ScriptedProvider::new(vec![respond_blocks(
            vec![
                AssistantContent::Text {
                    text: "let me check".to_owned(),
                },
                tool_call("call-1", "read-file", "a"),
                tool_call("call-2", "list-files", "."),
                AssistantContent::Text {
                    text: " then finish".to_owned(),
                },
            ],
            7,
            13,
        )]);
        let registry = ProviderRegistry::from_connections([("c", ready(provider))]);
        let plan = ResolvedRoutePlan::from_normalized(
            &routing(
                vec![("intent", vec![normalized_route("c", "m1", Some("coding"))])],
                Vec::new(),
            ),
            &no_overrides(),
        );
        let client = client(registry, plan);

        let mut request = prompt("x");
        request.tools = Some(vec![relay_providers::contract::ToolSpec {
            name: "read-file".into(),
            description: "Reads a file".into(),
            parameters: serde_json::Map::from_iter([(
                "type".to_owned(),
                serde_json::json!("object"),
            )]),
        }]);
        let response = client
            .complete(
                &model_route("c", "m1", Some("coding")),
                &request,
                &CancelToken::new(),
            )
            .expect("completes");

        let kinds: Vec<&str> = response
            .blocks
            .iter()
            .map(|block| match block {
                AssistantContent::Text { .. } => "text",
                AssistantContent::ToolCall { .. } => "tool-call",
                AssistantContent::Continuation { .. } => "continuation",
            })
            .collect();
        assert_eq!(kinds, ["text", "tool-call", "tool-call", "text"]);
        assert_eq!(response.text(), "let me check then finish");
        assert_eq!(
            response
                .tool_calls()
                .iter()
                .map(|call| (call.id.as_str(), call.name.as_str()))
                .collect::<Vec<_>>(),
            [("call-1", "read-file"), ("call-2", "list-files")]
        );
        // Usage and route diagnostics are complete for the successful route.
        assert_eq!(response.input_tokens, 7);
        assert_eq!(response.output_tokens, 13);
        assert_eq!(
            response.routes_attempted,
            vec![model_route("c", "m1", Some("coding"))]
        );
    }

    #[test]
    fn tool_turn_usage_and_routes_sum_across_a_fallback() {
        // The primary fails with an availability failure after consuming
        // tokens; the fallback answers a tool turn. Summed usage and both
        // routes must be reported.
        let flaky = ScriptedProvider::new(vec![ScriptedTurn::Fail(
            ProviderErrorKind::RateLimit,
            "throttled",
        )]);
        let backup = ScriptedProvider::new(vec![respond_blocks(
            vec![tool_call("call-1", "read-file", "a")],
            5,
            9,
        )]);
        let registry = ProviderRegistry::from_connections([
            ("flaky", ready(flaky)),
            ("backup", ready(backup)),
        ]);
        let plan = ResolvedRoutePlan::from_normalized(
            &routing(
                vec![(
                    "intent",
                    vec![
                        normalized_route("flaky", "m1", None),
                        normalized_route("backup", "m2", None),
                    ],
                )],
                Vec::new(),
            ),
            &no_overrides(),
        );
        let client = client(registry, plan);

        let response = client
            .complete(
                &model_route("flaky", "m1", None),
                &prompt("x"),
                &CancelToken::new(),
            )
            .expect("fallback completes the tool turn");
        assert_eq!(response.tool_calls().len(), 1);
        assert_eq!(response.input_tokens, 5);
        assert_eq!(response.output_tokens, 9);
        assert_eq!(
            response.routes_attempted,
            vec![
                model_route("flaky", "m1", None),
                model_route("backup", "m2", None),
            ]
        );
    }

    #[test]
    fn provider_selection_stays_model_independent_on_tool_turns() {
        // The granted route selects the connection; the model id travels
        // with the route, and nothing about a tool-carrying request changes
        // which provider serves it.
        let provider = ScriptedProvider::new(vec![respond_blocks(
            vec![tool_call("call-1", "read-file", "a")],
            1,
            1,
        )]);
        let registry = ProviderRegistry::from_connections([("conn-a", ready(provider.clone()))]);
        let plan = ResolvedRoutePlan::from_normalized(
            &routing(
                vec![(
                    "intent",
                    vec![normalized_route("conn-a", "model-a", Some("coding"))],
                )],
                Vec::new(),
            ),
            &no_overrides(),
        );
        let client = client(registry, plan);

        let response = client
            .complete(
                &model_route("conn-a", "model-a", Some("coding")),
                &prompt("x"),
                &CancelToken::new(),
            )
            .expect("completes");
        assert_eq!(response.tool_calls().len(), 1);
        assert_eq!(
            provider
                .requests()
                .iter()
                .map(|request| (request.model.as_str(), request.purpose.as_deref()))
                .collect::<Vec<_>>(),
            [("model-a", Some("coding"))]
        );
    }

    // ------------------------------------------------------------------
    // Continuation route pinning
    // ------------------------------------------------------------------

    /// A transcript whose assistant turn carries an opaque continuation
    /// block: provider-bound state that only its producing route can resume.
    fn continuation_transcript(prompt_text: &str) -> ModelTurnRequest {
        ModelTurnRequest {
            system: None,
            messages: vec![
                ModelMessage::user_text(prompt_text),
                ModelMessage::Assistant {
                    content: vec![
                        AssistantContent::Text {
                            text: "thinking...".to_owned(),
                        },
                        AssistantContent::Continuation {
                            discriminator: "thinking".to_owned(),
                            data: "opaque-signed-state".to_owned(),
                        },
                        tool_call("call-1", "read-file", "a"),
                    ],
                },
                ModelMessage::tool_result("call-1", "file body", false).expect("bounded"),
            ],
            tools: None,
            max_output_tokens: None,
            temperature: None,
        }
    }

    #[test]
    fn continuation_transcripts_never_fall_back_to_another_route() {
        // The primary fails with a retryable availability failure and the
        // backup would happily answer — but the continuation block pins the
        // call to the primary, so the backup must not be called.
        let primary = ScriptedProvider::new(vec![ScriptedTurn::Fail(
            ProviderErrorKind::RateLimit,
            "throttled",
        )]);
        let backup = ScriptedProvider::new(vec![respond("backup answer", 1, 1)]);
        let registry = ProviderRegistry::from_connections([
            ("primary", ready(primary)),
            ("backup", ready(backup.clone())),
        ]);
        let plan = ResolvedRoutePlan::from_normalized(
            &routing(
                vec![(
                    "intent",
                    vec![
                        normalized_route("primary", "m1", None),
                        normalized_route("backup", "m2", None),
                    ],
                )],
                Vec::new(),
            ),
            &no_overrides(),
        );
        let client = client(registry, plan);

        let error = client
            .complete(
                &model_route("primary", "m1", None),
                &continuation_transcript("x"),
                &CancelToken::new(),
            )
            .expect_err("pinned calls never fall back");
        let ModelClientError::Provider(failure) = &error else {
            panic!("expected a provider failure, got {error}");
        };
        assert!(
            backup.requests().is_empty(),
            "the continuation must not be sent to another route"
        );
        // The failure is actionable: it explains the pinning and both ways
        // out.
        let message = error.to_string();
        assert!(message.contains("route-pinned"), "{message}");
        assert!(message.contains("only connection \"primary\""), "{message}");
        assert!(message.contains("fresh turn boundary"), "{message}");
        assert!(
            message.contains("make connection \"primary\" available"),
            "{message}"
        );
        assert_eq!(failure.attempts().len(), 2, "failure plus the pin note");
    }

    #[test]
    fn pinned_continuation_failures_when_the_primary_is_unavailable() {
        // The primary connection exists but cannot serve (missing
        // credential); the pin note must still explain why there was no
        // fallback.
        let registry = ProviderRegistry::from_connections([
            (
                "primary",
                down("The RELAY_TEST_NEVER_SET_KEY environment variable is missing"),
            ),
            (
                "backup",
                ready(ScriptedProvider::new(vec![respond("backup", 1, 1)])),
            ),
        ]);
        let plan = ResolvedRoutePlan::from_normalized(
            &routing(
                vec![(
                    "intent",
                    vec![
                        normalized_route("primary", "m1", None),
                        normalized_route("backup", "m2", None),
                    ],
                )],
                Vec::new(),
            ),
            &no_overrides(),
        );
        let client = client(registry, plan);

        let error = client
            .complete(
                &model_route("primary", "m1", None),
                &continuation_transcript("x"),
                &CancelToken::new(),
            )
            .expect_err("unavailable pinned primary fails");
        let message = error.to_string();
        assert!(message.contains("connection unavailable"), "{message}");
        assert!(message.contains("route-pinned"), "{message}");
    }

    #[test]
    fn portable_transcripts_fall_back_replaying_the_complete_transcript() {
        // A tool-loop transcript WITHOUT continuation blocks is fully
        // Relay-owned and portable: after an availability failure the next
        // route must receive the complete transcript verbatim.
        let primary =
            ScriptedProvider::new(vec![ScriptedTurn::Fail(ProviderErrorKind::Server, "down")]);
        let backup = ScriptedProvider::new(vec![respond("done", 2, 3)]);
        let registry = ProviderRegistry::from_connections([
            ("primary", ready(primary)),
            ("backup", ready(backup.clone())),
        ]);
        let plan = ResolvedRoutePlan::from_normalized(
            &routing(
                vec![(
                    "intent",
                    vec![
                        normalized_route("primary", "m1", None),
                        normalized_route("backup", "m2", None),
                    ],
                )],
                Vec::new(),
            ),
            &no_overrides(),
        );
        let client = client(registry, plan);

        let request = ModelTurnRequest {
            system: Some("be brief".to_owned()),
            messages: vec![
                ModelMessage::user_text("read the file"),
                ModelMessage::Assistant {
                    content: vec![tool_call("call-1", "read-file", "a")],
                },
                ModelMessage::tool_result("call-1", "file body", false).expect("bounded"),
            ],
            tools: None,
            max_output_tokens: None,
            temperature: None,
        };
        let response = client
            .complete(
                &model_route("primary", "m1", None),
                &request,
                &CancelToken::new(),
            )
            .expect("portable transcripts fall back");
        assert_eq!(response.text(), "done");
        assert_eq!(
            response.routes_attempted,
            vec![
                model_route("primary", "m1", None),
                model_route("backup", "m2", None),
            ]
        );
        // The fallback saw the complete transcript, byte for byte.
        assert_eq!(backup.requests().len(), 1);
        assert_eq!(backup.requests()[0].messages, request.messages);
        assert_eq!(backup.requests()[0].system.as_deref(), Some("be brief"));
    }

    #[test]
    fn a_continuation_free_transcript_on_a_failing_solo_route_reports_plainly() {
        // Without continuation state the same failure carries no pin note:
        // the ordinary availability failure is the whole story.
        let primary = ScriptedProvider::new(vec![ScriptedTurn::Fail(
            ProviderErrorKind::RateLimit,
            "throttled",
        )]);
        let registry = ProviderRegistry::from_connections([("primary", ready(primary))]);
        let plan = ResolvedRoutePlan::from_normalized(
            &routing(
                vec![("intent", vec![normalized_route("primary", "m1", None)])],
                Vec::new(),
            ),
            &no_overrides(),
        );
        let client = client(registry, plan);
        let error = client
            .complete(
                &model_route("primary", "m1", None),
                &prompt("x"),
                &CancelToken::new(),
            )
            .expect_err("fails");
        assert!(!error.to_string().contains("route-pinned"), "{}", error);
    }

    // ------------------------------------------------------------------
    // Registry outcomes
    // ------------------------------------------------------------------

    #[test]
    fn unknown_connections_fail_hard_listing_available_names() {
        let provider = ScriptedProvider::new(vec![respond("ok", 1, 1)]);
        let registry = ProviderRegistry::from_connections([
            ("one", ready(provider)),
            ("two", ready(ScriptedProvider::new(Vec::new()))),
        ]);
        let plan = ResolvedRoutePlan::from_normalized(
            &routing(
                vec![(
                    "intent",
                    vec![
                        normalized_route("ghost", "m1", None),
                        normalized_route("one", "m2", None),
                    ],
                )],
                Vec::new(),
            ),
            &no_overrides(),
        );
        let client = client(registry, plan);

        let error = client
            .complete(
                &model_route("ghost", "m1", None),
                &prompt("x"),
                &CancelToken::new(),
            )
            .expect_err("unknown connection");
        let ModelClientError::Provider(failure) = &error else {
            panic!("expected a provider failure, got {error}");
        };
        assert!(
            error
                .to_string()
                .contains("unknown provider connection \"ghost\""),
            "{}",
            error
        );
        assert!(
            error
                .to_string()
                .contains("available connections: one, two"),
            "{}",
            error
        );
        assert_eq!(failure.attempts().len(), 1);
    }

    #[test]
    fn an_unavailable_primary_fails_hard_with_its_reason() {
        let registry = ProviderRegistry::from_connections([
            (
                "gateway",
                down("The RELAY_TEST_GATEWAY_KEY environment variable is missing"),
            ),
            (
                "fake",
                ready(ScriptedProvider::new(vec![respond("ok", 1, 1)])),
            ),
        ]);
        let plan = ResolvedRoutePlan::from_normalized(
            &routing(
                vec![(
                    "intent",
                    vec![
                        normalized_route("gateway", "claude-sonnet-5-5", None),
                        normalized_route("fake", "m2", None),
                    ],
                )],
                Vec::new(),
            ),
            &no_overrides(),
        );
        let client = client(registry, plan);

        let error = client
            .complete(
                &model_route("gateway", "claude-sonnet-5-5", None),
                &prompt("x"),
                &CancelToken::new(),
            )
            .expect_err("unavailable primary fails");
        let ModelClientError::Provider(failure) = &error else {
            panic!("expected a provider failure, got {error}");
        };
        assert!(
            error.to_string().contains("connection unavailable"),
            "{}",
            error
        );
        assert!(
            error
                .to_string()
                .contains("The RELAY_TEST_GATEWAY_KEY environment variable is missing"),
            "{}",
            error
        );
        assert_eq!(
            failure.attempts().len(),
            1,
            "no fallback after unavailable primary"
        );
    }

    #[test]
    fn an_unavailable_fallback_is_skipped_and_the_next_route_tried() {
        let flaky = ScriptedProvider::new(vec![ScriptedTurn::Fail(
            ProviderErrorKind::Timeout,
            "timed out",
        )]);
        let final_provider = ScriptedProvider::new(vec![respond("third try", 2, 3)]);
        let registry = ProviderRegistry::from_connections([
            ("flaky", ready(flaky)),
            (
                "down",
                down("The RELAY_TEST_DOWN_KEY environment variable is missing"),
            ),
            ("final", ready(final_provider.clone())),
        ]);
        let plan = ResolvedRoutePlan::from_normalized(
            &routing(
                vec![(
                    "intent",
                    vec![
                        normalized_route("flaky", "m1", None),
                        normalized_route("down", "m2", None),
                        normalized_route("final", "m3", None),
                    ],
                )],
                Vec::new(),
            ),
            &no_overrides(),
        );
        let client = client(registry, plan);

        let response = client
            .complete(
                &model_route("flaky", "m1", None),
                &prompt("x"),
                &CancelToken::new(),
            )
            .expect("skips the unavailable fallback");
        assert_eq!(response.text(), "third try");
        assert_eq!(
            response.routes_attempted,
            vec![
                model_route("flaky", "m1", None),
                model_route("final", "m3", None),
            ],
            "skipped routes are not listed as attempted"
        );
    }

    // ------------------------------------------------------------------
    // Limits
    // ------------------------------------------------------------------

    #[test]
    fn route_limits_apply_when_the_request_carries_no_values() {
        let provider = ScriptedProvider::new(vec![respond("ok", 1, 1)]);
        let registry = ProviderRegistry::from_connections([("c", ready(provider.clone()))]);
        let mut routing_view = NormalizedRouting::default();
        let mut limited = normalized_route("c", "m1", Some("fast"));
        limited.limits = Some(RouteLimits {
            max_output_tokens: Some(4_096),
            temperature: Some(0.25),
        });
        routing_view
            .purpose_chains
            .insert("intent".to_owned(), vec![limited]);
        let plan = ResolvedRoutePlan::from_normalized(&routing_view, &no_overrides());
        let client = client(registry, plan);

        client
            .complete(
                &model_route("c", "m1", Some("fast")),
                &prompt("x"),
                &CancelToken::new(),
            )
            .expect("completes");
        let request = &provider.requests()[0];
        assert_eq!(request.max_output_tokens, Some(4_096));
        assert_eq!(request.temperature, Some(0.25));
    }

    #[test]
    fn request_values_win_over_route_limits() {
        let provider = ScriptedProvider::new(vec![respond("ok", 1, 1)]);
        let registry = ProviderRegistry::from_connections([("c", ready(provider.clone()))]);
        let mut routing_view = NormalizedRouting::default();
        let mut limited = normalized_route("c", "m1", None);
        limited.limits = Some(RouteLimits {
            max_output_tokens: Some(4_096),
            temperature: Some(0.25),
        });
        routing_view
            .purpose_chains
            .insert("intent".to_owned(), vec![limited]);
        let plan = ResolvedRoutePlan::from_normalized(&routing_view, &no_overrides());
        let client = client(registry, plan);

        let mut request = prompt("x");
        request.max_output_tokens = Some(128);
        request.temperature = Some(0.9);
        client
            .complete(&model_route("c", "m1", None), &request, &CancelToken::new())
            .expect("completes");
        let provider_request = &provider.requests()[0];
        assert_eq!(provider_request.max_output_tokens, Some(128));
        assert_eq!(provider_request.temperature, Some(0.9));
    }

    // ------------------------------------------------------------------
    // Plan semantics
    // ------------------------------------------------------------------

    #[test]
    fn a_provider_override_that_empties_a_purpose_leaves_it_absent() {
        let routing_view = routing(
            vec![(
                "intent",
                vec![normalized_route("fake", "fake-standard", None)],
            )],
            Vec::new(),
        );
        let overrides = InvocationOverrides {
            provider: Some("gateway".to_owned()),
            ..InvocationOverrides::default()
        };
        let plan = ResolvedRoutePlan::from_normalized(&routing_view, &overrides);
        assert!(plan.chain("intent").is_none());
        let error = plan.primary("intent").expect_err("absent purpose errors");
        assert!(
            error
                .detail()
                .contains("no usable route remains for purpose \"intent\""),
            "{}",
            error
        );
    }

    #[test]
    fn a_role_override_replaces_every_chain_with_that_roles_chain() {
        let routing_view = routing(
            vec![(
                "intent",
                vec![normalized_route("conn-a", "model-a", Some("fast"))],
            )],
            vec![
                (
                    "fast",
                    vec![normalized_route("conn-b", "model-b", Some("fast"))],
                ),
                (
                    "coding",
                    vec![normalized_route("conn-a", "model-a", Some("coding"))],
                ),
            ],
        );
        let overrides = InvocationOverrides {
            role: Some("fast".to_owned()),
            ..InvocationOverrides::default()
        };
        let plan = ResolvedRoutePlan::from_normalized(&routing_view, &overrides);
        let chain = plan.chain("intent").expect("intent resolves");
        assert_eq!(
            chain
                .iter()
                .map(|planned| (
                    planned.route.provider.as_str(),
                    planned.route.model.as_str()
                ))
                .collect::<Vec<_>>(),
            [("conn-b", "model-b")]
        );
        // Every role key resolves to the override role's chain.
        let coding = plan.role_chain("coding").expect("coding resolves");
        assert_eq!(coding[0].route.provider, "conn-b");
        let fast = plan.role_chain("fast").expect("fast resolves");
        assert_eq!(fast[0].route.provider, "conn-b");
    }

    #[test]
    fn purposes_and_roles_iterate_in_configuration_order() {
        let routing_view = routing(
            vec![
                ("intent", vec![normalized_route("c", "m1", None)]),
                ("implementation", vec![normalized_route("c", "m2", None)]),
            ],
            vec![("coding", vec![normalized_route("c", "m3", None)])],
        );
        let plan = ResolvedRoutePlan::from_normalized(&routing_view, &no_overrides());
        assert_eq!(
            plan.purposes().collect::<Vec<_>>(),
            ["intent", "implementation"]
        );
        assert!(plan.role_chain("coding").is_some());
        assert!(plan.role_chain("review").is_none());
    }

    #[test]
    fn role_chains_win_when_purpose_chains_share_the_same_head() {
        // Two purposes share the granted route as their head but diverge
        // afterwards, and the granted route's role has its own chain with a
        // different tail: the role key must win, and neither purpose tail
        // may be reached.
        let role_tail = ScriptedProvider::new(vec![respond("role tail", 1, 1)]);
        let implementation_tail = ScriptedProvider::new(vec![respond("implementation tail", 1, 1)]);
        let verification_tail = ScriptedProvider::new(vec![respond("verification tail", 1, 1)]);
        let head = ScriptedProvider::new(vec![ScriptedTurn::Fail(
            ProviderErrorKind::Server,
            "head down",
        )]);
        let registry = ProviderRegistry::from_connections([
            ("head", ready(head)),
            ("role-tail", ready(role_tail.clone())),
            ("implementation-tail", ready(implementation_tail.clone())),
            ("verification-tail", ready(verification_tail.clone())),
        ]);
        let granted = model_route("head", "m1", Some("coding"));
        let granted_entry = normalized_route("head", "m1", Some("coding"));
        let routing_view = routing(
            vec![
                (
                    "implementation",
                    vec![
                        granted_entry.clone(),
                        normalized_route("implementation-tail", "m3", None),
                    ],
                ),
                (
                    "verification",
                    vec![
                        granted_entry,
                        normalized_route("verification-tail", "m4", None),
                    ],
                ),
            ],
            vec![(
                "coding",
                vec![
                    normalized_route("head", "m1", Some("coding")),
                    normalized_route("role-tail", "m2", None),
                ],
            )],
        );
        let plan = ResolvedRoutePlan::from_normalized(&routing_view, &no_overrides());
        let client = client(registry, plan);

        let response = client
            .complete(&granted, &prompt("x"), &CancelToken::new())
            .expect("completes");
        assert_eq!(response.text(), "role tail");
        assert!(
            implementation_tail.requests().is_empty() && verification_tail.requests().is_empty(),
            "purpose tails must not be reached when the role chain matches"
        );
    }

    #[test]
    fn without_a_role_match_the_first_purpose_chain_with_that_head_wins() {
        // No role on the granted route and no role chain for it: the first
        // purpose (configuration order) whose head matches supplies the
        // fallback chain.
        let first_tail = ScriptedProvider::new(vec![respond("first purpose tail", 1, 1)]);
        let second_tail = ScriptedProvider::new(vec![respond("second purpose tail", 1, 1)]);
        let head = ScriptedProvider::new(vec![ScriptedTurn::Fail(
            ProviderErrorKind::Server,
            "head down",
        )]);
        let registry = ProviderRegistry::from_connections([
            ("head", ready(head)),
            ("first-tail", ready(first_tail.clone())),
            ("second-tail", ready(second_tail.clone())),
        ]);
        let granted_entry = normalized_route("head", "m1", None);
        let routing_view = routing(
            vec![
                (
                    "implementation",
                    vec![
                        granted_entry.clone(),
                        normalized_route("first-tail", "m2", None),
                    ],
                ),
                (
                    "verification",
                    vec![granted_entry, normalized_route("second-tail", "m3", None)],
                ),
            ],
            Vec::new(),
        );
        let plan = ResolvedRoutePlan::from_normalized(&routing_view, &no_overrides());
        let client = client(registry, plan);

        let response = client
            .complete(
                &model_route("head", "m1", None),
                &prompt("x"),
                &CancelToken::new(),
            )
            .expect("completes");
        assert_eq!(response.text(), "first purpose tail");
        assert!(second_tail.requests().is_empty());
    }

    #[test]
    fn injected_routes_without_plan_context_stand_alone() {
        let provider = ScriptedProvider::new(vec![ScriptedTurn::Fail(
            ProviderErrorKind::RateLimit,
            "throttled",
        )]);
        let backup = ScriptedProvider::new(vec![respond("backup", 1, 1)]);
        let registry = ProviderRegistry::from_connections([
            ("primary", ready(provider)),
            ("backup", ready(backup.clone())),
        ]);
        // Empty plan: no purpose or role chain matches the granted route.
        let plan =
            ResolvedRoutePlan::from_normalized(&NormalizedRouting::default(), &no_overrides());
        let client = client(registry, plan);

        // The granted route stands alone: a retryable failure does not
        // wander into other connections' providers.
        let error = client
            .complete(
                &model_route("primary", "m1", None),
                &prompt("x"),
                &CancelToken::new(),
            )
            .expect_err("no chain to fall back to");
        assert!(matches!(error, ModelClientError::Provider(_)), "{error}");
        assert!(backup.requests().is_empty());
    }

    // ------------------------------------------------------------------
    // Registry construction from configuration
    // ------------------------------------------------------------------

    #[test]
    fn the_default_configuration_registers_the_offline_fake_as_ready() {
        let registry = ProviderRegistry::from_effective_config(&relay_config::default_config());
        assert_eq!(registry.names(), ["fake", "anthropic"]);
        assert!(registry.ready("fake").is_some());
        let Some(ConnectionState::Ready { adapter, .. }) = registry.connection("fake") else {
            panic!("fake connection must be ready");
        };
        assert_eq!(*adapter, AdapterKind::Fake);
    }

    #[test]
    fn missing_credentials_make_live_connections_unavailable_not_broken() {
        // Unique, test-only variable name that is never set anywhere, so the
        // outcome does not depend on the developer environment.
        let mut config = relay_config::default_config();
        config.providers.0.insert(
            "gateway".to_owned(),
            relay_config::ProviderConnection {
                adapter: AdapterKind::Anthropic,
                api_key_env: Some("RELAY_EXECUTOR_TEST_NEVER_SET_KEY_2026_10".to_owned()),
                base_url: None,
                preset: None,
                capabilities: relay_config::CapabilityOverrides::default(),
            },
        );
        let registry = ProviderRegistry::from_effective_config(&config);
        let Some(ConnectionState::Unavailable { adapter, reason }) = registry.connection("gateway")
        else {
            panic!("gateway must be unavailable without its credential variable");
        };
        assert_eq!(*adapter, AdapterKind::Anthropic);
        assert!(
            reason.contains(
                "The RELAY_EXECUTOR_TEST_NEVER_SET_KEY_2026_10 environment variable is missing"
            ),
            "{reason}"
        );
        assert!(registry.ready("gateway").is_none());
    }

    #[test]
    fn unresolvable_capability_presets_are_unavailable_defense_in_depth() {
        let mut config = relay_config::default_config();
        config.providers.0.insert(
            "weird".to_owned(),
            relay_config::ProviderConnection {
                adapter: AdapterKind::Anthropic,
                api_key_env: Some("RELAY_EXECUTOR_TEST_NEVER_SET_KEY_2026_10".to_owned()),
                base_url: None,
                preset: Some("bogus-preset".to_owned()),
                capabilities: relay_config::CapabilityOverrides::default(),
            },
        );
        let registry = ProviderRegistry::from_effective_config(&config);
        let Some(ConnectionState::Unavailable { reason, .. }) = registry.connection("weird") else {
            panic!("bogus preset must make the connection unavailable");
        };
        assert!(reason.contains("unknown capability preset"), "{reason}");
    }

    #[test]
    fn registry_construction_ignores_route_configuration() {
        // Only the providers section drives the registry; routing drives the
        // plan. A config with no routing still registers connections.
        let mut config = relay_config::default_config();
        config.routing.clear();
        config.models = None;
        let registry = ProviderRegistry::from_effective_config(&config);
        assert_eq!(registry.names(), ["fake", "anthropic"]);
        assert!(registry.ready("fake").is_some());
        // And the plan built from that config's routing is empty.
        let plan = ResolvedRoutePlan::from_normalized(&normalize_routing(&config), &no_overrides());
        assert_eq!(plan.purposes().count(), 0);
    }
}
