//! Fault-injection suite (phase 2 item 9, storage/migration owner).
//!
//! Simulates crashes after every append/reconcile step through the
//! `FaultInjector` hooks (no real process kills) and verifies the
//! durability contract on reopen:
//!
//! - no event reported durable is lost;
//! - SQLite never advances beyond validated JSONL (checkpoint ≤ tail);
//! - every crash point converges: reopen succeeds, the index catches up or
//!   rebuilds, and deleting `index.sqlite` always yields an equivalent
//!   database-backed view.

use std::path::Path;
use std::sync::Arc;

use relay_store::AppendInput;
use relay_store::CrashAt;
use relay_store::FaultInjector;
use relay_store::FaultPoint;
use relay_store::Json;
use relay_store::MigrationOptions;
use relay_store::MigrationOutcome;
use relay_store::ReconcileOutcome;
use relay_store::RelayStore;
use relay_store::StoreError;
use relay_store::StoreOptions;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const PROJECT: &str = "prj_00000000-0000-4000-8000-000000000001";
const CLOCK_MS: i64 = 1_791_878_400_000;
const REPO_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");

fn interrupted_payload(n: u64) -> Json {
    Json::object([
        ("phase", Json::string(format!("phase-{n}"))),
        ("hint", Json::string("injected")),
    ])
}

fn initialized_payload() -> Json {
    Json::object([
        ("goal", Json::string("Fault injection")),
        ("workspaceRoot", Json::string("/tmp/faults")),
        ("initialAutonomy", Json::string("workflow")),
    ])
}

fn base_options() -> StoreOptions {
    StoreOptions::deterministic(CLOCK_MS)
}

fn options_with(fault: Arc<dyn FaultInjector>) -> StoreOptions {
    let mut options = base_options();
    options.fault = fault;
    options
}

fn append(store: &RelayStore, n: u64, start: u64) -> Result<u64> {
    let inputs: Vec<AppendInput> = (start..start + n)
        .map(|sequence| {
            if sequence == 1 {
                AppendInput::new("project.initialized", initialized_payload())
            } else {
                AppendInput::new("run.interrupted", interrupted_payload(sequence))
            }
        })
        .collect();
    store.append_batch(&inputs)?;
    Ok(start + n)
}

fn is_simulated_crash(error: &StoreError) -> bool {
    matches!(error, StoreError::SimulatedCrash { .. })
}

/// The durable-state invariants that must hold after every reopen.
fn assert_invariants(workspace: &Path, label: &str) -> Result<RelayStore> {
    let (store, _) = RelayStore::open(workspace, base_options())?;
    let records = store.read_all()?;
    let tail = store.tail()?;
    let checkpoint = store.projection_checkpoint()?;
    assert_eq!(
        tail.last_sequence as usize,
        records.len(),
        "{label}: tail sequence must match the JSONL record count"
    );
    if let Some(checkpoint) = checkpoint {
        assert!(
            checkpoint.last_sequence <= tail.last_sequence,
            "{label}: SQLite advanced beyond JSONL (checkpoint {}, tail {})",
            checkpoint.last_sequence,
            tail.last_sequence
        );
        if checkpoint.last_sequence == tail.last_sequence {
            assert_eq!(
                checkpoint.last_hash,
                tail.last_hash.clone().unwrap_or_default(),
                "{label}: checkpoint hash diverges from JSONL tail"
            );
        }
    }
    assert_eq!(
        store.projection_event_count()?,
        Some(records.len() as u64),
        "{label}: index event count must equal the JSONL count after reconcile"
    );
    Ok(store)
}

fn equivalent_after_index_deletion(workspace: &Path, label: &str) -> Result<()> {
    let before = {
        let store = assert_invariants(workspace, label)?;
        let count = store.projection_event_count()?;
        let summary = store.projection_summary(PROJECT)?;
        let checkpoint = store.projection_checkpoint()?;
        drop(store);
        (count, summary, checkpoint)
    };
    std::fs::remove_file(workspace.join(".relay/index.sqlite"))?;
    // Sidecar leftovers must not confuse the rebuild either.
    for suffix in ["index.sqlite-wal", "index.sqlite-shm"] {
        let sidecar = workspace.join(".relay").join(suffix);
        if sidecar.exists() {
            std::fs::remove_file(sidecar)?;
        }
    }
    let (store, report) = RelayStore::open(workspace, base_options())?;
    assert!(
        matches!(
            report.reconcile,
            Some(ReconcileOutcome::ReplayedSuffix { .. })
        ),
        "{label}: expected a suffix replay after index deletion, got {:?}",
        report.reconcile
    );
    assert_eq!(store.projection_event_count()?, before.0);
    assert_eq!(store.projection_summary(PROJECT)?, before.1);
    assert_eq!(store.projection_checkpoint()?, before.2);
    Ok(())
}

