//! `accept_receipt`: deterministic acceptance of executor receipts, and
//! the material-change (staleness) rule attempts are pinned against.
//!
//! # The revision-matching rule (frozen ruling)
//!
//! An attempt records the `expected_revision` it was computed against.
//! A receipt for that attempt is **stale** — rejected with
//! [`Rejection::StaleRevision`] — if and only if a *material* change landed
//! since `expected_revision`. Material means exactly:
//!
//! 1. the work item's own node changed after `expected_revision`
//!    (scope growth, supersession, cancellation, dependency edits);
//! 2. a direct gating dependency's node changed after `expected_revision`;
//! 3. an exactly pinned input artifact published a newer version after
//!    `expected_revision` (the pin no longer names the current version).
//!
//! Anything else — unrelated work declared, sibling attempts started or
//! concluded, evidence on other criteria — is immaterial and must not
//! reject the receipt: read-only and model-only attempts complete
//! concurrently by design. Strict `expected == current` equality was
//! rejected because durable `AttemptStarted` facts make every concurrent
//! sibling bump the revision, which would make concurrent completion
//! impossible. `material_changes_since` is the single implementation of
//! this rule; evaluation's `inputs_stale` hint reuses it.

use relay_domain::{
    ArtifactVersionRef, AttemptConclusion, AttemptId, AttemptOutcome, AttemptReceipt, DomainFact,
    EvidenceTarget, GraphChangeProposal, InputRef, ProjectGraph, ProjectRevision, WorkItemId,
};

use crate::status::work_invalidity;

/// A validated receipt, expanded into the exact facts storage must append
/// (in order) plus the proposals the caller may separately submit to
/// `validate_graph_change`.
#[derive(Debug, Clone, PartialEq)]
pub struct Acceptance {
    /// Facts to append, in order: artifact publications (with
    /// engine-assigned versions and provenance), evidence records, then the
    /// attempt conclusion wrapping the receipt.
    pub facts: Vec<DomainFact>,
    /// The receipt's graph-change proposals, passed through for separate
    /// validation. Acceptance never installs proposals.
    pub proposals: Vec<GraphChangeProposal>,
}

/// Why a receipt was rejected.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum Rejection {
    /// The attempt does not exist in the graph.
    #[error("attempt {attempt} does not exist")]
    UnknownAttempt {
        /// The unknown attempt.
        attempt: AttemptId,
    },
    /// The attempt already has a terminal account.
    #[error("attempt {attempt} is already concluded")]
    AlreadyConcluded {
        /// The concluded attempt.
        attempt: AttemptId,
    },
    /// The receipt names a different work item than the attempt runs.
    #[error("receipt names work {receipt_work} but attempt {attempt} runs {attempt_work}")]
    WorkMismatch {
        /// The attempt.
        attempt: AttemptId,
        /// The work the attempt runs.
        attempt_work: WorkItemId,
        /// The work the receipt claims.
        receipt_work: WorkItemId,
    },
    /// Material changes landed since the attempt started.
    #[error(
        "attempt {attempt} is stale: expected revision {expected}, now at {current}; changed: {changes:?}"
    )]
    StaleRevision {
        /// The attempt.
        attempt: AttemptId,
        /// The revision the attempt started against.
        expected: ProjectRevision,
        /// The current revision.
        current: ProjectRevision,
        /// What materially changed.
        changes: Vec<String>,
    },
    /// The work item is currently invalid; completing it now would be void.
    #[error("work {work} is invalid and cannot accept a completing receipt")]
    WorkInvalid {
        /// The invalid work item.
        work: WorkItemId,
    },
    /// The work item is canceled or superseded.
    #[error("work {work} cannot accept receipts: {summary}")]
    WorkNotRunnable {
        /// The terminal work item.
        work: WorkItemId,
        /// Why.
        summary: String,
    },
    /// The receipt references state that does not exist.
    #[error("receipt references unknown state: {detail}")]
    UnknownReference {
        /// What was missing.
        detail: String,
    },
    /// The receipt reuses an evidence id that already exists.
    #[error("evidence {evidence} is already recorded")]
    DuplicateEvidence {
        /// The duplicated evidence id.
        evidence: relay_domain::EvidenceId,
    },
    /// A failed attempt cannot publish artifact versions.
    #[error("a failed attempt cannot publish outputs (got {count})")]
    OutputsOnFailure {
        /// How many outputs were proposed.
        count: usize,
    },
    /// A completed attempt must account for something.
    #[error("attempt {attempt} reports completion with no outputs, evidence, or proposals")]
    EmptyCompletion {
        /// The attempt.
        attempt: AttemptId,
    },
    /// The usage block is impossible.
    #[error("invalid usage: {detail}")]
    InvalidUsage {
        /// What was impossible.
        detail: String,
    },
    /// The receipt is inconsistent with graph state in some other way.
    #[error("receipt is invalid in the current state: {detail}")]
    InvalidState {
        /// What is inconsistent.
        detail: String,
    },
}

