//! The work-graph SQLite projection: the domain [`FactFold`] plus lookup
//! queries (`docs/plans/fact-format-proposal.md` §6).
//!
//! [`GraphFold`] folds each validated fact into normalized tables — work
//! items (kind, executor class, effective scopes), dependency edges,
//! decisions and their resolution state, artifact versions with digests and
//! `derived_from` inputs, evidence with outcomes and targets, attempts with
//! grant summaries and conclusions, and declared invalidations — alongside
//! the storage-owned `facts` sequence index and checkpoint (see
//! [`crate::fact::projection`]).
//!
//! **What is deliberately absent:** readiness, blockers, awaiting-human,
//! completion, and recommendations. Those are engine-derived over the
//! folded graph (`GraphFactStore::project_graph` +
//! `relay-engine`), never long-lived rows — the projection serves lookup
//! and search only.
//!
//! Rows carry the fact `sequence` that last changed them, which equals the
//! domain revision at that change
//! ([`ProjectRevision`](relay_domain::ProjectRevision) is the fact count).

use relay_domain::DependencyKind;
use relay_domain::DomainFact;
use relay_domain::Operation;
use relay_domain::ScopePath;
use relay_domain::ids::ArtifactId;
use relay_domain::ids::AttemptId;
use relay_domain::ids::DecisionId;
use relay_domain::ids::WorkItemId;
use relay_domain::work::ExecutorClass;

use crate::error::Result;
use crate::error::StoreError;
use crate::fact::domain::from_record;
use crate::fact::projection::FactFold;
use crate::fact::record::FactRecord;
use rusqlite::Connection;
use rusqlite::params;

/// The graph projection fold. Stateless: all state lives in the database,
/// and every table it writes is a pure function of the fact stream, so
/// delete-and-rebuild converges to identical rows.
#[derive(Debug, Default, Clone, Copy)]
pub struct GraphFold;

impl FactFold for GraphFold {
    fn prepare(&self, tx: &rusqlite::Transaction<'_>) -> Result<()> {
        tx.execute_batch(GRAPH_SCHEMA)?;
        Ok(())
    }

    fn apply(&self, tx: &rusqlite::Transaction<'_>, record: &FactRecord) -> Result<()> {
        let fact = from_record(record)?;
        let sequence = record.sequence as i64;
        match &fact {
            DomainFact::ProjectCreated { project } => {
                upsert_meta_project(tx, project.as_str(), sequence)?;
            }
            DomainFact::GoalDeclared { goal } => {
                upsert_meta_goal(
                    tx,
                    &serde_json::to_string(goal).map_err(json_error)?,
                    sequence,
                )?;
            }
            DomainFact::WorkDeclared { work } => {
                insert_work(tx, work, sequence)?;
                for edge in &work.dependencies {
                    upsert_dependency(tx, &edge.from, &edge.to, edge.kind, sequence)?;
                }
            }
            DomainFact::WorkSuperseded {
                work,
                superseded_by,
                ..
            } => {
                touch_work(tx, work)?;
                tx.execute(
                    "UPDATE graph_works SET superseded_by = ?2, last_changed_sequence = ?3
                     WHERE id = ?1;",
                    params![work.as_str(), superseded_by.as_str(), sequence],
                )?;
            }
            DomainFact::WorkCanceled { work, reason } => {
                touch_work(tx, work)?;
                tx.execute(
                    "UPDATE graph_works SET canceled_reason = ?2, last_changed_sequence = ?3
                     WHERE id = ?1;",
                    params![work.as_str(), reason, sequence],
                )?;
            }
            DomainFact::WorkScopeExpanded {
                work,
                added_write_scopes,
                added_operations,
                ..
            } => {
                expand_work(tx, work, added_write_scopes, added_operations, sequence)?;
            }
            DomainFact::DependencyAsserted { from, to, kind } => {
                upsert_dependency(tx, from, to, *kind, sequence)?;
            }
            DomainFact::DependencyRetracted { from, to, kind, .. } => {
                tx.execute(
                    "UPDATE graph_dependencies
                     SET retracted = 1, retracted_sequence = ?4
                     WHERE from_id = ?1 AND to_id = ?2 AND kind = ?3;",
                    params![
                        from.as_str(),
                        to.as_str(),
                        dependency_label(*kind),
                        sequence
                    ],
                )?;
            }
            DomainFact::DecisionOpened { decision } => {
                insert_decision(tx, decision, sequence)?;
            }
            DomainFact::DecisionResolved {
                decision,
                resolution,
            } => {
                tx.execute(
                    "UPDATE graph_decisions
                     SET resolution = ?2, last_changed_sequence = ?3
                     WHERE id = ?1;",
                    params![
                        decision.as_str(),
                        serde_json::to_string(resolution).map_err(json_error)?,
                        sequence
                    ],
                )?;
            }
            DomainFact::DecisionSuperseded {
                decision,
                superseded_by,
            } => {
                tx.execute(
                    "UPDATE graph_decisions
                     SET superseded_by = ?2, last_changed_sequence = ?3
                     WHERE id = ?1;",
                    params![decision.as_str(), superseded_by.as_str(), sequence],
                )?;
            }
            DomainFact::ArtifactVersionPublished {
                artifact,
                version,
                title,
                content,
                digest,
                derived_from,
                published_at_ms,
            } => {
                tx.execute(
                    "INSERT OR REPLACE INTO graph_artifacts (
                       artifact, version, title, content, digest,
                       published_at_ms, published_sequence
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7);",
                    params![
                        artifact.as_str(),
                        version.get() as i64,
                        title,
                        serde_json::to_string(&content.0).map_err(json_error)?,
                        digest.as_str(),
                        published_at_ms,
                        sequence
                    ],
                )?;
                tx.execute(
                    "DELETE FROM graph_artifact_inputs WHERE artifact = ?1 AND version = ?2;",
                    params![artifact.as_str(), version.get() as i64],
                )?;
                for input in derived_from {
                    tx.execute(
                        "INSERT OR IGNORE INTO graph_artifact_inputs (
                           artifact, version, input_artifact, input_version
                         ) VALUES (?1, ?2, ?3, ?4);",
                        params![
                            artifact.as_str(),
                            version.get() as i64,
                            input.artifact.as_str(),
                            input.version.get() as i64
                        ],
                    )?;
                }
            }
            DomainFact::EvidenceRecorded { evidence } => {
                let (target_kind, target_id, target_version) =
                    evidence_target_parts(&evidence.target);
                tx.execute(
                    "INSERT OR REPLACE INTO graph_evidence (
                       id, kind, target_kind, target_id, target_version,
                       outcome, summary, payload, recorded_at_ms, recorded_sequence
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10);",
                    params![
                        evidence.id.as_str(),
                        evidence_kind_label(evidence.kind),
                        target_kind,
                        target_id,
                        target_version,
                        evidence_outcome_label(evidence.outcome),
                        evidence.summary,
                        serde_json::to_string(&evidence.payload).map_err(json_error)?,
                        evidence.recorded_at_ms,
                        sequence
                    ],
                )?;
            }
            DomainFact::AttemptStarted {
                attempt,
                work,
                expected_revision,
                grant,
                started_at_ms,
            } => {
                tx.execute(
                    "INSERT OR REPLACE INTO graph_attempts (
                       id, work, expected_revision, grant_id, grant,
                       grant_read_paths, grant_write_paths, grant_operations, grant_delivery,
                       started_at_ms, started_sequence
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11);",
                    params![
                        attempt.as_str(),
                        work.as_str(),
                        expected_revision.get() as i64,
                        grant.id.as_str(),
                        serde_json::to_string(grant).map_err(json_error)?,
                        scopes_text(&grant.read_paths),
                        scopes_text(&grant.write_paths),
                        operations_text(&grant.operations),
                        format!("{:?}", grant.delivery).to_lowercase(),
                        started_at_ms,
                        sequence
                    ],
                )?;
            }
            DomainFact::AttemptConcluded {
                attempt,
                conclusion,
            } => {
                tx.execute(
                    "UPDATE graph_attempts
                     SET conclusion_status = ?2, conclusion = ?3, concluded_sequence = ?4
                     WHERE id = ?1;",
                    params![
                        attempt.as_str(),
                        conclusion_label(conclusion),
                        serde_json::to_string(conclusion).map_err(json_error)?,
                        sequence
                    ],
                )?;
            }
            DomainFact::InvalidationDeclared {
                target,
                cause,
                changed_input,
            } => {
                let (target_kind, target_id, target_version) = invalidation_target_parts(target);
                tx.execute(
                    "INSERT OR REPLACE INTO graph_invalidations (
                       target_kind, target_id, target_version, cause,
                       changed_input_artifact, changed_input_version, declared_sequence
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7);",
                    params![
                        target_kind,
                        target_id,
                        target_version,
                        serde_json::to_string(cause).map_err(json_error)?,
                        changed_input.as_ref().map(|input| input.artifact.as_str()),
                        changed_input
                            .as_ref()
                            .map(|input| input.version.get() as i64),
                        sequence
                    ],
                )?;
            }
        }
        upsert_meta_revision(tx, record.sequence)?;
        Ok(())
    }
}

