//! Shared helpers for the storage-owned integration suites
//! (`fact_replay`, `semantic_migration`, `crash_recovery`).
//!
//! Not a test target: no `#[test]` functions live here.

#![allow(dead_code)]

use std::sync::Arc;

use relay_domain::work::{CompletionRule, ExecutorClass, WorkItem};
use relay_domain::{
    ArtifactContent, ArtifactDigest, ArtifactId, ArtifactVersion, DomainFact, Evidence, EvidenceId,
    EvidenceKind, EvidenceOutcome, EvidenceTarget, Goal, ProjectId, ProjectRevision, Provenance,
    ProvenanceSource, WorkItemId, WorkKind,
};
use relay_store::{FactStoreOptions, GraphFactStore, Integrity};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

pub const CLOCK_MS: i64 = 1_791_878_400_000;

pub fn options() -> FactStoreOptions {
    FactStoreOptions::deterministic(CLOCK_MS)
}

pub fn parse<T>(value: &str) -> Result<T>
where
    T: std::str::FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    Ok(value.parse::<T>()?)
}

/// A fresh checked-open GraphFactStore in a temp workspace.
pub fn graph_store() -> Result<(tempfile::TempDir, GraphFactStore)> {
    let dir = tempfile::tempdir()?;
    let store = GraphFactStore::create(
        dir.path(),
        &parse("prj_crash")?,
        Integrity::Checksums,
        options(),
    )?;
    Ok((dir, store))
}

/// The minimal starting history: project + goal + one model work item.
pub fn seed_history(project: &ProjectId) -> Result<Vec<DomainFact>> {
    Ok(vec![
        DomainFact::ProjectCreated {
            project: project.clone(),
        },
        DomainFact::GoalDeclared {
            goal: Goal::new(
                parse("goal_1")?,
                "Ship the release gate",
                Provenance {
                    source: ProvenanceSource::User,
                    statement: None,
                    at_ms: CLOCK_MS,
                },
                CLOCK_MS,
            ),
        },
    ])
}

/// A receipt-completing model work item with no scopes.
pub fn model_work(id: &str, objective: &str) -> Result<WorkItem> {
    Ok(WorkItem::new(
        parse(id)?,
        WorkKind::parse("discover-opportunity")?,
        ExecutorClass::Model,
        objective,
        CompletionRule::Receipt,
    ))
}

/// A workspace-class work item with a declared write scope inside its
/// ceiling (the interesting case for abandoned reconciliation).
pub fn workspace_work(id: &str) -> Result<WorkItem> {
    let mut work = WorkItem::new(
        parse(id)?,
        WorkKind::parse("implement-task")?,
        ExecutorClass::Workspace,
        "Implement the gate evaluator",
        CompletionRule::Receipt,
    );
    work.write_scopes = vec![parse("src")?];
    work.capability_ceiling = relay_domain::work::CapabilityCeiling {
        write_scopes: vec![parse(".")?],
        operations: vec![
            relay_domain::Operation::Read,
            relay_domain::Operation::Write,
            relay_domain::Operation::Execute,
        ],
        delivery: relay_domain::DeliveryAuthority::None,
    };
    Ok(work)
}

/// Publish one artifact version (content committed through the shared
/// canonicalizer, per the lead ruling on content digests).
pub fn published(artifact: &str, version: u32) -> Result<DomainFact> {
    let content = serde_json::json!({
        "schema": "relay.brief@1",
        "problem": "traps regress",
        "audience": "agents",
        "outcome": "no false greens",
        "open_questions": [],
    });
    let digest = relay_artifacts::content_digest(&content)?;
    Ok(DomainFact::ArtifactVersionPublished {
        artifact: parse(artifact)?,
        version: ArtifactVersion::new(version)?,
        title: Some("Brief".to_string()),
        content: ArtifactContent::new(content),
        digest: ArtifactDigest::parse(&digest)?,
        derived_from: Vec::new(),
        published_at_ms: CLOCK_MS,
    })
}

