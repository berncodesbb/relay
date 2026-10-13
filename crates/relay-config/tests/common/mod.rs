//! Shared fixtures for the relay-config integration tests (ports of
//! `test/unit/config/*.test.ts`).

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use relay_config::load::LoadOptions;
use relay_config::schema::{
    BudgetLimits, InlineFallback, ModelRoleConfig, ProviderName, ProviderSettings, Providers,
    RelayConfig, RouteConfig, RouteLimits, RoutingValue, ToolSettings,
};
use relay_config::{default_config, load_config};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// A temporary directory removed when dropped.
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    /// Creates a unique temporary directory.
    pub fn new(prefix: &str) -> TempDir {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "relay-cfg-{prefix}-{}-{unique}",
            std::process::id()
        ));
        if std::fs::create_dir_all(&path).is_err() {
            panic!("could not create temporary directory {}", path.display());
        }
        TempDir { path }
    }

    /// The directory path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Writes a file inside the directory and returns its path.
    pub fn write(&self, name: &str, contents: &str) -> PathBuf {
        let path = self.path.join(name);
        if std::fs::write(&path, contents).is_err() {
            panic!("could not write {}", path.display());
        }
        path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// A workspace with an optional `relay.config.yaml` (pass `None::<&str>` for
/// none, or any `&str` to write one).
pub fn workspace_with_project_config<'a>(yaml: impl Into<Option<&'a str>>) -> TempDir {
    let dir = TempDir::new("load");
    if let Some(text) = yaml.into() {
        dir.write("relay.config.yaml", text);
    }
    dir
}

/// A user-layer config file outside any workspace.
pub fn write_user_config(yaml: &str) -> (TempDir, PathBuf) {
    let dir = TempDir::new("user");
    let path = dir.write("user.yaml", yaml);
    (dir, path)
}

/// Loads configuration for a workspace with defaults for every other option.
pub fn load_for(workspace_root: &Path) -> relay_config::load::LoadedConfig {
    load_config(LoadOptions {
        workspace_root: workspace_root.to_path_buf(),
        ..LoadOptions::default()
    })
    .expect("loadConfig should succeed")
}

/// A role-name routing reference.
pub fn role(name: &str) -> RoutingValue {
    RoutingValue::Role(name.to_string())
}

/// A legacy inline routing object.
pub fn inline(provider: ProviderName, model: &str) -> RoutingValue {
    RoutingValue::Inline(RouteConfig {
        provider,
        model: model.to_string(),
        fallbacks: None,
        limits: None,
    })
}

/// A legacy inline routing object with inline fallbacks.
pub fn inline_with_fallbacks(
    provider: ProviderName,
    model: &str,
    fallbacks: Vec<InlineFallback>,
) -> RoutingValue {
    RoutingValue::Inline(RouteConfig {
        provider,
        model: model.to_string(),
        fallbacks: Some(fallbacks),
        limits: None,
    })
}

/// A model-role definition.
pub fn model_role(provider: ProviderName, model: &str) -> ModelRoleConfig {
    ModelRoleConfig {
        provider,
        model: model.to_string(),
        fallbacks: None,
        limits: None,
    }
}

/// A model-role definition with role-name fallbacks.
pub fn model_role_with_fallbacks(
    provider: ProviderName,
    model: &str,
    fallbacks: Vec<&str>,
) -> ModelRoleConfig {
    ModelRoleConfig {
        provider,
        model: model.to_string(),
        fallbacks: Some(fallbacks.iter().map(|name| name.to_string()).collect()),
        limits: None,
    }
}

