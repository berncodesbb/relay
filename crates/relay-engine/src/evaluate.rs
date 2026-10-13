//! `evaluate`: the deterministic projection of everything the product UI
//! needs to answer "what is going on?".

use relay_domain::ExecutorClass;
use relay_domain::{AttemptId, DecisionId, GrantId, ProjectGraph, ProjectRevision, WorkItemId};

use crate::explain::explain_status;
use crate::receipt::material_changes_since;
use crate::status::{InvalidationChain, WorkStatus, work_status};

/// One piece of ready work, with its deterministic explanation.
#[derive(Debug, Clone, PartialEq)]
pub struct ReadyEntry {
    /// The ready work item.
    pub work: WorkItemId,
    /// Its executor class.
    pub executor: ExecutorClass,
    /// Why it is ready (deterministic, derived from graph structure).
    pub explanation: String,
}

/// One blocked work item with per-blocker reasons.
#[derive(Debug, Clone, PartialEq)]
pub struct BlockedEntry {
    /// The blocked work item.
    pub work: WorkItemId,
    /// Why, per blocker.
    pub reasons: Vec<crate::status::BlockReason>,
    /// The rendered explanation.
    pub explanation: String,
}

/// One human decision awaiting an answer, attached to ready human work.
#[derive(Debug, Clone, PartialEq)]
pub struct AwaitingHumanEntry {
    /// The work item waiting on a person.
    pub work: WorkItemId,
    /// The open decision.
    pub decision: DecisionId,
    /// The question to answer.
    pub question: String,
    /// The selectable option labels (empty for free-form clarifications).
    pub options: Vec<String>,
    /// The recommended option label, when present.
    pub recommendation: Option<String>,
}

/// One active (started, unconcluded) attempt.
#[derive(Debug, Clone, PartialEq)]
pub struct ActiveAttempt {
    /// The attempt identity.
    pub attempt: AttemptId,
    /// The work item it runs.
    pub work: WorkItemId,
    /// The grant it runs under.
    pub grant: GrantId,
    /// Wall-clock milliseconds when it started.
    pub started_at_ms: i64,
    /// The revision the attempt was started against.
    pub expected_revision: ProjectRevision,
    /// Whether material changes happened since the attempt started (the
    /// runtime should expect a stale-revision rejection or reconcile).
    pub inputs_stale: bool,
}

/// One invalidated work item with its cause chains.
#[derive(Debug, Clone, PartialEq)]
pub struct InvalidatedEntry {
    /// The invalidated work item.
    pub work: WorkItemId,
    /// One chain per root cause, each ordered from this node to the root.
    pub chains: Vec<InvalidationChain>,
    /// The rendered explanation.
    pub explanation: String,
}

/// Project completion, derived from required work and open decisions.
#[derive(Debug, Clone, PartialEq)]
pub enum ProjectCompletion {
    /// Every required work item is completed and valid.
    Complete,
    /// Not complete: what remains.
    InProgress {
        /// Required work not yet completed (including invalid or re-running
        /// required work).
        open_required: Vec<WorkItemId>,
        /// Decisions still open (awaiting a human or policy resolution).
        open_decisions: Vec<DecisionId>,
        /// Work currently blocked (a subset of `open_required`).
        blocked: Vec<WorkItemId>,
    },
}

/// The full derived state of a project at one revision: ready work,
/// blockers, awaiting-human decisions, active attempts, invalidations,
/// terminals, and completion. No modes, no pause flags — everything here is
/// a function of the graph alone.
#[derive(Debug, Clone, PartialEq)]
pub struct Evaluation {
    /// The revision this evaluation was computed at.
    pub revision: ProjectRevision,
    /// Work that can be attempted now.
    pub ready: Vec<ReadyEntry>,
    /// Work that cannot start yet, with reasons.
    pub blocked: Vec<BlockedEntry>,
    /// Human decisions waiting for answers.
    pub awaiting_human: Vec<AwaitingHumanEntry>,
    /// Started, unconcluded attempts.
    pub active_attempts: Vec<ActiveAttempt>,
    /// Invalid work, with cause chains.
    pub invalidated: Vec<InvalidatedEntry>,
    /// Completed work (valid at this revision).
    pub completed: Vec<WorkItemId>,
    /// Canceled work.
    pub canceled: Vec<WorkItemId>,
    /// Superseded work.
    pub superseded: Vec<WorkItemId>,
    /// Derived project completion.
    pub completion: ProjectCompletion,
}

