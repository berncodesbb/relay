//! Fact-layer replay, crash-recovery, and projection-rebuild suite
//! (storage/migration owner; `docs/plans/fact-format-proposal.md`).
//!
//! Covers the four durability contracts of the generic fact layer:
//!
//! 1. the JSONL fact files alone reconstruct the replayed record stream —
//!    no projection, no lock, nothing but the bytes on disk;
//! 2. a fact whose append was reported successful survives crash injection
//!    at every append step, and crash injection at every step converges on
//!    reopen (facts durable at the crash point are kept, earlier ones are
//!    not silently resurrected);
//! 3. an incomplete crash tail reconciles: quarantined, truncated, and the
//!    store stays usable;
//! 4. deleting the projection database and rebuilding yields equivalent
//!    domain state, purely from the fact log.
//!
//! The second half exercises the same contracts through the **domain**
//! seam (`GraphFactStore` + `DomainFactValidator` + `GraphFold`): a real
//! `relay-domain` fact stream round-trips, its graph projection rebuilds to
//! identical rows, revision-tracked appends refuse stale expectations, and
//! crashes never lose an acknowledged domain fact.

use std::path::Path;
use std::sync::Arc;

use relay_domain::ArtifactContent;
use relay_domain::ArtifactDigest;
use relay_domain::ArtifactId;
use relay_domain::ArtifactVersion;
use relay_domain::ArtifactVersionRef;
use relay_domain::AttemptConclusion;
use relay_domain::AttemptId;
use relay_domain::AttemptOutcome;
use relay_domain::AttemptReceipt;
use relay_domain::CapabilityGrant;
use relay_domain::ConversationId;
use relay_domain::Decision;
use relay_domain::DecisionId;
use relay_domain::DecisionOption;
use relay_domain::DecisionResolution;
use relay_domain::DecisionSubject;
use relay_domain::DependencyKind;
use relay_domain::DomainFact;
use relay_domain::Evidence;
use relay_domain::EvidenceId;
use relay_domain::EvidenceKind;
use relay_domain::EvidenceOutcome;
use relay_domain::EvidenceTarget;
use relay_domain::Goal;
use relay_domain::InvalidationCause;
use relay_domain::InvalidationTarget;
use relay_domain::Operation;
use relay_domain::ProjectId;
use relay_domain::ProjectRevision;
use relay_domain::Provenance;
use relay_domain::ProvenanceSource;
use relay_domain::ScopePath;
use relay_domain::Speaker;
use relay_domain::Usage;
use relay_domain::WorkItemId;
use relay_domain::WorkKind;
use relay_domain::work::CapabilityCeiling;
use relay_domain::work::CompletionRule;
use relay_domain::work::ExecutorClass;
use relay_domain::work::WorkItem;
use relay_store::AcceptAllFacts;
use relay_store::CrashAt;
use relay_store::FactFold;
use relay_store::FactInput;
use relay_store::FactRecord;
use relay_store::FactStore;
use relay_store::FactStoreOptions;
use relay_store::FaultPoint;
use relay_store::Integrity;
use relay_store::Json;
use relay_store::NoFold;
use relay_store::StoreError;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const STREAM: &str = "prj_00000000-0000-4000-8000-000000000001";
const CLOCK_MS: i64 = 1_791_878_400_000;

fn options() -> FactStoreOptions {
    FactStoreOptions::deterministic(CLOCK_MS)
}

fn options_with(fault: Arc<dyn relay_store::FaultInjector>) -> FactStoreOptions {
    let mut options = options();
    options.fault = fault;
    options
}

fn goal_input(n: u64) -> FactInput {
    FactInput::new(
        "goal.declared",
        1,
        Json::object([
            ("goal", Json::string(format!("Ship the gate {n}"))),
            ("n", Json::UInt(n)),
            ("confidence", Json::Float(0.85)),
            (
                "scope",
                Json::Array(vec![Json::string("src/cli"), Json::string("src/core")]),
            ),
            ("notes", Json::string("héllo → 🚀")),
        ]),
    )
}

fn work_input(n: u64) -> FactInput {
    FactInput::new(
        "work.completed",
        2,
        Json::object([("n", Json::UInt(n)), ("evidence", Json::Bool(true))]),
    )
}

/// A toy domain fold: per-type counts plus a `goal_rows` table. This is the
/// plug-in point a real domain projection uses; storage knows nothing about
/// it beyond the callback.
struct GoalFold;

impl FactFold for GoalFold {
    fn prepare(&self, tx: &rusqlite::Transaction<'_>) -> relay_store::Result<()> {
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS type_counts (
              type TEXT PRIMARY KEY,
              count INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS goal_rows (
              sequence INTEGER PRIMARY KEY,
              goal TEXT NOT NULL,
              confidence REAL NOT NULL
            );",
        )?;
        Ok(())
    }

    fn apply(
        &self,
        tx: &rusqlite::Transaction<'_>,
        record: &FactRecord,
    ) -> relay_store::Result<()> {
        tx.execute(
            "INSERT INTO type_counts (type, count) VALUES (?, 1)
             ON CONFLICT(type) DO UPDATE SET count = count + 1;",
            [record.fact_type.as_str()],
        )?;
        if record.fact_type == "goal.declared"
            && let Some(goal) = record.payload.get("goal").and_then(Json::as_str)
        {
            let confidence = record
                .payload
                .get("confidence")
                .and_then(Json::as_f64)
                .unwrap_or_default();
            tx.execute(
                "INSERT OR REPLACE INTO goal_rows (sequence, goal, confidence) VALUES (?, ?, ?);",
                rusqlite::params![record.sequence as i64, goal, confidence],
            )?;
        }
        Ok(())
    }
}

