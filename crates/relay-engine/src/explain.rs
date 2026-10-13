//! `explain`: why any work item is ready, blocked, invalid,
//! awaiting-human, running, or terminal — in deterministic prose derived
//! from graph structure.

use std::fmt;

use relay_domain::{CompletionRule, ProjectGraph, WorkItemId, WorkNode};

use crate::status::{BlockReason, InvalidationChain, InvalidityStep, WorkStatus};

/// Error raised when there is nothing to explain.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("cannot explain unknown work item {work}")]
pub struct ExplainError {
    /// The work item that was asked about.
    pub work: WorkItemId,
}

/// The explanation for one work item: its derived status plus a
/// deterministic, human-readable detail sentence.
#[derive(Debug, Clone, PartialEq)]
pub struct Explanation {
    /// The work item being explained.
    pub work: WorkItemId,
    /// Its derived status.
    pub status: WorkStatus,
    /// The rendered detail (why the status holds).
    pub detail: String,
}

impl fmt::Display for Explanation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} is {}: {}",
            self.work,
            self.status.name(),
            self.detail
        )
    }
}

/// Explain one work item. The prose is a pure function of the graph, so the
/// same facts always yield the same words — this is what the product's
/// "why" surface renders.
pub fn explain(graph: &ProjectGraph, work: &WorkItemId) -> Result<Explanation, ExplainError> {
    let node = graph
        .work(work)
        .ok_or_else(|| ExplainError { work: work.clone() })?;
    let status = crate::status::work_status(graph, node);
    let detail = explain_status(graph, node, &status);
    Ok(Explanation {
        work: work.clone(),
        status,
        detail,
    })
}

