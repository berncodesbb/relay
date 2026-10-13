//! Per-tool-call capability enforcement against a [`CapabilityGrant`].
//!
//! This is layer four of the enforcement stack: the fold validates grants
//! against work ceilings, the engine refuses overlapping write grants, the
//! orchestrator persists `AttemptStarted` as the durable lease, and this
//! gate checks every concrete tool call against the lease before the tool
//! body runs. Checks are deliberately layered with (not instead of) the
//! path guard: scope containment is lexical and pre-resolution here;
//! symlink resolution stays inside [`crate::path_guard`] underneath.
//!
//! Fail closed: anything the gate cannot prove permitted is denied, and
//! every denial names what authority would be needed.

use std::sync::Arc;

use relay_domain::{CapabilityGrant, DeliveryAuthority, Operation, ScopePath};
use serde_json::Value;

use crate::capability::ToolCapability;
use crate::patch::parse_patch;
use crate::path_guard::normalize_workspace_path;
use crate::registry::{
    PermissionDecision, ToolContext, ToolDefinition, ToolRegistry, ToolResult, call_tool,
};
use crate::spawn::{BASE_ENV_NAMES, BASE_ENV_PREFIXES};

/// Enforces one attempt's [`CapabilityGrant`] over the tool registry.
#[derive(Debug, Clone)]
pub struct GrantGate {
    grant: CapabilityGrant,
}

/// The workspace-relative argument a tool addresses, and the scope kind it
/// needs.
enum PathCheck {
    /// The path may only be read.
    Read(String),
    /// The path will be written.
    Write(String),
    /// The tool reads the whole workspace tree (search, git metadata);
    /// only a root read scope can cover it.
    WholeTree,
    /// The addressed paths cannot be determined (for example a patch that
    /// does not parse); containment can never be proven, so the call is
    /// denied.
    Unverifiable(&'static str),
}

impl GrantGate {
    /// Wrap a grant.
    pub fn new(grant: CapabilityGrant) -> Self {
        GrantGate { grant }
    }

    /// The enforced grant.
    pub fn grant(&self) -> &CapabilityGrant {
        &self.grant
    }

    /// Every registered tool whose capability class the grant permits.
    /// Provider-facing tool lists shown to a model must come from here, so
    /// an attempt never even sees tools outside its authority.
    pub fn visible_tools(&self, registry: &ToolRegistry) -> Vec<Arc<ToolDefinition>> {
        registry
            .list()
            .into_iter()
            .filter(|tool| matches!(self.permits_tool(tool), PermissionDecision::Allow))
            .collect()
    }

    /// The capability-class check: does the grant carry the operation this
    /// tool's side-effect class requires?
    pub fn permits_tool(&self, tool: &ToolDefinition) -> PermissionDecision {
        let operation = match tool.capability {
            ToolCapability::Read => Operation::Read,
            ToolCapability::Write => Operation::Write,
            ToolCapability::Execute => Operation::Execute,
        };
        if self.grant.permits(operation) {
            PermissionDecision::Allow
        } else {
            PermissionDecision::Deny {
                reason: format!(
                    "grant {} does not permit {} operations, so the {} tool is out of scope",
                    self.grant.id,
                    operation_name(operation),
                    tool.name
                ),
                needs_user: true,
            }
        }
    }

    /// The full per-call check: capability class, path scopes
    /// (pre-resolution), process allowlist, and environment names.
    pub fn check_call(&self, tool: &ToolDefinition, args: &Value) -> PermissionDecision {
        if let PermissionDecision::Deny { reason, needs_user } = self.permits_tool(tool) {
            return PermissionDecision::Deny { reason, needs_user };
        }
        if let Some(check) = self.path_check(tool, args)
            && let PermissionDecision::Deny { reason, needs_user } = self.check_paths(&check)
        {
            return PermissionDecision::Deny { reason, needs_user };
        }
        if tool.capability == ToolCapability::Execute
            && let PermissionDecision::Deny { reason, needs_user } = self.check_command(tool, args)
        {
            return PermissionDecision::Deny { reason, needs_user };
        }
        PermissionDecision::Allow
    }