#[test]
fn jsonl_facts_alone_reconstruct_the_record_stream() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let workspace = dir.path();
    let store = FactStore::create(workspace, STREAM, Integrity::Checksums, options())?;
    let outcome = store.append_facts(
        &[goal_input(1), work_input(2), goal_input(3)],
        &AcceptAllFacts,
        &GoalFold,
    )?;
    let appended: Vec<FactRecord> = outcome.records.clone();
    assert_eq!(appended.len(), 3);
    drop(store);

    // Nothing but the files on disk: reopen read-only (no lock taken, no
    // projection opened) and replay.
    let reader = FactStore::open_read_only(workspace, options(), &AcceptAllFacts)?;
    let entries = reader.read_all(&AcceptAllFacts)?;
    assert_eq!(entries.len(), 3);
    for (index, entry) in entries.iter().enumerate() {
        assert_eq!(
            entry.record, appended[index],
            "record {index} must replay identically"
        );
        assert_eq!(entry.record.sequence, (index as u64) + 1);
    }
    // Sequence, type, version, payload, timestamps, and checksums all
    // survived; checksum-only records carry no chain links.
    assert_eq!(entries[0].record.fact_type, "goal.declared");
    assert_eq!(entries[0].record.fact_version, 1);
    assert_eq!(entries[1].record.fact_type, "work.completed");
    assert_eq!(entries[1].record.fact_version, 2);
    assert!(entries.iter().all(|entry| entry.record.previous.is_none()));
    assert_eq!(reader.tail()?.last_sequence, 3);
    assert_eq!(
        reader.tail()?.last_checksum.as_deref(),
        appended.last().map(|r| r.checksum.as_str())
    );

    // The physical bytes are canonical: sorted keys, one record per line.
    let segment = workspace
        .join(".relay-graph")
        .join("facts")
        .join("0000000000000001-open.jsonl");
    let text = std::fs::read_to_string(segment)?;
    assert!(text.ends_with('\n'));
    for (line, entry) in text.lines().zip(entries.iter()) {
        assert!(
            line.starts_with("{\"checksum\":"),
            "keys must be sorted: {line}"
        );
        let parsed = relay_store::parse_json(line)?;
        assert_eq!(
            parsed.get("checksum").and_then(Json::as_str),
            Some(entry.record.checksum.as_str())
        );
        assert_eq!(
            parsed.get("recordedAt").and_then(Json::as_str),
            Some(entry.record.recorded_at.as_str())
        );
    }
    Ok(())
}

#[test]
fn reported_success_survives_crash_injection_at_every_append_step() -> Result<()> {
    let points = [
        FaultPoint::LockAcquired,
        FaultPoint::BatchValidated,
        FaultPoint::BeforeWrite,
        FaultPoint::AfterPartialWrite,
        FaultPoint::AfterWriteNoSync,
        FaultPoint::AfterDurableAppend,
        // AfterRotation needs a threshold-crossing batch and is covered by
        // crash_after_rotation_keeps_segments_valid below.
        FaultPoint::ProjectionChunkCommitted,
        FaultPoint::ProjectionCommitted,
    ];
    for point in points {
        let dir = tempfile::tempdir()?;
        let workspace = dir.path();

        // Three facts appended and reported successful with no faults.
        let store = FactStore::create(workspace, STREAM, Integrity::Checksums, options())?;
        store.append_facts(
            &[goal_input(1), work_input(2), goal_input(3)],
            &AcceptAllFacts,
            &GoalFold,
        )?;
        drop(store);

        // A fourth append dies at `point`.
        let (store, _) = FactStore::open(
            workspace,
            options_with(Arc::new(CrashAt::new(point))),
            &AcceptAllFacts,
            &GoalFold,
        )?;
        let outcome = store.append_facts(&[goal_input(4)], &AcceptAllFacts, &GoalFold);
        assert!(
            matches!(outcome, Err(StoreError::SimulatedCrash { .. })),
            "{point:?}: expected a simulated crash, got {outcome:?}"
        );
        drop(store);

        // Reopen: the three acknowledged facts must all have survived.
        let (reopened, report) = FactStore::open(workspace, options(), &AcceptAllFacts, &GoalFold)?;
        let entries = reopened.read_all(&AcceptAllFacts)?;
        let sequences: Vec<u64> = entries.iter().map(|e| e.record.sequence).collect();
        let durable_at_point = matches!(
            point,
            FaultPoint::AfterWriteNoSync
                | FaultPoint::AfterDurableAppend
                | FaultPoint::AfterRotation
                | FaultPoint::ProjectionChunkCommitted
                | FaultPoint::ProjectionCommitted
        );
        if durable_at_point {
            assert_eq!(sequences, vec![1, 2, 3, 4], "{point:?}: the fact survived");
        } else {
            assert_eq!(
                sequences,
                vec![1, 2, 3],
                "{point:?}: nothing partial leaked"
            );
        }
        // Reopen always converges and the projection always catches up.
        assert!(report.recovered_tails.is_empty() || point == FaultPoint::AfterPartialWrite);
        assert_eq!(
            reopened.projection_record_count()?,
            Some(entries.len() as u64),
            "{point:?}: projection must match the log after reopen"
        );

        // The store remains usable after recovery from that point.
        reopened.append_facts(&[work_input(5)], &AcceptAllFacts, &GoalFold)?;
        drop(reopened);
        let (final_open, _) = FactStore::open(workspace, options(), &AcceptAllFacts, &GoalFold)?;
        let count = final_open.read_all(&AcceptAllFacts)?.len() as u64;
        assert_eq!(
            count,
            if durable_at_point { 5 } else { 4 },
            "{point:?}: post-recovery append survived"
        );
        assert_eq!(final_open.tail()?.last_sequence, count);
    }
    Ok(())
}

#[test]
fn crash_after_rotation_keeps_segments_valid() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let workspace = dir.path();
    let store = FactStore::create(workspace, STREAM, Integrity::Checksums, options())?;
    store.append_facts(&[goal_input(1)], &AcceptAllFacts, &GoalFold)?;
    drop(store);

    // 1000 facts cross the rotation threshold; the append dies right after
    // the rotation completed (closed segment + fresh active segment +
    // manifest rewrite, projection not yet folded).
    let (store, _) = FactStore::open(
        workspace,
        options_with(Arc::new(CrashAt::new(FaultPoint::AfterRotation))),
        &AcceptAllFacts,
        &GoalFold,
    )?;
    let inputs: Vec<FactInput> = (2..=1001).map(goal_input).collect();
    let outcome = store.append_facts(&inputs, &AcceptAllFacts, &GoalFold);
    assert!(matches!(outcome, Err(StoreError::SimulatedCrash { .. })));
    drop(store);

    let (reopened, _) = FactStore::open(workspace, options(), &AcceptAllFacts, &GoalFold)?;
    let entries = reopened.read_all(&AcceptAllFacts)?;
    assert_eq!(
        entries.len(),
        1001,
        "every durable fact survived the rotation crash"
    );
    assert_eq!(reopened.tail()?.last_sequence, 1001);
    assert_eq!(reopened.projection_record_count()?, Some(1001));
    // The closed segment tiles contiguously and the active segment is fresh.
    let facts_dir = workspace.join(".relay-graph").join("facts");
    assert!(
        facts_dir
            .join("0000000000000001-00000000000003e9.jsonl")
            .exists()
    );
    assert!(facts_dir.join("00000000000003ea-open.jsonl").exists());
    // The store continues appending into the fresh active segment.
    reopened.append_facts(&[work_input(1002)], &AcceptAllFacts, &GoalFold)?;
    drop(reopened);
    let (final_open, _) = FactStore::open(workspace, options(), &AcceptAllFacts, &GoalFold)?;
    assert_eq!(final_open.read_all(&AcceptAllFacts)?.len(), 1002);
    Ok(())
}

