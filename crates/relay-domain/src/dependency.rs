//! Typed dependency edges between work items.

use std::fmt;

use crate::ids::WorkItemId;

/// The kind of relation one work item has to another.
///
/// Edge direction is uniform across kinds: an edge `from → to` means *from
/// depends on to* (`from` is the dependent, `to` the prerequisite).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum DependencyKind {
    /// `from` cannot start before `to` completes; invalidation of `to`
    /// propagates to `from`.
    Requires,
    /// `from` should consume what `to` produces before starting. Gates
    /// readiness, but does not propagate invalidation: work informed by later
    /// invalidated research is not automatically void.
    Informs,
    /// `from` (typically a verifier) evaluates the result of `to`. Gates
    /// readiness and propagates invalidation: verifying void work is void.
    Validates,
    /// `from` replaces `to`. Does not gate readiness or propagate
    /// invalidation; it records replacement history.
    Supersedes,
    /// `from` and `to` address the same concern in incompatible ways.
    /// Symmetric; never gates readiness and never propagates invalidation.
    ConflictsWith,
}

impl DependencyKind {
    /// Whether an unsatisfied edge of this kind keeps the dependent from
    /// becoming ready.
    pub fn gates_readiness(self) -> bool {
        matches!(
            self,
            DependencyKind::Requires | DependencyKind::Informs | DependencyKind::Validates
        )
    }

    /// Whether invalidity of the prerequisite propagates across this edge.
    pub fn propagates_invalidation(self) -> bool {
        matches!(self, DependencyKind::Requires | DependencyKind::Validates)
    }

    /// Whether this kind participates in acyclicity enforcement.
    /// `ConflictsWith` is symmetric, so two opposite edges state one fact and
    /// must be exempt from cycle detection.
    pub fn is_ordering(self) -> bool {
        self != DependencyKind::ConflictsWith
    }
}

impl fmt::Display for DependencyKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            DependencyKind::Requires => "requires",
            DependencyKind::Informs => "informs",
            DependencyKind::Validates => "validates",
            DependencyKind::Supersedes => "supersedes",
            DependencyKind::ConflictsWith => "conflicts-with",
        };
        f.write_str(name)
    }
}

/// The identity of a dependency: the triple (from, to, kind).
///
/// Edges are identified by this triple rather than by a separate id, so the
/// same edge can never be asserted twice and retraction addresses the exact
/// triple that was asserted.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct DependencyEdge {
    /// The dependent work item.
    pub from: WorkItemId,
    /// The prerequisite work item.
    pub to: WorkItemId,
    /// The typed relation.
    pub kind: DependencyKind,
}

impl DependencyEdge {
    /// Construct an edge from its parts.
    pub fn new(from: WorkItemId, to: WorkItemId, kind: DependencyKind) -> Self {
        DependencyEdge { from, to, kind }
    }
}

impl fmt::Display for DependencyEdge {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {} {}", self.from, self.kind, self.to)
    }
}