    /// Whether delivery-preparation activity (building the delivery package
    /// and its evidence) is authorized. Delivery itself is never grantable:
    /// only an explicit human decision authorizes it.
    pub fn permits_delivery_prep(&self) -> PermissionDecision {
        if self.grant.delivery == DeliveryAuthority::PreparePackage {
            PermissionDecision::Allow
        } else {
            PermissionDecision::Deny {
                reason: format!(
                    "grant {} has no delivery-preparation authority",
                    self.grant.id
                ),
                needs_user: true,
            }
        }
    }

    /// Validate arguments through the tool's schema and execute, but only
    /// when the grant permits the call. Denials (and unknown tools) return a
    /// failed result naming the reason; nothing runs.
    pub fn call(
        &self,
        registry: &ToolRegistry,
        name: &str,
        args: &Value,
        context: &ToolContext,
    ) -> ToolResult {
        let tool = match registry.get(name) {
            Ok(tool) => tool,
            Err(error) => return ToolResult::fail(error.to_string()),
        };
        match self.check_call(&tool, args) {
            PermissionDecision::Allow => call_tool(&tool, args, context),
            PermissionDecision::Deny { reason, needs_user } => ToolResult::fail(format!(
                "denied by capability grant{}: {reason}",
                if needs_user {
                    " (a user could widen this authority)"
                } else {
                    ""
                }
            )),
        }
    }

    /// Which workspace area the call addresses, by tool and arguments.
    fn path_check(&self, tool: &ToolDefinition, args: &Value) -> Option<PathCheck> {
        let arg_path = |key: &str, default: Option<&str>| -> Option<String> {
            let value = args.get(key).and_then(Value::as_str).or(default)?;
            Some(normalize_workspace_path(value))
        };
        match tool.name.as_str() {
            "read-file" => arg_path("path", None).map(PathCheck::Read),
            "list-dir" => arg_path("path", Some(".")).map(PathCheck::Read),
            "write-file" => arg_path("path", None).map(PathCheck::Write),
            // The patch tool writes every target named in its headers; a
            // patch that does not parse can never be proven in scope.
            "apply-patch" => {
                args.get("patch")
                    .and_then(Value::as_str)
                    .map(|patch| match parse_patch(patch) {
                        Ok(files) => PathCheck::Write(
                            files
                                .into_iter()
                                .map(|file| normalize_workspace_path(&file.rel_path))
                                .collect::<Vec<_>>()
                                .join("\n"),
                        ),
                        Err(_) => PathCheck::Unverifiable("the patch does not parse"),
                    })
            }
            // These read the whole tree or repository metadata.
            "search-text" | "git-status" | "git-diff" | "git-log" | "repo-instructions" => {
                Some(PathCheck::WholeTree)
            }
            "run-command" => arg_path("cwd", Some(".")).map(PathCheck::Read),
            _ => None,
        }
    }

    /// Containment check for one addressed path (or a multi-path payload
    /// joined by newlines, as produced for patches).
    fn check_paths(&self, check: &PathCheck) -> PermissionDecision {
        match check {
            PathCheck::Read(path) => self.check_one_path(path, &self.grant.read_paths, "read"),
            PathCheck::Write(path) => self.check_one_path(path, &self.grant.write_paths, "write"),
            PathCheck::Unverifiable(reason) => PermissionDecision::Deny {
                reason: format!("{reason}, so its targets cannot be checked against the grant"),
                needs_user: false,
            },
            PathCheck::WholeTree => {
                let root = ScopePath::root();
                if self
                    .grant
                    .read_paths
                    .iter()
                    .any(|scope| scope.contains(&root))
                {
                    PermissionDecision::Allow
                } else {
                    PermissionDecision::Deny {
                        reason: format!(
                            "{} reads the whole workspace, but grant {} only reads [{}]",
                            tool_name_for_reason(check),
                            self.grant.id,
                            scope_list(&self.grant.read_paths),
                        ),
                        needs_user: true,
                    }
                }
            }
        }
    }