fn json_error(error: serde_json::Error) -> StoreError {
    StoreError::Canonical {
        reason: format!("projecting a fact: {error}"),
    }
}

/// The graph projection schema. Owned by storage; versioned by the
/// fold-prepared `graph_schema` marker table so a future schema change can
/// migrate forward.
const GRAPH_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS graph_schema (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  version INTEGER NOT NULL
);
INSERT OR IGNORE INTO graph_schema (id, version) VALUES (1, 1);

CREATE TABLE IF NOT EXISTS graph_works (
  id TEXT PRIMARY KEY,
  kind TEXT NOT NULL,
  executor TEXT NOT NULL,
  objective TEXT NOT NULL,
  completion TEXT NOT NULL,
  required INTEGER NOT NULL,
  read_scopes TEXT NOT NULL,
  write_scopes TEXT NOT NULL,
  effective_write_scopes TEXT NOT NULL,
  effective_operations TEXT NOT NULL,
  inputs TEXT NOT NULL,
  expected_outputs TEXT NOT NULL,
  capability_ceiling TEXT NOT NULL,
  superseded_by TEXT,
  canceled_reason TEXT,
  declared_sequence INTEGER NOT NULL,
  last_changed_sequence INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS graph_works_kind ON graph_works (kind);
CREATE INDEX IF NOT EXISTS graph_works_executor ON graph_works (executor);

CREATE TABLE IF NOT EXISTS graph_dependencies (
  from_id TEXT NOT NULL,
  to_id TEXT NOT NULL,
  kind TEXT NOT NULL,
  asserted_sequence INTEGER NOT NULL,
  retracted INTEGER NOT NULL DEFAULT 0,
  retracted_sequence INTEGER,
  PRIMARY KEY (from_id, to_id, kind)
);
CREATE INDEX IF NOT EXISTS graph_dependencies_to ON graph_dependencies (to_id);

CREATE TABLE IF NOT EXISTS graph_decisions (
  id TEXT PRIMARY KEY,
  subject TEXT NOT NULL,
  question TEXT NOT NULL,
  options TEXT NOT NULL,
  recommendation TEXT,
  evidence TEXT NOT NULL,
  consequences TEXT NOT NULL,
  authority TEXT NOT NULL,
  opened_at_ms INTEGER NOT NULL,
  opened_sequence INTEGER NOT NULL,
  resolution TEXT,
  superseded_by TEXT,
  last_changed_sequence INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS graph_decisions_open ON graph_decisions (
  resolution, superseded_by
);

CREATE TABLE IF NOT EXISTS graph_artifacts (
  artifact TEXT NOT NULL,
  version INTEGER NOT NULL,
  title TEXT,
  content TEXT NOT NULL,
  digest TEXT NOT NULL,
  published_at_ms INTEGER NOT NULL,
  published_sequence INTEGER NOT NULL,
  PRIMARY KEY (artifact, version)
);

CREATE TABLE IF NOT EXISTS graph_artifact_inputs (
  artifact TEXT NOT NULL,
  version INTEGER NOT NULL,
  input_artifact TEXT NOT NULL,
  input_version INTEGER NOT NULL,
  PRIMARY KEY (artifact, version, input_artifact, input_version)
);
CREATE INDEX IF NOT EXISTS graph_artifact_inputs_input
  ON graph_artifact_inputs (input_artifact, input_version);

CREATE TABLE IF NOT EXISTS graph_evidence (
  id TEXT PRIMARY KEY,
  kind TEXT NOT NULL,
  target_kind TEXT NOT NULL,
  target_id TEXT NOT NULL,
  target_version INTEGER,
  outcome TEXT NOT NULL,
  summary TEXT NOT NULL,
  payload TEXT NOT NULL,
  recorded_at_ms INTEGER NOT NULL,
  recorded_sequence INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS graph_evidence_target
  ON graph_evidence (target_kind, target_id, target_version);
CREATE INDEX IF NOT EXISTS graph_evidence_outcome ON graph_evidence (outcome);

CREATE TABLE IF NOT EXISTS graph_attempts (
  id TEXT PRIMARY KEY,
  work TEXT NOT NULL,
  expected_revision INTEGER NOT NULL,
  grant_id TEXT NOT NULL,
  grant TEXT NOT NULL,
  grant_read_paths TEXT NOT NULL,
  grant_write_paths TEXT NOT NULL,
  grant_operations TEXT NOT NULL,
  grant_delivery TEXT NOT NULL,
  started_at_ms INTEGER NOT NULL,
  started_sequence INTEGER NOT NULL,
  conclusion_status TEXT,
  conclusion TEXT,
  concluded_sequence INTEGER
);
CREATE INDEX IF NOT EXISTS graph_attempts_work ON graph_attempts (work);

CREATE TABLE IF NOT EXISTS graph_invalidations (
  target_kind TEXT NOT NULL,
  target_id TEXT NOT NULL,
  target_version INTEGER,
  cause TEXT NOT NULL,
  changed_input_artifact TEXT,
  changed_input_version INTEGER,
  declared_sequence INTEGER NOT NULL,
  PRIMARY KEY (target_kind, target_id, target_version, declared_sequence)
);

CREATE TABLE IF NOT EXISTS graph_meta (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  project TEXT,
  goal TEXT,
  revision INTEGER NOT NULL DEFAULT 0
);
INSERT OR IGNORE INTO graph_meta (id, revision) VALUES (1, 0);
";

fn insert_work(
    tx: &rusqlite::Transaction<'_>,
    work: &relay_domain::WorkItem,
    sequence: i64,
) -> Result<()> {
    tx.execute(
        "INSERT OR REPLACE INTO graph_works (
           id, kind, executor, objective, completion, required,
           read_scopes, write_scopes, effective_write_scopes, effective_operations,
           inputs, expected_outputs, capability_ceiling,
           declared_sequence, last_changed_sequence
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?14);",
        params![
            work.id.as_str(),
            work.kind.as_str(),
            executor_label(work.executor),
            work.objective,
            serde_json::to_string(&work.completion).map_err(json_error)?,
            work.required as i64,
            scopes_text(&work.read_scopes),
            scopes_text(&work.write_scopes),
            scopes_text(&work.write_scopes),
            operations_text(&work.capability_ceiling.operations),
            serde_json::to_string(&work.inputs).map_err(json_error)?,
            serde_json::to_string(&work.expected_outputs).map_err(json_error)?,
            serde_json::to_string(&work.capability_ceiling).map_err(json_error)?,
            sequence,
        ],
    )?;
    Ok(())
}

/// Apply a scope/operation expansion: the effective columns are recomputed
/// as declared + added (matching
/// [`WorkNode::effective_write_scopes`](relay_domain::graph::WorkNode)).
fn expand_work(
    tx: &rusqlite::Transaction<'_>,
    work: &WorkItemId,
    added_scopes: &[ScopePath],
    added_operations: &[Operation],
    sequence: i64,
) -> Result<()> {
    touch_work(tx, work)?;
    let row = tx.query_row(
        "SELECT write_scopes, effective_operations FROM graph_works WHERE id = ?1;",
        params![work.as_str()],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
    );
    let (declared_scopes, operations_json) = match row {
        Ok(columns) => columns,
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            return Err(StoreError::Projection {
                reason: format!(
                    "work_scope_expanded for unknown work item {work}; the fact stream is \
                     inconsistent with the projection"
                )
                .to_string(),
                source: None,
            });
        }
        Err(error) => return Err(error.into()),
    };
    let mut scopes: Vec<String> = serde_json::from_str(&declared_scopes).map_err(json_error)?;
    for added in added_scopes {
        let text = added.as_str().to_string();
        if !scopes.contains(&text) {
            scopes.push(text);
        }
    }
    let mut operations: Vec<String> = serde_json::from_str(&operations_json).map_err(json_error)?;
    for added in added_operations {
        let label = operation_label(*added).to_string();
        if !operations.contains(&label) {
            operations.push(label);
        }
    }
    tx.execute(
        "UPDATE graph_works
         SET effective_write_scopes = ?2, effective_operations = ?3, last_changed_sequence = ?4
         WHERE id = ?1;",
        params![
            work.as_str(),
            serde_json::to_string(&scopes).map_err(json_error)?,
            serde_json::to_string(&operations).map_err(json_error)?,
            sequence
        ],
    )?;
    Ok(())
}

fn touch_work(tx: &rusqlite::Transaction<'_>, work: &WorkItemId) -> Result<()> {
    let changed = tx.execute(
        "UPDATE graph_works SET last_changed_sequence = last_changed_sequence WHERE id = ?1;",
        params![work.as_str()],
    )?;
    if changed == 0 {
        return Err(StoreError::Projection {
            reason: format!(
                "fact references unknown work item {work}; the fact stream is inconsistent"
            ),
            source: None,
        });
    }
    Ok(())
}

fn upsert_dependency(
    tx: &rusqlite::Transaction<'_>,
    from: &WorkItemId,
    to: &WorkItemId,
    kind: DependencyKind,
    sequence: i64,
) -> Result<()> {
    tx.execute(
        "INSERT INTO graph_dependencies (from_id, to_id, kind, asserted_sequence)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(from_id, to_id, kind) DO UPDATE SET
           asserted_sequence = excluded.asserted_sequence,
           retracted = 0,
           retracted_sequence = NULL;",
        params![from.as_str(), to.as_str(), dependency_label(kind), sequence],
    )?;
    Ok(())
}

fn insert_decision(
    tx: &rusqlite::Transaction<'_>,
    decision: &relay_domain::Decision,
    sequence: i64,
) -> Result<()> {
    tx.execute(
        "INSERT OR REPLACE INTO graph_decisions (
           id, subject, question, options, recommendation, evidence, consequences,
           authority, opened_at_ms, opened_sequence, last_changed_sequence
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10);",
        params![
            decision.id.as_str(),
            serde_json::to_string(&decision.subject).map_err(json_error)?,
            decision.question,
            serde_json::to_string(&decision.options).map_err(json_error)?,
            decision.recommendation,
            serde_json::to_string(&decision.evidence).map_err(json_error)?,
            decision.consequences,
            format!("{:?}", decision.authority).to_lowercase(),
            decision.opened_at_ms,
            sequence,
        ],
    )?;
    Ok(())
}

