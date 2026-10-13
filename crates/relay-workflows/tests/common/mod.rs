//! Deterministic fake-executor harness and fact builders for the workflow
//! scenario tests. Plain data and pure functions only: providers differ in
//! bytes, never in policy.

#![allow(dead_code)] // each scenario uses a different subset of the harness
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::ok_expect,
    clippy::err_expect
)] // test-only conveniences, house style

use relay_domain::{
    ArtifactContent, ArtifactDigest, ArtifactVersion, ArtifactVersionRef as Ref, AttemptId,
    AttemptOutcome, AttemptReceipt, CapabilityGrant, CompletionRule, DecisionResolution,
    DomainFact, Evidence, EvidenceId, EvidenceKind, EvidenceOutcome, EvidenceTarget, GrantId,
    GraphChangeProposal, OutputProposal, ProjectGraph, ProjectId, Provenance, ProvenanceSource,
    ScopePath, Usage, WorkItemId,
};
use relay_engine::WorkStatus;
use relay_workflows::WorkflowCatalog;

/// A deterministic fake provider. Providers change BYTES (content notes,
/// digests) but never policy: two providers produce different artifacts and
/// identical accepted-fact structure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FakeProvider {
    /// Provider name (surfaces in content notes).
    pub name: &'static str,
    /// A deterministic flavor distinguishing providers.
    pub flavor: u64,
}

impl FakeProvider {
    /// The canonical test provider.
    pub fn alpha() -> Self {
        FakeProvider {
            name: "fake-alpha",
            flavor: 1,
        }
    }

    /// A second, byte-different test provider.
    pub fn beta() -> Self {
        FakeProvider {
            name: "fake-beta",
            flavor: 2,
        }
    }

    /// A provider-flavored content note.
    pub fn note(&self) -> String {
        format!("rendered by {} (flavor {})", self.name, self.flavor)
    }

    /// A deterministic digest for an output salt.
    pub fn digest(&self, salt: u64) -> ArtifactDigest {
        ArtifactDigest::parse(&format!("{:064x}", self.flavor * 1_000_003 + salt))
            .ok()
            .expect("valid digest")
    }

    /// Wrap a structured document as provider-flavored content: the typed
    /// meaning (schema tag included) is identical across providers, the
    /// bytes differ.
    pub fn content<D: relay_workflows::StructuredDoc>(&self, doc: &D) -> ArtifactContent {
        let mut value = doc.to_content().0;
        if let serde_json::Value::Object(map) = &mut value {
            map.insert("rendered_by".to_string(), serde_json::json!(self.note()));
        }
        ArtifactContent(value)
    }

    /// An output proposal for an artifact, provider-flavored.
    pub fn output<D: relay_workflows::StructuredDoc>(
        &self,
        artifact: &str,
        title: &str,
        doc: &D,
        salt: u64,
    ) -> OutputProposal {
        OutputProposal {
            artifact: artifact.parse().expect("valid artifact id"),
            title: Some(title.to_string()),
            content: self.content(doc),
            digest: self.digest(salt),
        }
    }
}

pub fn project(id: &str) -> DomainFact {
    DomainFact::ProjectCreated {
        project: ProjectId::parse(id).expect("valid project id"),
    }
}