    fn check_one_path(&self, raw: &str, scopes: &[ScopePath], verb: &str) -> PermissionDecision {
        let joined = raw.split('\n').filter(|line| !line.is_empty());
        let mut targets = Vec::new();
        for line in joined {
            match ScopePath::parse(line) {
                Ok(scope) => targets.push(scope),
                Err(_) => {
                    // Unparsable shapes (odd characters, traversal, absolute
                    // paths) can never be proven inside a scope.
                    return PermissionDecision::Deny {
                        reason: format!(
                            "cannot verify that \"{line}\" stays inside the grant's {verb} scopes"
                        ),
                        needs_user: false,
                    };
                }
            }
        }
        for target in &targets {
            let covered = scopes.iter().any(|scope| scope.contains(target));
            if !covered {
                return PermissionDecision::Deny {
                    reason: format!(
                        "\"{}\" is outside grant {}'s {verb} scopes [{}]",
                        target,
                        self.grant.id,
                        scope_list(scopes),
                    ),
                    needs_user: true,
                };
            }
        }
        PermissionDecision::Allow
    }

    /// Process allowlist (exact program, argument prefix) and environment
    /// name check for `run-command`.
    fn check_command(&self, tool: &ToolDefinition, args: &Value) -> PermissionDecision {
        let command = args.get("command").and_then(Value::as_str).unwrap_or("");
        let arg_list: Vec<String> = args
            .get("args")
            .and_then(Value::as_array)
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        let permitted = self.grant.command_allowlist.iter().any(|pattern| {
            pattern.program == command
                && arg_list.len() >= pattern.args_prefix.len()
                && pattern
                    .args_prefix
                    .iter()
                    .zip(arg_list.iter())
                    .all(|(a, b)| a == b)
        });
        if !permitted {
            return PermissionDecision::Deny {
                reason: format!(
                    "command \"{}\" with [{}] is not in grant {}'s process allowlist",
                    command,
                    arg_list.join(", "),
                    self.grant.id,
                ),
                needs_user: true,
            };
        }
        if let Some(env) = args.get("env").and_then(Value::as_array) {
            for entry in env.iter().filter_map(Value::as_str) {
                let base = BASE_ENV_NAMES.contains(&entry)
                    || BASE_ENV_PREFIXES
                        .iter()
                        .any(|prefix| entry.starts_with(prefix));
                if !base
                    && !self
                        .grant
                        .environment_names
                        .iter()
                        .any(|name| name == entry)
                {
                    return PermissionDecision::Deny {
                        reason: format!(
                            "environment variable \"{entry}\" is neither a base variable nor named by grant {}",
                            self.grant.id,
                        ),
                        needs_user: true,
                    };
                }
            }
        }
        let _ = tool;
        PermissionDecision::Allow
    }
}

/// The domain spelling of an operation, for denial messages.
fn operation_name(operation: Operation) -> &'static str {
    match operation {
        Operation::Read => "read",
        Operation::Write => "write",
        Operation::Execute => "execute",
        Operation::Network => "network",
    }
}