fn upsert_meta_project(tx: &rusqlite::Transaction<'_>, project: &str, sequence: i64) -> Result<()> {
    tx.execute(
        "INSERT INTO graph_meta (id, project, revision) VALUES (1, ?1, ?2)
         ON CONFLICT(id) DO UPDATE SET project = excluded.project, revision = excluded.revision;",
        params![project, sequence],
    )?;
    Ok(())
}

fn upsert_meta_goal(tx: &rusqlite::Transaction<'_>, goal: &str, sequence: i64) -> Result<()> {
    tx.execute(
        "UPDATE graph_meta SET goal = ?1, revision = ?2 WHERE id = 1;",
        params![goal, sequence],
    )?;
    Ok(())
}

fn upsert_meta_revision(tx: &rusqlite::Transaction<'_>, sequence: u64) -> Result<()> {
    tx.execute(
        "UPDATE graph_meta SET revision = ?1 WHERE id = 1;",
        params![sequence as i64],
    )?;
    Ok(())
}

fn dependency_label(kind: DependencyKind) -> &'static str {
    match kind {
        DependencyKind::Requires => "requires",
        DependencyKind::Informs => "informs",
        DependencyKind::Validates => "validates",
        DependencyKind::Supersedes => "supersedes",
        DependencyKind::ConflictsWith => "conflicts_with",
    }
}