#[test]
fn incomplete_tail_reconciles() -> Result<()> {
    // Crash mid-write: half a record line is on disk.
    let dir = tempfile::tempdir()?;
    let workspace = dir.path();
    let store = FactStore::create(workspace, STREAM, Integrity::Checksums, options())?;
    store.append_facts(&[goal_input(1)], &AcceptAllFacts, &GoalFold)?;
    drop(store);

    let fault: Arc<dyn relay_store::FaultInjector> =
        Arc::new(CrashAt::new(FaultPoint::AfterPartialWrite));
    let store = {
        let (store, _) =
            FactStore::open(workspace, options_with(fault), &AcceptAllFacts, &GoalFold)?;
        store
    };
    let outcome = store.append_facts(&[goal_input(2)], &AcceptAllFacts, &GoalFold);
    assert!(matches!(outcome, Err(StoreError::SimulatedCrash { .. })));
    drop(store);

    // A reader sees the consistent prefix and ignores the partial tail.
    let reader = FactStore::open_read_only(workspace, options(), &AcceptAllFacts)?;
    assert_eq!(reader.read_all(&AcceptAllFacts)?.len(), 1);
    drop(reader);

    // The writer quarantines and truncates the tail, and keeps going.
    let (writer, report) = FactStore::open(workspace, options(), &AcceptAllFacts, &GoalFold)?;
    assert_eq!(
        report.recovered_tails.len(),
        1,
        "the crash tail must be recovered"
    );
    let recovery = report.recovered_tails.first().ok_or("missing recovery")?;
    assert!(
        recovery.quarantine_path.exists(),
        "quarantine must be retained"
    );
    assert!(recovery.bytes > 0);
    assert_eq!(writer.read_all(&AcceptAllFacts)?.len(), 1);
    writer.append_facts(&[goal_input(2), goal_input(3)], &AcceptAllFacts, &GoalFold)?;
    drop(writer);

    let (reopened, _) = FactStore::open(workspace, options(), &AcceptAllFacts, &GoalFold)?;
    let entries = reopened.read_all(&AcceptAllFacts)?;
    assert_eq!(
        entries
            .iter()
            .map(|e| e.record.sequence)
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );

    // A foreign incomplete tail (e.g. an editor or a crashed external tool)
    // reconciles the same way.
    let segment = workspace
        .join(".relay-graph")
        .join("facts")
        .join("0000000000000001-open.jsonl");
    let mut text = std::fs::read_to_string(&segment)?;
    text.push_str("{\"sequence\":4,\"typ");
    std::fs::write(&segment, text)?;
    let (recovered, report) = FactStore::open(workspace, options(), &AcceptAllFacts, &GoalFold)?;
    assert_eq!(report.recovered_tails.len(), 1);
    assert_eq!(recovered.read_all(&AcceptAllFacts)?.len(), 3);
    Ok(())
}

#[test]
fn deleting_the_projection_db_and_rebuilding_yields_equivalent_state() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let workspace = dir.path();
    let store = FactStore::create(workspace, STREAM, Integrity::Checksums, options())?;
    for n in 1..=6 {
        let input = if n % 2 == 0 {
            work_input(n)
        } else {
            goal_input(n)
        };
        store.append_facts(&[input], &AcceptAllFacts, &GoalFold)?;
    }
    let before = domain_view(workspace)?;
    assert_eq!(
        before.type_counts,
        vec![
            ("goal.declared".to_string(), 3),
            ("work.completed".to_string(), 3),
        ]
    );
    assert_eq!(before.goal_rows.len(), 3);
    drop(store);

    // Delete the projection database (and any WAL sidecars) entirely.
    let db = workspace.join(".relay-graph").join("projection.sqlite");
    for suffix in ["", "-wal", "-shm"] {
        let path = db.with_file_name(format!("projection.sqlite{suffix}"));
        if path.exists() {
            std::fs::remove_file(path)?;
        }
    }
    assert!(!db.exists());

    // Reopen: the missing database is rebuilt purely from the fact log.
    let (reopened, report) = FactStore::open(workspace, options(), &AcceptAllFacts, &GoalFold)?;
    assert!(
        matches!(
            report.reconcile,
            Some(relay_store::FactReconcileOutcome::ReplayedSuffix { from: 1, to: 6 })
        ),
        "expected a full replay, got {:?}",
        report.reconcile
    );
    let after = domain_view(workspace)?;
    assert_eq!(after.type_counts, before.type_counts);
    assert_eq!(after.goal_rows, before.goal_rows);
    assert_eq!(after.checkpoint, before.checkpoint);
    assert_eq!(reopened.projection_record_count()?, Some(6));

    // And the explicit rebuild service converges to the same view.
    reopened.rebuild_projection(&AcceptAllFacts, &GoalFold)?;
    let rebuilt = domain_view(workspace)?;
    assert_eq!(rebuilt.type_counts, before.type_counts);
    assert_eq!(rebuilt.goal_rows, before.goal_rows);
    Ok(())
}

#[derive(Debug, PartialEq)]
struct DomainView {
    type_counts: Vec<(String, i64)>,
    goal_rows: Vec<(i64, String, f64)>,
    checkpoint: Option<(i64, String)>,
}

/// Read the domain fold's tables straight from the projection file with an
/// independent connection (the projection is just a database).
fn domain_view(workspace: &Path) -> Result<DomainView> {
    let db = workspace.join(".relay-graph").join("projection.sqlite");
    let conn = rusqlite::Connection::open(&db)?;
    let mut type_counts = Vec::new();
    {
        let mut statement = conn.prepare("SELECT type, count FROM type_counts ORDER BY type;")?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        for row in rows {
            type_counts.push(row?);
        }
    }
    let mut goal_rows = Vec::new();
    {
        let mut statement =
            conn.prepare("SELECT sequence, goal, confidence FROM goal_rows ORDER BY sequence;")?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, f64>(2)?,
            ))
        })?;
        for row in rows {
            goal_rows.push(row?);
        }
    }
    let checkpoint = conn
        .query_row(
            "SELECT last_sequence, last_checksum FROM fact_checkpoint WHERE id = 1;",
            [],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    Ok(DomainView {
        type_counts,
        goal_rows,
        checkpoint,
    })
}

