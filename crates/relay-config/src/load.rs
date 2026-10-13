//! Configuration loading and precedence.
//!
//! `defaults < user config (~/.config/relay/config.yaml) < project config
//! (relay.config.yaml in the workspace root) < programmatic overrides`.
//! Routing entries merge by key; the `models` role catalog merges by role name
//! so a later layer replaces one complete role definition while unrelated
//! roles from lower layers survive. After merging, the effective configuration
//! is cross-referenced (every role reference and fallback role must resolve,
//! no self-references or cycles, expanded chains within one primary plus
//! [`MAX_FALLBACKS`]) before it is returned, and can be normalized into the
//! runtime-facing routing contract with [`crate::normalize_routing`].

use crate::defaults::default_config;
use crate::normalized::{MAX_FALLBACKS, expand_role_names};
use crate::raw::RawValue;
use crate::schema::{
    ConfigValidationError, ModelRoleConfig, ProviderName, RelayConfig, RouteConfig, RoutingValue,
    parse_relay_config,
};
use indexmap::IndexMap;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// The project-layer configuration file name at the workspace root.
pub const PROJECT_CONFIG_FILENAME: &str = "relay.config.yaml";

/// Fallback error-source label when no layer file is known for a piece.
pub const EFFECTIVE_CONFIG_SOURCE: &str = "the effective configuration";

/// Errors raised while loading configuration layers.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// A layer failed schema or cross-reference validation.
    #[error(transparent)]
    Validation(#[from] ConfigValidationError),
    /// A layer file could not be read.
    #[error("Cannot read Relay configuration at {path}: {message}")]
    Read {
        /// The layer file path.
        path: String,
        /// The underlying read failure.
        message: String,
    },
    /// A layer file is not valid YAML.
    #[error("Cannot parse YAML in {path}: {message}")]
    Yaml {
        /// The layer file path.
        path: String,
        /// The underlying parse failure.
        message: String,
    },
    /// The process working directory is unavailable for resolving a relative
    /// workspace root.
    #[error("Cannot determine the current working directory: {0}")]
    CurrentDirectory(String),
    /// The effective configuration could not be serialized for inspection.
    #[error("Cannot serialize the effective configuration: {0}")]
    Serialization(String),
}

/// One applied configuration layer, in precedence order (lowest first).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigLayer {
    /// Which stage of the precedence stack this layer came from.
    pub source: ConfigLayerSource,
    /// The layer file path, or `None` for the defaults and override layers.
    pub path: Option<PathBuf>,
}

/// The stage a configuration layer came from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfigLayerSource {
    /// The built-in defaults.
    Defaults,
    /// The user-level configuration file.
    User,
    /// The workspace project configuration file.
    Project,
    /// Programmatic invocation overrides.
    Overrides,
}

impl ConfigLayerSource {
    /// The lowercase layer label used in `config show`/`config path` output.
    pub fn as_str(&self) -> &'static str {
        match self {
            ConfigLayerSource::Defaults => "defaults",
            ConfigLayerSource::User => "user",
            ConfigLayerSource::Project => "project",
            ConfigLayerSource::Overrides => "overrides",
        }
    }
}

/// The result of loading configuration: the effective configuration plus the
/// layers that applied.
#[derive(Clone, Debug, PartialEq)]
pub struct LoadedConfig {
    /// The effective (merged, validated) configuration.
    pub config: RelayConfig,
    /// Layers in precedence order, lowest first; `None` path for defaults.
    pub layers: Vec<ConfigLayer>,
}

/// Programmatic overrides applied above every file layer.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ConfigOverrides {
    /// Replacement autonomy bound (`--auto` / `--autonomy`).
    pub autonomy: Option<String>,
    /// Force one provider everywhere (`--provider` and `RELAY_FAKE`); model
    /// ids, limits, and role-name references stay untouched.
    pub force_provider: Option<ProviderName>,
}

/// Options for [`load_config`].
#[derive(Clone, Debug, Default)]
pub struct LoadOptions {
    /// The workspace root (absolute, or relative to the process working
    /// directory) that holds `relay.config.yaml`.
    pub workspace_root: PathBuf,
    /// Replacement user-layer path (tests inject a temporary file); defaults
    /// to [`user_config_path`].
    pub user_config_path: Option<PathBuf>,
    /// Programmatic overrides applied last.
    pub overrides: Option<ConfigOverrides>,
}

