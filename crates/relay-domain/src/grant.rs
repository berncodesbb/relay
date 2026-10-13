//! Capability grants: the exact authority an attempt carries, as pure data.

use crate::ids::GrantId;
use crate::work::ScopePath;

/// Classes of operation a grant may permit.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    /// Read files and directories.
    Read,
    /// Modify files and directories.
    Write,
    /// Spawn allowed processes.
    Execute,
    /// Reach the network through the provider or tools.
    Network,
}

/// How far an attempt may go toward delivery. Delivery itself is deliberately
/// not grantable: only an explicit human decision authorizes it.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryAuthority {
    /// No delivery activity at all (the default).
    None,
    /// Prepare a delivery package and its evidence for human review.
    PreparePackage,
}

/// A process allowlist entry. A command is permitted when its program equals
/// `program` and its arguments start with `args_prefix` — a deterministic
/// prefix match, deliberately not a regex.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CommandPattern {
    /// The exact program name or path.
    pub program: String,
    /// The leading arguments a permitted command must carry (may be empty).
    pub args_prefix: Vec<String>,
}

/// Which provider model an attempt may use. Pure configuration data; the
/// kernel never interprets provider semantics.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ModelRoute {
    /// Provider name as configured.
    pub provider: String,
    /// Model name as configured.
    pub model: String,
    /// Optional semantic role the route was resolved for.
    pub role: Option<String>,
}

/// Resource ceilings for one attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct Budget {
    /// Maximum total input tokens.
    pub max_input_tokens: Option<u64>,
    /// Maximum total output tokens.
    pub max_output_tokens: Option<u64>,
    /// Maximum wall-clock duration in milliseconds.
    pub max_duration_ms: Option<i64>,
    /// Maximum number of model/tool attempts.
    pub max_attempts: Option<u64>,
}

/// The exact authority granted to one attempt.
///
/// Grants are immutable data recorded with `AttemptStarted`; executors cannot
/// exceed them because runtime tool gates check every operation against the
/// grant. The domain additionally validates at fold time that the grant covers
/// the work item's declared scopes.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CapabilityGrant {
    /// Addressable grant identity.
    pub id: GrantId,
    /// Workspace areas the attempt may read.
    pub read_paths: Vec<ScopePath>,
    /// Workspace areas the attempt may write.
    pub write_paths: Vec<ScopePath>,
    /// Operation classes permitted.
    pub operations: Vec<Operation>,
    /// Process allowlist.
    pub command_allowlist: Vec<CommandPattern>,
    /// Names of environment variables passed through to child processes.
    pub environment_names: Vec<String>,
    /// The model route the attempt may use, if any.
    pub model_route: Option<ModelRoute>,
    /// Resource ceilings.
    pub budget: Budget,
    /// Delivery authority.
    pub delivery: DeliveryAuthority,
}

impl CapabilityGrant {
    /// A read-only grant: no writes, no commands, no delivery.
    pub fn read_only(id: GrantId, read_paths: Vec<ScopePath>) -> Self {
        CapabilityGrant {
            id,
            read_paths,
            write_paths: Vec::new(),
            operations: vec![Operation::Read],
            command_allowlist: Vec::new(),
            environment_names: Vec::new(),
            model_route: None,
            budget: Budget::default(),
            delivery: DeliveryAuthority::None,
        }
    }

    /// A grant covering exactly the given read and write scopes with read,
    /// write, and execute operations.
    pub fn for_scopes(
        id: GrantId,
        read_paths: Vec<ScopePath>,
        write_paths: Vec<ScopePath>,
    ) -> Self {
        CapabilityGrant {
            id,
            read_paths,
            write_paths,
            operations: vec![Operation::Read, Operation::Write, Operation::Execute],
            command_allowlist: Vec::new(),
            environment_names: Vec::new(),
            model_route: None,
            budget: Budget::default(),
            delivery: DeliveryAuthority::None,
        }
    }

    /// Whether the grant permits the given operation class.
    pub fn permits(&self, operation: Operation) -> bool {
        self.operations.contains(&operation)
    }
}
