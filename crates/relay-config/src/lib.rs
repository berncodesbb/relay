//! Layered Relay configuration.
//!
//! Relay is configured in YAML with strict, actionable validation. This crate
//! ports the retired TypeScript implementation (`src/config/**`, removed from
//! the tree 2026-10-14): raw layered
//! parsing (defaults < user < project < invocation), the named model-role
//! catalog with role-reference or legacy inline routing values, effective
//! (merged) configuration cross-reference validation, the runtime-facing
//! normalized routing contract, and invocation override precedence.
//!
//! Configuration never contains credentials — only the *names* of environment
//! variables that hold them; keys are read from the environment at provider
//! time and are out of scope here.
//!
//! Typical use:
//!
//! ```
//! use relay_config::load::{load_config, LoadOptions};
//! use relay_config::normalized::normalize_routing;
//!
//! let loaded = load_config(LoadOptions {
//!     // Keep the example hermetic: never read a developer's real layers.
//!     workspace_root: "/nonexistent".into(),
//!     user_config_path: Some("/nonexistent/relay-user-config.yaml".into()),
//!     overrides: None,
//! })?;
//! let routing = normalize_routing(&loaded.config);
//! assert!(routing.purpose_chains.contains_key("intent"));
//! # Ok::<(), relay_config::load::ConfigError>(())
//! ```

pub mod defaults;
pub mod inspect;
pub mod invocation;
pub mod load;
pub mod normalized;
pub mod raw;
pub mod schema;

pub use defaults::default_config;
pub use invocation::{InvocationOverrides, RouteResolutionError, apply_invocation_overrides};
pub use load::{
    ConfigError, ConfigLayer, ConfigLayerSource, ConfigOverrides, EFFECTIVE_CONFIG_SOURCE,
    EffectiveConfigOrigins, PROJECT_CONFIG_FILENAME, apply_forced_provider, load_config,
    parse_yaml_layer, user_config_path, validate_effective_config,
};
pub use normalized::{
    INTENT_PURPOSE, MAX_FALLBACKS, NormalizedChain, NormalizedRoute, NormalizedRouting,
    expand_role_names, normalize_routing,
};
pub use raw::{RawMapping, RawValue, YamlError};
pub use schema::{
    ALLOWED_ROUTING_KEYS, AUTONOMY_MESSAGE, BudgetLimits, CONFIG_SCHEMA_VERSION,
    ConfigValidationError, InlineFallback, ModelRoleConfig, PROVIDER_NAMES, ProviderName,
    ProviderSettings, Providers, ROLE_NAME_MESSAGE, ROLE_NAME_PATTERN, ROUTING_VALUE_MESSAGE,
    RelayConfig, RouteConfig, RouteLimits, RoutingValue, ToolSettings, WORKFLOW_MODES,
    is_valid_role_name, parse_relay_config,
};
