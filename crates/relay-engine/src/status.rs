//! Derived work status: the single source of readiness, invalidity, and
//! completion truth shared by every engine entry point.

use relay_domain::{
    ArtifactVersion, ArtifactVersionRef, AttemptConclusion, AttemptId, AttemptOutcome,
    CompletionRule, DecisionId, DependencyKind, Evidence, EvidenceTarget, InvalidationCause,
    InvalidationTarget, ProjectGraph, WorkItemId, WorkNode,
};

/// Why a work item cannot start (or re-start) yet.
#[derive(Debug, Clone, PartialEq)]
pub enum BlockReason {
    /// A gating dependency has not completed.
    DependencyOutstanding {
        /// The prerequisite.
        dependency: WorkItemId,
        /// The edge kind.
        kind: DependencyKind,
        /// The prerequisite's current derived status.
        status: Box<WorkStatus>,
    },
    /// A declared input artifact has no published version yet.
    InputNotPublished {
        /// The input that cannot resolve.
        input: relay_domain::InputRef,
    },
    /// The completion decision was superseded and nothing resolves the work
    /// anymore.
    CompletionDecisionSuperseded {
        /// The superseded decision.
        decision: DecisionId,
        /// Its replacement.
        superseded_by: DecisionId,
    },
}

/// One root-cause chain of invalidity, ordered from the node in question
/// (first step) to the root cause (last step).
pub type InvalidationChain = Vec<InvalidityStep>;

/// One link in an invalidity chain.
#[derive(Debug, Clone, PartialEq)]
pub enum InvalidityStep {
    /// An invalidation was declared against this node directly.
    Declared {
        /// The declared cause.
        cause: InvalidationCause,
    },
    /// An exactly pinned input version is no longer the artifact's current
    /// version.
    SupersededInput {
        /// The pinned version.
        pinned: ArtifactVersionRef,
        /// What replaced it.
        current: ArtifactVersion,
    },
    /// A referenced artifact version is itself invalid.
    InvalidVersion {
        /// The invalid version.
        version: ArtifactVersionRef,
        /// Why it is invalid.
        chain: Box<InvalidationChain>,
    },
    /// A dependency (or, for evidence, the targeted work item) is invalid.
    InvalidDependency {
        /// The invalid work item.
        dependency: WorkItemId,
        /// Why it is invalid.
        chain: Box<InvalidationChain>,
    },
}

/// The derived status of one work item. Never stored; always computed from
/// the graph.
#[derive(Debug, Clone, PartialEq)]
pub enum WorkStatus {
    /// Dependencies satisfied, inputs resolved, no active attempt, nothing
    /// invalid, and completion not yet achieved: the work can be attempted.
    Ready,
    /// The work waits on a human resolving its completion decision.
    AwaitingHuman {
        /// The open decision.
        decision: DecisionId,
    },
    /// An attempt is running.
    Running {
        /// The active attempt.
        attempt: AttemptId,
    },
    /// Not startable yet, for the listed reasons.
    Blocked {
        /// Why, per blocker.
        reasons: Vec<BlockReason>,
    },
    /// The completion rule is satisfied.
    Completed,
    /// Canceled by declaration.
    Canceled,
    /// Superseded by a replacement work item.
    Superseded {
        /// The replacement.
        by: WorkItemId,
    },
    /// Invalid, with cause chains. Invalidity overrides every other state
    /// (a completed item can become invalid when its inputs are
    /// superseded; a running item stays listed among active attempts until
    /// the runtime reconciles it).
    Invalid {
        /// One chain per root cause.
        chains: Vec<InvalidationChain>,
    },
}

impl WorkStatus {
    /// A short lowercase name for the status, for explanations and views.
    pub fn name(&self) -> &'static str {
        match self {
            WorkStatus::Ready => "ready",
            WorkStatus::AwaitingHuman { .. } => "awaiting-human",
            WorkStatus::Running { .. } => "running",
            WorkStatus::Blocked { .. } => "blocked",
            WorkStatus::Completed => "completed",
            WorkStatus::Canceled => "canceled",
            WorkStatus::Superseded { .. } => "superseded",
            WorkStatus::Invalid { .. } => "invalid",
        }
    }
}

/// The derived status of one published artifact version.
#[derive(Debug, Clone, PartialEq)]
pub enum VersionStatus {
    /// The version exists, is current, and has valid provenance.
    Current,
    /// A newer version has replaced it (normal history, not a judgment).
    Superseded {
        /// The replacing version.
        current: ArtifactVersion,
    },
    /// The version's basis is void: declared invalid, or derived from
    /// invalid or superseded inputs.
    Invalid {
        /// One chain per root cause.
        chains: Vec<InvalidationChain>,
    },
    /// The referenced version does not exist.
    Unknown,
}

/// The derived status of one piece of evidence.
#[derive(Debug, Clone, PartialEq)]
pub enum EvidenceStatus {
    /// The evidence stands.
    Valid,
    /// Declared invalid, or attached to an invalid node.
    Invalid {
        /// One chain per root cause.
        chains: Vec<InvalidationChain>,
    },
}

