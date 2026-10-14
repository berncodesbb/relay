//! Effective-configuration inspection.
//!
//! Library support for `relay config show` and `relay config get <key>`: a
//! serializable view of the merged configuration (rendering the provider
//! connections, role catalog, and routing map coherently — the CLI owns
//! printing) and dotted-path lookup over that view (`providers`,
//! `providers.<name>`, `providers.<name>.baseUrl`, `models.<role>`,
//! `routing.intent`, `models.<role>.fallbacks.0`, ...). Every connection
//! renders its effective `adapter`, including connections whose adapter was
//! inferred from a legacy key.
//!
//! The view is a [`serde_yaml::Value`] rather than a JSON value because its
//! mappings preserve insertion order: merged-configuration key order (and
//! connection/catalog/routing order inside it) is part of the show/get output
//! contract, and the reference prints these values as YAML.

use crate::load::ConfigError;
use crate::schema::RelayConfig;
use serde_yaml::Value;

/// Serializes the effective configuration for display or lookup.
///
/// Field order matches the merged-configuration key order of the TypeScript
/// reference (`version, providers, models, routing, budgets, autonomy,
/// tools`), absent optional sections are omitted, and mapping order is
/// insertion (catalog and routing) order.
pub fn effective_value(config: &RelayConfig) -> Result<Value, ConfigError> {
    serde_yaml::to_value(config).map_err(|error| ConfigError::Serialization(error.to_string()))
}

/// Dotted-path lookup over the effective configuration, mirroring the
/// `relay config get <key>` traversal: segments are split on `.`, empty
/// segments are ignored, mapping segments look up keys, and numeric segments
/// index into sequences. Returns `None` for unknown keys.
pub fn lookup_dotted(config: &RelayConfig, key: &str) -> Option<Value> {
    let root = effective_value(config).ok()?;
    let mut current: &Value = &root;
    for part in key.split('.').filter(|piece| !piece.is_empty()) {
        current = match current {
            Value::Mapping(mapping) => mapping.get(Value::String(part.to_string()))?,
            Value::Sequence(items) => {
                let index: usize = part.parse().ok()?;
                items.get(index)?
            }
            _ => return None,
        };
    }
    Some(current.clone())
}