/// Evaluate a project graph. Deterministic: identical graphs produce
/// identical evaluations, and work is iterated in id order.
pub fn evaluate(graph: &ProjectGraph) -> Evaluation {
    let mut ready = Vec::new();
    let mut blocked = Vec::new();
    let mut awaiting_human = Vec::new();
    let mut invalidated = Vec::new();
    let mut completed = Vec::new();
    let mut canceled = Vec::new();
    let mut superseded = Vec::new();
    let mut open_required = Vec::new();

    for (id, node) in graph.works() {
        let status = work_status(graph, node);
        // Required work stays open until it completes; invalid or re-running
        // required work counts as open too.
        let required_open = node.work.required
            && !matches!(
                status,
                WorkStatus::Completed | WorkStatus::Canceled | WorkStatus::Superseded { .. }
            );
        if required_open {
            open_required.push(id.clone());
        }
        match &status {
            WorkStatus::Ready => ready.push(ReadyEntry {
                work: id.clone(),
                executor: node.work.executor,
                explanation: explain_status(graph, node, &status),
            }),
            WorkStatus::Blocked { reasons } => blocked.push(BlockedEntry {
                work: id.clone(),
                reasons: reasons.clone(),
                explanation: explain_status(graph, node, &status),
            }),
            WorkStatus::AwaitingHuman { decision } => {
                if let Some(decision_node) = graph.decision(decision) {
                    awaiting_human.push(AwaitingHumanEntry {
                        work: id.clone(),
                        decision: decision.clone(),
                        question: decision_node.decision.question.clone(),
                        options: decision_node
                            .decision
                            .options
                            .iter()
                            .map(|option| option.label.clone())
                            .collect(),
                        recommendation: decision_node.decision.recommendation.clone(),
                    });
                }
            }
            WorkStatus::Invalid { chains } => {
                invalidated.push(InvalidatedEntry {
                    work: id.clone(),
                    chains: chains.clone(),
                    explanation: explain_status(graph, node, &status),
                });
            }
            WorkStatus::Completed => completed.push(id.clone()),
            WorkStatus::Canceled => canceled.push(id.clone()),
            WorkStatus::Superseded { .. } => superseded.push(id.clone()),
            WorkStatus::Running { .. } => {}
        }
    }

    let open_decisions: Vec<DecisionId> = graph
        .decisions()
        .filter(|(_, node)| node.is_open())
        .map(|(id, _)| id.clone())
        .collect();

    let active_attempts = graph
        .attempts()
        .filter(|(_, node)| node.is_active())
        .map(|(id, node)| ActiveAttempt {
            attempt: id.clone(),
            work: node.work.clone(),
            grant: node.grant.id.clone(),
            started_at_ms: node.started_at_ms,
            expected_revision: node.expected_revision,
            inputs_stale: !material_changes_since(graph, &node.work, node.expected_revision)
                .is_empty(),
        })
        .collect();

    let completion = if graph.works().any(|(_, node)| node.work.required) {
        if open_required.is_empty() {
            ProjectCompletion::Complete
        } else {
            ProjectCompletion::InProgress {
                open_required,
                open_decisions,
                blocked: blocked.iter().map(|entry| entry.work.clone()).collect(),
            }
        }
    } else {
        // No required work declared: completion is not derivable yet.
        ProjectCompletion::InProgress {
            open_required,
            open_decisions,
            blocked: blocked.iter().map(|entry| entry.work.clone()).collect(),
        }
    };

    Evaluation {
        revision: graph.revision(),
        ready,
        blocked,
        awaiting_human,
        active_attempts,
        invalidated,
        completed,
        canceled,
        superseded,
        completion,
    }
}