/// Render the detail sentence for a known node and status. Public for
/// `evaluate`, which already holds the status it wants explained.
pub fn explain_status(graph: &ProjectGraph, node: &WorkNode, status: &WorkStatus) -> String {
    let id = &node.work.id;
    match status {
        WorkStatus::Ready => {
            let deps = satisfied_dependency_summary(graph, node);
            let inputs = input_summary(node);
            let prior: Vec<String> = graph
                .attempts_of(id)
                .filter_map(|attempt| match attempt.conclusion.as_ref()? {
                    relay_domain::AttemptConclusion::Failed { reason } => {
                        Some(format!("{} failed ({reason})", attempt.id))
                    }
                    relay_domain::AttemptConclusion::Abandoned { reason } => {
                        Some(format!("{} was abandoned ({reason})", attempt.id))
                    }
                    relay_domain::AttemptConclusion::Accepted { receipt } => match &receipt.outcome
                    {
                        relay_domain::AttemptOutcome::Failed { reason } => {
                            Some(format!("{} reported failure ({reason})", attempt.id))
                        }
                        relay_domain::AttemptOutcome::Completed => None,
                    },
                    relay_domain::AttemptConclusion::Canceled { .. } => None,
                })
                .collect();
            let mut detail = format!(
                "all gating dependencies are satisfied ({deps}); inputs resolve ({inputs}); no attempt is active; executor class {}",
                node.work.executor
            );
            if !prior.is_empty() {
                detail.push_str(&format!("; prior attempts: {}", prior.join(", ")));
            }
            if let CompletionRule::CriterionSatisfied { criterion } = &node.work.completion
                && !crate::status::criterion_satisfied(graph, criterion)
            {
                let failing: Vec<String> = graph
                    .evidence_for(&relay_domain::EvidenceTarget::ArtifactVersion(
                        criterion.clone(),
                    ))
                    .iter()
                    .filter(|e| {
                        e.outcome == relay_domain::EvidenceOutcome::Fail
                            && matches!(
                                crate::status::evidence_status(graph, e),
                                crate::status::EvidenceStatus::Valid
                            )
                    })
                    .map(|e| e.id.to_string())
                    .collect();
                if failing.is_empty() {
                    detail.push_str(&format!(
                        "; criterion {criterion} has no passing evidence yet, so the work stays open for another attempt"
                    ));
                } else {
                    detail.push_str(&format!(
                        "; criterion {criterion} is refuted by failing evidence {}, so the work stays open for another attempt",
                        failing.join(", ")
                    ));
                }
            }
            detail
        }
        WorkStatus::AwaitingHuman { decision } => match &node.work.completion {
            CompletionRule::Decision { .. } => match graph.decision(decision) {
                Some(decision_node) => {
                    let options = decision_node
                        .decision
                        .options
                        .iter()
                        .map(|option| option.label.as_str())
                        .collect::<Vec<_>>()
                        .join(", ");
                    let options_text = if options.is_empty() {
                        "a free-form answer".to_string()
                    } else {
                        format!("one of: {options}")
                    };
                    format!(
                        "completion requires resolving decision {} (\"{}\"; answer {options_text})",
                        decision, decision_node.decision.question
                    )
                }
                None => format!(
                    "completion requires decision {decision}, which is not open in this graph"
                ),
            },
            other => format!(
                "work has completion rule {rule:?} but is awaiting decision {decision}",
                rule = rule_name(other)
            ),
        },
        WorkStatus::Running { attempt } => match graph.attempt(attempt) {
            Some(attempt_node) => format!(
                "attempt {} (grant {}) is active, started against revision {}",
                attempt, attempt_node.grant.id, attempt_node.expected_revision
            ),
            None => format!("attempt {attempt} is active"),
        },
        WorkStatus::Blocked { reasons } => {
            let rendered: Vec<String> = reasons.iter().map(render_block_reason).collect();
            format!("blocked by: {}", rendered.join("; "))
        }
        WorkStatus::Completed => match &node.work.completion {
            CompletionRule::Decision { decision } => {
                match graph.decision(decision).and_then(|d| d.resolution.as_ref()) {
                    Some(resolution) => {
                        format!(
                            "decision {decision} was resolved with \"{}\"",
                            resolution.choice
                        )
                    }
                    None => format!(
                        "completion rule satisfied ({})",
                        rule_name(&node.work.completion)
                    ),
                }
            }
            CompletionRule::Receipt => {
                let accepted: Vec<String> = graph
                    .attempts_of(id)
                    .filter_map(|attempt| match &attempt.conclusion {
                        Some(relay_domain::AttemptConclusion::Accepted { receipt })
                            if matches!(
                                receipt.outcome,
                                relay_domain::AttemptOutcome::Completed
                            ) =>
                        {
                            Some(attempt.id.to_string())
                        }
                        _ => None,
                    })
                    .collect();
                format!("accepted completed receipt: {}", accepted.join(", "))
            }
            CompletionRule::CriterionSatisfied { criterion } => {
                let evidence = graph.evidence_for(&relay_domain::EvidenceTarget::ArtifactVersion(
                    criterion.clone(),
                ));
                let passing: Vec<String> = evidence
                    .iter()
                    .filter(|e| e.outcome == relay_domain::EvidenceOutcome::Pass)
                    .map(|e| e.id.to_string())
                    .collect();
                format!(
                    "criterion {criterion} has passing evidence: {}",
                    passing.join(", ")
                )
            }
        },
        WorkStatus::Canceled => match &node.canceled {
            Some(reason) => format!("canceled: {reason}"),
            None => "canceled".to_string(),
        },
        WorkStatus::Superseded { by } => format!("superseded by {by}"),
        WorkStatus::Invalid { chains } => {
            let rendered: Vec<String> = chains.iter().map(render_chain).collect();
            format!("invalid because {}", rendered.join("; and "))
        }
    }
}