pub fn goal(id: &str, outcome: &str) -> DomainFact {
    DomainFact::GoalDeclared {
        goal: relay_domain::Goal::new(
            id.parse().expect("valid goal id"),
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

pub fn vref(artifact: &str, version: u32) -> Ref {
    Ref::new(
        artifact.parse().expect("valid artifact id"),
        ArtifactVersion::new(version).expect("version >= 1"),
    )
}

/// Fold facts, failing loudly on any inconsistency.
pub fn fold(facts: &[DomainFact]) -> ProjectGraph {
    ProjectGraph::from_facts(facts).expect("facts fold")
}

/// Apply one fact to a graph under test.
pub fn apply(graph: &mut ProjectGraph, fact: &DomainFact) {
    match graph.apply(fact) {
        Ok(()) => {}
        Err(error) => panic!("fact {fact:?} failed to apply: {error}"),
    }
}

/// Run one round of workflow expansion: propose, validate against the
/// evolving graph, and apply everything accepted. Returns the applied
/// facts (the caller that needs history — crash-recovery tests — pushes
/// them into its own fact list).
pub fn expand(graph: &mut ProjectGraph) -> Vec<DomainFact> {
    let catalog = WorkflowCatalog::builtin();
    let mut applied_facts = Vec::new();
    let snapshot = graph.clone();
    for proposal in relay_workflows::propose_next(&snapshot, &catalog) {
        let applied = relay_engine::validate_graph_change(graph, &proposal)
            .ok()
            .unwrap_or_else(|| panic!("expansion proposal rejected: {proposal:?}"));
        for fact in &applied.facts {
            apply(graph, fact);
            applied_facts.push(fact.clone());
        }
    }
    applied_facts
}

/// Start an attempt for a work item under a grant that exactly covers the
/// work's needs at the current revision. Returns the applied fact so
/// history-tracking tests can mirror it.
pub fn start(graph: &mut ProjectGraph, attempt: &str, work: &str) -> DomainFact {
    let fact = {
        let node = graph
            .work(&work.parse::<WorkItemId>().expect("valid work id"))
            .expect("work exists");
        let reads = if node.work.read_scopes.is_empty() {
            vec![ScopePath::root()]
        } else {
            node.work.read_scopes.clone()
        };
        DomainFact::AttemptStarted {
            attempt: attempt.parse::<AttemptId>().expect("valid attempt id"),
            work: node.work.id.clone(),
            expected_revision: graph.revision(),
            grant: CapabilityGrant::for_scopes(
                GrantId::parse(&format!("grant_{}", attempt.replace('_', "-")))
                    .ok()
                    .expect("valid grant id"),
                reads,
                node.effective_write_scopes(),
            ),
            started_at_ms: 5_000,
        }
    };
    apply(graph, &fact);
    fact
}

/// Human executor: resolve the open decision completing a work item.
pub fn human_resolves(
    graph: &ProjectGraph,
    work: &str,
    choice: &str,
    rationale: &str,
) -> DomainFact {
    let node = graph
        .work(&work.parse::<WorkItemId>().expect("valid work id"))
        .expect("work exists");
    match &node.work.completion {
        CompletionRule::Decision { decision } => DomainFact::DecisionResolved {
            decision: decision.clone(),
            resolution: DecisionResolution {
                choice: choice.to_string(),
                rationale: rationale.to_string(),
                resolved_at_ms: 6_000,
            },
        },
        other => panic!("work {work} does not complete by decision ({other:?})"),
    }
}

/// Model/workspace/verifier executor: submit a receipt and fold its
/// acceptance (publications, evidence, conclusion). Returns the applied
/// facts so history-tracking tests can mirror them.
pub fn executor_delivers(graph: &mut ProjectGraph, receipt: AttemptReceipt) -> Vec<DomainFact> {
    let acceptance = match relay_engine::accept_receipt(graph, &receipt) {
        Ok(acceptance) => acceptance,
        Err(rejection) => panic!("receipt rejected: {rejection}"),
    };
    let mut applied = Vec::new();
    for fact in &acceptance.facts {
        apply(graph, fact);
        applied.push(fact.clone());
    }
    // The runtime loop: the receipt's graph-change proposals go through
    // policy individually and apply only what validates.
    for proposal in &receipt.graph_changes {
        let accepted = relay_engine::validate_graph_change(graph, proposal)
            .ok()
            .unwrap_or_else(|| panic!("receipt proposal rejected: {proposal:?}"));
        for fact in &accepted.facts {
            apply(graph, fact);
            applied.push(fact.clone());
        }
    }
    applied
}

/// A completed receipt for one attempt.
pub fn receipt(
    attempt: &str,
    work: &str,
    outputs: Vec<OutputProposal>,
    evidence: Vec<Evidence>,
    graph_changes: Vec<GraphChangeProposal>,
) -> AttemptReceipt {
    AttemptReceipt {
        attempt: attempt.parse().expect("valid attempt id"),
        work: work.parse().expect("valid work id"),
        outcome: AttemptOutcome::Completed,
        completed_at_ms: 7_000,
        outputs,
        evidence,
        graph_changes,
        usage: Usage {
            input_tokens: 100,
            output_tokens: 50,
            tool_calls: 1,
            duration_ms: 500,
        },
    }
}

/// A failed receipt for one attempt.
pub fn failed_receipt(attempt: &str, work: &str, reason: &str) -> AttemptReceipt {
    AttemptReceipt {
        attempt: attempt.parse().expect("valid attempt id"),
        work: work.parse().expect("valid work id"),
        outcome: AttemptOutcome::Failed {
            reason: reason.to_string(),
        },
        completed_at_ms: 7_000,
        outputs: Vec::new(),
        evidence: Vec::new(),
        graph_changes: Vec::new(),
        usage: Usage::default(),
    }
}

/// Verifier evidence against an exact criterion version.
pub fn criterion_evidence(
    id: &str,
    criterion: (&str, u32),
    outcome: EvidenceOutcome,
    summary: &str,
) -> Evidence {
    Evidence::new(
        EvidenceId::parse(id).expect("valid evidence id"),
        EvidenceKind::TestRun,
        EvidenceTarget::ArtifactVersion(vref(criterion.0, criterion.1)),
        outcome,
        summary,
        7_000,
    )
}

/// The status of one work item by id.
pub fn status_of(graph: &ProjectGraph, work: &str) -> WorkStatus {
    let id: WorkItemId = work.parse().expect("valid work id");
    relay_engine::work_status(graph, graph.work(&id).expect("work exists"))
}

/// Find the work id of the single live instance of a kind.
pub fn sole_work_of_kind(graph: &ProjectGraph, kind: &str) -> WorkItemId {
    let matches: Vec<WorkItemId> = graph
        .works()
        .filter(|(_, node)| node.work.kind.as_str() == kind && !node.is_declared_terminal())
        .map(|(id, _)| id.clone())
        .collect();
    match matches.len() {
        1 => matches[0].clone(),
        n => panic!("expected one live {kind} work, found {n}"),
    }
}

/// All live work ids of a kind, in id order.
pub fn works_of_kind(graph: &ProjectGraph, kind: &str) -> Vec<WorkItemId> {
    graph
        .works()
        .filter(|(_, node)| node.work.kind.as_str() == kind && !node.is_declared_terminal())
        .map(|(id, _)| id.clone())
        .collect()
}

/// The ArtifactContent of a current artifact version.
pub fn content_of(graph: &ProjectGraph, artifact: &str) -> ArtifactContent {
    let id = artifact
        .parse::<relay_domain::ArtifactId>()
        .expect("valid artifact id");
    let version = graph.current_version(&id).expect("artifact exists");
    graph
        .version_record(&Ref::new(id, version))
        .expect("version exists")
        .content
        .clone()
}
