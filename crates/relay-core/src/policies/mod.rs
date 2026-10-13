//! Deterministic policies layered over the core domain: tool permissions,
//! user-intervention requirements, and the policy facade over the core
//! transition rules. Ported from `src/policies/**`; nothing here consults a
//! model or performs IO.

pub mod intervention;
pub mod patterns;
pub mod permissions;
pub mod transitions;

pub use intervention::{InterventionAction, InterventionDecision, requires_user_intervention};
pub use patterns::{DENIED_COMMAND_PATTERNS, DeniedPattern, scan_denied_command};
pub use permissions::{
    PermissionContext, PermissionDecision, PermissionVerdict, ToolDescriptor,
    evaluate_tool_permission, extract_command_fragment, js_stringify_value,
};
pub use transitions::{PolicyError, assert_transition_allowed, evaluate_policy_transition};
