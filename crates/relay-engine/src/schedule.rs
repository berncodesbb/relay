//! `recommend`: deterministic scheduling advice over the evaluated graph.
//! Autonomy lives here as a policy parameter — never as a graph node or a
//! global mode.

use relay_domain::{ExecutorClass, ProjectGraph, ScopePath, WorkItemId, WorkNode};

use crate::evaluate::evaluate;
use crate::status::{WorkStatus, is_workspace_writer, work_status};

/// How far the runtime may proceed without a human. A scheduling hint the
/// runtime may further constrain; it never changes graph semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Autonomy {
    /// Never schedule executor work automatically; surface it for choice.
    Manual,
    /// Schedule ready work — read-only/model work in parallel, writers
    /// serialized — without holding back for open decisions.
    Safe,
    /// Like `Safe`, but any open awaiting-human decision freezes new
    /// executor scheduling.
    UntilDecision,
    /// Like `Safe`, capped at `n` recommended attempts in total.
    Bounded(u32),
}

/// Scheduling policy inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SchedulingPolicy {
    /// The autonomy bound.
    pub autonomy: Autonomy,
    /// How many workspace-writing attempts may run concurrently. The first
    /// release serializes writes: 1.
    pub max_concurrent_writers: u32,
}

impl Default for SchedulingPolicy {
    fn default() -> Self {
        SchedulingPolicy {
            autonomy: Autonomy::Safe,
            max_concurrent_writers: 1,
        }
    }
}

/// One recommended attempt.
#[derive(Debug, Clone, PartialEq)]
pub struct Recommended {
    /// The work item to attempt.
    pub work: WorkItemId,
    /// Its executor class.
    pub executor: ExecutorClass,
    /// Effective read scopes.
    pub read_scopes: Vec<ScopePath>,
    /// Effective write scopes (always empty in the parallel batch).
    pub write_scopes: Vec<ScopePath>,
    /// Why this work is recommended now.
    pub explanation: String,
}

/// Ready work held back by policy, with the reason.
#[derive(Debug, Clone, PartialEq)]
pub struct Deferred {
    /// The held-back work item.
    pub work: WorkItemId,
    /// Why it was not recommended.
    pub reason: String,
}

/// The scheduling recommendation at one revision.
#[derive(Debug, Clone, PartialEq)]
pub struct Recommendation {
    /// Read-only/model-only work that may run concurrently. Write scopes
    /// are guaranteed empty for every entry.
    pub parallel: Vec<Recommended>,
    /// Workspace-writing work, in deterministic priority order, to run one
    /// at a time. Scope-disjoint up to the writer cap; overlapping extras
    /// are deferred instead.
    pub serialized: Vec<Recommended>,
    /// Ready work held back by autonomy or writer serialization.
    pub deferred: Vec<Deferred>,
    /// The overall deterministic rationale, suitable for display.
    pub rationale: String,
}