/// Evidence targeting an exact artifact version.
pub fn evidence(
    id: &str,
    artifact: &str,
    version: u32,
    outcome: EvidenceOutcome,
) -> Result<DomainFact> {
    Ok(DomainFact::EvidenceRecorded {
        evidence: Evidence::new(
            parse(id)?,
            EvidenceKind::TestRun,
            EvidenceTarget::ArtifactVersion(relay_domain::ArtifactVersionRef {
                artifact: parse(artifact)?,
                version: ArtifactVersion::new(version)?,
            }),
            outcome,
            "suite ran",
            CLOCK_MS,
        ),
    })
}

/// A completed receipt for `attempt` running `work`, carrying one piece
/// of evidence (a completion with nothing at all is rejected by the
/// engine's empty-completion rule).
pub fn completed_receipt(attempt: &str, work: &WorkItemId) -> Result<relay_domain::AttemptReceipt> {
    Ok(relay_domain::AttemptReceipt {
        attempt: parse(attempt)?,
        work: work.clone(),
        outcome: relay_domain::AttemptOutcome::Completed,
        completed_at_ms: CLOCK_MS + 10,
        outputs: Vec::new(),
        evidence: vec![Evidence::new(
            parse(&format!(
                "ev_{}-run",
                attempt.trim_start_matches("att_").replace('_', "-")
            ))?,
            EvidenceKind::Review,
            EvidenceTarget::WorkItem(work.clone()),
            EvidenceOutcome::Pass,
            "work reviewed",
            CLOCK_MS + 5,
        )],
        graph_changes: Vec::new(),
        usage: relay_domain::Usage::default(),
    })
}

/// The fact-store-backed FactSink: the durable face the orchestrator speaks
/// to in production (the app wires this in Phase 6; here it drives
/// end-to-end reconciliation over a real store).
pub struct StoreFactSink {
    store: Arc<GraphFactStore>,
}

impl StoreFactSink {
    /// Wrap an open store.
    pub fn new(store: Arc<GraphFactStore>) -> StoreFactSink {
        StoreFactSink { store }
    }
}

impl relay_executor::FactSink for StoreFactSink {
    fn append(&self, fact: DomainFact) -> std::result::Result<(), relay_executor::FactSinkError> {
        self.store
            .append(&[fact])
            .map(|_| ())
            .map_err(|error| relay_executor::FactSinkError::Failed(error.to_string()))
    }

    fn facts(&self) -> std::result::Result<Vec<DomainFact>, relay_executor::FactSinkError> {
        self.store
            .read_facts()
            .map_err(|error| relay_executor::FactSinkError::Failed(error.to_string()))
    }
}

/// The revision of a store as a count (for expected-revision pins).
pub fn revision_of(store: &GraphFactStore) -> Result<ProjectRevision> {
    Ok(store.revision()?)
}

/// Grant id helper.
pub fn grant_id(id: &str) -> Result<relay_domain::GrantId> {
    parse(id)
}

/// Evidence id helper.
pub fn evidence_id(id: &str) -> Result<EvidenceId> {
    parse(id)
}

/// Artifact id helper.
pub fn artifact_id(id: &str) -> Result<ArtifactId> {
    parse(id)
}

/// Write the publication blob for one published version, per the accepted
/// convention: `<root>/.relay-graph/artifacts/<artifact>/<version>-<digest16>.blob`
/// holding the canonical content bytes.
pub fn write_blob(
    workspace_root: &std::path::Path,
    artifact: &str,
    version: u32,
    content: &serde_json::Value,
) -> Result<String> {
    let digest = relay_artifacts::content_digest(content)?;
    let prefix: String = digest.chars().take(16).collect();
    let dir = workspace_root
        .join(".relay-graph")
        .join("artifacts")
        .join(artifact);
    std::fs::create_dir_all(&dir)?;
    let blob = dir.join(format!("{version}-{prefix}.blob"));
    std::fs::write(&blob, relay_artifacts::canonical_json_bytes(content)?)?;
    Ok(digest)
}
