//! Crash-recovery gate (storage/migration owner) — the plan's named
//! `crash_recovery` focus.
//!
//! Consolidates the crash/recovery evidence in one place:
//!
//! 1. fact-store crash injection at every `FaultPoint`: any fact whose
//!    append was acknowledged survives, nothing partial is resurrected, and
//!    every crash point converges on reopen (projection catches up);
//! 2. end-to-end abandoned-attempt reconciliation through the orchestrator
//!    over a **real fact store** (the `FactSink` port backed by
//!    `GraphFactStore`): started-but-unconcluded attempts conclude
//!    `Abandoned` — never success — with write authority surfaced;
//! 3. projection quarantine + rebuild-from-JSONL equivalence for a corrupt
//!    database;
//! 4. stale-attempt receipt acceptance refused by the engine.

mod common;

use std::sync::Arc;

use common::{
    completed_receipt, graph_store, model_work, options, parse, published, revision_of,
    seed_history, workspace_work,
};
use relay_domain::DomainFact;
use relay_domain::ProjectRevision;
use relay_store::{CrashAt, FaultPoint, StoreError};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

use common::write_blob;

fn brief_content() -> serde_json::Value {
    serde_json::json!({
        "schema": "relay.brief@1",
        "problem": "traps regress",
        "audience": "agents",
        "outcome": "no false greens",
        "open_questions": [],
    })
}

#[test]
fn acknowledged_facts_survive_crash_injection_at_every_point() -> Result<()> {
    let points = [
        FaultPoint::LockAcquired,
        FaultPoint::BatchValidated,
        FaultPoint::BeforeWrite,
        FaultPoint::AfterPartialWrite,
        FaultPoint::AfterWriteNoSync,
        FaultPoint::AfterDurableAppend,
        // AfterRotation needs a threshold-crossing batch (covered by
        // fact_replay::crash_after_rotation_keeps_segments_valid).
        FaultPoint::ProjectionChunkCommitted,
        FaultPoint::ProjectionCommitted,
    ];
    for point in points {
        let (dir, store) = graph_store()?;
        let workspace = dir.path().to_path_buf();
        store.append(&seed_history(&parse("prj_crash")?)?)?;
        store.append(&[DomainFact::WorkDeclared {
            work: model_work("work_a", "first")?,
        }])?;
        drop(store);

        // A fourth fact dies at `point`.
        let mut faulted = options();
        faulted.fault = Arc::new(CrashAt::new(point));
        let (store, _) = relay_store::GraphFactStore::open(&workspace, faulted)?;
        let outcome = store.append(&[DomainFact::WorkDeclared {
            work: model_work("work_b", "second")?,
        }]);
        assert!(
            matches!(outcome, Err(StoreError::SimulatedCrash { .. })),
            "{point:?}: expected a simulated crash"
        );
        drop(store);

        // The three acknowledged facts survive; the crashed one only when
        // the crash point is at or past the durable write.
        let (reopened, _) = relay_store::GraphFactStore::open(&workspace, options())?;
        let kinds: Vec<&'static str> = reopened
            .read_facts()?
            .iter()
            .map(DomainFact::kind)
            .collect();
        let durable_at_point = matches!(
            point,
            FaultPoint::AfterWriteNoSync
                | FaultPoint::AfterDurableAppend
                | FaultPoint::ProjectionChunkCommitted
                | FaultPoint::ProjectionCommitted
        );
        let expected = if durable_at_point { 4 } else { 3 };
        assert_eq!(kinds.len(), expected, "{point:?}: {kinds:?}");
        assert!(kinds.contains(&"project_created"), "{point:?}");
        assert!(kinds.contains(&"goal_declared"), "{point:?}");
        // Reopen converges: the projection matches the log and the graph
        // folds.
        assert_eq!(
            reopened.projection_checkpoint()?.map(|c| c.last_sequence),
            Some(expected as u64),
            "{point:?}: projection must catch up"
        );
        assert!(reopened.project_graph().is_ok(), "{point:?}");

        // Recovery leaves the store usable.
        reopened.append(&[DomainFact::WorkCanceled {
            work: parse("work_a")?,
            reason: "post-crash cleanup".to_string(),
        }])?;
        drop(reopened);
        let (final_open, _) = relay_store::GraphFactStore::open(&workspace, options())?;
        assert_eq!(
            final_open.revision()?.get(),
            (expected + 1) as u64,
            "{point:?}: post-recovery append survived"
        );
    }
    Ok(())
}

