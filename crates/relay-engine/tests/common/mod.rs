//! Shared deterministic fact builders for the engine's property and
//! scenario tests. Plain data only — no fakes, no IO, no clocks.
//!
//! Each test binary links this module separately and uses a different
//! subset of the builders, so unused-per-binary helpers are expected.

#![allow(dead_code)] // each test binary uses a different subset of the builders
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::ok_expect,
    clippy::err_expect
)] // test-only conveniences, house style

use std::collections::BTreeMap;

use relay_domain::{
    ArtifactContent, ArtifactDigest, ArtifactVersion, AttemptId, AttemptOutcome, AttemptReceipt,
    CapabilityGrant, CompletionRule, Decision, DecisionOption, DecisionResolution, DecisionSubject,
    DependencyKind, DomainFact, Evidence, EvidenceId, EvidenceKind, EvidenceOutcome,
    EvidenceTarget, ExecutorClass, Goal, GraphChangeProposal, OutputProposal, ProjectGraph,
    Provenance, ProvenanceSource, ScopePath, Usage, WorkItem, WorkItemId, WorkKind,
};
use relay_engine::WorkStatus;

pub fn project(id: &str) -> DomainFact {
    DomainFact::ProjectCreated {
        project: id.parse().ok().expect("valid project id"),
    }
}

pub fn goal(id: &str, outcome: &str) -> DomainFact {
    DomainFact::GoalDeclared {
        goal: Goal::new(
            id.parse().ok().expect("valid goal id"),
            outcome,
            Provenance {
                source: ProvenanceSource::User,
                statement: Some(outcome.to_string()),
                at_ms: 1_000,
            },
            1_000,
        ),
    }
}

/// A model-executor work item with no scopes, inputs, or dependencies.
pub fn model_work(id: &str, kind: &str, objective: &str) -> WorkItem {
    WorkItem::new(
        id.parse().ok().expect("valid work id"),
        WorkKind::parse(kind).ok().expect("valid kind"),
        ExecutorClass::Model,
        objective,
        CompletionRule::Receipt,
    )
}

/// A human work item completed by resolving `decision`.
pub fn human_work(id: &str, kind: &str, objective: &str, decision: &str) -> WorkItem {
    WorkItem::new(
        id.parse().ok().expect("valid work id"),
        WorkKind::parse(kind).ok().expect("valid kind"),
        ExecutorClass::Human,
        objective,
        CompletionRule::Decision {
            decision: decision.parse().ok().expect("valid decision id"),
        },
    )
}

/// A verifier work item that must satisfy the pinned criterion version.
pub fn verifier_work(id: &str, kind: &str, objective: &str, criterion: (&str, u32)) -> WorkItem {
    WorkItem::new(
        id.parse().ok().expect("valid work id"),
        WorkKind::parse(kind).ok().expect("valid kind"),
        ExecutorClass::Verifier,
        objective,
        CompletionRule::CriterionSatisfied {
            criterion: version_ref(criterion.0, criterion.1),
        },
    )
}

/// A workspace-executor work item with write scopes inside `root`.
pub fn workspace_work(id: &str, kind: &str, objective: &str, write_root: &str) -> WorkItem {
    let mut work = WorkItem::new(
        id.parse().ok().expect("valid work id"),
        WorkKind::parse(kind).ok().expect("valid kind"),
        ExecutorClass::Workspace,
        objective,
        CompletionRule::Receipt,
    );
    let scope = ScopePath::parse(write_root).ok().expect("valid scope");
    work.write_scopes = vec![scope.clone()];
    work.capability_ceiling.write_scopes = vec![scope];
    work
}

pub fn work_declared(work: WorkItem) -> DomainFact {
    DomainFact::WorkDeclared { work }
}

pub fn requires(from: &str, to: &str) -> DomainFact {
    dependency(from, to, DependencyKind::Requires)
}

pub fn informs(from: &str, to: &str) -> DomainFact {
    dependency(from, to, DependencyKind::Informs)
}

pub fn validates(from: &str, to: &str) -> DomainFact {
    dependency(from, to, DependencyKind::Validates)
}