/// Compute a scheduling recommendation. Deterministic: work is considered
/// in id order, writers are admitted in id order, and overlapping write
/// scopes never share a batch — regardless of `max_concurrent_writers`.
pub fn recommend(graph: &ProjectGraph, policy: &SchedulingPolicy) -> Recommendation {
    let evaluation = evaluate(graph);
    let mut deferred: Vec<Deferred> = Vec::new();
    let mut candidates: Vec<&WorkNode> = Vec::new();

    let first_open_decision = evaluation
        .awaiting_human
        .first()
        .map(|entry| entry.decision.clone());
    for entry in &evaluation.ready {
        let node = graph.work(&entry.work);
        let Some(node) = node else {
            continue;
        };
        match policy.autonomy {
            Autonomy::Manual => deferred.push(Deferred {
                work: entry.work.clone(),
                reason: "autonomy is manual: choose work explicitly".to_string(),
            }),
            Autonomy::UntilDecision => {
                if let Some(decision) = &first_open_decision {
                    deferred.push(Deferred {
                        work: entry.work.clone(),
                        reason: format!(
                            "autonomy is until-decision and decision {decision} is open"
                        ),
                    });
                } else {
                    candidates.push(node);
                }
            }
            Autonomy::Safe | Autonomy::Bounded(_) => candidates.push(node),
        }
    }

    let cap = match policy.autonomy {
        Autonomy::Bounded(limit) => limit as usize,
        _ => usize::MAX,
    };

    let mut parallel = Vec::new();
    let mut serialized = Vec::new();
    for node in candidates {
        if parallel.len() + serialized.len() >= cap {
            deferred.push(Deferred {
                work: node.work.id.clone(),
                reason: format!(
                    "autonomy bound reached ({} recommended attempt(s))",
                    parallel.len() + serialized.len()
                ),
            });
            continue;
        }
        let entry = Recommended {
            work: node.work.id.clone(),
            executor: node.work.executor,
            read_scopes: node.work.read_scopes.clone(),
            write_scopes: node.effective_write_scopes(),
            explanation: explain_recommendation(graph, node),
        };
        if is_workspace_writer(node) {
            serialized.push(entry);
        } else {
            parallel.push(entry);
        }
    }

    // Enforce writer serialization: admit only up to the cap and never two
    // attempts with overlapping write scopes.
    let mut admitted: Vec<Recommended> = Vec::new();
    let mut overflow: Vec<Recommended> = Vec::new();
    for candidate in serialized {
        let scopes_overlap = admitted.iter().any(|other| {
            candidate.write_scopes.iter().any(|scope| {
                other
                    .write_scopes
                    .iter()
                    .any(|other_scope| scope.overlaps(other_scope))
            })
        });
        if admitted.len() < policy.max_concurrent_writers as usize && !scopes_overlap {
            admitted.push(candidate);
        } else {
            overflow.push(candidate);
        }
    }
    for candidate in overflow {
        deferred.push(Deferred {
            work: candidate.work.clone(),
            reason: format!(
                "workspace writes serialize (cap {}); {} waits",
                policy.max_concurrent_writers, candidate.work
            ),
        });
    }

    let rationale = render_rationale(
        policy,
        &parallel,
        &admitted,
        &deferred,
        &evaluation.awaiting_human.len(),
    );
    Recommendation {
        parallel,
        serialized: admitted,
        deferred,
        rationale,
    }
}

/// Why one specific work item is recommended (reuses the status
/// explanation, prefixed with the recommendation context).
fn explain_recommendation(graph: &ProjectGraph, node: &WorkNode) -> String {
    let status = work_status(graph, node);
    let kind_word = if is_workspace_writer(node) {
        "serialized writer"
    } else {
        "parallelizable"
    };
    let executor = node.work.executor;
    format!(
        "{kind_word} ({executor} executor); {}",
        crate::explain::explain_status(graph, node, &status)
    )
}

fn render_rationale(
    policy: &SchedulingPolicy,
    parallel: &[Recommended],
    serialized: &[Recommended],
    deferred: &[Deferred],
    awaiting_human: &usize,
) -> String {
    let mut parts = Vec::new();
    parts.push(format!(
        "{} parallelizable attempt(s) and {} serialized writer attempt(s) recommended at autonomy {:?} with max {} concurrent writer(s)",
        parallel.len(),
        serialized.len(),
        policy.autonomy,
        policy.max_concurrent_writers
    ));
    if *awaiting_human > 0 {
        parts.push(format!("{awaiting_human} decision(s) awaiting a human"));
    }
    if !deferred.is_empty() {
        parts.push(format!("{} ready item(s) deferred", deferred.len()));
    }
    parts.join("; ")
}

/// A helper for the runtime: the status of a recommended item re-checked at
/// dispatch time (guards against acting on a stale recommendation).
pub fn status_at_dispatch(graph: &ProjectGraph, work: &WorkItemId) -> Option<WorkStatus> {
    graph.work(work).map(|node| work_status(graph, node))
}
