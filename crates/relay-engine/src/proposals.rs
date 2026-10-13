//! `validate_graph_change`: deterministic policy over proposed graph
//! changes. Executors and models propose; this module accepts or rejects.

use relay_domain::validation::{check_work_shape, scopes_within_ceiling};
use relay_domain::{
    DomainFact, FactError, GraphChangeProposal, ProjectGraph, ScopePath, WorkItemId,
};

/// An accepted proposal: the exact facts to append (in order), deterministic
/// notes, and whether the change is consequential enough to require a human
/// decision before the runtime may act on it.
#[derive(Debug, Clone, PartialEq)]
pub struct Applied {
    /// Facts to append, in order.
    pub facts: Vec<DomainFact>,
    /// Deterministic notes about effects (e.g. downstream review needed).
    pub notes: Vec<String>,
    /// Whether a human decision is required before acting on this change.
    /// True for write-scope growth and for canceling required work.
    pub requires_human_decision: bool,
}

/// Why a proposed graph change was rejected.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum Rejected {
    /// The dependency would close a cycle.
    #[error("would create a dependency cycle: {would_create:?}")]
    Cycle {
        /// The edges forming the would-be cycle.
        would_create: Vec<relay_domain::DependencyEdge>,
    },
    /// The proposal collides with existing state.
    #[error("duplicate: {detail}")]
    Duplicate {
        /// What collided.
        detail: String,
    },
    /// The proposal names something that does not exist.
    #[error("unknown reference: {detail}")]
    UnknownReference {
        /// What was missing.
        detail: String,
    },
    /// The proposal exceeds the capability ceiling (operations, delivery
    /// authority, grant magnitude).
    #[error("capability ceiling exceeded: {detail}")]
    CapabilityCeilingExceeded {
        /// What exceeded the ceiling.
        detail: String,
    },
    /// The proposal writes outside the sanctioned area.
    #[error("unauthorized scope expansion: requested {requested:?}, allowed {allowed:?}")]
    UnauthorizedScopeExpansion {
        /// The requested write scopes.
        requested: Vec<ScopePath>,
        /// The ceiling's allowed write scopes.
        allowed: Vec<ScopePath>,
    },
    /// The proposal is inconsistent with current state.
    #[error("invalid in current state: {detail}")]
    InvalidState {
        /// What is inconsistent.
        detail: String,
    },
}