/// Small extension so the closure above reads naturally.
trait OptionalRow {
    fn optional(self) -> Result<Option<(i64, String)>>;
}

impl OptionalRow for rusqlite::Result<(i64, String)> {
    fn optional(self) -> Result<Option<(i64, String)>> {
        match self {
            Ok(value) => Ok(Some(value)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(Box::new(error) as Box<dyn std::error::Error>),
        }
    }
}

#[test]
fn no_fold_projection_still_rebuilds() -> Result<()> {
    // The generic primitive works with no domain fold at all.
    let dir = tempfile::tempdir()?;
    let workspace = dir.path();
    let store = FactStore::create(workspace, STREAM, Integrity::Chained, options())?;
    store.append_facts(&[goal_input(1), work_input(2)], &AcceptAllFacts, &NoFold)?;
    assert_eq!(store.integrity()?, Integrity::Chained);
    drop(store);
    let db = workspace.join(".relay-graph").join("projection.sqlite");
    std::fs::remove_file(&db)?;
    let (reopened, _) = FactStore::open(workspace, options(), &AcceptAllFacts, &NoFold)?;
    assert_eq!(reopened.projection_record_count()?, Some(2));
    // Chained records replay with their links intact.
    let entries = reopened.read_all(&AcceptAllFacts)?;
    for window in entries.windows(2) {
        assert_eq!(
            window[1].record.previous.as_deref(),
            Some(window[0].record.checksum.as_str())
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Domain seam: GraphFactStore + DomainFactValidator + GraphFold over a real
// relay-domain fact stream.
// ---------------------------------------------------------------------------

fn parse_id<T>(value: &str) -> Result<T>
where
    T: std::str::FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    Ok(value.parse::<T>()?)
}

fn domain_project() -> Result<ProjectId> {
    parse_id("prj_demo")
}

/// A domain-valid fact history exercising every projection table: goal,
/// three work items (one workspace-class with scopes), edges (asserted and
/// retracted), decisions (resolved and superseded), two artifact versions
/// with provenance, evidence, a full attempt lifecycle, an invalidation,
/// and a cancellation.
fn domain_stream(project: &ProjectId) -> Result<Vec<DomainFact>> {
    let goal = Goal::new(
        parse_id("goal_1")?,
        "Ship the release gate",
        Provenance {
            source: ProvenanceSource::User,
            statement: Some("Ship it".to_string()),
            at_ms: CLOCK_MS,
        },
        CLOCK_MS,
    );
    let discovery = parse_id::<WorkItemId>("work_discovery")?;
    let spec = parse_id::<WorkItemId>("work_spec")?;
    let implementation = parse_id::<WorkItemId>("work_implementation")?;
    let cleanup = parse_id::<WorkItemId>("work_cleanup")?;
    let mut facts = vec![
        DomainFact::ProjectCreated {
            project: project.clone(),
        },
        DomainFact::GoalDeclared { goal },
        DomainFact::WorkDeclared {
            work: WorkItem::new(
                discovery.clone(),
                WorkKind::parse("discover-opportunity")?,
                ExecutorClass::Model,
                "Understand the goal and the repository",
                CompletionRule::Receipt,
            ),
        },
        DomainFact::WorkDeclared {
            work: WorkItem::new(
                spec.clone(),
                WorkKind::parse("define-requirements")?,
                ExecutorClass::Model,
                "Specify the gate",
                CompletionRule::Receipt,
            ),
        },
        DomainFact::DependencyAsserted {
            from: spec.clone(),
            to: discovery.clone(),
            kind: DependencyKind::Requires,
        },
    ];
    // A workspace-class work item with declared write scopes inside its
    // ceiling, which is later expanded within the ceiling.
    let mut implementation_work = WorkItem::new(
        implementation.clone(),
        WorkKind::parse("implement-task")?,
        ExecutorClass::Workspace,
        "Implement the gate evaluator",
        CompletionRule::Receipt,
    );
    implementation_work.write_scopes = vec![ScopePath::parse("src")?];
    implementation_work.capability_ceiling = CapabilityCeiling {
        write_scopes: vec![ScopePath::root()],
        operations: vec![Operation::Read, Operation::Write, Operation::Execute],
        delivery: relay_domain::DeliveryAuthority::None,
    };
    facts.push(DomainFact::WorkDeclared {
        work: implementation_work,
    });
    facts.push(DomainFact::WorkDeclared {
        work: WorkItem::new(
            cleanup.clone(),
            WorkKind::parse("tidy-workspace")?,
            ExecutorClass::System,
            "Remove scratch files",
            CompletionRule::Receipt,
        ),
    });
    // An edge that is asserted and then retracted.
    facts.push(DomainFact::DependencyAsserted {
        from: implementation.clone(),
        to: cleanup.clone(),
        kind: DependencyKind::Informs,
    });
    facts.push(DomainFact::DependencyRetracted {
        from: implementation.clone(),
        to: cleanup.clone(),
        kind: DependencyKind::Informs,
        reason: "cleanup is not actually informative".to_string(),
    });
    facts.push(DomainFact::WorkScopeExpanded {
        work: implementation.clone(),
        added_write_scopes: vec![ScopePath::parse("src/core")?],
        added_operations: vec![],
        reason: "the evaluator lives in src/core".to_string(),
    });
    // Decisions: one resolved, one superseded by a reconsideration.
    let approve = parse_id::<DecisionId>("dec_approve")?;
    let reconsider = parse_id::<DecisionId>("dec_reconsider")?;
    facts.push(DomainFact::DecisionOpened {
        decision: Decision {
            id: approve.clone(),
            subject: DecisionSubject::WorkItem(discovery.clone()),
            question: "Approve the discovery brief?".to_string(),
            options: vec![
                DecisionOption {
                    label: "approve".to_string(),
                    description: "Proceed to specification".to_string(),
                },
                DecisionOption {
                    label: "reject".to_string(),
                    description: "Redo discovery".to_string(),
                },
            ],
            recommendation: Some("approve".to_string()),
            evidence: Vec::new(),
            consequences: "Specification work waits on this".to_string(),
            authority: relay_domain::Authority::Human,
            opened_at_ms: CLOCK_MS,
        },
    });
    facts.push(DomainFact::DecisionResolved {
        decision: approve.clone(),
        resolution: DecisionResolution {
            choice: "approve".to_string(),
            rationale: "The brief is grounded".to_string(),
            resolved_at_ms: CLOCK_MS,
        },
    });
    facts.push(DomainFact::DecisionOpened {
        decision: Decision {
            id: reconsider.clone(),
            subject: DecisionSubject::WorkItem(discovery.clone()),
            question: "Reconsider the approval?".to_string(),
            options: Vec::new(),
            recommendation: None,
            evidence: Vec::new(),
            consequences: "Reopens the approval".to_string(),
            authority: relay_domain::Authority::Human,
            opened_at_ms: CLOCK_MS,
        },
    });
    facts.push(DomainFact::DecisionSuperseded {
        decision: approve,
        superseded_by: reconsider,
    });
    // Two artifact versions with provenance, then evidence on the newest.
    let brief = parse_id::<ArtifactId>("art_brief")?;
    let digest_of = |tag: u8| -> Result<ArtifactDigest> {
        Ok(ArtifactDigest::parse(&format!("{tag:02x}").repeat(32))?)
    };
    let v1 = ArtifactVersionRef {
        artifact: brief.clone(),
        version: ArtifactVersion::FIRST,
    };
    let v2 = ArtifactVersionRef {
        artifact: brief.clone(),
        version: ArtifactVersion::new(2)?,
    };
    facts.push(DomainFact::ArtifactVersionPublished {
        artifact: brief.clone(),
        version: ArtifactVersion::FIRST,
        title: Some("Opportunity brief".to_string()),
        content: ArtifactContent::new(serde_json::json!({"sections": ["problem"]})),
        digest: digest_of(1)?,
        derived_from: Vec::new(),
        published_at_ms: CLOCK_MS,
    });
    facts.push(DomainFact::ArtifactVersionPublished {
        artifact: brief.clone(),
        version: ArtifactVersion::new(2)?,
        title: Some("Opportunity brief v2".to_string()),
        content: ArtifactContent::new(serde_json::json!({"sections": ["problem", "users"]})),
        digest: digest_of(2)?,
        derived_from: vec![v1.clone()],
        published_at_ms: CLOCK_MS + 1,
    });
    let evidence = parse_id::<EvidenceId>("ev_suite")?;
    facts.push(DomainFact::EvidenceRecorded {
        evidence: Evidence::new(
            evidence.clone(),
            EvidenceKind::TestRun,
            EvidenceTarget::ArtifactVersion(v2.clone()),
            EvidenceOutcome::Pass,
            "suite green: 12 passed",
            CLOCK_MS,
        ),
    });
    // Attempt lifecycle against the discovery work.
    let attempt = parse_id::<AttemptId>("att_1")?;
    let grant = CapabilityGrant::read_only(parse_id("grant_discovery")?, vec![ScopePath::root()]);
    facts.push(DomainFact::AttemptStarted {
        attempt: attempt.clone(),
        work: discovery.clone(),
        // Filled in by the caller with the live revision.
        expected_revision: ProjectRevision::default(),
        grant,
        started_at_ms: CLOCK_MS,
    });
    facts.push(DomainFact::AttemptConcluded {
        attempt: attempt.clone(),
        conclusion: AttemptConclusion::Accepted {
            receipt: AttemptReceipt {
                attempt: attempt.clone(),
                work: discovery.clone(),
                outcome: AttemptOutcome::Completed,
                completed_at_ms: CLOCK_MS + 10,
                outputs: Vec::new(),
                evidence: Vec::new(),
                graph_changes: Vec::new(),
                usage: Usage {
                    input_tokens: 100,
                    output_tokens: 50,
                    tool_calls: 2,
                    duration_ms: 10_000,
                },
            },
        },
    });
    // Invalidation of the spec work driven by the superseded input, and a
    // cancellation of the cleanup work.
    facts.push(DomainFact::InvalidationDeclared {
        target: InvalidationTarget::Work(spec.clone()),
        cause: InvalidationCause::SupersededInput {
            previous: v1,
            current: v2.clone(),
        },
        changed_input: Some(v2),
    });
    facts.push(DomainFact::WorkCanceled {
        work: cleanup,
        reason: "no longer needed".to_string(),
    });
    Ok(facts)
}

/// The full projection view, captured for delete-and-rebuild equivalence.
#[derive(Debug, Default, PartialEq)]
struct GraphView {
    works: Vec<relay_store::WorkRow>,
    dependencies: Vec<relay_store::DependencyRow>,
    decisions: Vec<relay_store::DecisionRow>,
    artifacts: Vec<relay_store::ArtifactVersionRow>,
    evidence: Vec<relay_store::EvidenceRow>,
    attempts: Vec<relay_store::AttemptRow>,
    invalidations: Vec<relay_store::InvalidationRow>,
    meta: relay_store::GraphMeta,
    checkpoint: Option<(u64, String)>,
}

fn graph_view(store: &relay_store::GraphFactStore) -> Result<GraphView> {
    Ok(store.projection(|conn| {
        Ok(GraphView {
            works: relay_store::works(
                conn,
                &relay_store::WorkFilter {
                    kind: None,
                    executor: None,
                    declared_terminal: None,
                },
            )?,
            dependencies: relay_store::dependencies(conn, None, true)?,
            decisions: relay_store::decisions(conn, relay_store::DecisionFilter::All)?,
            artifacts: relay_store::artifact_versions(conn, None)?,
            evidence: relay_store::evidence(conn, None, None, None)?,
            attempts: relay_store::attempts(conn, None, None)?,
            invalidations: relay_store::invalidations(conn, None)?,
            meta: relay_store::graph_meta(conn)?,
            checkpoint: None,
        })
    })?)
}

#[test]
fn domain_stream_round_trips_and_rebuilds_equivalently() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let workspace = dir.path();
    let project = domain_project()?;
    let store =
        relay_store::GraphFactStore::create(workspace, &project, Integrity::Checksums, options())?;

    // Append the prefix, then the attempt with its exact expected revision,
    // then the suffix — mirroring how the runtime pins revisions.
    let mut facts = domain_stream(&project)?;
    let attempt_position = facts
        .iter()
        .position(|fact| matches!(fact, DomainFact::AttemptStarted { .. }))
        .ok_or("no attempt in stream")?;
    let prefix: Vec<DomainFact> = facts.drain(..attempt_position).collect();
    let attempt_fact = facts.remove(0);
    let outcome = store.append(&prefix)?;
    let revision = outcome.revision;
    let attempt_fact = match attempt_fact {
        DomainFact::AttemptStarted {
            attempt,
            work,
            expected_revision: _,
            grant,
            started_at_ms,
        } => DomainFact::AttemptStarted {
            attempt,
            work,
            expected_revision: revision,
            grant,
            started_at_ms,
        },
        other => other,
    };
    // The attempt must be pinned at the *current* revision; the domain fold
    // rejects anything else.
    store.append(&[attempt_fact])?;
    store.append(&facts)?;

    let timeline = store.read_timeline()?;
    assert_eq!(timeline.len(), 21, "the whole history replays typed");
    assert_eq!(store.revision()?, ProjectRevision::from_count(21));
    // Every sequence is contiguous and the timeline order is append order.
    for (index, fact) in timeline.iter().enumerate() {
        assert_eq!(fact.sequence, (index as u64) + 1);
    }

    // The domain fold accepts the timeline and agrees with the revision.
    let graph = store.project_graph()?;
    assert_eq!(graph.revision(), ProjectRevision::from_count(21));
    assert_eq!(graph.works().count(), 4);
    assert_eq!(graph.artifacts().count(), 1);
    assert_eq!(graph.evidence().count(), 1);
    assert_eq!(graph.attempts().count(), 1);

    // Capture the projection view, then delete the database entirely.
    let mut before = graph_view(&store)?;
    before.checkpoint = store
        .projection_checkpoint()?
        .map(|checkpoint| (checkpoint.last_sequence, checkpoint.last_checksum));
    assert_eq!(before.works.len(), 4);
    assert_eq!(before.artifacts.len(), 2);
    assert_eq!(before.attempts.len(), 1);
    assert_eq!(before.invalidations.len(), 1);
    assert_eq!(before.meta.revision, 21);
    drop(store);

    let db = workspace.join(".relay-graph").join("projection.sqlite");
    for suffix in ["", "-wal", "-shm"] {
        let path = db.with_file_name(format!("projection.sqlite{suffix}"));
        if path.exists() {
            std::fs::remove_file(path)?;
        }
    }
    assert!(!db.exists());

    // Reopen: the missing database is rebuilt purely from the fact log and
    // the rebuilt rows are identical, including the fold's sequence stamps.
    let (reopened, report) = relay_store::GraphFactStore::open(workspace, options())?;
    assert!(
        matches!(
            report.reconcile,
            Some(relay_store::FactReconcileOutcome::ReplayedSuffix { from: 1, to: 21 })
        ),
        "expected a full replay, got {:?}",
        report.reconcile
    );
    let mut after = graph_view(&reopened)?;
    after.checkpoint = reopened
        .projection_checkpoint()?
        .map(|checkpoint| (checkpoint.last_sequence, checkpoint.last_checksum));
    assert_eq!(after.works, before.works);
    assert_eq!(after.dependencies, before.dependencies);
    assert_eq!(after.decisions, before.decisions);
    assert_eq!(after.artifacts, before.artifacts);
    assert_eq!(after.evidence, before.evidence);
    assert_eq!(after.attempts, before.attempts);
    assert_eq!(after.invalidations, before.invalidations);
    assert_eq!(after.meta, before.meta);
    assert_eq!(after.checkpoint, before.checkpoint);

    // The rebuilt graph still folds identically.
    assert_eq!(reopened.project_graph()?, graph);
    Ok(())
}

#[test]
fn domain_projection_rows_are_shape_correct() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let workspace = dir.path();
    let project = domain_project()?;
    let store =
        relay_store::GraphFactStore::create(workspace, &project, Integrity::Checksums, options())?;
    let mut facts = domain_stream(&project)?;
    let attempt_position = facts
        .iter()
        .position(|fact| matches!(fact, DomainFact::AttemptStarted { .. }))
        .ok_or("no attempt in stream")?;
    let prefix: Vec<DomainFact> = facts.drain(..attempt_position).collect();
    let revision = store.append(&prefix)?.revision;
    let mut attempt_fact = facts.remove(0);
    if let DomainFact::AttemptStarted {
        expected_revision, ..
    } = &mut attempt_fact
    {
        *expected_revision = revision;
    }
    store.append(&[attempt_fact])?;
    store.append(&facts)?;

    let view = graph_view(&store)?;

    // Effective scopes fold declared + expanded (src + src/core).
    let implementation = view
        .works
        .iter()
        .find(|work| work.id == "work_implementation")
        .ok_or("missing implementation row")?;
    assert_eq!(implementation.executor, "workspace");
    assert_eq!(implementation.write_scopes, r#"["src"]"#);
    assert_eq!(
        implementation.effective_write_scopes, r#"["src","src/core"]"#,
        "expansion must fold into the effective scopes"
    );
    // Fact 10 is the scope expansion that last changed this node.
    assert_eq!(implementation.last_changed_sequence, 10);

    // The retracted edge is present but marked retracted; the requires edge
    // is live.
    let retracted = view
        .dependencies
        .iter()
        .find(|edge| edge.kind == "informs")
        .ok_or("missing informs edge")?;
    assert!(retracted.retracted);
    assert!(retracted.retracted_sequence.is_some());
    let requires = view
        .dependencies
        .iter()
        .find(|edge| edge.kind == "requires")
        .ok_or("missing requires edge")?;
    assert!(!requires.retracted);

    // Decisions: one resolved-then-superseded, one open.
    let superseded = view
        .decisions
        .iter()
        .find(|decision| decision.id == "dec_approve")
        .ok_or("missing decision")?;
    assert!(superseded.resolution.is_some());
    assert_eq!(superseded.superseded_by.as_deref(), Some("dec_reconsider"));
    let (store_for_open, _) = relay_store::GraphFactStore::open(workspace, options())?;
    let open = store_for_open
        .projection(|conn| relay_store::decisions(conn, relay_store::DecisionFilter::Open))?;
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].id, "dec_reconsider");

