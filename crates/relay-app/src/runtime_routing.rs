//! Configuration-driven runtime routing for runs and conversational turns.
//!
//! One place loads the effective configuration, validates the optional
//! provider-connection override against it, and builds everything a run or
//! a turn needs from that single value: the named-connection registry and
//! the resolved route plan. Configuration, credential-variable, and route
//! failures surface as [`AppError::Configuration`] (the usage exit class)
//! and happen **before** any project state is created — the caller resolves
//! routing first, then opens the session.
//!
//! The retired hard-coded provider factory lived in `providers.rs`; this
//! module replaces it. Providers are constructed only from configuration:
//! a `fake` connection is always ready, a live connection whose credential
//! variable is missing is recorded unavailable with its bounded, redacted
//! reason, and only the *names* of credential variables ever appear in
//! messages.

use std::path::Path;
use std::sync::Arc;

use relay_config::InvocationOverrides;
use relay_config::RelayConfig;
use relay_config::load::LoadOptions;
use relay_config::load::apply_forced_provider;
use relay_config::load::load_config;
use relay_config::normalize_routing;
use relay_domain::ModelRoute;
use relay_executor::routed_model::ConnectionState;
use relay_executor::routed_model::PlannedRoute;
use relay_executor::routed_model::ProviderRegistry;
use relay_executor::routed_model::ResolvedRoutePlan;
use relay_executor::routed_model::RoutedModelClient;
use relay_providers::contract::ModelProvider;

use crate::error::AppError;
use crate::runner::RuntimeWiring;

/// Everything one invocation resolved from the effective configuration:
/// the named-connection registry, the purpose-resolved route plan, and the
/// (override-applied) configuration both were built from.
pub struct RuntimeRouting {
    /// The named provider connections, built once from the effective
    /// configuration (credential values resolve inside the adapters and are
    /// never stored or logged).
    pub registry: ProviderRegistry,
    /// The purpose- and role-resolved route plan grants select from.
    pub plan: Arc<ResolvedRoutePlan>,
    /// The effective configuration the registry and plan were built from,
    /// override already applied.
    pub config: RelayConfig,
}

impl RuntimeRouting {
    /// Load the effective configuration once, validate the optional
    /// provider-connection override against it, and build the registry and
    /// route plan.
    ///
    /// `provider_override` (the `--provider` flag) must name a configured
    /// connection — the legacy `fake` and `anthropic` spellings are
    /// the default connection names — and forces every route onto that
    /// connection, exactly the historical flag behavior. Unknown names fail
    /// with the configured names listed. Every failure here is
    /// [`AppError::Configuration`] and happens before any project state is
    /// created.
    pub fn resolve(
        workspace_root: &Path,
        provider_override: Option<&str>,
    ) -> Result<Self, AppError> {
        Self::resolve_with_user_config(workspace_root, None, provider_override)
    }

    /// [`Self::resolve`] with an explicit user-layer path (tests inject a
    /// hermetic file; production passes `None` for the real user layer).
    pub fn resolve_with_user_config(
        workspace_root: &Path,
        user_config_path: Option<&Path>,
        provider_override: Option<&str>,
    ) -> Result<Self, AppError> {
        let loaded = load_config(LoadOptions {
            workspace_root: workspace_root.to_path_buf(),
            user_config_path: user_config_path.map(Path::to_path_buf),
            overrides: None,
        })
        .map_err(|error| AppError::Configuration(error.to_string()))?;
        let config = match provider_override {
            Some(name) => {
                if !loaded.config.providers.contains(name) {
                    let names = connection_listing(&loaded.config);
                    return Err(AppError::Configuration(format!(
                        "unknown provider connection \"{name}\" (configured connections: {names}); \
                         define it under providers in relay configuration or use one of the \
                         configured names"
                    )));
                }
                apply_forced_provider(&loaded.config, name)
            }
            None => loaded.config,
        };
        let registry = ProviderRegistry::from_effective_config(&config);
        let plan = Arc::new(resolved_plan(&config));
        Ok(RuntimeRouting {
            registry,
            plan,
            config,
        })
    }

