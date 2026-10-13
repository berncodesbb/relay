//! Work items: the bounded objectives that make up the work graph.

use crate::artifact::ArtifactVersionRef;
use crate::dependency::DependencyEdge;
use crate::grant::{DeliveryAuthority, Operation};
use crate::ids::{ArtifactId, DecisionId, WorkItemId};

/// Error raised when a work kind label is malformed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "invalid work kind \"{0}\": expected 1-48 chars of [a-z0-9-] not starting or ending with '-'"
)]
pub struct WorkKindError(String);

/// The kind of work, as an extensible kebab-case label (for example
/// `discover-opportunity`, `verify-criterion`, `correct-implementation`).
///
/// This is identity data, not state: the kernel and engine never branch on
/// the text. All behavioral distinctions live in typed fields — executor
/// class, completion rule, scopes, dependencies — so the built-in workflow
/// (Phase 2) can grow its vocabulary without kernel changes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WorkKind(String);

impl WorkKind {
    /// Validate a kebab-case kind label.
    pub fn parse(value: &str) -> Result<Self, WorkKindError> {
        let reject = || WorkKindError(value.to_string());
        let bytes = value.as_bytes();
        if bytes.is_empty()
            || bytes.len() > 48
            || bytes[0] == b'-'
            || bytes[bytes.len() - 1] == b'-'
        {
            return Err(reject());
        }
        if !bytes
            .iter()
            .all(|&b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err(reject());
        }
        Ok(WorkKind(value.to_string()))
    }

    /// The label text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for WorkKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::str::FromStr for WorkKind {
    type Err = WorkKindError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        WorkKind::parse(value)
    }
}

impl serde::Serialize for WorkKind {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> serde::Deserialize<'de> for WorkKind {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        WorkKind::parse(&value).map_err(serde::de::Error::custom)
    }
}

/// Which executor class performs a work item.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum ExecutorClass {
    /// A human: approvals, clarifications, consequential choices, delivery
    /// authorization. Human work completes through decision resolution.
    Human,
    /// A model reasoning and proposing structured artifacts, with no
    /// workspace mutation.
    Model,
    /// Model-guided repository work under an explicit capability grant. The
    /// only class that may declare write scopes.
    Workspace,
    /// Read/execute-only evidence collection and criterion evaluation.
    Verifier,
    /// Deterministic system transformations: rendering, projection,
    /// invalidation.
    System,
}

impl ExecutorClass {
    /// Whether work of this class may mutate the workspace.
    pub fn may_write(self) -> bool {
        self == ExecutorClass::Workspace
    }
}

impl std::fmt::Display for ExecutorClass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            ExecutorClass::Human => "human",
            ExecutorClass::Model => "model",
            ExecutorClass::Workspace => "workspace",
            ExecutorClass::Verifier => "verifier",
            ExecutorClass::System => "system",
        };
        f.write_str(name)
    }
}

/// Error raised for malformed workspace scopes.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "invalid workspace scope \"{0}\": expected \".\" or a relative path of non-empty segments [A-Za-z0-9._-] without \"..\" segments, leading '/', or trailing '/'"
)]
pub struct ScopeError(String);

/// A workspace area, expressed as a relative path. `.` denotes the whole
/// workspace root. Path containment (not regex or glob) defines scope
/// overlap and coverage.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ScopePath {
    raw: String,
    segments: Vec<String>,
}

impl ScopePath {
    /// The whole-workspace scope.
    pub fn root() -> Self {
        ScopePath {
            raw: ".".to_string(),
            segments: Vec::new(),
        }
    }