/// Compute the material changes since `expected` that concern `work`: its
/// own node, its direct dependencies, and its exactly pinned inputs. Used
/// both by acceptance (rejections) and evaluation (staleness hints).
pub fn material_changes_since(
    graph: &ProjectGraph,
    work: &WorkItemId,
    expected: ProjectRevision,
) -> Vec<String> {
    let mut changes = Vec::new();
    let Some(node) = graph.work(work) else {
        return changes;
    };
    if node.last_changed > expected {
        changes.push(format!(
            "work {work} changed at revision {revision}",
            revision = node.last_changed
        ));
    }
    for edge in graph
        .dependencies_of(work)
        .filter(|edge| edge.kind.gates_readiness())
    {
        if let Some(dep) = graph.work(&edge.to)
            && dep.last_changed > expected
        {
            changes.push(format!(
                "dependency {dependency} changed at revision {revision}",
                dependency = edge.to,
                revision = dep.last_changed
            ));
        }
    }
    for input in &node.work.inputs {
        if let InputRef::Exact(pinned) = input
            && let Some(artifact) = graph.artifact(&pinned.artifact)
            && artifact.last_changed > expected
            && artifact.current_version() != pinned.version
        {
            changes.push(format!(
                "pinned input {pinned} was superseded by {artifact_id}@{current}",
                artifact_id = pinned.artifact,
                current = artifact.current_version()
            ));
        }
    }
    changes
}