#[test]
fn abandoned_attempts_reconcile_never_succeed_over_a_real_store() -> Result<()> {
    use relay_executor::FactSink as _;
    use relay_executor::Orchestrator;

    let (dir, store) = graph_store()?;
    store.append(&seed_history(&parse("prj_crash")?)?)?;
    let work = workspace_work("work_writer")?;
    store.append(&[DomainFact::WorkDeclared { work: work.clone() }])?;

    // Simulate the crash: the attempt started durably, then the process
    // died before any conclusion.
    let revision = revision_of(&store)?;
    let facts = store.read_facts()?;
    let graph = relay_domain::ProjectGraph::from_facts(&facts)?;
    let node = graph.work(&parse("work_writer")?).ok_or("work missing")?;
    let grant = relay_executor::grant_from_work(
        common::grant_id("grant_writer")?,
        node,
        None,
        relay_domain::Budget::default(),
    );
    store.append(&[DomainFact::AttemptStarted {
        attempt: parse("att_writer")?,
        work: parse("work_writer")?,
        expected_revision: revision,
        grant,
        started_at_ms: common::CLOCK_MS,
    }])?;
    drop(store);

    // Reopen over a new process face and reconcile through the
    // orchestrator, backed by the durable store.
    let (reopened, _) = relay_store::GraphFactStore::open(dir.path(), options())?;
    let sink = Arc::new(common::StoreFactSink::new(Arc::new(reopened)));
    let orchestrator = Orchestrator::new(
        Arc::clone(&sink) as Arc<dyn relay_executor::FactSink>,
        relay_executor::system_clock(),
    );

    let reports = orchestrator.reconcile_abandoned()?;
    assert_eq!(reports.len(), 1, "{reports:?}");
    let report = &reports[0];
    assert_eq!(report.attempt.as_str(), "att_writer");
    assert_eq!(report.work.as_str(), "work_writer");
    assert!(
        report.had_write_authority,
        "dirty-write authority must surface for a human decision"
    );

    // The conclusion is durable: Abandoned, never success.
    let store_facts = sink.facts()?;
    let conclusion = store_facts
        .iter()
        .find_map(|fact| match fact {
            DomainFact::AttemptConcluded { conclusion, .. } => Some(conclusion),
            _ => None,
        })
        .ok_or("no conclusion appended")?;
    assert!(
        matches!(
            conclusion,
            relay_domain::AttemptConclusion::Abandoned { .. }
        ),
        "reconciliation must never conclude success: {conclusion:?}"
    );

    // Reconciling again is a no-op (idempotent).
    let again = orchestrator.reconcile_abandoned()?;
    assert!(again.is_empty(), "{again:?}");

    // The store itself still validates, folds, and projects the conclusion.
    let (writer, _) = relay_store::GraphFactStore::open(dir.path(), options())?;
    assert!(writer.project_graph().is_ok());
    let writer_work: relay_domain::WorkItemId = parse("work_writer")?;
    let attempts =
        writer.projection(|conn| relay_store::attempts(conn, Some(&writer_work), None))?;
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].conclusion_status.as_deref(), Some("abandoned"));
    Ok(())
}

#[test]
fn corrupt_projection_quarantines_and_rebuilds_from_jsonl() -> Result<()> {
    let (dir, store) = graph_store()?;
    store.append(&seed_history(&parse("prj_crash")?)?)?;
    store.append(&[DomainFact::WorkDeclared {
        work: model_work("work_a", "first")?,
    }])?;
    store.append(&[published("art_brief", 1)?])?;
    write_blob(dir.path(), "art_brief", 1, &brief_content())?;
    let before = store.projection(|conn| relay_store::works(conn, &Default::default()))?;
    let before_artifacts = store.projection(|conn| relay_store::artifact_versions(conn, None))?;
    let checkpoint = store.projection_checkpoint()?;
    drop(store);

    // Corrupt the projection database beyond repair.
    let db = dir.path().join(".relay-graph").join("projection.sqlite");
    std::fs::write(&db, b"this is not a sqlite database")?;

    let (reopened, report) = relay_store::GraphFactStore::open(dir.path(), options())?;
    // A corrupt database is quarantined at open; the fresh one replays the
    // whole log (from sequence 1). Either path is a full rebuild.
    assert!(
        matches!(
            report.reconcile,
            Some(relay_store::FactReconcileOutcome::Rebuilt { .. })
                | Some(relay_store::FactReconcileOutcome::ReplayedSuffix { from: 1, .. })
        ),
        "a corrupt database must be quarantined and rebuilt, got {:?}",
        report.reconcile
    );
    assert_eq!(
        reopened.projection_checkpoint()?,
        checkpoint,
        "the rebuilt checkpoint matches the fact tail"
    );
    let after = reopened.projection(|conn| relay_store::works(conn, &Default::default()))?;
    let after_artifacts = reopened.projection(|conn| relay_store::artifact_versions(conn, None))?;
    assert_eq!(after, before, "work rows rebuild identically");
    assert_eq!(
        after_artifacts, before_artifacts,
        "artifact rows rebuild identically"
    );
    assert!(reopened.project_graph().is_ok());

    // Doctor is clean on the rebuilt store.
    let findings = relay_store::fact_doctor_check(dir.path(), &options());
    assert!(
        !findings
            .iter()
            .any(|f| f.severity == relay_store::fact::doctor::FactSeverity::Error),
        "{findings:?}"
    );
    // The quarantined file was retained for review, not deleted.
    let recovery: Vec<String> =
        std::fs::read_dir(dir.path().join(".relay-graph").join("recovery"))?
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .collect();
    assert!(
        recovery.iter().any(|name| name.contains("quarantine")),
        "{recovery:?}"
    );
    Ok(())
}