fn scope_list(scopes: &[ScopePath]) -> String {
    if scopes.is_empty() {
        "nothing".to_owned()
    } else {
        scopes
            .iter()
            .map(|scope| scope.as_str().to_owned())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn tool_name_for_reason(check: &PathCheck) -> &'static str {
    match check {
        PathCheck::WholeTree => "this tool",
        _ => "the call",
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::create_default_tool_registry;
    use relay_config::ToolSettings;
    use relay_domain::{GrantId, Operation};
    use serde_json::json;

    fn grant(adjust: impl FnOnce(&mut CapabilityGrant)) -> CapabilityGrant {
        let mut value = CapabilityGrant {
            id: GrantId::parse("grant_test").unwrap(),
            read_paths: vec![ScopePath::root()],
            write_paths: vec![ScopePath::parse("src").unwrap()],
            operations: vec![Operation::Read, Operation::Write, Operation::Execute],
            command_allowlist: vec![relay_domain::CommandPattern {
                program: "cargo".to_owned(),
                args_prefix: vec!["test".to_owned()],
            }],
            environment_names: vec!["RELAY_TOOL_TEST_VAR".to_owned()],
            model_route: None,
            budget: relay_domain::Budget::default(),
            delivery: DeliveryAuthority::None,
        };
        adjust(&mut value);
        value
    }

    fn registry() -> ToolRegistry {
        create_default_tool_registry(&ToolSettings::default()).expect("registry")
    }

    fn tool(registry: &ToolRegistry, name: &str) -> Arc<ToolDefinition> {
        registry.get(name).expect("tool")
    }

    #[test]
    fn reads_are_permitted_within_read_scopes() {
        let gate = GrantGate::new(grant(|_| {}));
        let registry = registry();
        let read = tool(&registry, "read-file");
        assert_eq!(
            gate.check_call(&read, &json!({ "path": "src/lib.rs" })),
            PermissionDecision::Allow
        );
        // The grant reads the root, so a sibling directory is covered too.
        assert_eq!(
            gate.check_call(&read, &json!({ "path": "docs/notes.md" })),
            PermissionDecision::Allow
        );
    }

    #[test]
    fn reads_outside_a_narrow_scope_are_denied() {
        let gate = GrantGate::new(grant(|value| {
            value.read_paths = vec![ScopePath::parse("src").unwrap()];
        }));
        let registry = registry();
        let read = tool(&registry, "read-file");
        let decision = gate.check_call(&read, &json!({ "path": "docs/notes.md" }));
        match decision {
            PermissionDecision::Deny { reason, needs_user } => {
                assert!(reason.contains("outside grant"), "{reason}");
                assert!(reason.contains("docs/notes.md"), "{reason}");
                assert!(needs_user);
            }
            PermissionDecision::Allow => panic!("read outside scope must be denied"),
        }
    }

    #[test]
    fn whole_tree_tools_need_a_root_read_scope() {
        let gate = GrantGate::new(grant(|value| {
            value.read_paths = vec![ScopePath::parse("src").unwrap()];
        }));
        let registry = registry();
        let search = tool(&registry, "search-text");
        assert!(matches!(
            gate.check_call(&search, &json!({ "query": "x" })),
            PermissionDecision::Deny { .. }
        ));
        let gate = GrantGate::new(grant(|_| {}));
        assert_eq!(
            gate.check_call(&search, &json!({ "query": "x" })),
            PermissionDecision::Allow
        );
    }

    #[test]
    fn writes_are_permitted_only_within_write_scopes() {
        let gate = GrantGate::new(grant(|_| {}));
        let registry = registry();
        let write = tool(&registry, "write-file");
        assert_eq!(
            gate.check_call(&write, &json!({ "path": "src/new.rs", "content": "x" })),
            PermissionDecision::Allow
        );
        let decision = gate.check_call(&write, &json!({ "path": "docs/new.md", "content": "x" }));
        assert!(matches!(decision, PermissionDecision::Deny { .. }));
    }

    #[test]
    fn write_tools_are_denied_without_the_write_operation() {
        let gate = GrantGate::new(grant(|value| {
            value
                .operations
                .retain(|operation| *operation != Operation::Write);
        }));
        let registry = registry();
        let write = tool(&registry, "write-file");
        assert!(matches!(
            gate.permits_tool(&write),
            PermissionDecision::Deny { .. }
        ));
        assert!(
            gate.visible_tools(&registry)
                .iter()
                .all(|tool| tool.capability != ToolCapability::Write),
            "write tools must not be visible without the write operation"
        );
    }

    #[test]
    fn patch_targets_are_checked_against_write_scopes() {
        let gate = GrantGate::new(grant(|_| {}));
        let registry = registry();
        let patch = tool(&registry, "apply-patch");
        let inside = "--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1 +1 @@\n-a\n+b\n";
        assert_eq!(
            gate.check_call(&patch, &json!({ "patch": inside })),
            PermissionDecision::Allow
        );
        let outside = "--- a/docs/a.md\n+++ b/docs/a.md\n@@ -1 +1 @@\n-a\n+b\n";
        let decision = gate.check_call(&patch, &json!({ "patch": outside }));
        assert!(matches!(decision, PermissionDecision::Deny { .. }));
        // Malformed patches never reach the tool body from the gate either.
        let garbage = gate.check_call(&patch, &json!({ "patch": "not a patch" }));
        assert!(matches!(garbage, PermissionDecision::Deny { .. }));
    }

    #[test]
    fn commands_need_an_allowlist_prefix_match() {
        let gate = GrantGate::new(grant(|_| {}));
        let registry = registry();
        let run = tool(&registry, "run-command");
        // Exact program + prefix; trailing arguments are permitted.
        assert_eq!(
            gate.check_call(
                &run,
                &json!({ "command": "cargo", "args": ["test", "--workspace"] })
            ),
            PermissionDecision::Allow
        );
        // Different program or subcommand is denied.
        assert!(matches!(
            gate.check_call(&run, &json!({ "command": "cargo", "args": ["publish"] })),
            PermissionDecision::Deny { .. }
        ));
        assert!(matches!(
            gate.check_call(&run, &json!({ "command": "rm", "args": ["-rf", "/"] })),
            PermissionDecision::Deny { .. }
        ));
        // An empty allowlist denies everything (fail closed).
        let gate = GrantGate::new(grant(|value| value.command_allowlist.clear()));
        assert!(matches!(
            gate.check_call(&run, &json!({ "command": "cargo", "args": ["test"] })),
            PermissionDecision::Deny { .. }
        ));
    }

    #[test]
    fn environment_names_are_the_grant_plus_the_base_allowlist() {
        let gate = GrantGate::new(grant(|_| {}));
        let registry = registry();
        let run = tool(&registry, "run-command");
        let base = json!({ "command": "cargo", "args": ["test"], "env": ["PATH", "LC_ALL"] });
        assert_eq!(gate.check_call(&run, &base), PermissionDecision::Allow);
        let named = json!({
            "command": "cargo",
            "args": ["test"],
            "env": ["RELAY_TOOL_TEST_VAR"],
        });
        assert_eq!(gate.check_call(&run, &named), PermissionDecision::Allow);
        let unlisted = json!({
            "command": "cargo",
            "args": ["test"],
            "env": ["RELAY_NEVER_NAMED"],
        });
        assert!(matches!(
            gate.check_call(&run, &unlisted),
            PermissionDecision::Deny { .. }
        ));
    }

    #[test]
    fn unparsable_paths_are_denied_fail_closed() {
        let gate = GrantGate::new(grant(|_| {}));
        let registry = registry();
        let read = tool(&registry, "read-file");
        assert!(matches!(
            gate.check_call(&read, &json!({ "path": "od d chars/here" })),
            PermissionDecision::Deny { .. }
        ));
    }

    #[test]
    fn delivery_preparation_needs_explicit_authority() {
        let gate = GrantGate::new(grant(|_| {}));
        assert!(matches!(
            gate.permits_delivery_prep(),
            PermissionDecision::Deny { .. }
        ));
        let gate = GrantGate::new(grant(|value| {
            value.delivery = DeliveryAuthority::PreparePackage;
        }));
        assert_eq!(gate.permits_delivery_prep(), PermissionDecision::Allow);
    }

    #[test]
    fn gate_call_denies_without_running_the_tool() {
        let workspace = tempfile::Builder::new()
            .prefix("relay-gate-")
            .tempdir()
            .expect("temp dir")
            .keep();
        let gate = GrantGate::new(grant(|value| {
            value.write_paths = vec![ScopePath::parse("docs").unwrap()];
        }));
        let registry = registry();
        let result = gate.call(
            &registry,
            "write-file",
            &json!({ "path": "src/evil.rs", "content": "nope" }),
            &ToolContext::new(&workspace),
        );
        assert!(!result.ok);
        assert!(
            result.output.contains("denied by capability grant"),
            "{}",
            result.output
        );
        assert!(
            !workspace.join("src/evil.rs").exists(),
            "a denied call must not touch the workspace"
        );
        std::fs::create_dir_all(workspace.join("docs")).expect("mkdir");
        let allowed = gate.call(
            &registry,
            "write-file",
            &json!({ "path": "docs/ok.md", "content": "fine" }),
            &ToolContext::new(&workspace),
        );
        assert!(allowed.ok, "{}", allowed.output);
    }
}