fn executor_label(executor: ExecutorClass) -> &'static str {
    match executor {
        ExecutorClass::Human => "human",
        ExecutorClass::Model => "model",
        ExecutorClass::Workspace => "workspace",
        ExecutorClass::Verifier => "verifier",
        ExecutorClass::System => "system",
    }
}

fn operation_label(operation: Operation) -> &'static str {
    match operation {
        Operation::Read => "read",
        Operation::Write => "write",
        Operation::Execute => "execute",
        Operation::Network => "network",
    }
}

fn scopes_text(scopes: &[ScopePath]) -> String {
    serde_json::to_string(&scopes.iter().map(ScopePath::as_str).collect::<Vec<_>>())
        .unwrap_or_else(|_| "[]".to_string())
}

fn operations_text(operations: &[Operation]) -> String {
    serde_json::to_string(
        &operations
            .iter()
            .map(|o| operation_label(*o))
            .collect::<Vec<_>>(),
    )
    .unwrap_or_else(|_| "[]".to_string())
}

fn evidence_kind_label(kind: relay_domain::EvidenceKind) -> &'static str {
    match kind {
        relay_domain::EvidenceKind::TestRun => "test_run",
        relay_domain::EvidenceKind::CommandOutput => "command_output",
        relay_domain::EvidenceKind::StaticAnalysis => "static_analysis",
        relay_domain::EvidenceKind::Inspection => "inspection",
        relay_domain::EvidenceKind::Review => "review",
        relay_domain::EvidenceKind::HumanStatement => "human_statement",
    }
}

fn evidence_outcome_label(outcome: relay_domain::EvidenceOutcome) -> &'static str {
    match outcome {
        relay_domain::EvidenceOutcome::Pass => "pass",
        relay_domain::EvidenceOutcome::Fail => "fail",
        relay_domain::EvidenceOutcome::Info => "info",
    }
}

fn conclusion_label(conclusion: &relay_domain::AttemptConclusion) -> &'static str {
    match conclusion {
        relay_domain::AttemptConclusion::Accepted { .. } => "accepted",
        relay_domain::AttemptConclusion::Failed { .. } => "failed",
        relay_domain::AttemptConclusion::Canceled { .. } => "canceled",
        relay_domain::AttemptConclusion::Abandoned { .. } => "abandoned",
    }
}

fn evidence_target_parts(
    target: &relay_domain::EvidenceTarget,
) -> (&'static str, String, Option<i64>) {
    match target {
        relay_domain::EvidenceTarget::WorkItem(id) => ("work_item", id.as_str().to_string(), None),
        relay_domain::EvidenceTarget::ArtifactVersion(reference) => (
            "artifact_version",
            reference.artifact.as_str().to_string(),
            Some(reference.version.get() as i64),
        ),
        relay_domain::EvidenceTarget::Decision(id) => ("decision", id.as_str().to_string(), None),
    }
}