#[test]
fn stale_attempt_receipts_are_refused() -> Result<()> {
    let (_dir, store) = graph_store()?;
    store.append(&seed_history(&parse("prj_crash")?)?)?;
    let work = workspace_work("work_writer")?;
    store.append(&[DomainFact::WorkDeclared { work: work.clone() }])?;

    // The attempt pins the current revision…
    let pinned = revision_of(&store)?;
    let facts = store.read_facts()?;
    let graph = relay_domain::ProjectGraph::from_facts(&facts)?;
    let node = graph.work(&parse("work_writer")?).ok_or("work missing")?;
    let grant = relay_executor::grant_from_work(
        common::grant_id("grant_writer")?,
        node,
        None,
        relay_domain::Budget::default(),
    );
    store.append(&[DomainFact::AttemptStarted {
        attempt: parse("att_writer")?,
        work: parse("work_writer")?,
        expected_revision: pinned,
        grant,
        started_at_ms: common::CLOCK_MS,
    }])?;

    // …then a material change lands on that very work item (a scope
    // expansion changes the node the receipt was computed against).
    store.append(&[DomainFact::WorkScopeExpanded {
        work: parse("work_writer")?,
        added_write_scopes: vec![parse("src/core")?],
        added_operations: vec![],
        reason: "the evaluator lives in src/core".to_string(),
    }])?;

    let graph = store.project_graph()?;
    let receipt = completed_receipt("att_writer", &parse("work_writer")?)?;
    let rejection = relay_engine::accept_receipt(&graph, &receipt)
        .err()
        .ok_or("expected a stale-revision rejection")?;
    assert!(
        matches!(rejection, relay_engine::Rejection::StaleRevision { .. }),
        "{rejection}"
    );

    // And a fresh (non-stale) attempt of the same work is still acceptable:
    // reconciliation is never a dead end.
    let fresh_revision = revision_of(&store)?;
    let fresh_node = graph.work(&parse("work_writer")?).ok_or("work missing")?;
    let fresh_grant = relay_executor::grant_from_work(
        common::grant_id("grant_writer-2")?,
        fresh_node,
        None,
        relay_domain::Budget::default(),
    );
    store.append(&[DomainFact::AttemptStarted {
        attempt: parse("att_writer-2")?,
        work: parse("work_writer")?,
        expected_revision: fresh_revision,
        grant: fresh_grant,
        started_at_ms: common::CLOCK_MS,
    }])?;
    let fresh_graph = store.project_graph()?;
    let fresh_receipt = completed_receipt("att_writer-2", &parse("work_writer")?)?;
    assert!(
        relay_engine::accept_receipt(&fresh_graph, &fresh_receipt).is_ok(),
        "a materially-fresh receipt must be acceptable"
    );
    Ok(())
}

#[test]
fn revision_pins_refuse_stale_appends_after_recovery() -> Result<()> {
    // After a crash + recovery, a writer holding a pre-crash revision is
    // refused rather than silently racing the recovered stream.
    let (dir, store) = graph_store()?;
    store.append(&seed_history(&parse("prj_crash")?)?)?;
    let pre_crash_revision = revision_of(&store)?;
    drop(store);

    // Someone else appends (the recovered stream moves on).
    let (other, _) = relay_store::GraphFactStore::open(dir.path(), options())?;
    other.append(&[DomainFact::WorkDeclared {
        work: model_work("work_a", "first")?,
    }])?;
    drop(other);

    let (stale_writer, _) = relay_store::GraphFactStore::open(dir.path(), options())?;
    let error = stale_writer
        .append_at(
            Some(pre_crash_revision),
            &[DomainFact::WorkCanceled {
                work: parse("work_a")?,
                reason: "stale".to_string(),
            }],
        )
        .err()
        .map(|e| e.to_string())
        .unwrap_or_default();
    assert!(error.contains("stale expected revision"), "{error}");
    assert_eq!(stale_writer.revision()?, ProjectRevision::from_count(3));
    Ok(())
}