/// Validate one graph-change proposal against the graph.
///
/// Validation is deterministic policy and nothing else: structural checks
/// (cycles, duplicates, references, ceilings) run first for precise typed
/// reasons, then the resulting facts are probe-folded onto a clone of the
/// graph so proposal semantics can never drift from fold semantics. No fact
/// is applied to the real graph here.
pub fn validate_graph_change(
    graph: &ProjectGraph,
    proposal: &GraphChangeProposal,
) -> Result<Applied, Rejected> {
    // Precise pre-checks where the rejection variant must carry structured
    // data the fold cannot express.
    match proposal {
        GraphChangeProposal::DeclareWork { work } => {
            if graph.work(&work.id).is_some() {
                return Err(Rejected::Duplicate {
                    detail: format!("work {} is already declared", work.id),
                });
            }
            if let Err(error) = check_work_shape(work) {
                return Err(Rejected::InvalidState {
                    detail: error.to_string(),
                });
            }
        }
        GraphChangeProposal::DeclareDependency { edge } => {
            if graph.dependency(edge).is_some() {
                return Err(Rejected::Duplicate {
                    detail: format!("dependency {edge} is already asserted"),
                });
            }
        }
        GraphChangeProposal::SupersedeWork {
            supersede,
            replacement,
            ..
        } => {
            if graph.work(supersede).is_none() {
                return Err(Rejected::UnknownReference {
                    detail: format!("work {supersede} does not exist"),
                });
            }
            if graph.work(&replacement.id).is_some() {
                return Err(Rejected::Duplicate {
                    detail: format!("replacement work {} is already declared", replacement.id),
                });
            }
            if let Err(error) = check_work_shape(replacement) {
                return Err(Rejected::InvalidState {
                    detail: error.to_string(),
                });
            }
        }
        GraphChangeProposal::OpenDecision { decision } => {
            if graph.decision(&decision.id).is_some() {
                return Err(Rejected::Duplicate {
                    detail: format!("decision {} is already opened", decision.id),
                });
            }
        }
        GraphChangeProposal::ExpandScope {
            work,
            added_write_scopes,
            added_operations,
            ..
        } => {
            let node = graph.work(work).ok_or_else(|| Rejected::UnknownReference {
                detail: format!("work {work} does not exist"),
            })?;
            let mut combined = node.effective_write_scopes();
            for scope in added_write_scopes {
                if !combined.contains(scope) {
                    combined.push(scope.clone());
                }
            }
            if !scopes_within_ceiling(&combined, &node.work.capability_ceiling.write_scopes) {
                return Err(Rejected::UnauthorizedScopeExpansion {
                    requested: combined,
                    allowed: node.work.capability_ceiling.write_scopes.clone(),
                });
            }
            let mut operations = node.work.capability_ceiling.operations.clone();
            for operation in added_operations {
                if !operations.contains(operation) {
                    operations.push(*operation);
                }
            }
            let ceiling_operations = node.work.capability_ceiling.operations.clone();
            if !operations
                .iter()
                .all(|operation| ceiling_operations.contains(operation))
            {
                return Err(Rejected::CapabilityCeilingExceeded {
                    detail: format!(
                        "operations {operations:?} exceed the ceiling {ceiling_operations:?} of work {work}"
                    ),
                });
            }
        }
        GraphChangeProposal::CancelWork { work, .. } => {
            if graph.work(work).is_none() {
                return Err(Rejected::UnknownReference {
                    detail: format!("work {work} does not exist"),
                });
            }
        }
        GraphChangeProposal::RetractDependency { edge, .. } => {
            if graph.dependency(edge).is_none() {
                return Err(Rejected::UnknownReference {
                    detail: format!("dependency {edge} is not asserted"),
                });
            }
        }
        GraphChangeProposal::DeclareInvalidation { target, .. } => {
            let known = match target {
                relay_domain::InvalidationTarget::Work(work) => graph.work(work).is_some(),
                relay_domain::InvalidationTarget::ArtifactVersion(reference) => {
                    graph.version_record(reference).is_some()
                }
                relay_domain::InvalidationTarget::Evidence(evidence) => {
                    graph.evidence_by_id(evidence).is_some()
                }
            };
            if !known {
                return Err(Rejected::UnknownReference {
                    detail: format!("invalidation target does not exist: {target:?}"),
                });
            }
        }
    }

    // Expand into facts.
    let (facts, requires_human_decision, notes) = match proposal {
        GraphChangeProposal::DeclareWork { work } => (
            vec![DomainFact::WorkDeclared { work: work.clone() }],
            false,
            vec![format!("declared work {} ({})", work.id, work.kind)],
        ),
        GraphChangeProposal::DeclareDependency { edge } => (
            vec![DomainFact::DependencyAsserted {
                from: edge.from.clone(),
                to: edge.to.clone(),
                kind: edge.kind,
            }],
            false,
            vec![format!("asserted {edge}")],
        ),
        GraphChangeProposal::SupersedeWork {
            supersede,
            replacement,
            reason,
        } => {
            let dependents: Vec<WorkItemId> = graph
                .dependents_of(supersede)
                .map(|edge| edge.from.clone())
                .collect();
            let mut facts = vec![DomainFact::WorkDeclared {
                work: replacement.clone(),
            }];
            facts.push(DomainFact::DependencyAsserted {
                from: replacement.id.clone(),
                to: supersede.clone(),
                kind: relay_domain::DependencyKind::Supersedes,
            });
            facts.push(DomainFact::WorkSuperseded {
                work: supersede.clone(),
                superseded_by: replacement.id.clone(),
                reason: reason.clone(),
            });
            let mut notes = vec![format!("work {supersede} superseded by {}", replacement.id)];
            if !dependents.is_empty() {
                notes.push(format!(
                    "{} dependent work item(s) now depend on the superseding chain: {:?}",
                    dependents.len(),
                    dependents
                ));
            }
            (facts, false, notes)
        }
        GraphChangeProposal::CancelWork { work, reason } => {
            let required = graph.work(work).is_some_and(|node| node.work.required);
            (
                vec![DomainFact::WorkCanceled {
                    work: work.clone(),
                    reason: reason.clone(),
                }],
                required,
                vec![format!("canceled work {work}: {reason}")],
            )
        }
        GraphChangeProposal::RetractDependency { edge, reason } => (
            vec![DomainFact::DependencyRetracted {
                from: edge.from.clone(),
                to: edge.to.clone(),
                kind: edge.kind,
                reason: reason.clone(),
            }],
            false,
            vec![format!("retracted {edge}")],
        ),
        GraphChangeProposal::OpenDecision { decision } => (
            vec![DomainFact::DecisionOpened {
                decision: decision.clone(),
            }],
            false,
            vec![format!("opened decision {} for a human", decision.id)],
        ),
        GraphChangeProposal::ExpandScope {
            work,
            added_write_scopes,
            added_operations,
            reason,
        } => (
            vec![DomainFact::WorkScopeExpanded {
                work: work.clone(),
                added_write_scopes: added_write_scopes.clone(),
                added_operations: added_operations.clone(),
                reason: reason.clone(),
            }],
            !added_write_scopes.is_empty(),
            vec![format!(
                "expanded scopes of work {work}: +{added_write_scopes:?} write, +{added_operations:?} operations ({reason})"
            )],
        ),
        GraphChangeProposal::DeclareInvalidation {
            target,
            cause,
            changed_input,
        } => (
            vec![DomainFact::InvalidationDeclared {
                target: target.clone(),
                cause: cause.clone(),
                changed_input: changed_input.clone(),
            }],
            false,
            vec![format!("declared {target:?} invalid: {cause}")],
        ),
    };

    // Probe-fold onto a clone so acceptance here guarantees the fold
    // accepts the same facts later.
    let mut probe = graph.clone();
    for fact in &facts {
        probe.apply(fact).map_err(map_fold_error)?;
    }
    Ok(Applied {
        facts,
        notes,
        requires_human_decision,
    })
}

/// Map fold failures onto proposal rejections.
fn map_fold_error(error: FactError) -> Rejected {
    match error {
        FactError::DependencyCycle { path, .. } => Rejected::Cycle { would_create: path },
        FactError::Duplicate { detail, .. } => Rejected::Duplicate { detail },
        FactError::UnknownReference { detail, .. } => Rejected::UnknownReference { detail },
        FactError::CapabilityExceeded { detail, .. } => {
            Rejected::CapabilityCeilingExceeded { detail }
        }
        other => Rejected::InvalidState {
            detail: other.to_string(),
        },
    }
}