fn invalidation_target_parts(
    target: &relay_domain::InvalidationTarget,
) -> (&'static str, String, Option<i64>) {
    match target {
        relay_domain::InvalidationTarget::Work(id) => ("work", id.as_str().to_string(), None),
        relay_domain::InvalidationTarget::ArtifactVersion(reference) => (
            "artifact_version",
            reference.artifact.as_str().to_string(),
            Some(reference.version.get() as i64),
        ),
        relay_domain::InvalidationTarget::Evidence(id) => {
            ("evidence", id.as_str().to_string(), None)
        }
    }
}

// ---------------------------------------------------------------------------
// Lookup queries (search over the projection; never policy)
// ---------------------------------------------------------------------------

/// One `graph_works` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkRow {
    /// Work item id (`work_…`).
    pub id: String,
    /// Work kind label.
    pub kind: String,
    /// Executor class label.
    pub executor: String,
    /// The bounded objective.
    pub objective: String,
    /// Canonical JSON of the completion rule.
    pub completion: String,
    /// Whether the project goal needs this work.
    pub required: bool,
    /// Canonical JSON array of read scope paths.
    pub read_scopes: String,
    /// Canonical JSON array of declared write scope paths.
    pub write_scopes: String,
    /// Canonical JSON array of effective (declared + expanded) write
    /// scopes.
    pub effective_write_scopes: String,
    /// Canonical JSON array of effective operation labels.
    pub effective_operations: String,
    /// Canonical JSON array of input refs.
    pub inputs: String,
    /// Canonical JSON array of expected outputs.
    pub expected_outputs: String,
    /// Canonical JSON of the capability ceiling.
    pub capability_ceiling: String,
    /// Replacement work item, when superseded.
    pub superseded_by: Option<String>,
    /// Cancellation reason, when canceled.
    pub canceled_reason: Option<String>,
    /// Sequence of the declaring fact.
    pub declared_sequence: u64,
    /// Sequence of the last fact that changed this node.
    pub last_changed_sequence: u64,
}

impl WorkRow {
    /// Whether the work is in a declared terminal state (superseded or
    /// canceled); completion itself is engine-derived.
    pub fn is_declared_terminal(&self) -> bool {
        self.superseded_by.is_some() || self.canceled_reason.is_some()
    }
}

/// Filters for [`works`].
#[derive(Debug, Clone, Default)]
pub struct WorkFilter {
    /// Match the kind label exactly.
    pub kind: Option<String>,
    /// Match the executor class label exactly.
    pub executor: Option<String>,
    /// When `Some`, only return work in (true) or out (false) of a declared
    /// terminal state.
    pub declared_terminal: Option<bool>,
}

/// Work rows matching `filter`, ordered by declaration sequence.
pub fn works(conn: &Connection, filter: &WorkFilter) -> Result<Vec<WorkRow>> {
    let mut statement = conn
        .prepare(
            "SELECT id, kind, executor, objective, completion, required,
                    read_scopes, write_scopes, effective_write_scopes, effective_operations,
                    inputs, expected_outputs, capability_ceiling,
                    superseded_by, canceled_reason, declared_sequence, last_changed_sequence
             FROM graph_works
             WHERE (?1 IS NULL OR kind = ?1)
               AND (?2 IS NULL OR executor = ?2)
             ORDER BY declared_sequence, id;",
        )
        .map_err(|error| StoreError::Projection {
            reason: "preparing works".to_string(),
            source: Some(error),
        })?;
    let rows = statement
        .query_map(params![filter.kind, filter.executor,], |row| {
            Ok(WorkRow {
                id: row.get(0)?,
                kind: row.get(1)?,
                executor: row.get(2)?,
                objective: row.get(3)?,
                completion: row.get(4)?,
                required: row.get::<_, i64>(5)? != 0,
                read_scopes: row.get(6)?,
                write_scopes: row.get(7)?,
                effective_write_scopes: row.get(8)?,
                effective_operations: row.get(9)?,
                inputs: row.get(10)?,
                expected_outputs: row.get(11)?,
                capability_ceiling: row.get(12)?,
                superseded_by: row.get(13)?,
                canceled_reason: row.get(14)?,
                declared_sequence: row.get::<_, i64>(15)? as u64,
                last_changed_sequence: row.get::<_, i64>(16)? as u64,
            })
        })
        .map_err(|error| StoreError::Projection {
            reason: "querying works".to_string(),
            source: Some(error),
        })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    if let Some(terminal) = filter.declared_terminal {
        out.retain(|work| work.is_declared_terminal() == terminal);
    }
    Ok(out)
}

/// One work row by id.
pub fn work(conn: &Connection, id: &WorkItemId) -> Result<Option<WorkRow>> {
    use rusqlite::OptionalExtension;
    let mut statement = conn
        .prepare(
            "SELECT id, kind, executor, objective, completion, required,
                    read_scopes, write_scopes, effective_write_scopes, effective_operations,
                    inputs, expected_outputs, capability_ceiling,
                    superseded_by, canceled_reason, declared_sequence, last_changed_sequence
             FROM graph_works WHERE id = ?1;",
        )
        .map_err(|error| StoreError::Projection {
            reason: "preparing work by id".to_string(),
            source: Some(error),
        })?;
    statement
        .query_row(params![id.as_str()], |row| {
            Ok(WorkRow {
                id: row.get(0)?,
                kind: row.get(1)?,
                executor: row.get(2)?,
                objective: row.get(3)?,
                completion: row.get(4)?,
                required: row.get::<_, i64>(5)? != 0,
                read_scopes: row.get(6)?,
                write_scopes: row.get(7)?,
                effective_write_scopes: row.get(8)?,
                effective_operations: row.get(9)?,
                inputs: row.get(10)?,
                expected_outputs: row.get(11)?,
                capability_ceiling: row.get(12)?,
                superseded_by: row.get(13)?,
                canceled_reason: row.get(14)?,
                declared_sequence: row.get::<_, i64>(15)? as u64,
                last_changed_sequence: row.get::<_, i64>(16)? as u64,
            })
        })
        .optional()
        .map_err(|error| StoreError::Projection {
            reason: "querying work by id".to_string(),
            source: Some(error),
        })
}