/// Validate a receipt against the graph and produce the facts that would
/// record its acceptance.
///
/// Checks: the attempt exists, is unconcluded, and matches the receipt's
/// work; nothing material to the work changed since the expected revision;
/// the work is currently valid and not terminal; evidence ids are fresh and
/// their targets exist; outputs are only proposed by completed attempts;
/// versions are assigned here, never proposed. The returned facts are not
/// applied by this function — the caller persists them.
pub fn accept_receipt(
    graph: &ProjectGraph,
    receipt: &AttemptReceipt,
) -> Result<Acceptance, Rejection> {
    let attempt = graph
        .attempt(&receipt.attempt)
        .ok_or(Rejection::UnknownAttempt {
            attempt: receipt.attempt.clone(),
        })?;
    if attempt.conclusion.is_some() {
        return Err(Rejection::AlreadyConcluded {
            attempt: receipt.attempt.clone(),
        });
    }
    if receipt.work != attempt.work {
        return Err(Rejection::WorkMismatch {
            attempt: receipt.attempt.clone(),
            attempt_work: attempt.work.clone(),
            receipt_work: receipt.work.clone(),
        });
    }
    let changes = material_changes_since(graph, &attempt.work, attempt.expected_revision);
    if !changes.is_empty() {
        return Err(Rejection::StaleRevision {
            attempt: receipt.attempt.clone(),
            expected: attempt.expected_revision,
            current: graph.revision(),
            changes,
        });
    }
    if receipt.completed_at_ms < attempt.started_at_ms {
        return Err(Rejection::InvalidState {
            detail: format!(
                "receipt concludes attempt {} at {} before it started at {}",
                receipt.attempt, receipt.completed_at_ms, attempt.started_at_ms
            ),
        });
    }
    if receipt.usage.duration_ms < 0 {
        return Err(Rejection::InvalidUsage {
            detail: format!("negative duration {}ms", receipt.usage.duration_ms),
        });
    }
    let node = graph
        .work(&attempt.work)
        .ok_or(Rejection::UnknownReference {
            detail: format!("work {} does not exist", attempt.work),
        })?;
    if let Some(reason) = &node.canceled {
        return Err(Rejection::WorkNotRunnable {
            work: attempt.work.clone(),
            summary: format!("the work was canceled: {reason}"),
        });
    }
    if let Some(by) = &node.superseded_by {
        return Err(Rejection::WorkNotRunnable {
            work: attempt.work.clone(),
            summary: format!("the work was superseded by {by}"),
        });
    }
    if !work_invalidity(graph, node).is_empty() {
        return Err(Rejection::WorkInvalid {
            work: attempt.work.clone(),
        });
    }
    match &receipt.outcome {
        AttemptOutcome::Completed => {
            if receipt.outputs.is_empty()
                && receipt.evidence.is_empty()
                && receipt.graph_changes.is_empty()
            {
                return Err(Rejection::EmptyCompletion {
                    attempt: receipt.attempt.clone(),
                });
            }
        }
        AttemptOutcome::Failed { .. } => {
            if !receipt.outputs.is_empty() {
                return Err(Rejection::OutputsOnFailure {
                    count: receipt.outputs.len(),
                });
            }
        }
    }
    // Evidence: fresh ids, resolvable targets.
    let mut seen_evidence = std::collections::BTreeSet::new();
    for evidence in &receipt.evidence {
        if graph.evidence_by_id(&evidence.id).is_some()
            || !seen_evidence.insert(evidence.id.clone())
        {
            return Err(Rejection::DuplicateEvidence {
                evidence: evidence.id.clone(),
            });
        }
        match &evidence.target {
            EvidenceTarget::WorkItem(work) => {
                if graph.work(work).is_none() {
                    return Err(Rejection::UnknownReference {
                        detail: format!("evidence {} targets unknown work {work}", evidence.id),
                    });
                }
            }
            EvidenceTarget::ArtifactVersion(reference) => {
                if graph.version_record(reference).is_none() {
                    return Err(Rejection::UnknownReference {
                        detail: format!(
                            "evidence {} targets unknown version {reference}",
                            evidence.id
                        ),
                    });
                }
            }
            EvidenceTarget::Decision(decision) => {
                if graph.decision(decision).is_none() {
                    return Err(Rejection::UnknownReference {
                        detail: format!(
                            "evidence {} targets unknown decision {decision}",
                            evidence.id
                        ),
                    });
                }
            }
        }
    }
    // Outputs: assign versions deterministically from graph state.
    let mut assigned: std::collections::BTreeMap<
        relay_domain::ArtifactId,
        relay_domain::ArtifactVersion,
    > = std::collections::BTreeMap::new();
    let mut facts: Vec<DomainFact> = Vec::new();
    for output in &receipt.outputs {
        let next = match assigned
            .get(&output.artifact)
            .copied()
            .or_else(|| graph.current_version(&output.artifact))
        {
            // New artifacts start at version 1; existing ones increment.
            None => relay_domain::ArtifactVersion::FIRST,
            Some(base) => relay_domain::ArtifactVersion::new(base.get() + 1).map_err(|error| {
                Rejection::InvalidState {
                    detail: format!(
                        "cannot assign next version for {}: {error}",
                        output.artifact
                    ),
                }
            })?,
        };
        assigned.insert(output.artifact.clone(), next);
        let derived_from = resolved_inputs(graph, node);
        facts.push(DomainFact::ArtifactVersionPublished {
            artifact: output.artifact.clone(),
            version: next,
            title: output.title.clone(),
            content: output.content.clone(),
            digest: output.digest.clone(),
            derived_from,
            published_at_ms: receipt.completed_at_ms,
        });
    }
    for evidence in &receipt.evidence {
        facts.push(DomainFact::EvidenceRecorded {
            evidence: evidence.clone(),
        });
    }
    facts.push(DomainFact::AttemptConcluded {
        attempt: receipt.attempt.clone(),
        conclusion: AttemptConclusion::Accepted {
            receipt: receipt.clone(),
        },
    });
    Ok(Acceptance {
        facts,
        proposals: receipt.graph_changes.clone(),
    })
}

/// Resolve the work's inputs to exact version references at acceptance
/// time: exact pins pass through; floating inputs bind to the current
/// version, which must exist by now (the work was runnable).
fn resolved_inputs(graph: &ProjectGraph, node: &relay_domain::WorkNode) -> Vec<ArtifactVersionRef> {
    let mut references = Vec::new();
    for input in &node.work.inputs {
        match input {
            InputRef::Exact(reference) => references.push(reference.clone()),
            InputRef::LatestPublished(artifact) => {
                if let Some(version) = graph.current_version(artifact) {
                    references.push(ArtifactVersionRef::new(artifact.clone(), version));
                }
            }
        }
    }
    references
}