fn dependency(from: &str, to: &str, kind: DependencyKind) -> DomainFact {
    DomainFact::DependencyAsserted {
        from: from.parse().ok().expect("valid work id"),
        to: to.parse().ok().expect("valid work id"),
        kind,
    }
}

pub fn decision_opened(
    id: &str,
    subject: DecisionSubject,
    question: &str,
    options: &[(&str, &str)],
    recommendation: Option<&str>,
) -> DomainFact {
    let mut decision = Decision::new(
        id.parse().ok().expect("valid decision id"),
        subject,
        question,
        options
            .iter()
            .map(|(label, description)| DecisionOption {
                label: label.to_string(),
                description: description.to_string(),
            })
            .collect(),
        2_000,
    );
    decision.recommendation = recommendation.map(str::to_string);
    DomainFact::DecisionOpened { decision }
}

pub fn decision_resolved(id: &str, choice: &str, rationale: &str) -> DomainFact {
    DomainFact::DecisionResolved {
        decision: id.parse().ok().expect("valid decision id"),
        resolution: DecisionResolution {
            choice: choice.to_string(),
            rationale: rationale.to_string(),
            resolved_at_ms: 2_500,
        },
    }
}

pub fn version_ref(artifact: &str, version: u32) -> relay_domain::ArtifactVersionRef {
    relay_domain::ArtifactVersionRef::new(
        artifact.parse().ok().expect("valid artifact id"),
        ArtifactVersion::new(version).ok().expect("version >= 1"),
    )
}

pub fn publish(artifact: &str, version: u32, note: u64) -> DomainFact {
    DomainFact::ArtifactVersionPublished {
        artifact: artifact.parse().ok().expect("valid artifact id"),
        version: ArtifactVersion::new(version).ok().expect("version >= 1"),
        title: Some(format!("{artifact} v{version}")),
        content: ArtifactContent::empty(),
        digest: ArtifactDigest::parse(&format!("{note:064x}"))
            .ok()
            .expect("valid digest"),
        derived_from: Vec::new(),
        published_at_ms: 3_000,
    }
}

pub fn evidence(
    id: &str,
    target: EvidenceTarget,
    outcome: EvidenceOutcome,
    summary: &str,
) -> Evidence {
    Evidence::new(
        id.parse().ok().expect("valid evidence id"),
        EvidenceKind::TestRun,
        target,
        outcome,
        summary,
        4_000,
    )
}

pub fn grant_read(id: &str, roots: &[&str]) -> CapabilityGrant {
    CapabilityGrant::read_only(
        id.parse().ok().expect("valid grant id"),
        roots
            .iter()
            .map(|root| ScopePath::parse(root).ok().expect("valid scope"))
            .collect(),
    )
}

pub fn grant_for_scopes(id: &str, reads: &[&str], writes: &[&str]) -> CapabilityGrant {
    CapabilityGrant::for_scopes(
        id.parse().ok().expect("valid grant id"),
        reads
            .iter()
            .map(|root| ScopePath::parse(root).ok().expect("valid scope"))
            .collect(),
        writes
            .iter()
            .map(|root| ScopePath::parse(root).ok().expect("valid scope"))
            .collect(),
    )
}

/// An `AttemptStarted` fact pinned to the graph's current revision.
pub fn started(
    graph: &ProjectGraph,
    attempt: &str,
    work: &str,
    grant: CapabilityGrant,
) -> DomainFact {
    DomainFact::AttemptStarted {
        attempt: attempt.parse().ok().expect("valid attempt id"),
        work: work.parse().ok().expect("valid work id"),
        expected_revision: graph.revision(),
        grant,
        started_at_ms: 5_000,
    }
}

/// Start an attempt in place: build the fact at the current revision and
/// apply it.
pub fn start(graph: &mut ProjectGraph, attempt: &str, work: &str, grant: CapabilityGrant) {
    let fact = started(graph, attempt, work, grant);
    graph.apply(&fact).ok().expect("start applies");
}