/// Mirrors the TypeScript tests' `configWith` helper: a configuration with a
/// hand-built routing table and optional models catalog.
pub fn config_with(
    routing: Vec<(&str, RoutingValue)>,
    models: Option<Vec<(&str, ModelRoleConfig)>>,
) -> RelayConfig {
    RelayConfig {
        version: 1,
        providers: Providers {
            openai: ProviderSettings {
                api_key_env: "OPENAI_API_KEY".to_string(),
                base_url: None,
            },
            anthropic: ProviderSettings {
                api_key_env: "ANTHROPIC_API_KEY".to_string(),
                base_url: None,
            },
        },
        models: models.map(|entries| {
            entries
                .into_iter()
                .map(|(name, definition)| (name.to_string(), definition))
                .collect()
        }),
        routing: routing
            .into_iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect(),
        budgets: BudgetLimits::default(),
        autonomy: "next-checkpoint".to_string(),
        tools: ToolSettings::default(),
    }
}

/// The seven inline mode routes the TypeScript normalize tests use.
pub fn inline_routes() -> Vec<(&'static str, RoutingValue)> {
    vec![
        (
            "discovery",
            inline(ProviderName::Anthropic, "claude-sonnet-5-5"),
        ),
        (
            "specification",
            inline(ProviderName::Anthropic, "claude-sonnet-5-5"),
        ),
        (
            "technical-design",
            inline(ProviderName::Anthropic, "claude-opus-5-5"),
        ),
        (
            "planning",
            inline(ProviderName::Anthropic, "claude-sonnet-5-5"),
        ),
        (
            "implementation",
            inline(ProviderName::Anthropic, "claude-sonnet-5-5"),
        ),
        ("verification", inline(ProviderName::OpenAi, "gpt-5.2")),
        (
            "delivery",
            inline(ProviderName::Anthropic, "claude-haiku-4-5"),
        ),
    ]
}

/// A role-based layer routing table for all seven modes (per-layer
/// requirement), without `intent`.
pub const ROLE_ROUTING_YAML: &str = r#"
routing:
  discovery: reasoning
  specification: reasoning
  technical-design: architect
  planning: reasoning
  implementation: coding
  verification: review
  delivery: fast
"#;

/// The plan's default models/routing block, verbatim, plus required headers.
pub const PLAN_DEFAULT_YAML: &str = r#"
version: 1
budgets: {}
models:
  reasoning:
    provider: anthropic
    model: claude-sonnet-5-5
  architect:
    provider: anthropic
    model: claude-opus-5-5
  coding:
    provider: anthropic
    model: claude-sonnet-5-5
  review:
    provider: openai
    model: gpt-5.2
    fallbacks: [reasoning]
  fast:
    provider: anthropic
    model: claude-haiku-4-5

routing:
  intent: fast
  discovery: reasoning
  specification: reasoning
  technical-design: architect
  planning: reasoning
  implementation: coding
  verification: review
  delivery: fast
"#;

/// A complete version-1 inline-only (legacy) configuration.
pub const LEGACY_YAML: &str = r#"
version: 1
budgets: {}
routing:
  discovery: { provider: anthropic, model: claude-sonnet-5-5 }
  specification: { provider: anthropic, model: claude-sonnet-5-5 }
  technical-design: { provider: anthropic, model: claude-opus-5-5 }
  planning: { provider: openai, model: gpt-5-mini, fallbacks: [{ provider: anthropic, model: claude-sonnet-5-5 }] }
  implementation: { provider: anthropic, model: claude-sonnet-5-5 }
  verification: { provider: openai, model: gpt-5.2 }
  delivery: { provider: anthropic, model: claude-haiku-4-5 }
"#;

/// The user-layer fixture from the TypeScript layering tests.
pub const USER_LAYER_YAML: &str = r#"
version: 1
budgets: {}
models:
  reasoning: { provider: openai, model: gpt-5.2 }
routing:
  discovery: reasoning
  specification: reasoning
  technical-design: architect
  planning: reasoning
  implementation: coding
  verification: review
  delivery: fast
"#;

/// The built-in defaults.
pub fn defaults() -> RelayConfig {
    default_config()
}

/// Limits fixture used by several ports.
pub fn limits(max_output_tokens: Option<i64>, temperature: Option<f64>) -> Option<RouteLimits> {
    Some(RouteLimits {
        max_output_tokens,
        temperature,
    })
}