    /// The primary intent route for a conversational turn, with its
    /// connection verified ready. An unavailable (or unknown) connection is
    /// an actionable configuration error naming the bounded reason — which
    /// names the missing credential *variable*, never a value.
    pub fn intent_route(&self) -> Result<PlannedRoute, AppError> {
        let planned = self
            .plan
            .primary(relay_config::INTENT_PURPOSE)
            .map_err(|error| AppError::Configuration(error.to_string()))?;
        self.require_ready(&planned.route)?;
        Ok(planned.clone())
    }

    /// Fail with an actionable configuration error unless every routing
    /// purpose that resolved to a chain has a ready primary connection. An
    /// unavailable primary would fail every one of its attempts (the routed
    /// client never falls back past an unavailable primary), so the run
    /// refuses to start instead — before any state is created or changed.
    /// Fallback connections may be unavailable; only primaries are checked.
    pub fn preflight_purposes(&self) -> Result<(), AppError> {
        for purpose in self.plan.purposes() {
            let planned = self
                .plan
                .primary(purpose)
                .map_err(|error| AppError::Configuration(error.to_string()))?;
            self.require_ready_for(purpose, &planned.route)?;
        }
        Ok(())
    }

    /// Consume the routing into the conversational-turn seam: the intent
    /// route's provider adapter (the turn streams through it directly),
    /// the granted intent route, and the run wiring proposal-driven runs
    /// share with `relay run`.
    pub fn turn_routing(self) -> Result<TurnRouting, AppError> {
        let planned = self.intent_route()?;
        let provider = self.provider_for(&planned.route)?;
        let run_wiring = self.into_wiring();
        Ok(TurnRouting {
            provider,
            route: planned.route,
            run_wiring,
        })
    }

    /// Consume the routing into production run wiring: the routed model
    /// client owns one resolution of the plan (fallback chains, route
    /// limits); grant construction shares another Arc'd resolution of the
    /// same pure value (purpose → primary routes). Both are deterministic
    /// functions of the same effective configuration, so they cannot
    /// disagree.
    pub fn into_wiring(self) -> RuntimeWiring {
        let client = RoutedModelClient::new(self.registry, resolved_plan(&self.config));
        RuntimeWiring::routed(client, self.plan)
    }

    /// The ready provider adapter serving one route's connection.
    fn provider_for(&self, route: &ModelRoute) -> Result<Arc<dyn ModelProvider>, AppError> {
        self.require_ready(route)?;
        self.registry
            .ready(&route.provider)
            .ok_or_else(|| AppError::Configuration(unavailable_message(route, "the intent route")))
    }

    /// Verify one route's connection exists and is ready, with the
    /// actionable (redacted) reason otherwise.
    fn require_ready(&self, route: &ModelRoute) -> Result<(), AppError> {
        self.require_ready_for("a route", route)
    }

    fn require_ready_for(&self, what: &str, route: &ModelRoute) -> Result<(), AppError> {
        match self.registry.connection(&route.provider) {
            Some(ConnectionState::Ready { .. }) => Ok(()),
            Some(ConnectionState::Unavailable { reason, .. }) => Err(AppError::Configuration(
                unavailable_for(what, route, &format!("it is unavailable: {reason}")),
            )),
            None => Err(AppError::Configuration(unavailable_for(
                what,
                route,
                &format!(
                    "it is not configured (configured connections: {})",
                    connection_listing(&self.config)
                ),
            ))),
        }
    }
}