/// Provenance for the effective configuration: which layer label (usually a
/// file path) last defined each routing entry and each model role. Used to
/// attribute cross-reference errors to the layer that introduced the offending
/// piece instead of blaming the whole effective configuration.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EffectiveConfigOrigins {
    /// Routing key to the layer label that last defined it.
    pub routing: IndexMap<String, String>,
    /// Model role to the layer label that last defined it.
    pub models: IndexMap<String, String>,
}

/// The user-level configuration path (`~/.config/relay/config.yaml`), or
/// `None` when no home directory can be determined.
pub fn user_config_path() -> Option<PathBuf> {
    std::env::home_dir().map(|home| home.join(".config").join("relay").join("config.yaml"))
}

/// Parses one YAML layer file's text into a validated layer configuration.
///
/// An empty or whitespace-only file parses as an empty object, exactly like
/// the TypeScript reference's layer reader; the layer still has to satisfy
/// the schema (so it fails on the missing `routing` table, not on YAML).
pub fn parse_yaml_layer(source: &str, text: &str) -> Result<RelayConfig, ConfigError> {
    let parsed = if text.trim().is_empty() {
        RawValue::Mapping(IndexMap::new())
    } else {
        RawValue::parse_yaml(text).map_err(|error| ConfigError::Yaml {
            path: source.to_string(),
            message: error.to_string(),
        })?
    };
    Ok(parse_relay_config(source, &parsed)?)
}

/// Merges `layer` over `merged` and records which routing keys and model roles
/// the layer introduced. Later layers replace whole sections except routing
/// (merged by key) and the `models` catalog (merged by role name, so a later
/// layer replaces one complete role definition while unrelated roles
/// survive). `layer_label` (usually the file path) becomes the origin for
/// everything the layer defines.
pub fn merge_config_layers(
    merged: RelayConfig,
    layer: &RelayConfig,
    layer_label: Option<&str>,
    origins: &mut EffectiveConfigOrigins,
) -> RelayConfig {
    let models = match &layer.models {
        Some(layer_catalog) => {
            let mut catalog = merged.models.clone().unwrap_or_default();
            for (role, definition) in layer_catalog {
                if let Some(label) = layer_label {
                    origins.models.insert(role.clone(), label.to_string());
                }
                catalog.insert(role.clone(), definition.clone());
            }
            Some(catalog)
        }
        None => merged.models.clone(),
    };
    let mut routing = merged.routing.clone();
    for (key, value) in &layer.routing {
        if let Some(label) = layer_label {
            origins.routing.insert(key.clone(), label.to_string());
        }
        routing.insert(key.clone(), value.clone());
    }
    RelayConfig {
        version: merged.version,
        // Every parsed layer carries complete provider/budget/tool sections
        // (schema defaults fill omitted entries), so the field-wise merge of
        // the reference always resolves to the layer's values.
        providers: layer.providers.clone(),
        models,
        routing,
        budgets: layer.budgets,
        autonomy: layer.autonomy.clone(),
        tools: layer.tools,
    }
}

fn read_yaml_layer(
    path: &Path,
    merged: RelayConfig,
    origins: &mut EffectiveConfigOrigins,
) -> Result<RelayConfig, ConfigError> {
    let text = std::fs::read_to_string(path).map_err(|error| ConfigError::Read {
        path: path.display().to_string(),
        message: error.to_string(),
    })?;
    let label = path.display().to_string();
    let layer = parse_yaml_layer(&label, &text)?;
    Ok(merge_config_layers(merged, &layer, Some(&label), origins))
}

/// Loads the effective configuration: defaults, then the user and project
/// layer files when they exist, then programmatic overrides. The merged
/// configuration is cross-reference validated before it is returned.
pub fn load_config(options: LoadOptions) -> Result<LoadedConfig, ConfigError> {
    let user_path = options.user_config_path.clone().or_else(user_config_path);
    let project_path = if options.workspace_root.is_absolute() {
        options.workspace_root.join(PROJECT_CONFIG_FILENAME)
    } else {
        let cwd = std::env::current_dir()
            .map_err(|error| ConfigError::CurrentDirectory(error.to_string()))?;
        cwd.join(&options.workspace_root)
            .join(PROJECT_CONFIG_FILENAME)
    };

    let mut layers = vec![ConfigLayer {
        source: ConfigLayerSource::Defaults,
        path: None,
    }];
    let mut origins = EffectiveConfigOrigins::default();
    let mut config = default_config();

    if let Some(user_path) = user_path.as_ref().filter(|path| path.exists()) {
        config = read_yaml_layer(user_path, config, &mut origins)?;
        layers.push(ConfigLayer {
            source: ConfigLayerSource::User,
            path: Some(user_path.clone()),
        });
    }
    if Some(&project_path) != user_path.as_ref() && project_path.exists() {
        config = read_yaml_layer(&project_path, config, &mut origins)?;
        layers.push(ConfigLayer {
            source: ConfigLayerSource::Project,
            path: Some(project_path.clone()),
        });
    }

    // The effective configuration always exposes a models catalog, even when
    // every file layer is a legacy inline-only configuration.
    if config.models.is_none() {
        config.models = Some(IndexMap::new());
    }
    validate_effective_config(&config, EFFECTIVE_CONFIG_SOURCE, Some(&origins))?;

    if let Some(overrides) = options.overrides.as_ref() {
        if let Some(autonomy) = overrides.autonomy.as_ref() {
            config.autonomy = autonomy.clone();
            layers.push(ConfigLayer {
                source: ConfigLayerSource::Overrides,
                path: None,
            });
        }
        if let Some(provider) = overrides.force_provider {
            config = apply_forced_provider(&config, provider);
            layers.push(ConfigLayer {
                source: ConfigLayerSource::Overrides,
                path: None,
            });
        }
    }

    Ok(LoadedConfig { config, layers })
}