    /// Validate a relative scope path.
    pub fn parse(value: &str) -> Result<Self, ScopeError> {
        let reject = || ScopeError(value.to_string());
        if value == "." {
            return Ok(ScopePath::root());
        }
        if value.starts_with('/')
            || value.ends_with('/')
            || value.contains('\\')
            || value.is_empty()
        {
            return Err(reject());
        }
        let mut segments = Vec::new();
        for segment in value.split('/') {
            if segment.is_empty() || segment == "." || segment == ".." {
                return Err(reject());
            }
            let ok = segment
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-');
            if !ok {
                return Err(reject());
            }
            segments.push(segment.to_string());
        }
        Ok(ScopePath {
            raw: value.to_string(),
            segments,
        })
    }

    /// The canonical text form.
    pub fn as_str(&self) -> &str {
        &self.raw
    }

    /// The path segments; empty for the root scope.
    pub fn segments(&self) -> &[String] {
        &self.segments
    }

    /// Whether `self` is `other` or an ancestor of it.
    pub fn contains(&self, other: &ScopePath) -> bool {
        self.segments.len() <= other.segments.len()
            && self
                .segments
                .iter()
                .zip(other.segments.iter())
                .all(|(a, b)| a == b)
    }

    /// Whether either scope contains the other.
    pub fn overlaps(&self, other: &ScopePath) -> bool {
        self.contains(other) || other.contains(self)
    }
}

impl std::fmt::Display for ScopePath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.raw)
    }
}

impl std::str::FromStr for ScopePath {
    type Err = ScopeError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        ScopePath::parse(value)
    }
}

impl serde::Serialize for ScopePath {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.raw)
    }
}

impl<'de> serde::Deserialize<'de> for ScopePath {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        ScopePath::parse(&value).map_err(serde::de::Error::custom)
    }
}

/// A work item's input: an exact artifact version, or an explicit
/// version-selection rule that floats to the newest publication.
///
/// Exact pins are the default and carry precise invalidation; floating inputs
/// trade that precision for declare-ahead convenience.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum InputRef {
    /// Use exactly this published version.
    #[serde(rename = "exact")]
    Exact(ArtifactVersionRef),
    /// Use the newest published version of the artifact at evaluation time.
    #[serde(rename = "latest_published")]
    LatestPublished(ArtifactId),
}

impl std::fmt::Display for InputRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InputRef::Exact(reference) => write!(f, "exact {reference}"),
            InputRef::LatestPublished(artifact) => write!(f, "latest of {artifact}"),
        }
    }
}

/// What a work item is expected to produce. Descriptive contract data; the
/// enforcement of per-kind output rules arrives with the built-in workflow
/// definitions in Phase 2.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ExpectedOutput {
    /// A short stable label, e.g. `opportunity-brief`.
    pub label: String,
    /// What the output must contain or achieve.
    pub description: String,
}

/// How a work item's completion is recognized.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CompletionRule {
    /// Completes when the referenced decision is resolved with a choice and
    /// has not been superseded. This is how human work completes.
    #[serde(rename = "decision")]
    Decision {
        /// The decision that must be resolved.
        decision: DecisionId,
    },
    /// Completes when an accepted attempt receipt reports the outcome
    /// `completed` for this work item.
    #[serde(rename = "receipt")]
    Receipt,
    /// Completes when the referenced criterion artifact version carries
    /// passing evidence and no failing evidence. Used by verification work:
    /// a failing run leaves the criterion unsatisfied and the work
    /// re-attemptable rather than completed.
    #[serde(rename = "criterion_satisfied")]
    CriterionSatisfied {
        /// The exact criterion version that must be satisfied.
        criterion: ArtifactVersionRef,
    },
}

/// The maximum authority a work item may ever be granted or expanded to.
/// Scope-expanding proposals are checked against this ceiling.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CapabilityCeiling {
    /// The widest writable area the work may ever cover.
    pub write_scopes: Vec<ScopePath>,
    /// The widest operation set the work may ever carry.
    pub operations: Vec<Operation>,
    /// The highest delivery authority the work may ever carry.
    pub delivery: DeliveryAuthority,
}