    // Artifact provenance: v2 derives from v1.
    let v2 = view
        .artifacts
        .iter()
        .find(|version| version.version == 2)
        .ok_or("missing v2")?;
    assert_eq!(v2.derived_from, vec!["art_brief@1".to_string()]);
    assert_eq!(v2.digest, "02".repeat(32));

    // Evidence targets the exact version with a pass outcome.
    assert_eq!(view.evidence[0].target_kind, "artifact_version");
    assert_eq!(view.evidence[0].target_id, "art_brief");
    assert_eq!(view.evidence[0].target_version, Some(2));
    assert_eq!(view.evidence[0].outcome, "pass");

    // The attempt carries its grant summary and accepted conclusion.
    let attempt = &view.attempts[0];
    assert_eq!(attempt.work, "work_discovery");
    assert_eq!(attempt.expected_revision, revision.get());
    assert_eq!(attempt.grant_id, "grant_discovery");
    assert_eq!(attempt.conclusion_status.as_deref(), Some("accepted"));

    // The invalidation names the superseded input.
    let invalidation = &view.invalidations[0];
    assert_eq!(invalidation.target_kind, "work");
    assert_eq!(invalidation.target_id, "work_spec");
    assert_eq!(
        invalidation.changed_input_artifact.as_deref(),
        Some("art_brief")
    );
    assert_eq!(invalidation.changed_input_version, Some(2));