/// One `graph_dependencies` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DependencyRow {
    /// The dependent work item.
    pub from_id: String,
    /// The prerequisite work item.
    pub to_id: String,
    /// The typed relation label.
    pub kind: String,
    /// Sequence of the asserting fact.
    pub asserted_sequence: u64,
    /// Whether the edge was retracted.
    pub retracted: bool,
    /// Sequence of the retracting fact, when retracted.
    pub retracted_sequence: Option<u64>,
}

/// Dependency edges, optionally restricted to edges touching `of` (as
/// dependent or prerequisite). Retracted edges are excluded unless
/// `include_retracted`.
pub fn dependencies(
    conn: &Connection,
    of: Option<&WorkItemId>,
    include_retracted: bool,
) -> Result<Vec<DependencyRow>> {
    let mut statement = conn
        .prepare(
            "SELECT from_id, to_id, kind, asserted_sequence, retracted, retracted_sequence
             FROM graph_dependencies
             WHERE (?1 IS NULL OR from_id = ?1 OR to_id = ?1)
             ORDER BY asserted_sequence, from_id, to_id;",
        )
        .map_err(|error| StoreError::Projection {
            reason: "preparing dependencies".to_string(),
            source: Some(error),
        })?;
    let rows = statement
        .query_map(params![of.map(WorkItemId::as_str)], |row| {
            Ok(DependencyRow {
                from_id: row.get(0)?,
                to_id: row.get(1)?,
                kind: row.get(2)?,
                asserted_sequence: row.get::<_, i64>(3)? as u64,
                retracted: row.get::<_, i64>(4)? != 0,
                retracted_sequence: row.get::<_, Option<i64>>(5)?.map(|value| value as u64),
            })
        })
        .map_err(|error| StoreError::Projection {
            reason: "querying dependencies".to_string(),
            source: Some(error),
        })?;
    let mut out = Vec::new();
    for row in rows {
        let row = row?;
        if !include_retracted && row.retracted {
            continue;
        }
        out.push(row);
    }
    Ok(out)
}

/// Lifecycle filter for [`decisions`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionFilter {
    /// Every decision.
    All,
    /// Unresolved and not superseded (these are the ones awaiting a human
    /// or policy — the *list* is a projection; what unblocks is not).
    Open,
    /// Resolved with a choice.
    Resolved,
    /// Superseded by a reconsideration.
    Superseded,
}

/// One `graph_decisions` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionRow {
    /// Decision id (`dec_…`).
    pub id: String,
    /// Canonical JSON of the subject.
    pub subject: String,
    /// The question.
    pub question: String,
    /// Canonical JSON array of options.
    pub options: String,
    /// Recommended option label.
    pub recommendation: Option<String>,
    /// Canonical JSON array of supporting artifact version refs.
    pub evidence: String,
    /// What hinges on the decision.
    pub consequences: String,
    /// Authority label (`human` / `system`).
    pub authority: String,
    /// Caller-supplied open timestamp (ms).
    pub opened_at_ms: i64,
    /// Canonical JSON of the resolution, when resolved.
    pub resolution: Option<String>,
    /// Reconsidering decision, when superseded.
    pub superseded_by: Option<String>,
    /// Sequence of the opening fact.
    pub opened_sequence: u64,
    /// Sequence of the last changing fact.
    pub last_changed_sequence: u64,
}

/// Decision rows matching `filter`, ordered by open sequence.
pub fn decisions(conn: &Connection, filter: DecisionFilter) -> Result<Vec<DecisionRow>> {
    let predicate = match filter {
        DecisionFilter::All => "1 = 1".to_string(),
        DecisionFilter::Open => "resolution IS NULL AND superseded_by IS NULL".to_string(),
        DecisionFilter::Resolved => "resolution IS NOT NULL".to_string(),
        DecisionFilter::Superseded => "superseded_by IS NOT NULL".to_string(),
    };
    let mut statement = conn
        .prepare(&format!(
            "SELECT id, subject, question, options, recommendation, evidence, consequences,
                    authority, opened_at_ms, opened_sequence, resolution, superseded_by,
                    last_changed_sequence
             FROM graph_decisions
             WHERE {predicate}
             ORDER BY opened_sequence, id;",
        ))
        .map_err(|error| StoreError::Projection {
            reason: "preparing decisions".to_string(),
            source: Some(error),
        })?;
    let rows = statement
        .query_map([], |row| {
            Ok(DecisionRow {
                id: row.get(0)?,
                subject: row.get(1)?,
                question: row.get(2)?,
                options: row.get(3)?,
                recommendation: row.get(4)?,
                evidence: row.get(5)?,
                consequences: row.get(6)?,
                authority: row.get(7)?,
                opened_at_ms: row.get(8)?,
                opened_sequence: row.get::<_, i64>(9)? as u64,
                resolution: row.get(10)?,
                superseded_by: row.get(11)?,
                last_changed_sequence: row.get::<_, i64>(12)? as u64,
            })
        })
        .map_err(|error| StoreError::Projection {
            reason: "querying decisions".to_string(),
            source: Some(error),
        })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// One `graph_artifacts` version row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactVersionRow {
    /// Artifact id (`art_…`).
    pub artifact: String,
    /// Version number.
    pub version: u32,
    /// Human-readable title.
    pub title: Option<String>,
    /// Canonical JSON of the structured content.
    pub content: String,
    /// The digest commitment.
    pub digest: String,
    /// Caller-supplied publication timestamp (ms).
    pub published_at_ms: i64,
    /// Sequence of the publishing fact.
    pub published_sequence: u64,
    /// The exact input versions this content was derived from, as
    /// `artifact@version` strings.
    pub derived_from: Vec<String>,
}

