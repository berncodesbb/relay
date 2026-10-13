//! Pure validation helpers shared by the fact fold and engine policy.

use std::collections::BTreeMap;
use std::collections::BTreeSet;

use crate::dependency::DependencyEdge;
use crate::grant::CapabilityGrant;
use crate::work::{ExecutorClass, ScopePath, WorkItem};

/// Finds a cycle among the ordering-kind dependency edges, returning the
/// edges of one cycle path (in path order). Deterministic: nodes are visited
/// in sorted order.
pub fn find_cycle(edges: &[DependencyEdge]) -> Option<Vec<DependencyEdge>> {
    let mut adjacency: BTreeMap<&crate::ids::WorkItemId, BTreeSet<&DependencyEdge>> =
        BTreeMap::new();
    for edge in edges {
        if edge.kind.is_ordering() {
            adjacency.entry(&edge.from).or_default().insert(edge);
        }
    }
    // DFS with colors: 0 = unvisited, 1 = on stack, 2 = done.
    let mut color: BTreeMap<&crate::ids::WorkItemId, u8> = BTreeMap::new();
    let mut stack: Vec<&DependencyEdge> = Vec::new();

    fn dfs<'a>(
        node: &'a crate::ids::WorkItemId,
        adjacency: &BTreeMap<&'a crate::ids::WorkItemId, BTreeSet<&'a DependencyEdge>>,
        color: &mut BTreeMap<&'a crate::ids::WorkItemId, u8>,
        stack: &mut Vec<&'a DependencyEdge>,
    ) -> Option<Vec<DependencyEdge>> {
        color.insert(node, 1);
        if let Some(outgoing) = adjacency.get(node) {
            for edge in outgoing {
                match color.get(&edge.to).copied().unwrap_or(0) {
                    1 => {
                        // Reconstruct the cycle from the stack.
                        let start = stack.iter().position(|e| e.from == edge.to);
                        let mut cycle: Vec<DependencyEdge> = Vec::new();
                        if let Some(index) = start {
                            cycle.extend(stack[index..].iter().map(|e| (*e).clone()));
                        }
                        cycle.push((*edge).clone());
                        return Some(cycle);
                    }
                    0 => {
                        stack.push(edge);
                        if let Some(cycle) = dfs(&edge.to, adjacency, color, stack) {
                            return Some(cycle);
                        }
                        stack.pop();
                    }
                    _ => {}
                }
            }
        }
        color.insert(node, 2);
        None
    }

    let nodes: Vec<&crate::ids::WorkItemId> = adjacency.keys().copied().collect();
    for node in nodes {
        if color.get(node).copied().unwrap_or(0) == 0
            && let Some(cycle) = dfs(node, &adjacency, &mut color, &mut stack)
        {
            return Some(cycle);
        }
    }
    None
}

/// Error raised when a work item's declaration is internally inconsistent.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WorkShapeError {
    /// Write scopes were declared for an executor class that cannot write.
    #[error("write scopes {scopes:?} require executor class workspace, but the work is {executor}")]
    WriteScopesRequireWorkspaceExecutor {
        /// The offending scopes.
        scopes: Vec<ScopePath>,
        /// The declared executor class.
        executor: ExecutorClass,
    },
    /// A declared write scope sits above the capability ceiling.
    #[error("write scope {scope} exceeds the capability ceiling")]
    WriteScopeAboveCeiling {
        /// The offending scope.
        scope: ScopePath,
    },
}

/// Check a work item's declaration for internal coherence: only
/// `Workspace`-class work may write, and write scopes must sit within the
/// declared capability ceiling.
pub fn check_work_shape(work: &WorkItem) -> Result<(), WorkShapeError> {
    if !work.write_scopes.is_empty() && !work.executor.may_write() {
        return Err(WorkShapeError::WriteScopesRequireWorkspaceExecutor {
            scopes: work.write_scopes.clone(),
            executor: work.executor,
        });
    }
    for scope in &work.write_scopes {
        let covered = work
            .capability_ceiling
            .write_scopes
            .iter()
            .any(|allowed| allowed.contains(scope));
        if !covered {
            return Err(WorkShapeError::WriteScopeAboveCeiling {
                scope: scope.clone(),
            });
        }
    }
    Ok(())
}

/// Error raised when a grant does not cover a work item's needs.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GrantGap {
    /// A write scope is outside the grant's writable area.
    #[error("write scope {scope} is not covered by the grant")]
    WriteScope {
        /// The uncovered scope.
        scope: ScopePath,
    },
    /// A read scope is outside the grant's readable area.
    #[error("read scope {scope} is not covered by the grant")]
    ReadScope {
        /// The uncovered scope.
        scope: ScopePath,
    },
}

/// Check that a grant covers the given read and write scopes (each scope
/// must sit inside some granted path).
pub fn grant_covers_scopes(
    grant: &CapabilityGrant,
    read_scopes: &[ScopePath],
    write_scopes: &[ScopePath],
) -> Result<(), GrantGap> {
    for scope in read_scopes {
        let covered = grant
            .read_paths
            .iter()
            .any(|granted| granted.contains(scope));
        if !covered {
            return Err(GrantGap::ReadScope {
                scope: scope.clone(),
            });
        }
    }
    for scope in write_scopes {
        let covered = grant
            .write_paths
            .iter()
            .any(|granted| granted.contains(scope));
        if !covered {
            return Err(GrantGap::WriteScope {
                scope: scope.clone(),
            });
        }
    }
    Ok(())
}

/// Check that every scope in `scopes` sits within at least one allowed
/// ceiling scope.
pub fn scopes_within_ceiling(scopes: &[ScopePath], allowed: &[ScopePath]) -> bool {
    scopes
        .iter()
        .all(|scope| allowed.iter().any(|a| a.contains(scope)))
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
    use crate::dependency::DependencyKind;
    use crate::ids::WorkItemId;

    fn edge(from: &str, to: &str, kind: DependencyKind) -> DependencyEdge {
        DependencyEdge::new(
            WorkItemId::parse(from).ok().unwrap(),
            WorkItemId::parse(to).ok().unwrap(),
            kind,
        )
    }

    #[test]
    fn detects_direct_and_transitive_cycles() {
        let a_b = edge("work_a", "work_b", DependencyKind::Requires);
        let b_c = edge("work_b", "work_c", DependencyKind::Requires);
        let c_a = edge("work_c", "work_a", DependencyKind::Requires);
        assert!(find_cycle(&[a_b.clone(), b_c.clone(), c_a.clone()]).is_some());
        assert!(find_cycle(&[a_b.clone(), b_c.clone()]).is_none());
        let a_a = edge("work_a", "work_a", DependencyKind::Requires);
        assert!(find_cycle(&[a_a]).is_some());
    }

    #[test]
    fn conflicts_with_edges_are_exempt_from_cycle_detection() {
        let a_b = edge("work_a", "work_b", DependencyKind::ConflictsWith);
        let b_a = edge("work_b", "work_a", DependencyKind::ConflictsWith);
        assert!(find_cycle(&[a_b, b_a]).is_none());
    }

    #[test]
    fn supersedes_edges_participate_in_cycles() {
        let a_b = edge("work_a", "work_b", DependencyKind::Supersedes);
        let b_a = edge("work_b", "work_a", DependencyKind::Supersedes);
        assert!(find_cycle(&[a_b, b_a]).is_some());
    }
}