/// Validates the effective (merged) configuration's cross-references. Layer
/// parsing already validated each layer's own shape (providers, limits,
/// inline fallback objects); this catches problems that only exist once layers
/// combine: unknown role references, unknown fallback roles, self-references,
/// cycles, and expanded chains longer than one primary plus [`MAX_FALLBACKS`].
///
/// Returns a [`ConfigValidationError`] naming the offending dotted path; when
/// `origins` names the layer file that introduced the piece, that file is used
/// as the error source.
pub fn validate_effective_config(
    config: &RelayConfig,
    source: &str,
    origins: Option<&EffectiveConfigOrigins>,
) -> Result<(), ConfigValidationError> {
    let models = config.models.as_ref();
    let empty_catalog: IndexMap<String, ModelRoleConfig> = IndexMap::new();
    let catalog = models.unwrap_or(&empty_catalog);
    let role_names: Vec<&str> = catalog.keys().map(String::as_str).collect();
    let available_roles = if role_names.is_empty() {
        "(no roles are defined in the models catalog)".to_string()
    } else {
        role_names.join(", ")
    };
    let routing_source = |key: &str| -> String {
        origins
            .and_then(|origins| origins.routing.get(key))
            .map_or(source, |label| label.as_str())
            .to_string()
    };
    let role_source = |role: &str| -> String {
        origins
            .and_then(|origins| origins.models.get(role))
            .map_or(source, |label| label.as_str())
            .to_string()
    };

    for (key, value) in &config.routing {
        let RoutingValue::Role(role) = value else {
            continue;
        };
        if !catalog.contains_key(role) {
            return Err(ConfigValidationError::new(
                routing_source(key),
                vec![format!(
                    "routing.{key}: references unknown model role \"{role}\"; available roles: {available_roles}. \
                     Define a models.{role} entry or route \"{key}\" with an inline provider/model object."
                )],
            ));
        }
    }

    for (role, definition) in catalog {
        for (index, fallback) in definition.fallbacks.iter().flatten().enumerate() {
            if !catalog.contains_key(fallback) {
                return Err(ConfigValidationError::new(
                    role_source(role),
                    vec![format!(
                        "models.{role}.fallbacks[{index}]: references unknown model role \"{fallback}\"; \
                         available roles: {available_roles}"
                    )],
                ));
            }
            if fallback == role {
                return Err(ConfigValidationError::new(
                    role_source(role),
                    vec![format!(
                        "models.{role}.fallbacks[{index}]: role \"{role}\" cannot fall back to itself"
                    )],
                ));
            }
        }
    }

    detect_fallback_cycles(catalog, &role_source)?;

    // Excessive chains: every routing-referenced role is checked through its
    // routing key (actionable `routing.<key>` path)...
    let mut referenced_roles: HashSet<&str> = HashSet::new();
    for (key, value) in &config.routing {
        let RoutingValue::Role(role) = value else {
            continue;
        };
        referenced_roles.insert(role.as_str());
        let chain = expand_role_names(role, catalog, None);
        if chain.len() > 1 + MAX_FALLBACKS {
            return Err(ConfigValidationError::new(
                routing_source(key),
                vec![format!(
                    "routing.{key}: role \"{role}\" expands to {n} routes ({joined}); \
                     the maximum is 1 primary plus {MAX_FALLBACKS} fallbacks",
                    n = chain.len(),
                    joined = chain.join(" -> ")
                )],
            ));
        }
    }
    // ...and every remaining catalog role too: an unreferenced role is still
    // reachable at runtime through `--role`, so an excessive chain must fail
    // here rather than silently truncate during normalization.
    for role in catalog.keys() {
        if referenced_roles.contains(role.as_str()) {
            continue;
        }
        let chain = expand_role_names(role, catalog, None);
        if chain.len() > 1 + MAX_FALLBACKS {
            return Err(ConfigValidationError::new(
                role_source(role),
                vec![format!(
                    "models.{role}: role \"{role}\" expands to {n} routes ({joined}); \
                     the maximum is 1 primary plus {MAX_FALLBACKS} fallbacks",
                    n = chain.len(),
                    joined = chain.join(" -> ")
                )],
            ));
        }
    }
    Ok(())
}

