//! Invocation overrides (`--role`, `--provider`, `--model`).
//!
//! Flags override every file layer for a single invocation. Applied to a
//! normalized routing view in the documented precedence — later wins:
//!
//! ```text
//! configured route  <  --role <name>  <  --provider <connection>  <  --model <name>
//! ```
//!
//! `--role` replaces the whole chain with one catalog role's expanded chain;
//! `--provider` filters the chain to routes on that provider *connection*
//! (a `providers.<name>` key of the effective configuration, so the legacy
//! `fake`/`anthropic` spellings and any user-defined connection name all
//! work); `--model` rewrites the model id on every surviving route.
//! Provider *registration* (credential variables present in the environment)
//! is a provider-time concern and is deliberately not modeled here.

use crate::normalized::{NormalizedChain, NormalizedRouting};

/// Values carried from the parsed command line.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct InvocationOverrides {
    /// Route every call this invocation to one catalog role (with that role's
    /// fallback chain).
    pub role: Option<String>,
    /// Keep only routes on this provider connection.
    pub provider: Option<String>,
    /// Rewrite the model id on every surviving route.
    pub model: Option<String>,
}

impl InvocationOverrides {
    /// Whether any override is set.
    pub fn is_empty(&self) -> bool {
        self.role.is_none() && self.provider.is_none() && self.model.is_none()
    }
}

/// Raised when the configured routing plus overrides yield no usable route.
/// The display text matches the runtime's `RouteUnavailableError`.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("No usable model route: {0}")]
pub struct RouteResolutionError(String);

impl RouteResolutionError {
    /// Builds the error from its detail message.
    pub fn new(detail: impl Into<String>) -> RouteResolutionError {
        RouteResolutionError(detail.into())
    }

    /// The detail message after the `No usable model route: ` prefix.
    pub fn detail(&self) -> &str {
        &self.0
    }
}

fn describe_key(purpose: &str, mode: Option<&str>) -> String {
    match mode {
        Some(mode) => format!("mode \"{mode}\" (purpose \"{purpose}\")"),
        None => format!("purpose \"{purpose}\""),
    }
}

/// Resolves the chain for one call and applies the invocation overrides.
///
/// Lookup order for the configured route: an explicit purpose entry (today
/// only `intent`), then the workflow mode, then — for non-mode purposes with
/// no explicit entry only — the legacy first-configured route. A mode without
/// a route is a defensive error (effective configuration validation makes it
/// impossible): never borrow another mode.
pub fn apply_invocation_overrides(
    routing: &NormalizedRouting,
    purpose: &str,
    mode: Option<&str>,
    overrides: &InvocationOverrides,
) -> Result<NormalizedChain, RouteResolutionError> {
    let mut chain: NormalizedChain = routing
        .purpose_chains
        .get(purpose)
        .cloned()
        .or_else(|| {
            mode.and_then(|mode| routing.purpose_chains.get(mode))
                .cloned()
                .or_else(|| {
                    if mode.is_none() {
                        routing.legacy_fallback.clone()
                    } else {
                        None
                    }
                })
        })
        .ok_or_else(|| match mode {
            Some(mode) => RouteResolutionError::new(format!(
                "no route is configured for mode \"{mode}\" (purpose \"{purpose}\"). \
                 Effective configuration guarantees every workflow mode a route; \
                 add a routing entry for the mode in relay configuration."
            )),
            None => RouteResolutionError::new(format!(
                "no route is configured for purpose \"{purpose}\" and no fallback route exists. \
                 Add a routing entry for the purpose (for example \"intent\") in relay configuration."
            )),
        })?;

    if let Some(role) = overrides.role.as_deref() {
        chain = routing.role_chains.get(role).cloned().ok_or_else(|| {
            let available = routing.role_chains.keys().cloned().collect::<Vec<_>>();
            let listing = if available.is_empty() {
                "(no roles are defined in the models catalog)".to_string()
            } else {
                available.join(", ")
            };
            RouteResolutionError::new(format!(
                "unknown model role \"{role}\"; available roles: {listing}. \
                 Define a models.{role} entry in relay configuration."
            ))
        })?;
        if chain.is_empty() {
            return Err(RouteResolutionError::new(format!(
                "the chain for role \"{role}\" is empty"
            )));
        }
    }

    if chain.is_empty() {
        return Err(RouteResolutionError::new(format!(
            "the configured route for {} is empty",
            describe_key(purpose, mode)
        )));
    }

    if let Some(provider) = overrides.provider.as_deref() {
        chain.retain(|route| route.provider == provider);
        if chain.is_empty() {
            return Err(RouteResolutionError::new(format!(
                "provider connection override \"{provider}\" matched no route in the chain for \
                 purpose \"{purpose}\""
            )));
        }
    }

    if let Some(model) = overrides.model.as_deref() {
        for route in &mut chain {
            route.model = model.to_string();
        }
    }

    Ok(chain)
}