/// Render one block reason as a clause.
fn render_block_reason(reason: &BlockReason) -> String {
    match reason {
        BlockReason::DependencyOutstanding {
            dependency,
            kind,
            status,
        } => format!(
            "{kind} {dependency}, which is {} ({})",
            status.name(),
            short_status_detail(status)
        ),
        BlockReason::InputNotPublished { input } => {
            format!("input {input} has no published version yet")
        }
        BlockReason::CompletionDecisionSuperseded {
            decision,
            superseded_by,
        } => format!(
            "its completion decision {decision} was superseded by {superseded_by}; the work needs a new completion rule or decision"
        ),
    }
}

/// A compact status detail for nested dependency mentions.
fn short_status_detail(status: &WorkStatus) -> String {
    match status {
        WorkStatus::Blocked { reasons } => {
            let first = reasons
                .first()
                .map(render_block_reason)
                .unwrap_or_else(|| "unresolved".to_string());
            let suffix = if reasons.len() > 1 {
                format!(" (+{} more)", reasons.len() - 1)
            } else {
                String::new()
            };
            format!("{first}{suffix}")
        }
        WorkStatus::AwaitingHuman { decision } => format!("decision {decision} is open"),
        WorkStatus::Running { attempt } => format!("attempt {attempt} is active"),
        WorkStatus::Invalid { chains } => render_chain(chains.first().unwrap_or(&Vec::new())),
        _ => status.name().to_string(),
    }
}

/// Render an invalidity chain: steps ordered from the explained node to the
/// root cause, joined as "X because Y because Z".
pub fn render_chain(chain: &InvalidationChain) -> String {
    let parts: Vec<String> = chain.iter().map(render_step).collect();
    if parts.is_empty() {
        return "declared invalid".to_string();
    }
    parts.join(" because ")
}

fn render_step(step: &InvalidityStep) -> String {
    match step {
        InvalidityStep::Declared { cause } => cause.to_string(),
        InvalidityStep::SupersededInput { pinned, current } => {
            let replacing =
                relay_domain::ArtifactVersionRef::new(pinned.artifact.clone(), *current);
            format!("pinned input {pinned} was superseded by {replacing}")
        }
        InvalidityStep::InvalidVersion { version, chain } => {
            format!("input {version} is invalid ({})", render_chain(chain))
        }
        InvalidityStep::InvalidDependency { dependency, chain } => {
            format!(
                "dependency {dependency} is invalid ({})",
                render_chain(chain)
            )
        }
    }
}

fn satisfied_dependency_summary(graph: &ProjectGraph, node: &WorkNode) -> String {
    let gating: Vec<&relay_domain::DependencyEdge> = graph
        .dependencies_of(&node.work.id)
        .filter(|edge| edge.kind.gates_readiness())
        .collect();
    if gating.is_empty() {
        return "none declared".to_string();
    }
    let summary: Vec<String> = gating
        .iter()
        .map(|edge| {
            let state = match crate::status::work_status_by_id(graph, &edge.to) {
                Some(WorkStatus::Completed) => "completed".to_string(),
                Some(other) => other.name().to_string(),
                None => "unknown".to_string(),
            };
            format!("{} {state}", edge.to)
        })
        .collect();
    format!("{}/{}: {}", gating.len(), gating.len(), summary.join(", "))
}

fn input_summary(node: &WorkNode) -> String {
    if node.work.inputs.is_empty() {
        return "none".to_string();
    }
    let parts: Vec<String> = node
        .work
        .inputs
        .iter()
        .map(|input| match input {
            relay_domain::InputRef::Exact(reference) => format!("{reference} (pinned)"),
            relay_domain::InputRef::LatestPublished(artifact) => {
                format!("{artifact} (latest)", artifact = artifact)
            }
        })
        .collect();
    parts.join(", ")
}

fn rule_name(rule: &CompletionRule) -> &'static str {
    match rule {
        CompletionRule::Decision { .. } => "decision",
        CompletionRule::Receipt => "receipt",
        CompletionRule::CriterionSatisfied { .. } => "criterion-satisfied",
    }
}