    // The canceled cleanup work is terminally flagged; nothing stores
    // readiness.
    let cleanup = view
        .works
        .iter()
        .find(|work| work.id == "work_cleanup")
        .ok_or("missing cleanup row")?;
    assert!(cleanup.is_declared_terminal());
    assert_eq!(cleanup.canceled_reason.as_deref(), Some("no longer needed"));
    Ok(())
}

#[test]
fn domain_append_with_expected_revision_and_crash_durability() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let workspace = dir.path();
    let project = domain_project()?;
    let store =
        relay_store::GraphFactStore::create(workspace, &project, Integrity::Checksums, options())?;
    store.append(&[
        DomainFact::ProjectCreated {
            project: project.clone(),
        },
        DomainFact::GoalDeclared {
            goal: Goal::new(
                parse_id("goal_1")?,
                "Ship it",
                Provenance {
                    source: ProvenanceSource::User,
                    statement: None,
                    at_ms: CLOCK_MS,
                },
                CLOCK_MS,
            ),
        },
    ])?;
    let revision = store.revision()?;

    // A stale expected revision refuses.
    let error = store
        .append_at(
            Some(ProjectRevision::from_count(1)),
            &[DomainFact::WorkDeclared {
                work: WorkItem::new(
                    parse_id("work_x")?,
                    WorkKind::parse("discover-opportunity")?,
                    ExecutorClass::Model,
                    "x",
                    CompletionRule::Receipt,
                ),
            }],
        )
        .err()
        .map(|e| e.to_string())
        .unwrap_or_default();
    assert!(error.contains("stale expected revision"), "{error}");

    // The correct expected revision succeeds and is acknowledged.
    let outcome = store.append_at(
        Some(revision),
        &[DomainFact::WorkDeclared {
            work: WorkItem::new(
                parse_id("work_x")?,
                WorkKind::parse("discover-opportunity")?,
                ExecutorClass::Model,
                "x",
                CompletionRule::Receipt,
            ),
        }],
    )?;
    assert_eq!(outcome.revision, ProjectRevision::from_count(3));
    drop(store);

    // A crash after the durable append of a fourth fact: it survives.
    let (store, _) = relay_store::GraphFactStore::open(
        workspace,
        options_with(Arc::new(CrashAt::new(FaultPoint::AfterDurableAppend))),
    )?;
    let crashed = store.append(&[DomainFact::WorkCanceled {
        work: parse_id("work_x")?,
        reason: "done elsewhere".to_string(),
    }]);
    assert!(matches!(crashed, Err(StoreError::SimulatedCrash { .. })));
    drop(store);

    let (reopened, _) = relay_store::GraphFactStore::open(workspace, options())?;
    let timeline = reopened.read_timeline()?;
    assert_eq!(
        timeline.len(),
        4,
        "the acknowledged facts survived the crash"
    );
    assert_eq!(timeline.last().ok_or("empty")?.fact.kind(), "work_canceled");
    assert_eq!(reopened.revision()?, ProjectRevision::from_count(4));
    // The projection caught up and the graph folds.
    assert_eq!(
        reopened.projection_checkpoint()?.map(|c| c.last_sequence),
        Some(4)
    );
    assert_eq!(reopened.project_graph()?.works().count(), 1);
    Ok(())
}