/// Depth-first cycle detection over the role fallback graph, reported exactly
/// like the TypeScript reference: the cycle path is printed in visit order and
/// attributed to the layer that introduced the cycle's origin role.
fn detect_fallback_cycles(
    catalog: &IndexMap<String, ModelRoleConfig>,
    role_source: &dyn Fn(&str) -> String,
) -> Result<(), ConfigValidationError> {
    const IN_PROGRESS: u8 = 1;
    const DONE: u8 = 2;
    let mut state: IndexMap<String, u8> = IndexMap::new();
    let mut path: Vec<String> = Vec::new();
    #[derive(Clone)]
    enum Frame {
        Enter(String),
        Exit(String),
    }
    let mut stack: Vec<Frame> = catalog
        .keys()
        .rev()
        .map(|role| Frame::Enter(role.clone()))
        .collect();

    while let Some(frame) = stack.pop() {
        match frame {
            Frame::Enter(role) => {
                match state.get(&role).copied().unwrap_or(0) {
                    IN_PROGRESS => {
                        let start = path.iter().position(|entry| entry == &role).unwrap_or(0);
                        let mut cycle: Vec<&str> =
                            path[start..].iter().map(String::as_str).collect();
                        cycle.push(&role);
                        let origin = cycle.first().copied().unwrap_or(role.as_str());
                        return Err(ConfigValidationError::new(
                            role_source(origin),
                            vec![format!(
                                "models.{origin}.fallbacks: role fallback cycle detected ({cycle}); \
                                 remove the cycle in the models catalog",
                                cycle = cycle.join(" -> ")
                            )],
                        ));
                    }
                    DONE => continue,
                    _ => {}
                }
                state.insert(role.clone(), IN_PROGRESS);
                path.push(role.clone());
                stack.push(Frame::Exit(role.clone()));
                if let Some(definition) = catalog.get(&role) {
                    for fallback in definition.fallbacks.iter().flatten().rev() {
                        stack.push(Frame::Enter(fallback.clone()));
                    }
                }
            }
            Frame::Exit(role) => {
                path.pop();
                state.insert(role, DONE);
            }
        }
    }
    Ok(())
}

/// Rewrites `--provider`/`RELAY_FAKE` across both routing syntaxes: every
/// models catalog entry and every inline routing object (including inline
/// fallback objects) moves to the forced provider while model ids, limits,
/// and role-name fallback references stay untouched.
pub fn apply_forced_provider(config: &RelayConfig, provider: ProviderName) -> RelayConfig {
    let models: IndexMap<String, ModelRoleConfig> = config
        .models
        .as_ref()
        .map(|catalog| {
            catalog
                .iter()
                .map(|(role, definition)| {
                    (
                        role.clone(),
                        ModelRoleConfig {
                            provider,
                            model: definition.model.clone(),
                            fallbacks: definition.fallbacks.clone(),
                            limits: definition.limits.clone(),
                        },
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    let routing: IndexMap<String, RoutingValue> = config
        .routing
        .iter()
        .map(|(key, value)| {
            let forced = match value {
                RoutingValue::Role(role) => RoutingValue::Role(role.clone()),
                RoutingValue::Inline(route) => RoutingValue::Inline(RouteConfig {
                    provider,
                    model: route.model.clone(),
                    fallbacks: route.fallbacks.as_ref().map(|fallbacks| {
                        fallbacks
                            .iter()
                            .map(|fallback| crate::schema::InlineFallback {
                                provider,
                                model: fallback.model.clone(),
                            })
                            .collect()
                    }),
                    limits: route.limits.clone(),
                }),
            };
            (key.clone(), forced)
        })
        .collect();
    RelayConfig {
        version: config.version,
        providers: config.providers.clone(),
        models: Some(models),
        routing,
        budgets: config.budgets,
        autonomy: config.autonomy.clone(),
        tools: config.tools,
    }
}