/// Artifact version rows, newest first within each artifact; optionally
/// restricted to one artifact.
pub fn artifact_versions(
    conn: &Connection,
    artifact: Option<&ArtifactId>,
) -> Result<Vec<ArtifactVersionRow>> {
    let mut statement = conn
        .prepare(
            "SELECT artifact, version, title, content, digest, published_at_ms,
                    published_sequence
             FROM graph_artifacts
             WHERE (?1 IS NULL OR artifact = ?1)
             ORDER BY artifact, version DESC;",
        )
        .map_err(|error| StoreError::Projection {
            reason: "preparing artifact_versions".to_string(),
            source: Some(error),
        })?;
    let rows = statement
        .query_map(params![artifact.map(ArtifactId::as_str)], |row| {
            Ok((
                ArtifactVersionRow {
                    artifact: row.get(0)?,
                    version: row.get::<_, i64>(1)? as u32,
                    title: row.get(2)?,
                    content: row.get(3)?,
                    digest: row.get(4)?,
                    published_at_ms: row.get(5)?,
                    published_sequence: row.get::<_, i64>(6)? as u64,
                    derived_from: Vec::new(),
                },
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
            ))
        })
        .map_err(|error| StoreError::Projection {
            reason: "querying artifact_versions".to_string(),
            source: Some(error),
        })?;
    let mut out: Vec<ArtifactVersionRow> = Vec::new();
    {
        let mut inputs = conn
            .prepare(
                "SELECT input_artifact, input_version FROM graph_artifact_inputs
                 WHERE artifact = ?1 AND version = ?2 ORDER BY input_artifact, input_version;",
            )
            .map_err(|error| StoreError::Projection {
                reason: "preparing artifact inputs".to_string(),
                source: Some(error),
            })?;
        for row in rows {
            let (mut version_row, artifact, version) = row?;
            let references = inputs
                .query_map(params![artifact, version], |row| {
                    Ok(format!(
                        "{}@{}",
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?
                    ))
                })
                .map_err(|error| StoreError::Projection {
                    reason: "querying artifact inputs".to_string(),
                    source: Some(error),
                })?;
            for reference in references {
                version_row.derived_from.push(reference?);
            }
            out.push(version_row);
        }
    }
    Ok(out)
}

/// One `graph_evidence` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceRow {
    /// Evidence id (`ev_…`).
    pub id: String,
    /// Kind label.
    pub kind: String,
    /// Target kind label (`work_item`, `artifact_version`, `decision`).
    pub target_kind: String,
    /// Target id.
    pub target_id: String,
    /// Target version, for artifact-version targets.
    pub target_version: Option<u32>,
    /// Outcome label (`pass`, `fail`, `info`).
    pub outcome: String,
    /// One-line summary.
    pub summary: String,
    /// Canonical JSON observation payload.
    pub payload: String,
    /// Caller-supplied record timestamp (ms).
    pub recorded_at_ms: i64,
    /// Sequence of the recording fact.
    pub recorded_sequence: u64,
}

/// Evidence rows, optionally restricted to one target (kind, id, optional
/// version), ordered by record sequence.
pub fn evidence(
    conn: &Connection,
    target_kind: Option<&str>,
    target_id: Option<&str>,
    target_version: Option<u32>,
) -> Result<Vec<EvidenceRow>> {
    let mut statement = conn
        .prepare(
            "SELECT id, kind, target_kind, target_id, target_version, outcome, summary,
                    payload, recorded_at_ms, recorded_sequence
             FROM graph_evidence
             WHERE (?1 IS NULL OR target_kind = ?1)
               AND (?2 IS NULL OR target_id = ?2)
               AND (?3 IS NULL OR target_version = ?3)
             ORDER BY recorded_sequence, id;",
        )
        .map_err(|error| StoreError::Projection {
            reason: "preparing evidence".to_string(),
            source: Some(error),
        })?;
    let rows = statement
        .query_map(
            params![
                target_kind,
                target_id,
                target_version.map(|version| version as i64),
            ],
            |row| {
                Ok(EvidenceRow {
                    id: row.get(0)?,
                    kind: row.get(1)?,
                    target_kind: row.get(2)?,
                    target_id: row.get(3)?,
                    target_version: row.get::<_, Option<i64>>(4)?.map(|value| value as u32),
                    outcome: row.get(5)?,
                    summary: row.get(6)?,
                    payload: row.get(7)?,
                    recorded_at_ms: row.get(8)?,
                    recorded_sequence: row.get::<_, i64>(9)? as u64,
                })
            },
        )
        .map_err(|error| StoreError::Projection {
            reason: "querying evidence".to_string(),
            source: Some(error),
        })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// One `graph_attempts` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptRow {
    /// Attempt id (`att_…`).
    pub id: String,
    /// The work item being attempted.
    pub work: String,
    /// The project revision the attempt pinned.
    pub expected_revision: u64,
    /// Grant id.
    pub grant_id: String,
    /// Canonical JSON of the full grant.
    pub grant: String,
    /// Canonical JSON array of grant read paths.
    pub grant_read_paths: String,
    /// Canonical JSON array of grant write paths.
    pub grant_write_paths: String,
    /// Canonical JSON array of grant operation labels.
    pub grant_operations: String,
    /// Grant delivery authority label.
    pub grant_delivery: String,
    /// Caller-supplied start timestamp (ms).
    pub started_at_ms: i64,
    /// Sequence of the starting fact.
    pub started_sequence: u64,
    /// Conclusion label (`accepted`, `failed`, `canceled`, `abandoned`).
    pub conclusion_status: Option<String>,
    /// Canonical JSON of the conclusion.
    pub conclusion: Option<String>,
    /// Sequence of the concluding fact.
    pub concluded_sequence: Option<u64>,
}