/// Derive the status of one work item from its node. Callers that hold only
/// an id look the node up first; absence of a node is their concern, not a
/// status.
pub fn work_status(graph: &ProjectGraph, node: &WorkNode) -> WorkStatus {
    let id = &node.work.id;
    if node.canceled.is_some() {
        return WorkStatus::Canceled;
    }
    if let Some(by) = &node.superseded_by {
        return WorkStatus::Superseded { by: by.clone() };
    }
    let chains = work_invalidity(graph, node);
    if !chains.is_empty() {
        return WorkStatus::Invalid { chains };
    }
    if work_is_completed(graph, node) {
        return WorkStatus::Completed;
    }
    if let Some(attempt) = graph.attempts_of(id).find(|attempt| attempt.is_active()) {
        return WorkStatus::Running {
            attempt: attempt.id.clone(),
        };
    }
    let reasons = work_blockers(graph, node);
    if !reasons.is_empty() {
        return WorkStatus::Blocked { reasons };
    }
    if let CompletionRule::Decision { decision } = &node.work.completion
        && let Some(decision_node) = graph.decision(decision)
        && decision_node.is_open()
    {
        return WorkStatus::AwaitingHuman {
            decision: decision.clone(),
        };
    }
    WorkStatus::Ready
}

/// Derive the status of a work item by id, when it exists.
pub fn work_status_by_id(graph: &ProjectGraph, id: &WorkItemId) -> Option<WorkStatus> {
    graph.work(id).map(|node| work_status(graph, node))
}

/// Compute every invalidity root-cause chain for a work item.
pub fn work_invalidity(graph: &ProjectGraph, node: &WorkNode) -> Vec<InvalidationChain> {
    let id = &node.work.id;
    let mut chains: Vec<InvalidationChain> = Vec::new();
    for invalidation in graph.invalidations_for(&InvalidationTarget::Work(id.clone())) {
        chains.push(vec![InvalidityStep::Declared {
            cause: invalidation.cause.clone(),
        }]);
    }
    for input in &node.work.inputs {
        match input {
            relay_domain::InputRef::Exact(pinned) => {
                if let Some(current) = graph.current_version(&pinned.artifact) {
                    if current != pinned.version {
                        chains.push(vec![InvalidityStep::SupersededInput {
                            pinned: pinned.clone(),
                            current,
                        }]);
                    } else if let VersionStatus::Invalid { chains: sub } =
                        version_status(graph, pinned)
                    {
                        chains.extend(sub.into_iter().map(|chain| {
                            vec![InvalidityStep::InvalidVersion {
                                version: pinned.clone(),
                                chain: Box::new(chain),
                            }]
                        }));
                    }
                }
            }
            relay_domain::InputRef::LatestPublished(artifact) => {
                if let Some(current) = graph.current_version(artifact) {
                    let resolved = ArtifactVersionRef::new(artifact.clone(), current);
                    if let VersionStatus::Invalid { chains: sub } = version_status(graph, &resolved)
                    {
                        chains.extend(sub.into_iter().map(|chain| {
                            vec![InvalidityStep::InvalidVersion {
                                version: resolved.clone(),
                                chain: Box::new(chain),
                            }]
                        }));
                    }
                }
            }
        }
    }
    for edge in graph.dependencies_of(id) {
        if !edge.kind.propagates_invalidation() {
            continue;
        }
        if let Some(dep_node) = graph.work(&edge.to) {
            for chain in work_invalidity(graph, dep_node) {
                chains.push(vec![InvalidityStep::InvalidDependency {
                    dependency: edge.to.clone(),
                    chain: Box::new(chain),
                }]);
            }
        }
    }
    chains
}

/// Derive the status of one published artifact version.
pub fn version_status(graph: &ProjectGraph, reference: &ArtifactVersionRef) -> VersionStatus {
    let Some(node) = graph.artifact(&reference.artifact) else {
        return VersionStatus::Unknown;
    };
    let Some(record) = node.versions.get(&reference.version) else {
        return VersionStatus::Unknown;
    };
    let mut chains: Vec<InvalidationChain> = Vec::new();
    for invalidation in
        graph.invalidations_for(&InvalidationTarget::ArtifactVersion(reference.clone()))
    {
        chains.push(vec![InvalidityStep::Declared {
            cause: invalidation.cause.clone(),
        }]);
    }
    for source in &record.derived_from {
        match version_status(graph, source) {
            VersionStatus::Superseded { current } => {
                chains.push(vec![InvalidityStep::SupersededInput {
                    pinned: source.clone(),
                    current,
                }]);
            }
            VersionStatus::Invalid { chains: sub } => {
                chains.extend(sub.into_iter().map(|chain| {
                    vec![InvalidityStep::InvalidVersion {
                        version: source.clone(),
                        chain: Box::new(chain),
                    }]
                }));
            }
            _ => {}
        }
    }
    if !chains.is_empty() {
        return VersionStatus::Invalid { chains };
    }
    let current = node.current_version();
    if current != reference.version {
        return VersionStatus::Superseded { current };
    }
    VersionStatus::Current
}