impl Default for CapabilityCeiling {
    fn default() -> Self {
        CapabilityCeiling {
            write_scopes: Vec::new(),
            operations: vec![Operation::Read],
            delivery: DeliveryAuthority::None,
        }
    }
}

/// One bounded objective in the work graph.
///
/// Work items are declared, not mutated: scope growth arrives as explicit
/// `WorkScopeExpanded` facts, replacement as supersession. Operational state
/// (ready, blocked, running, …) is always derived by `relay-engine`, never
/// stored.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WorkItem {
    /// Addressable work identity.
    pub id: WorkItemId,
    /// The kind label (identity data; see [`WorkKind`]).
    pub kind: WorkKind,
    /// The bounded objective, in one or two sentences.
    pub objective: String,
    /// Which executor class performs the work.
    pub executor: ExecutorClass,
    /// How completion is recognized.
    pub completion: CompletionRule,
    /// Workspace areas the work needs to read.
    pub read_scopes: Vec<ScopePath>,
    /// Workspace areas the work needs to write. Only `Workspace`-class work
    /// may declare write scopes.
    pub write_scopes: Vec<ScopePath>,
    /// Input artifact references (exact versions or selection rules).
    pub inputs: Vec<InputRef>,
    /// What the work is expected to produce.
    pub expected_outputs: Vec<ExpectedOutput>,
    /// Dependencies asserted atomically with the declaration. Later edges
    /// arrive as `DependencyAsserted` facts.
    pub dependencies: Vec<DependencyEdge>,
    /// The maximum authority this work may ever carry.
    pub capability_ceiling: CapabilityCeiling,
    /// Whether the project goal is met without this work completing.
    pub required: bool,
}

impl WorkItem {
    /// Construct a work item with no scopes, inputs, outputs, dependencies,
    /// a read-only ceiling, and `required = false`.
    pub fn new(
        id: WorkItemId,
        kind: WorkKind,
        executor: ExecutorClass,
        objective: impl Into<String>,
        completion: CompletionRule,
    ) -> Self {
        WorkItem {
            id,
            kind,
            objective: objective.into(),
            executor,
            completion,
            read_scopes: Vec::new(),
            write_scopes: Vec::new(),
            inputs: Vec::new(),
            expected_outputs: Vec::new(),
            dependencies: Vec::new(),
            capability_ceiling: CapabilityCeiling::default(),
            required: false,
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::ok_expect,
    clippy::err_expect
)] // test-only conveniences, house style
mod tests {
    use super::*;

    #[test]
    fn kind_labels_validate() {
        assert!(WorkKind::parse("verify-criterion").is_ok());
        assert!(WorkKind::parse("discovery").is_ok());
        assert!(WorkKind::parse("").is_err());
        assert!(WorkKind::parse("-x").is_err());
        assert!(WorkKind::parse("x-").is_err());
        assert!(WorkKind::parse("Kind").is_err());
        assert!(WorkKind::parse(&"k".repeat(49)).is_err());
    }

    #[test]
    fn scopes_parse_and_contain() {
        let root = ScopePath::parse(".").ok().unwrap();
        let src = ScopePath::parse("src").ok().unwrap();
        let src_core = ScopePath::parse("src/core").ok().unwrap();
        let docs = ScopePath::parse("docs").ok().unwrap();
        assert!(root.contains(&src));
        assert!(src.contains(&src_core));
        assert!(!src_core.contains(&src));
        assert!(!src.contains(&docs));
        assert!(src.overlaps(&src_core));
        assert!(root.overlaps(&docs));
        assert!(!src.overlaps(&docs));
        assert!(ScopePath::parse("/abs").is_err());
        assert!(ScopePath::parse("a//b").is_err());
        assert!(ScopePath::parse("../up").is_err());
        assert!(ScopePath::parse("a/..").is_err());
        assert!(ScopePath::parse("trail/").is_err());
        assert!(ScopePath::parse("a b").is_err());
        assert!(ScopePath::parse("src/main.rs").is_ok());
    }
}