impl AttemptRow {
    /// Whether the attempt started and has not concluded (the
    /// reconciliation-relevant set).
    pub fn is_active(&self) -> bool {
        self.conclusion_status.is_none()
    }
}

/// Attempt rows, optionally restricted to one work item and/or active vs
/// concluded, ordered by start sequence.
pub fn attempts(
    conn: &Connection,
    work: Option<&WorkItemId>,
    active: Option<bool>,
) -> Result<Vec<AttemptRow>> {
    let mut statement = conn
        .prepare(
            "SELECT id, work, expected_revision, grant_id, grant, grant_read_paths,
                    grant_write_paths, grant_operations, grant_delivery, started_at_ms,
                    started_sequence, conclusion_status, conclusion, concluded_sequence
             FROM graph_attempts
             WHERE (?1 IS NULL OR work = ?1)
             ORDER BY started_sequence, id;",
        )
        .map_err(|error| StoreError::Projection {
            reason: "preparing attempts".to_string(),
            source: Some(error),
        })?;
    let rows = statement
        .query_map(params![work.map(WorkItemId::as_str)], |row| {
            Ok(AttemptRow {
                id: row.get(0)?,
                work: row.get(1)?,
                expected_revision: row.get::<_, i64>(2)? as u64,
                grant_id: row.get(3)?,
                grant: row.get(4)?,
                grant_read_paths: row.get(5)?,
                grant_write_paths: row.get(6)?,
                grant_operations: row.get(7)?,
                grant_delivery: row.get(8)?,
                started_at_ms: row.get(9)?,
                started_sequence: row.get::<_, i64>(10)? as u64,
                conclusion_status: row.get(11)?,
                conclusion: row.get(12)?,
                concluded_sequence: row.get::<_, Option<i64>>(13)?.map(|value| value as u64),
            })
        })
        .map_err(|error| StoreError::Projection {
            reason: "querying attempts".to_string(),
            source: Some(error),
        })?;
    let mut out = Vec::new();
    for row in rows {
        let row = row?;
        if let Some(active) = active
            && row.is_active() != active
        {
            continue;
        }
        out.push(row);
    }
    Ok(out)
}

/// One `graph_invalidations` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidationRow {
    /// Target kind label (`work`, `artifact_version`, `evidence`).
    pub target_kind: String,
    /// Target id.
    pub target_id: String,
    /// Target version, for artifact-version targets.
    pub target_version: Option<u32>,
    /// Canonical JSON of the cause.
    pub cause: String,
    /// The changed input artifact, when the invalidation was input-driven.
    pub changed_input_artifact: Option<String>,
    /// The changed input version.
    pub changed_input_version: Option<u32>,
    /// Sequence of the declaring fact.
    pub declared_sequence: u64,
}

/// Declared invalidations, optionally restricted to one target, ordered by
/// declaring sequence.
pub fn invalidations(
    conn: &Connection,
    target: Option<(&str, &str, Option<u32>)>,
) -> Result<Vec<InvalidationRow>> {
    let mut statement = conn
        .prepare(
            "SELECT target_kind, target_id, target_version, cause, changed_input_artifact,
                    changed_input_version, declared_sequence
             FROM graph_invalidations
             WHERE (?1 IS NULL OR target_kind = ?1)
               AND (?2 IS NULL OR target_id = ?2)
               AND (?3 IS NULL OR target_version = ?3)
             ORDER BY declared_sequence;",
        )
        .map_err(|error| StoreError::Projection {
            reason: "preparing invalidations".to_string(),
            source: Some(error),
        })?;
    let rows = statement
        .query_map(
            params![
                target.map(|(kind, _, _)| kind),
                target.map(|(_, id, _)| id),
                target.and_then(|(_, _, version)| version).map(|v| v as i64),
            ],
            |row| {
                Ok(InvalidationRow {
                    target_kind: row.get(0)?,
                    target_id: row.get(1)?,
                    target_version: row.get::<_, Option<i64>>(2)?.map(|value| value as u32),
                    cause: row.get(3)?,
                    changed_input_artifact: row.get(4)?,
                    changed_input_version: row.get::<_, Option<i64>>(5)?.map(|value| value as u32),
                    declared_sequence: row.get::<_, i64>(6)? as u64,
                })
            },
        )
        .map_err(|error| StoreError::Projection {
            reason: "querying invalidations".to_string(),
            source: Some(error),
        })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// The one-row graph summary.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GraphMeta {
    /// Project id, once `project_created` was folded.
    pub project: Option<String>,
    /// Canonical JSON of the goal, once declared.
    pub goal: Option<String>,
    /// The folded revision (the fact count at the last applied record).
    pub revision: u64,
}

/// The graph summary row.
pub fn graph_meta(conn: &Connection) -> Result<GraphMeta> {
    use rusqlite::OptionalExtension;
    conn.query_row(
        "SELECT project, goal, revision FROM graph_meta WHERE id = 1;",
        [],
        |row| {
            Ok(GraphMeta {
                project: row.get(0)?,
                goal: row.get(1)?,
                revision: row.get::<_, i64>(2)? as u64,
            })
        },
    )
    .optional()
    .map_err(|error| StoreError::Projection {
        reason: "reading graph meta".to_string(),
        source: Some(error),
    })
    .map(|meta| meta.unwrap_or_default())
}

/// One attempt row by id (helper for reconciliation views).
pub fn attempt_id(conn: &Connection, id: &AttemptId) -> Result<Option<AttemptRow>> {
    let all = attempts(conn, None, None)?;
    Ok(all.into_iter().find(|attempt| attempt.id == id.as_str()))
}

/// One decision row by id (helper for inspect views).
pub fn decision_by_id(conn: &Connection, id: &DecisionId) -> Result<Option<DecisionRow>> {
    let all = decisions(conn, DecisionFilter::All)?;
    Ok(all.into_iter().find(|decision| decision.id == id.as_str()))
}