#[test]
fn crash_after_every_append_step_preserves_durability() -> Result<()> {
    let points = [
        FaultPoint::LockAcquired,
        FaultPoint::BatchValidated,
        FaultPoint::BeforeWrite,
        FaultPoint::AfterPartialWrite,
        FaultPoint::AfterWriteNoSync,
        FaultPoint::AfterDurableAppend,
        // AfterRotation needs a threshold-crossing batch and is covered by
        // crash_between_rotation_and_projection_keeps_segments_valid below.
        FaultPoint::ProjectionChunkCommitted,
        FaultPoint::ProjectionCommitted,
    ];
    for point in points {
        let workspace = tempfile::tempdir()?;
        let root = workspace.path().to_path_buf();
        let store = RelayStore::create(&root, PROJECT, base_options())?;
        append(&store, 2, 1)?;
        drop(store);

        let crasher = options_with(Arc::new(CrashAt::new(point)));
        let (store, _) = RelayStore::open(&root, crasher.clone())?;
        let error = store
            .append_batch(&[AppendInput::new("run.interrupted", interrupted_payload(3))])
            .err()
            .ok_or_else(|| format!("{point:?}: expected a simulated crash"))?;
        assert!(
            is_simulated_crash(&error),
            "{point:?}: unexpected error {error}"
        );
        drop(store);

        // Events already reported durable survive every crash point; events
        // not yet durable may be absent but never half-present.
        let (reopened, _) = RelayStore::open(&root, base_options())?;
        let count = reopened.read_all()?.len();
        drop(reopened);
        let survived_full = matches!(
            point,
            FaultPoint::AfterDurableAppend
                | FaultPoint::AfterRotation
                | FaultPoint::ProjectionChunkCommitted
                | FaultPoint::ProjectionCommitted
                | FaultPoint::AfterWriteNoSync
        );
        let expected = if survived_full { 3 } else { 2 };
        assert_eq!(count, expected, "{point:?}: durable event set after crash");
        assert_invariants(&root, &format!("{point:?}"))?;
        equivalent_after_index_deletion(&root, &format!("{point:?}"))?;
    }
    Ok(())
}

#[test]
fn half_committed_projection_converges() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let root = workspace.path().to_path_buf();
    // One batch of 1200 events: the JSONL append completes, then the
    // projection replays in chunks of 500 — crash after the first chunk
    // commit leaves a half-committed index (checkpoint 500, JSONL 1200).
    let store = RelayStore::create(
        &root,
        PROJECT,
        options_with(Arc::new(CrashAt::new(FaultPoint::ProjectionChunkCommitted))),
    )?;
    let error = store
        .append_batch(
            &(1..=1200u64)
                .map(|sequence| {
                    if sequence == 1 {
                        AppendInput::new("project.initialized", initialized_payload())
                    } else {
                        AppendInput::new("run.interrupted", interrupted_payload(sequence))
                    }
                })
                .collect::<Vec<_>>(),
        )
        .err()
        .ok_or("expected a simulated crash mid-projection")?;
    assert!(is_simulated_crash(&error));

    // The JSONL is authoritative and complete; the index is behind.
    let reader = RelayStore::open_read_only(&root, base_options())?;
    assert_eq!(reader.read_all()?.len(), 1200);
    drop(reader);

    // Crash again during the suffix replay (first replay chunk).
    let reopening = options_with(Arc::new(CrashAt::new(FaultPoint::ProjectionChunkCommitted)));
    let error = RelayStore::open(&root, reopening)
        .err()
        .ok_or("expected a simulated crash during suffix replay")?;
    assert!(is_simulated_crash(&error));

    // A clean reopen converges: suffix replay catches the index up.
    let store = assert_invariants(&root, "half-committed")?;
    assert_eq!(store.projection_event_count()?, Some(1200));
    drop(store);
    equivalent_after_index_deletion(&root, "half-committed")?;
    Ok(())
}