pub fn output(artifact: &str, title: &str, note: u64) -> OutputProposal {
    OutputProposal {
        artifact: artifact.parse().ok().expect("valid artifact id"),
        title: Some(title.to_string()),
        content: ArtifactContent::empty(),
        digest: ArtifactDigest::parse(&format!("{note:064x}"))
            .ok()
            .expect("valid digest"),
    }
}

pub fn completed_receipt(
    attempt: &str,
    work: &str,
    outputs: Vec<OutputProposal>,
    evidence: Vec<Evidence>,
    graph_changes: Vec<GraphChangeProposal>,
) -> AttemptReceipt {
    AttemptReceipt {
        attempt: attempt.parse().ok().expect("valid attempt id"),
        work: work.parse().ok().expect("valid work id"),
        outcome: AttemptOutcome::Completed,
        completed_at_ms: 6_000,
        outputs,
        evidence,
        graph_changes,
        usage: Usage {
            input_tokens: 1_200,
            output_tokens: 800,
            tool_calls: 2,
            duration_ms: 5_000,
        },
    }
}

pub fn failed_receipt(
    attempt: &str,
    work: &str,
    reason: &str,
    evidence: Vec<Evidence>,
) -> AttemptReceipt {
    AttemptReceipt {
        attempt: attempt.parse().ok().expect("valid attempt id"),
        work: work.parse().ok().expect("valid work id"),
        outcome: AttemptOutcome::Failed {
            reason: reason.to_string(),
        },
        completed_at_ms: 6_000,
        outputs: Vec::new(),
        evidence,
        graph_changes: Vec::new(),
        usage: Usage::default(),
    }
}

/// Fold facts into a graph, failing the test on any fold error.
pub fn fold(facts: &[DomainFact]) -> ProjectGraph {
    ProjectGraph::from_facts(facts).ok().expect("facts fold")
}

/// Apply one fact, failing the test on any fold error.
pub fn apply(graph: &mut ProjectGraph, fact: &DomainFact) {
    match graph.apply(fact) {
        Ok(()) => {}
        Err(error) => panic!("fact {fact:?} failed to apply: {error}"),
    }
}

/// Apply an acceptance's facts in order.
pub fn apply_all(graph: &mut ProjectGraph, facts: &[DomainFact]) {
    for fact in facts {
        apply(graph, fact);
    }
}

/// Every work item's derived status, for order-independence comparisons.
pub fn status_map(graph: &ProjectGraph) -> BTreeMap<WorkItemId, WorkStatus> {
    graph
        .works()
        .map(|(id, node)| (id.clone(), relay_engine::work_status(graph, node)))
        .collect()
}

/// A deterministic tiny LCG for shuffles (no external dependencies).
pub struct Lcg(pub u64);

impl Lcg {
    /// A fixed-seed generator; identical on every platform and run.
    pub fn new(seed: u64) -> Self {
        Lcg(seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407))
    }

    /// Next pseudo-random value in `[0, bound)`.
    pub fn below(&mut self, bound: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.0 >> 33) as usize) % bound.max(1)
    }
}

/// Merge two fact sequences while preserving the order inside each,
/// choosing the next sequence with the LCG.
pub fn interleave(a: &[DomainFact], b: &[DomainFact], lcg: &mut Lcg) -> Vec<DomainFact> {
    let (mut i, mut j) = (0, 0);
    let mut merged = Vec::new();
    while i < a.len() || j < b.len() {
        let take_a = if i == a.len() {
            false
        } else if j == b.len() {
            true
        } else {
            lcg.below(2) == 0
        };
        if take_a {
            merged.push(a[i].clone());
            i += 1;
        } else {
            merged.push(b[j].clone());
            j += 1;
        }
    }
    merged
}

/// Convenience id parsers.
pub fn work(id: &str) -> WorkItemId {
    id.parse().ok().expect("valid work id")
}

pub fn evidence_id(id: &str) -> EvidenceId {
    id.parse().ok().expect("valid evidence id")
}

pub fn attempt_id(id: &str) -> AttemptId {
    id.parse().ok().expect("valid attempt id")
}