#[test]
fn domain_doctor_reports_healthy_store() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let workspace = dir.path();
    let project = domain_project()?;
    let store =
        relay_store::GraphFactStore::create(workspace, &project, Integrity::Checksums, options())?;
    store.append(&[
        DomainFact::ProjectCreated {
            project: project.clone(),
        },
        DomainFact::WorkDeclared {
            work: WorkItem::new(
                parse_id("work_discovery")?,
                WorkKind::parse("discover-opportunity")?,
                ExecutorClass::Model,
                "Understand the goal",
                CompletionRule::Receipt,
            ),
        },
    ])?;
    drop(store);

    let findings = relay_store::fact::doctor::check(workspace, &options());
    assert!(
        findings.iter().any(|f| f.code == "manifest-ok"),
        "{findings:?}"
    );
    assert!(
        findings.iter().any(|f| f.code == "facts-ok"),
        "{findings:?}"
    );
    assert!(
        findings.iter().any(|f| f.code == "projection-ok"),
        "{findings:?}"
    );
    // No artifact versions were published, so no blob findings either way.
    assert!(
        !findings.iter().any(|f| f.code.starts_with("artifact-blob")),
        "{findings:?}"
    );

    // Tampering with the log is detected read-only.
    let segment = workspace
        .join(".relay-graph")
        .join("facts")
        .join("0000000000000001-open.jsonl");
    let text = std::fs::read_to_string(&segment)?;
    std::fs::write(
        &segment,
        text.replace("discover-opportunity", "discover-opportunitY"),
    )?;
    let findings = relay_store::fact::doctor::check(workspace, &options());
    assert!(
        findings.iter().any(|f| f.code == "facts-invalid"),
        "{findings:?}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Conversation facts: round-trip, rebuild equivalence, fail-closed decoding.
// ---------------------------------------------------------------------------

/// A domain-valid conversation history exercising all five conversation
/// fact kinds across two conversations: a completed turn (with related
/// annotations naming ids that do not exist in the graph), a failed turn,
/// an interrupted turn, and a cancelled turn in an older conversation.
/// No goal and no work: conversations are legal without either.
fn conversation_stream() -> Result<Vec<DomainFact>> {
    let archive: ConversationId = parse_id("cnv_archive")?;
    let chat: ConversationId = parse_id("cnv_chat")?;
    Ok(vec![
        DomainFact::ProjectCreated {
            project: domain_project()?,
        },
        DomainFact::ConversationStarted {
            conversation: archive.clone(),
            started_at_ms: CLOCK_MS,
        },
        DomainFact::ConversationStarted {
            conversation: chat.clone(),
            started_at_ms: CLOCK_MS + 1,
        },
        DomainFact::UserMessageRecorded {
            conversation: chat.clone(),
            turn: parse_id("turn_1")?,
            message: parse_id("msg_u1")?,
            text: "Explain this repository".to_string(),
            at_ms: CLOCK_MS + 2,
        },
        DomainFact::AssistantMessageRecorded {
            conversation: chat.clone(),
            turn: parse_id("turn_1")?,
            message: parse_id("msg_a1")?,
            text: "It is a work-graph CLI harness".to_string(),
            at_ms: CLOCK_MS + 3,
            // Annotations only: none of these ids exist in the graph.
            related_attempts: vec![parse_id("att_ghost")?],
            related_decisions: vec![parse_id("dec_ghost")?],
            related_work: vec![parse_id("work_ghost")?],
        },
        DomainFact::UserMessageRecorded {
            conversation: chat.clone(),
            turn: parse_id("turn_2")?,
            message: parse_id("msg_u2")?,
            text: "Ship it".to_string(),
            at_ms: CLOCK_MS + 4,
        },
        DomainFact::ConversationTurnFailed {
            conversation: chat.clone(),
            turn: parse_id("turn_2")?,
            safe_message: "provider unavailable".to_string(),
            at_ms: CLOCK_MS + 5,
        },
        DomainFact::UserMessageRecorded {
            conversation: chat.clone(),
            turn: parse_id("turn_3")?,
            message: parse_id("msg_u3")?,
            text: "Never answered".to_string(),
            at_ms: CLOCK_MS + 6,
        },
        DomainFact::UserMessageRecorded {
            conversation: archive.clone(),
            turn: parse_id("turn_old")?,
            message: parse_id("msg_old-u")?,
            text: "Old question".to_string(),
            at_ms: CLOCK_MS - 10,
        },
        DomainFact::ConversationTurnCancelled {
            conversation: archive,
            turn: parse_id("turn_old")?,
            reason: "user interrupted".to_string(),
            at_ms: CLOCK_MS - 9,
        },
    ])
}

#[test]
fn conversation_facts_round_trip_rebuild_and_fail_closed() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let workspace = dir.path();
    let facts = conversation_stream()?;
    let store = relay_store::GraphFactStore::create(
        workspace,
        &domain_project()?,
        Integrity::Checksums,
        options(),
    )?;
    let outcome = store.append(&facts)?;
    assert_eq!(outcome.len(), facts.len());
    assert_eq!(
        store.revision()?,
        ProjectRevision::from_count(facts.len() as u64)
    );

    // The typed timeline replays in append order with contiguous sequence.
    let timeline = store.read_timeline()?;
    let kinds: Vec<&'static str> = timeline.iter().map(|f| f.fact.kind()).collect();
    assert_eq!(
        kinds,
        vec![
            "project_created",
            "conversation_started",
            "conversation_started",
            "user_message_recorded",
            "assistant_message_recorded",
            "user_message_recorded",
            "conversation_turn_failed",
            "user_message_recorded",
            "user_message_recorded",
            "conversation_turn_cancelled",
        ]
    );
    for (index, fact) in timeline.iter().enumerate() {
        assert_eq!(fact.sequence, (index as u64) + 1);
    }

    // The pure fold projects the conversation state, without any goal/work.
    let graph = store.project_graph()?;
    assert!(graph.goal().is_none());
    assert_eq!(graph.works().count(), 0);
    assert_eq!(graph.conversations().count(), 2);
    assert_eq!(graph.latest_conversation(), Some(parse_id("cnv_chat")?));
    let chat: ConversationId = parse_id("cnv_chat")?;
    let archive: ConversationId = parse_id("cnv_archive")?;
    assert_eq!(
        graph.interrupted_turns(&chat),
        vec![parse_id::<relay_domain::TurnId>("turn_3")?]
    );
    let tail = graph.transcript_tail(&chat, 10);
    assert_eq!(tail.len(), 4);
    assert_eq!(tail[0].speaker, Speaker::User);
    assert_eq!(tail[1].speaker, Speaker::Assistant);
    // The cancelled turn contributes its user side only.
    assert_eq!(graph.transcript_tail(&archive, 10).len(), 1);

    // The SQL projection is unchanged conversation-wise (no conversation
    // tables) and the revision stamp counts every conversation fact.
    let before = graph_view(&store)?;
    assert!(before.works.is_empty());
    assert_eq!(before.meta.revision, facts.len() as u64);
    drop(store);

    // Delete the projection database entirely: the JSONL log alone
    // rebuilds the same SQL view and the same folded conversation state.
    let db = workspace.join(".relay-graph").join("projection.sqlite");
    for suffix in ["", "-wal", "-shm"] {
        let path = db.with_file_name(format!("projection.sqlite{suffix}"));
        if path.exists() {
            std::fs::remove_file(path)?;
        }
    }
    assert!(!db.exists());
    let (reopened, report) = relay_store::GraphFactStore::open(workspace, options())?;
    assert!(
        matches!(
            report.reconcile,
            Some(relay_store::FactReconcileOutcome::ReplayedSuffix { from: 1, to }) if to == facts.len() as u64
        ),
        "expected a full replay, got {:?}",
        report.reconcile
    );
    let after = graph_view(&reopened)?;
    assert_eq!(after, before);
    assert_eq!(reopened.project_graph()?, graph);

    // Doctor passes on a conversation-fact store.
    let findings = relay_store::fact_doctor_check(workspace, &options());
    assert!(
        !findings
            .iter()
            .any(|f| f.severity == relay_store::fact::doctor::FactSeverity::Error),
        "{findings:?}"
    );
    drop(reopened);

    // Fail-closed: a hand-written "conversation_started" record at payload
    // version 2 is refused by the domain gate at append time…
    let newer_dir = tempfile::tempdir()?;
    let raw = FactStore::create(newer_dir.path(), STREAM, Integrity::Checksums, options())?;
    let (kind, payload) = relay_domain::fact_to_parts(&facts[1]);
    let v2 = FactInput::new(kind, 2, Json::from_serde(&payload)?);
    let error = raw
        .append_facts(&[v2], &relay_store::DomainFactValidator, &NoFold)
        .err()
        .map(|e| e.to_string())
        .unwrap_or_default();
    assert!(error.contains("version 2"), "{error}");
    assert!(error.contains("Upgrade Relay"), "{error}");

    // …and a store that already contains one fails to open through the
    // domain seam (nothing downstream may read past an unknown version).
    let older_dir = tempfile::tempdir()?;
    let raw = FactStore::create(older_dir.path(), STREAM, Integrity::Checksums, options())?;
    raw.append_facts(
        &[FactInput::new(kind, 2, Json::from_serde(&payload)?)],
        &AcceptAllFacts,
        &NoFold,
    )?;
    drop(raw);
    assert!(
        relay_store::GraphFactStore::open(older_dir.path(), options()).is_err(),
        "a v2 conversation record must fail closed at open"
    );
    Ok(())
}