/// The routing seam of one conversational turn.
///
/// The turn's own model call streams through `provider` — the adapter of
/// the intent route's primary connection — directly on the provider port,
/// never the executor `ModelClient` port. `route` is the granted intent
/// route (its `provider` is the *connection name*, not the adapter kind),
/// and `run_wiring` is the production wiring a `run-ready-work` proposal
/// runs through, so proposal-driven runs route exactly like `relay run`.
#[derive(Clone)]
pub struct TurnRouting {
    /// The adapter serving the intent route's primary connection.
    pub provider: Arc<dyn ModelProvider>,
    /// The granted intent route (connection name, model, role).
    pub route: ModelRoute,
    /// The run wiring proposal-driven runs use.
    pub run_wiring: RuntimeWiring,
}

impl TurnRouting {
    /// A turn routing over one injected provider: the provider serves the
    /// turn's streaming call under the `fake` connection name, and
    /// proposal-driven runs route through the same provider on the offline
    /// default plan (every purpose on `fake`/`fake-standard`). Deterministic
    /// offline support for tests and injected wirings.
    pub fn with_provider(provider: Arc<dyn ModelProvider>) -> Self {
        let config = relay_config::default_config();
        let adapter = provider.name();
        let registry = ProviderRegistry::from_connections([(
            relay_config::DEFAULT_CONNECTION_NAMES[0],
            ConnectionState::Ready {
                adapter,
                provider: Arc::clone(&provider),
            },
        )]);
        let client = RoutedModelClient::new(registry, resolved_plan(&config));
        TurnRouting {
            provider,
            route: ModelRoute {
                provider: relay_config::DEFAULT_CONNECTION_NAMES[0].to_string(),
                model: relay_config::DEFAULT_FAKE_MODEL.to_string(),
                role: None,
            },
            run_wiring: RuntimeWiring::routed(client, offline_plan()),
        }
    }
}

/// Resolve the route plan from a configuration. Pure and deterministic, so
/// the two owners of the plan (the routed client and grant construction)
/// resolve identical values from the same configuration.
pub(crate) fn resolved_plan(config: &RelayConfig) -> ResolvedRoutePlan {
    ResolvedRoutePlan::from_normalized(&normalize_routing(config), &InvocationOverrides::default())
}

/// The deterministic offline route plan: the default configuration's
/// profile, every purpose on the `fake` connection.
pub(crate) fn offline_plan() -> Arc<ResolvedRoutePlan> {
    Arc::new(resolved_plan(&relay_config::default_config()))
}

/// The configured connection names joined for diagnostics.
fn connection_listing(config: &RelayConfig) -> String {
    let names: Vec<&str> = config.providers.names().collect();
    if names.is_empty() {
        "(none are configured)".to_string()
    } else {
        names.join(", ")
    }
}

fn unavailable_message(route: &ModelRoute, what: &str) -> String {
    unavailable_for(what, route, "it is not ready")
}

fn unavailable_for(what: &str, route: &ModelRoute, why: &str) -> String {
    format!(
        "provider connection \"{}\" for {what} cannot serve route {}/{}: {why}",
        route.provider, route.provider, route.model
    )
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)] // test-only, house style
mod tests {
    use super::*;

