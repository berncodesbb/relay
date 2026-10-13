//! Workspace-bounded tools for Relay.
//!
//! Ported from the TypeScript reference implementation in `src/tools/**`.
//! Tools are schema-validated, capability-classified, time-bounded, and
//! cancelable. The registry is the only surface the agent loop uses; it never
//! spawns processes or touches the filesystem directly.
//!
//! Safety properties preserved from the reference implementation:
//!
//! - workspace containment with symlink checks (`path_guard`),
//! - create-only writes and no-overwrite publication (`filesystem`, `patch`),
//! - strict all-or-nothing unified-diff application with a re-stat guard
//!   (`patch`),
//! - bounded, redacted output (`redact`),
//! - argument-array child processes only, never shell strings, with an
//!   environment allowlist that refuses secret-shaped names (`process`,
//!   `spawn`, `supervisor`),
//! - read-only Git tools (`git`),
//! - per-call capability enforcement against a domain
//!   [`CapabilityGrant`](relay_domain::CapabilityGrant) (`grant_gate`).
//!
//! Argument validation mirrors the TypeScript Zod pipeline: the same JSON
//! Schema (draft 2020-12) that is shown to providers validates incoming
//! arguments, unknown keys are stripped (never added), defaults are applied,
//! and per-field refinements run afterwards with the reference messages.
//! Issues render as `path: message` joined by `; `.

pub mod capability;
pub mod filesystem;
pub mod git;
pub mod grant_gate;
pub mod patch;
pub mod path_guard;
pub mod process;
pub mod redact;
pub mod registry;
pub mod schemas;
pub mod search;
pub mod spawn;
pub mod supervisor;

pub use capability::ToolCapability;
pub use filesystem::register_filesystem_tools;
pub use git::{register_git_tools, run_git};
pub use grant_gate::GrantGate;
pub use patch::register_patch_tools;
pub use path_guard::{
    PathEscapeError, is_within_workspace, normalize_workspace_path, resolve_workspace_path,
};
pub use process::register_process_tools;
pub use redact::{
    SECRET_ENV_PATTERNS, bound_output, env_name_looks_secret, redact_secrets,
    redact_secrets_with_env, sanitize_env_for_record,
};
pub use registry::{
    ArgumentIssues, PermissionDecision, ToolArgsSchema, ToolContext, ToolDefinition, ToolOutcome,
    ToolRegistrationError, ToolRegistry, ToolResult, UnknownToolError, call_tool,
    parse_tool_arguments, tool_json_schema,
};
pub use schemas::{RELATIVE_PATH_MESSAGE, command_arg_refinements, is_safe_relative_path};
pub use search::register_search_tools;
pub use spawn::{
    BASE_ENV_NAMES, BASE_ENV_PREFIXES, BoundedSpawnRequest, BoundedSpawnResult,
    DEFAULT_STREAM_CAP_CHARS, build_bounded_env, cap_stream_text, spawn_bounded,
};

use relay_config::ToolSettings;

/// Build a registry containing every built-in tool, configured by `settings`.
///
/// Port of `createDefaultToolRegistry` in `src/tools/index.ts`, where a
/// registration failure throws; here the error propagates to the caller.
pub fn create_default_tool_registry(
    settings: &ToolSettings,
) -> Result<ToolRegistry, ToolRegistrationError> {
    let mut registry = ToolRegistry::new();
    register_filesystem_tools(&mut registry, settings)?;
    register_search_tools(&mut registry, settings)?;
    register_patch_tools(&mut registry)?;
    register_process_tools(&mut registry, settings)?;
    register_git_tools(&mut registry)?;
    Ok(registry)
}