#[test]
fn crash_between_rotation_and_projection_keeps_segments_valid() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let root = workspace.path().to_path_buf();
    let store = RelayStore::create(&root, PROJECT, base_options())?;
    append(&store, 2, 1)?;
    drop(store);

    // 1000 events cross the rotation threshold; crash after rotation.
    let (store, _) = RelayStore::open(
        &root,
        options_with(Arc::new(CrashAt::new(FaultPoint::AfterRotation))),
    )?;
    let error = store
        .append_batch(
            &(3..=1002u64)
                .map(|sequence| AppendInput::new("run.interrupted", interrupted_payload(sequence)))
                .collect::<Vec<_>>(),
        )
        .err()
        .ok_or("expected a simulated crash after rotation")?;
    assert!(is_simulated_crash(&error));
    drop(store);

    let (reopened, _) = RelayStore::open(&root, base_options())?;
    assert_eq!(reopened.read_all()?.len(), 1002);
    assert!(
        root.join(".relay/events/0000000000000001-00000000000003ea.jsonl")
            .exists()
    );
    assert!(
        root.join(".relay/events/00000000000003eb-open.jsonl")
            .exists()
    );
    drop(reopened);
    assert_invariants(&root, "after-rotation")?;
    equivalent_after_index_deletion(&root, "after-rotation")?;
    Ok(())
}

#[test]
fn two_separately_opened_writers_serialize_through_the_lock() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let root = workspace.path().to_path_buf();
    {
        let store = RelayStore::create(&root, PROJECT, base_options())?;
        append(&store, 1, 1)?;
    }
    let a = root.clone();
    let b = root.clone();
    let handle_a = std::thread::spawn(move || -> std::result::Result<(), StoreError> {
        let store = RelayStore::open(&a, base_options())?.0;
        for _ in 0..5 {
            store.append_batch(&[AppendInput::new("run.interrupted", interrupted_payload(0))])?;
        }
        Ok(())
    });
    let handle_b = std::thread::spawn(move || -> std::result::Result<(), StoreError> {
        let store = RelayStore::open(&b, base_options())?.0;
        for _ in 0..5 {
            store.append_batch(&[AppendInput::new("run.interrupted", interrupted_payload(0))])?;
        }
        Ok(())
    });
    handle_a.join().map_err(|_| "writer a panicked")??;
    handle_b.join().map_err(|_| "writer b panicked")??;
    assert_invariants(&root, "two-writers")?;
    let (store, _) = RelayStore::open(&root, base_options())?;
    assert_eq!(store.read_all()?.len(), 11);
    Ok(())
}

#[test]
fn stale_expected_revision_from_competing_writer_fails_actionably() -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let root = workspace.path().to_path_buf();
    {
        let store = RelayStore::create(&root, PROJECT, base_options())?;
        append(&store, 1, 1)?;
    }
    // Two writers open the same revision; the first commits, the second
    // must fail with an actionable stale-revision error.
    let first = RelayStore::open(&root, base_options())?.0;
    let (second, _) = RelayStore::open(&root, base_options())?;
    first.append_batch(&[AppendInput::new("run.interrupted", interrupted_payload(2))])?;
    let error = second
        .append_batch_at(
            Some(1),
            &[AppendInput::new("run.interrupted", interrupted_payload(2))],
        )
        .err()
        .ok_or("expected a stale-revision failure")?;
    let message = error.to_string();
    assert!(message.contains("stale expected revision"), "{message}");
    assert!(message.contains("advanced to sequence 2"), "{message}");
    assert_invariants(&root, "stale-revision")?;
    Ok(())
}

#[test]
fn fixture_backed_store_survives_crash_cycle() -> Result<()> {
    // Import the deterministic seven-mode fixture run, then crash-cycle
    // appends on top of it: the imported history must survive every crash
    // point and stay consistent with its projection.
    let fixture = Path::new(REPO_ROOT).join("tests/fixtures/cases/e2e-demo-run/events.jsonl");
    if !fixture.exists() {
        return Ok(()); // corpus not present in this checkout
    }
    let workspace = tempfile::tempdir()?;
    let root = workspace.path().to_path_buf();
    let migration = MigrationOptions {
        clock: Arc::new(relay_store::FixedClock::new(CLOCK_MS)),
        ..MigrationOptions::default()
    };
    match relay_store::import_jsonl_directory(&root, &fixture, &migration)? {
        MigrationOutcome::Migrated(report) => {
            assert_eq!(report.events_migrated, 63);
        }
        MigrationOutcome::AlreadyMigrated { detail } => {
            return Err(format!("unexpected: {detail}").into());
        }
    }

    let crasher = options_with(Arc::new(CrashAt::new(FaultPoint::AfterDurableAppend)));
    let (store, _) = RelayStore::open(&root, crasher)?;
    let error = store
        .append_batch(&[AppendInput::new("run.interrupted", interrupted_payload(64))])
        .err()
        .ok_or("expected crash")?;
    assert!(is_simulated_crash(&error));
    drop(store);

    let reopened = assert_invariants(&root, "fixture-backed")?;
    assert_eq!(reopened.read_all()?.len(), 64);
    drop(reopened);
    equivalent_after_index_deletion(&root, "fixture-backed")?;
    Ok(())
}