/// Derive the status of one piece of evidence.
pub fn evidence_status(graph: &ProjectGraph, evidence: &Evidence) -> EvidenceStatus {
    let mut chains: Vec<InvalidationChain> = Vec::new();
    for invalidation in graph.invalidations_for(&InvalidationTarget::Evidence(evidence.id.clone()))
    {
        chains.push(vec![InvalidityStep::Declared {
            cause: invalidation.cause.clone(),
        }]);
    }
    match &evidence.target {
        EvidenceTarget::ArtifactVersion(reference) => {
            if let VersionStatus::Invalid { chains: sub } = version_status(graph, reference) {
                chains.extend(sub.into_iter().map(|chain| {
                    vec![InvalidityStep::InvalidVersion {
                        version: reference.clone(),
                        chain: Box::new(chain),
                    }]
                }));
            }
        }
        EvidenceTarget::WorkItem(work) => {
            if let Some(node) = graph.work(work) {
                for chain in work_invalidity(graph, node) {
                    chains.push(vec![InvalidityStep::InvalidDependency {
                        dependency: work.clone(),
                        chain: Box::new(chain),
                    }]);
                }
            }
        }
        EvidenceTarget::Decision(_) => {}
    }
    if chains.is_empty() {
        EvidenceStatus::Valid
    } else {
        EvidenceStatus::Invalid { chains }
    }
}

/// Whether a work item's completion rule is satisfied.
pub fn work_is_completed(graph: &ProjectGraph, node: &WorkNode) -> bool {
    match &node.work.completion {
        CompletionRule::Decision { decision } => graph
            .decision(decision)
            .is_some_and(|decision| decision.resolution.is_some() && decision.superseded_by.is_none()),
        CompletionRule::Receipt => graph.attempts_of(&node.work.id).any(|attempt| {
            matches!(
                &attempt.conclusion,
                Some(AttemptConclusion::Accepted { receipt }) if matches!(receipt.outcome, AttemptOutcome::Completed)
            )
        }),
        CompletionRule::CriterionSatisfied { criterion } => criterion_satisfied(graph, criterion),
    }
}

/// Whether a criterion version is satisfied: among the evidence attached
/// to that exact version that still stands (not declared invalid), at least
/// one passes and none fails. Deterministic and order-independent over the
/// valid evidence set; invalidation is the mechanism that retires stale
/// observations (for example a failure recorded against an implementation a
/// corrective work has since replaced).
pub fn criterion_satisfied(graph: &ProjectGraph, criterion: &ArtifactVersionRef) -> bool {
    let stands: Vec<&Evidence> = graph
        .evidence_for(&EvidenceTarget::ArtifactVersion(criterion.clone()))
        .into_iter()
        .filter(|e| matches!(evidence_status(graph, e), EvidenceStatus::Valid))
        .collect();
    !stands.is_empty()
        && stands
            .iter()
            .any(|e| e.outcome == relay_domain::EvidenceOutcome::Pass)
        && stands
            .iter()
            .all(|e| e.outcome != relay_domain::EvidenceOutcome::Fail)
}

/// Compute why a work item cannot start yet. Empty means startable (given
/// the other status gates).
pub fn work_blockers(graph: &ProjectGraph, node: &WorkNode) -> Vec<BlockReason> {
    let id = &node.work.id;
    let mut reasons: Vec<BlockReason> = Vec::new();
    for edge in graph
        .dependencies_of(id)
        .filter(|edge| edge.kind.gates_readiness())
    {
        // The fold guarantees dependency endpoints exist, so a missing node
        // here is unreachable; skip defensively rather than invent a status.
        let Some(status) = work_status_by_id(graph, &edge.to) else {
            continue;
        };
        if !matches!(status, WorkStatus::Completed) {
            reasons.push(BlockReason::DependencyOutstanding {
                dependency: edge.to.clone(),
                kind: edge.kind,
                status: Box::new(status),
            });
        }
    }
    for input in &node.work.inputs {
        let resolved = match input {
            relay_domain::InputRef::Exact(reference) => graph.version_record(reference).is_some(),
            relay_domain::InputRef::LatestPublished(artifact) => {
                graph.current_version(artifact).is_some()
            }
        };
        if !resolved {
            reasons.push(BlockReason::InputNotPublished {
                input: input.clone(),
            });
        }
    }
    if let CompletionRule::Decision { decision } = &node.work.completion
        && let Some(decision_node) = graph.decision(decision)
        && let Some(by) = &decision_node.superseded_by
    {
        reasons.push(BlockReason::CompletionDecisionSuperseded {
            decision: decision.clone(),
            superseded_by: by.clone(),
        });
    }
    reasons
}

/// Whether the work mutates the workspace: `Workspace` executor class or
/// any effective write scope. Such attempts serialize.
pub fn is_workspace_writer(node: &WorkNode) -> bool {
    node.work.executor == relay_domain::ExecutorClass::Workspace
        || !node.effective_write_scopes().is_empty()
}