    fn workspace(config: Option<&str>) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        if let Some(text) = config {
            std::fs::write(dir.path().join("relay.config.yaml"), text).unwrap();
        }
        dir
    }

    /// A fresh workspace with no configuration resolves the offline default:
    /// every purpose on the fake connection, fake ready, anthropic present
    /// but not required.
    #[test]
    fn a_fresh_workspace_resolves_the_offline_default() {
        let dir = workspace(None);
        let routing =
            RuntimeRouting::resolve(dir.path(), None).expect("the default profile resolves");
        assert_eq!(
            routing.registry.names(),
            ["fake", "anthropic"],
            "the default connections register"
        );
        assert!(routing.registry.ready("fake").is_some());
        for purpose in ["intent", "discovery", "implementation", "delivery"] {
            let planned = routing.plan.primary(purpose).expect("purpose resolves");
            assert_eq!(planned.route.provider, "fake", "{purpose}");
            assert_eq!(planned.route.model, "fake-standard", "{purpose}");
        }
        routing.preflight_purposes().expect("all primaries ready");
    }

    /// An invalid configuration file fails as a configuration error naming
    /// the file, before anything else happens.
    #[test]
    fn an_invalid_configuration_fails_as_a_configuration_error() {
        let dir = workspace(Some("version: 1\nbudgets: {}\nrouting: [not, a, map]\n"));
        let error = match RuntimeRouting::resolve(dir.path(), None) {
            Ok(_) => panic!("the malformed layer must fail"),
            Err(error) => error,
        };
        assert!(matches!(error, AppError::Configuration(_)), "{error}");
        assert!(error.message().contains("relay.config.yaml"), "{error}");
    }

    /// An unknown provider override lists the configured connections.
    #[test]
    fn an_unknown_provider_override_lists_configured_connections() {
        let dir = workspace(None);
        let error = match RuntimeRouting::resolve(dir.path(), Some("nonsense")) {
            Ok(_) => panic!("the unknown name must fail"),
            Err(error) => error,
        };
        assert!(matches!(error, AppError::Configuration(_)), "{error}");
        assert!(
            error.message().contains("\"nonsense\""),
            "{}",
            error.message()
        );
        for name in ["fake", "anthropic"] {
            assert!(error.message().contains(name), "{}", error.message());
        }
    }

    /// A provider override forces every route onto that connection — the
    /// historical `--provider` behavior — and unknown connections defined
    /// by configuration are accepted.
    #[test]
    fn a_provider_override_forces_the_named_connection_everywhere() {
        let config = "
version: 1
budgets: {}
providers:
  claude-proxy:
    adapter: anthropic
    baseUrl: https://example.invalid/claude
    apiKeyEnv: RELAY_APP_TEST_ANTHROPIC_NEVER_SET_KEY
routing:
  intent: fast
  discovery: reasoning
  specification: reasoning
  technical-design: architect
  planning: reasoning
  implementation: coding
  verification: review
  delivery: fast
";
        let dir = workspace(Some(config));
        let routing =
            RuntimeRouting::resolve(dir.path(), Some("fake")).expect("fake is configurable");
        for purpose in ["intent", "verification", "delivery"] {
            let planned = routing.plan.primary(purpose).expect("purpose resolves");
            assert_eq!(planned.route.provider, "fake", "{purpose}");
        }
        // Without the override the configured proxy connection is present but
        // unavailable (its variable is never set) — and that is not an error
        // until a route needs it.
        let routing = RuntimeRouting::resolve(dir.path(), None).expect("resolves");
        assert!(matches!(
            routing.registry.connection("claude-proxy"),
            Some(ConnectionState::Unavailable { .. })
        ));
        routing
            .preflight_purposes()
            .expect("claude-proxy is not a primary");
    }

    /// A missing credential on a route's *primary* connection fails the
    /// preflight with the variable name, never a value.
    #[test]
    fn a_missing_primary_credential_fails_preflight_naming_the_variable() {
        let config = "
version: 1
budgets: {}
providers:
  claude-proxy:
    adapter: anthropic
    baseUrl: https://example.invalid/claude
    apiKeyEnv: RELAY_APP_TEST_ANTHROPIC_NEVER_SET_KEY
models:
  fast: { provider: claude-proxy, model: claude-sonnet-5-5 }
routing:
  intent: fast
  discovery: reasoning
  specification: reasoning
  technical-design: architect
  planning: reasoning
  implementation: coding
  verification: review
  delivery: fast
";
        let dir = workspace(Some(config));
        let routing = RuntimeRouting::resolve(dir.path(), None).expect("resolves");
        let error = routing
            .intent_route()
            .expect_err("claude-proxy has no credential");
        assert!(matches!(error, AppError::Configuration(_)), "{error}");
        assert!(
            error
                .message()
                .contains("RELAY_APP_TEST_ANTHROPIC_NEVER_SET_KEY"),
            "{}",
            error.message()
        );
        assert!(!error.message().contains("sk-"), "{}", error.message());
    }
}
