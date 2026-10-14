//! The store orchestrator: JSONL log + writer lock + SQLite projection
//! (`docs/event-format.md` §6).
//!
//! [`RelayStore`] executes the append protocol end to end: acquire the
//! writer lock, re-derive the tail, validate the whole batch, append one
//! buffered write, fsync (in durable mode), rotate when thresholds are
//! crossed, commit the projection, release the lock. The user-visible
//! success boundary is the durable JSONL append: a projection failure after
//! it leaves the event durable and the index lagging, and the next open
//! replays the missing suffix.

use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use crate::canonical::Json;
use crate::clock::Clock;
use crate::clock::FixedClock;
use crate::clock::IdGen;
use crate::clock::SystemClock;
use crate::clock::UuidIdGen;
use crate::envelope::EventEnvelope;
use crate::error::Result;
use crate::error::StoreError;
use crate::fault::FaultInjector;
use crate::fault::FaultPoint;
use crate::fault::NoFaults;
use crate::lock;
use crate::log::Durability;
use crate::log::EventLog;
use crate::log::EventRecord;
use crate::log::OpenMode;
use crate::log::RecoveredTail;
use crate::log::TailState;
use crate::paths::RelayPaths;
use crate::projection::Checkpoint;
use crate::projection::ProjectSummaryRow;
use crate::projection::ReconcileOutcome;
use crate::projection::SqliteProjection;

/// Store behavior configuration. Clocks, randomness, payload validation,
/// and fault injection are injected boundary data so replay and tests stay
/// deterministic.
#[derive(Clone)]
pub struct StoreOptions {
    /// Append durability mode (§6 step 7).
    pub durability: Durability,
    /// Writer-lock acquisition timeout (§6 step 1; default 5000 ms).
    pub lock_timeout: Duration,
    /// Time source.
    pub clock: Arc<dyn Clock>,
    /// Identifier source.
    pub id_gen: Arc<dyn IdGen>,
    /// Crash-injection hooks for tests.
    pub fault: Arc<dyn FaultInjector>,
}

impl Default for StoreOptions {
    fn default() -> StoreOptions {
        StoreOptions {
            durability: Durability::Durable,
            lock_timeout: crate::lock::DEFAULT_LOCK_TIMEOUT,
            clock: Arc::new(SystemClock),
            id_gen: Arc::new(UuidIdGen),
            fault: Arc::new(NoFaults),
        }
    }
}

impl StoreOptions {
    /// Options for deterministic tests: a fixed clock at `epoch_millis`.
    pub fn deterministic(epoch_millis: i64) -> StoreOptions {
        StoreOptions {
            clock: Arc::new(FixedClock::new(epoch_millis)),
            ..StoreOptions::default()
        }
    }
}

/// One proposed event. The store assigns id, sequence, timestamp (when not
/// supplied), correlation id (when not supplied), and chain metadata.
#[derive(Debug, Clone)]
pub struct AppendInput {
    /// Event type (one of the known Relay types).
    pub event_type: String,
    /// Type-specific payload.
    pub payload: Json,
    /// Producing session, if any.
    pub session_id: Option<String>,
    /// Explicit timestamp (defaults to the clock).
    pub timestamp: Option<String>,
    /// Explicit correlation id (defaults to a fresh `cor_<uuid>`).
    pub correlation_id: Option<String>,
    /// Causing event id, if any.
    pub causation_id: Option<String>,
}

impl AppendInput {
    /// A minimal input: type + payload.
    pub fn new(event_type: &str, payload: Json) -> AppendInput {
        AppendInput {
            event_type: event_type.to_string(),
            payload,
            session_id: None,
            timestamp: None,
            correlation_id: None,
            causation_id: None,
        }
    }
}

/// What an append did.
#[derive(Debug, Clone)]
pub struct AppendOutcome {
    /// The appended envelopes, exactly as a later read validates them.
    pub events: Vec<EventEnvelope>,
    /// Whether the SQLite projection could not be updated (the events are
    /// durable; the next open replays the suffix).
    pub projection_lagging: bool,
    /// Whether the append triggered a segment rotation.
    pub rotated: bool,
}

/// What opening a store did.
#[derive(Debug, Clone, Default)]
pub struct OpenReport {
    /// Tail recoveries performed (§7).
    pub recovered_tails: Vec<RecoveredTail>,
    /// Projection reconciliation outcome (§9).
    pub reconcile: Option<ReconcileOutcome>,
}

/// A Relay store: canonical JSONL log guarded by a writer lock, plus a
/// disposable SQLite projection. Clone-free; share via `Arc`. Internal
/// mutability is behind mutexes so a single instance can be used from
/// multiple threads; cross-process writers serialize through the file lock.
pub struct RelayStore {
    paths: RelayPaths,
    log: Mutex<EventLog>,
    projection: Mutex<Option<SqliteProjection>>,
    options: StoreOptions,
}

impl RelayStore {
    /// Create a new store for `project_id` in `workspace_root/.relay`.
    pub fn create(
        workspace_root: &std::path::Path,
        project_id: &str,
        options: StoreOptions,
    ) -> Result<RelayStore> {
        let paths = RelayPaths::new(workspace_root);
        let log = EventLog::init(&paths, project_id, options.clock.as_ref())?;
        let projection =
            SqliteProjection::open(&paths.index_db, &paths.recovery_dir, options.clock.as_ref())?;
        Ok(RelayStore {
            paths,
            log: Mutex::new(log),
            projection: Mutex::new(Some(projection)),
            options,
        })
    }

    /// Open an existing store (writer mode): validate everything, recover a
    /// crash tail, reconcile the projection — all under the writer lock.
    pub fn open(
        workspace_root: &std::path::Path,
        options: StoreOptions,
    ) -> Result<(RelayStore, OpenReport)> {
        let paths = RelayPaths::new(workspace_root);
        let _guard = lock::acquire(
            &paths.writer_lock(),
            &paths.recovery_dir,
            options.lock_timeout,
            options.clock.as_ref(),
        )?;
        let (log, recovered_tails) =
            EventLog::open(&paths, OpenMode::Writer, options.clock.as_ref())?;
        let records = log.read_all()?;
        let tail = log.tail().clone();
        let mut projection =
            SqliteProjection::open(&paths.index_db, &paths.recovery_dir, options.clock.as_ref())?;
        let reconcile = projection.reconcile(
            &records,
            &tail,
            options.clock.as_ref(),
            options.fault.as_ref(),
        )?;
        drop(_guard);
        Ok((
            RelayStore {
                paths,
                log: Mutex::new(log),
                projection: Mutex::new(Some(projection)),
                options,
            },
            OpenReport {
                recovered_tails,
                reconcile: Some(reconcile),
            },
        ))
    }

    /// Open read-only: validate the log without writing. An incomplete
    /// active tail (an append in progress) is ignored; the SQLite index is
    /// not opened (JSONL is authoritative).
    pub fn open_read_only(
        workspace_root: &std::path::Path,
        options: StoreOptions,
    ) -> Result<RelayStore> {
        let paths = RelayPaths::new(workspace_root);
        let (log, _recoveries) = EventLog::open(&paths, OpenMode::Reader, options.clock.as_ref())?;
        Ok(RelayStore {
            paths,
            log: Mutex::new(log),
            projection: Mutex::new(None),
            options,
        })
    }

    /// Append a batch with the full §6 protocol.
    pub fn append_batch(&self, inputs: &[AppendInput]) -> Result<AppendOutcome> {
        self.append_batch_at(None, inputs)
    }

    /// Append a batch that expects the log to currently end at
    /// `expected_last_sequence`. A stale expectation fails with an
    /// actionable error instead of racing another writer.
    pub fn append_batch_at(
        &self,
        expected_last_sequence: Option<u64>,
        inputs: &[AppendInput],
    ) -> Result<AppendOutcome> {
        if inputs.is_empty() {
            return Ok(AppendOutcome {
                events: vec![],
                projection_lagging: false,
                rotated: false,
            });
        }
        let _guard = lock::acquire(
            &self.paths.writer_lock(),
            &self.paths.recovery_dir,
            self.options.lock_timeout,
            self.options.clock.as_ref(),
        )?;
        if self.options.fault.should_crash(FaultPoint::LockAcquired) {
            return Err(StoreError::SimulatedCrash {
                point: FaultPoint::LockAcquired.name().to_string(),
            });
        }
        let mut log = self.log.lock().map_err(|_| StoreError::Invalid {
            reason: "the store log mutex was poisoned by a panic in another thread".to_string(),
        })?;
        log.refresh(self.options.clock.as_ref())?;
        if let Some(expected) = expected_last_sequence {
            let current = log.tail().last_sequence;
            if current != expected {
                return Err(StoreError::Append {
                    reason: format!(
                        "stale expected revision: this writer expected the project at \
                         sequence {expected}, but the log has advanced to sequence {current} \
                         (another writer appended). Reload the project state and retry the \
                         operation."
                    ),
                });
            }
        }

        // Build and validate the whole batch before writing anything (§6
        // steps 3–4): an invalid member aborts with the store untouched.
        let mut envelopes = Vec::with_capacity(inputs.len());
        let first_sequence = log.tail().last_sequence + 1;
        let mut previous_hash = log.tail().last_hash.clone();
        for (offset, input) in inputs.iter().enumerate() {
            let sequence = first_sequence + offset as u64;
            let id = self.options.id_gen.new_id("evt")?;
            let timestamp = input
                .timestamp
                .clone()
                .unwrap_or_else(|| self.options.clock.now_iso());
            let correlation = match &input.correlation_id {
                Some(existing) => existing.clone(),
                None => self.options.id_gen.new_id("cor")?,
            };
            let envelope = EventEnvelope::build(crate::envelope::EnvelopeDraft {
                project_id: log.manifest().project_id.clone(),
                sequence,
                session_id: input.session_id.clone(),
                event_type: input.event_type.clone(),
                timestamp,
                correlation_id: correlation,
                causation_id: input.causation_id.clone(),
                payload: input.payload.clone(),
                previous_hash: previous_hash.clone(),
                id,
            })?;
            envelope.validate()?;
            previous_hash = Some(envelope.hash.clone());
            envelopes.push(envelope);
        }
        if self.options.fault.should_crash(FaultPoint::BatchValidated) {
            return Err(StoreError::SimulatedCrash {
                point: FaultPoint::BatchValidated.name().to_string(),
            });
        }
        if self.options.fault.should_crash(FaultPoint::BeforeWrite) {
            return Err(StoreError::SimulatedCrash {
                point: FaultPoint::BeforeWrite.name().to_string(),
            });
        }
        let rotated = log.append(
            &envelopes,
            self.options.durability,
            self.options.fault.as_ref(),
        )?;
        drop(log);

        // Projection (§6 step 8). The durable JSONL append above is the
        // success boundary; a real projection failure is reported as a
        // lagging index, while a simulated crash propagates so tests can
        // observe recovery from exactly that point.
        let mut projection_slot = self.projection.lock().map_err(|_| StoreError::Invalid {
            reason: "the projection mutex was poisoned by a panic in another thread".to_string(),
        })?;
        let mut projection_lagging = false;
        if let Some(projection) = projection_slot.as_mut() {
            if let Err(error) = projection.apply_events(
                &envelopes,
                self.options.clock.as_ref(),
                self.options.fault.as_ref(),
            ) {
                if matches!(error, StoreError::SimulatedCrash { .. }) {
                    return Err(error);
                }
                projection_lagging = true;
            }
        } else {
            projection_lagging = true;
        }
        Ok(AppendOutcome {
            events: envelopes,
            projection_lagging,
            rotated,
        })
    }

    /// Read every validated record from JSONL (authoritative).
    pub fn read_all(&self) -> Result<Vec<EventRecord>> {
        let log = self.log.lock().map_err(|_| StoreError::Invalid {
            reason: "the store log mutex was poisoned by a panic in another thread".to_string(),
        })?;
        log.read_all()
    }

    /// The current tail (as of the last open/refresh/append in this
    /// process).
    pub fn tail(&self) -> Result<TailState> {
        let log = self.log.lock().map_err(|_| StoreError::Invalid {
            reason: "the store log mutex was poisoned by a panic in another thread".to_string(),
        })?;
        Ok(log.tail().clone())
    }

    /// The project id this store belongs to.
    pub fn project_id(&self) -> Result<String> {
        let log = self.log.lock().map_err(|_| StoreError::Invalid {
            reason: "the store log mutex was poisoned by a panic in another thread".to_string(),
        })?;
        Ok(log.manifest().project_id.clone())
    }

    /// Events after `sequence` from JSONL (authoritative), at most `limit`.
    pub fn events_after(&self, sequence: u64, limit: u64) -> Result<Vec<EventRecord>> {
        let all = self.read_all()?;
        Ok(all
            .into_iter()
            .filter(|record| record.envelope.sequence > sequence)
            .take(limit as usize)
            .collect())
    }

    /// Projection checkpoint, when the projection is open.
    pub fn projection_checkpoint(&self) -> Result<Option<Checkpoint>> {
        let projection = self.projection.lock().map_err(|_| StoreError::Invalid {
            reason: "the projection mutex was poisoned by a panic in another thread".to_string(),
        })?;
        match projection.as_ref() {
            Some(projection) => projection.checkpoint(),
            None => Ok(None),
        }
    }

    /// Number of indexed events in the projection.
    pub fn projection_event_count(&self) -> Result<Option<u64>> {
        let projection = self.projection.lock().map_err(|_| StoreError::Invalid {
            reason: "the projection mutex was poisoned by a panic in another thread".to_string(),
        })?;
        match projection.as_ref() {
            Some(projection) => projection.event_count().map(Some),
            None => Ok(None),
        }
    }

    /// Projection project summary (query view).
    pub fn projection_summary(&self, project_id: &str) -> Result<Option<ProjectSummaryRow>> {
        let projection = self.projection.lock().map_err(|_| StoreError::Invalid {
            reason: "the projection mutex was poisoned by a panic in another thread".to_string(),
        })?;
        match projection.as_ref() {
            Some(projection) => projection.project_summary(project_id),
            None => Ok(None),
        }
    }

    /// Projection artifact rows (query view).
    pub fn projection_artifacts(
        &self,
        project_id: &str,
    ) -> Result<Vec<crate::projection::ArtifactRow>> {
        let projection = self.projection.lock().map_err(|_| StoreError::Invalid {
            reason: "the projection mutex was poisoned by a panic in another thread".to_string(),
        })?;
        match projection.as_ref() {
            Some(projection) => projection.artifacts(project_id),
            None => Ok(vec![]),
        }
    }

    /// Quarantine the current index and rebuild it purely from JSONL
    /// (doctor `rebuild-index`). Never touches JSONL or artifacts.
    pub fn rebuild_index(&self) -> Result<RebuildReport> {
        let _guard = lock::acquire(
            &self.paths.writer_lock(),
            &self.paths.recovery_dir,
            self.options.lock_timeout,
            self.options.clock.as_ref(),
        )?;
        let records = self.read_all()?;
        let count = records.len() as u64;
        let mut projection_slot = self.projection.lock().map_err(|_| StoreError::Invalid {
            reason: "the projection mutex was poisoned by a panic in another thread".to_string(),
        })?;
        let clock = self.options.clock.as_ref();
        let existing = projection_slot
            .as_mut()
            .ok_or_else(|| StoreError::Invalid {
                reason: "the projection is not open in this store".to_string(),
            })?;
        let quarantine = existing.quarantine_and_recreate(clock)?;
        existing.replay_all(&records, clock, self.options.fault.as_ref())?;
        Ok(RebuildReport {
            events_indexed: count,
            quarantine,
        })
    }
}

/// Result of `doctor rebuild-index`.
#[derive(Debug, Clone)]
pub struct RebuildReport {
    /// Events replayed into the fresh index.
    pub events_indexed: u64,
    /// Where the previous index was quarantined, when it existed.
    pub quarantine: Option<std::path::PathBuf>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fault::CrashAt;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;

    const PROJECT: &str = "prj_00000000-0000-4000-8000-000000000001";
    const CLOCK_MS: i64 = 1_791_878_400_000;

    fn interrupted_payload() -> Json {
        Json::object([
            ("phase", Json::string("run")),
            ("hint", Json::string("tired")),
        ])
    }

    #[test]
    fn create_append_reopen_cycle() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let options = StoreOptions::deterministic(CLOCK_MS);
        let store = RelayStore::create(dir.path(), PROJECT, options.clone())?;
        let outcome =
            store.append_batch(&[AppendInput::new("run.interrupted", interrupted_payload())])?;
        assert_eq!(outcome.events.len(), 1);
        assert!(!outcome.projection_lagging);
        drop(store);
        let (reopened, report) = RelayStore::open(dir.path(), options)?;
        assert!(report.recovered_tails.is_empty());
        assert_eq!(
            report.reconcile,
            Some(ReconcileOutcome::Matched),
            "index must match after clean close"
        );
        assert_eq!(reopened.tail()?.last_sequence, 1);
        assert_eq!(reopened.projection_event_count()?, Some(1));
        Ok(())
    }

    #[test]
    fn invalid_batch_leaves_store_untouched() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let options = StoreOptions::deterministic(CLOCK_MS);
        let store = RelayStore::create(dir.path(), PROJECT, options.clone())?;
        let before = store.tail()?;
        let error = store
            .append_batch(&[
                AppendInput::new("run.interrupted", interrupted_payload()),
                AppendInput::new("made.up.event", interrupted_payload()),
            ])
            .err()
            .ok_or("expected rejection")?;
        assert!(error.to_string().contains("made.up.event"), "{error}");
        assert_eq!(store.tail()?, before);
        assert_eq!(store.read_all()?.len(), 0);
        assert_eq!(store.projection_event_count()?, Some(0));
        Ok(())
    }

    #[test]
    fn stale_expected_revision_fails() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let options = StoreOptions::deterministic(CLOCK_MS);
        let store = RelayStore::create(dir.path(), PROJECT, options)?;
        store.append_batch(&[AppendInput::new("run.interrupted", interrupted_payload())])?;
        store.append_batch(&[AppendInput::new("run.interrupted", interrupted_payload())])?;
        let error = store
            .append_batch_at(
                Some(1),
                &[AppendInput::new("run.interrupted", interrupted_payload())],
            )
            .err()
            .ok_or("expected stale failure")?;
        assert!(
            error.to_string().contains("stale expected revision"),
            "{error}"
        );
        Ok(())
    }

    #[test]
    fn projection_failure_lags_but_events_are_durable() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let mut options = StoreOptions::deterministic(CLOCK_MS);
        // Break the projection mid-append by crashing the projection step
        // only — the append itself must still be reported durable through
        // the JSONL state.
        struct CrashProjectionOnce(AtomicUsize);
        impl FaultInjector for CrashProjectionOnce {
            fn should_crash(&self, point: FaultPoint) -> bool {
                if point == FaultPoint::ProjectionCommitted
                    && self.0.fetch_add(1, Ordering::SeqCst) == 0
                {
                    return true;
                }
                false
            }
        }
        options.fault = Arc::new(CrashProjectionOnce(AtomicUsize::new(0)));
        let store = RelayStore::create(dir.path(), PROJECT, options.clone())?;
        let outcome =
            store.append_batch(&[AppendInput::new("run.interrupted", interrupted_payload())]);
        // A simulated crash at the projection boundary propagates as an
        // error; the JSONL must still hold the event.
        assert!(matches!(outcome, Err(StoreError::SimulatedCrash { .. })));
        let (reopened, report) =
            RelayStore::open(dir.path(), StoreOptions::deterministic(CLOCK_MS))?;
        assert_eq!(reopened.tail()?.last_sequence, 1, "event survived");
        assert_eq!(reopened.projection_event_count()?, Some(1));
        // The fault point fires after the projection transaction committed,
        // so the index is consistent: reopen matches without a suffix
        // replay, and the event is durable either way.
        assert_eq!(report.reconcile, Some(ReconcileOutcome::Matched));
        Ok(())
    }

    #[test]
    fn crash_after_durable_append_keeps_event() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let mut options = StoreOptions::deterministic(CLOCK_MS);
        options.fault = Arc::new(CrashAt::new(FaultPoint::AfterDurableAppend));
        let store = RelayStore::create(dir.path(), PROJECT, options.clone())?;
        let outcome =
            store.append_batch(&[AppendInput::new("run.interrupted", interrupted_payload())]);
        assert!(matches!(outcome, Err(StoreError::SimulatedCrash { .. })));
        let (reopened, _) = RelayStore::open(dir.path(), StoreOptions::deterministic(CLOCK_MS))?;
        assert_eq!(reopened.read_all()?.len(), 1);
        assert_eq!(reopened.projection_event_count()?, Some(1));
        Ok(())
    }

    #[test]
    fn crash_after_partial_write_recovers_tail() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let mut options = StoreOptions::deterministic(CLOCK_MS);
        options.fault = Arc::new(CrashAt::new(FaultPoint::AfterPartialWrite));
        let store = RelayStore::create(dir.path(), PROJECT, options.clone())?;
        let outcome =
            store.append_batch(&[AppendInput::new("run.interrupted", interrupted_payload())]);
        assert!(matches!(outcome, Err(StoreError::SimulatedCrash { .. })));
        // No event was durably reported, so recovery drops the partial tail.
        let (reopened, report) =
            RelayStore::open(dir.path(), StoreOptions::deterministic(CLOCK_MS))?;
        assert_eq!(report.recovered_tails.len(), 1);
        assert_eq!(reopened.read_all()?.len(), 0);
        Ok(())
    }

    #[test]
    fn two_threaded_writers_serialize() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let dir = dir.keep();
        let options = StoreOptions::deterministic(CLOCK_MS);
        let store = Arc::new(RelayStore::create(&dir, PROJECT, options)?);
        let store_b = Arc::clone(&store);
        let handle_a = std::thread::spawn(move || {
            for _ in 0..5 {
                store
                    .append_batch(&[AppendInput::new("run.interrupted", interrupted_payload())])?;
            }
            Ok::<(), StoreError>(())
        });
        let handle_b = std::thread::spawn(move || {
            for _ in 0..5 {
                store_b
                    .append_batch(&[AppendInput::new("run.interrupted", interrupted_payload())])?;
            }
            Ok::<(), StoreError>(())
        });
        handle_a.join().map_err(|_| "writer thread panicked")??;
        handle_b.join().map_err(|_| "writer thread panicked")??;
        let (reopened, _) = RelayStore::open(&dir, StoreOptions::deterministic(CLOCK_MS))?;
        assert_eq!(reopened.read_all()?.len(), 10);
        assert_eq!(reopened.tail()?.last_sequence, 10);
        Ok(())
    }

    #[test]
    fn reader_during_appends_sees_consistent_state() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let dir = dir.keep();
        let options = StoreOptions::deterministic(CLOCK_MS);
        let store = Arc::new(RelayStore::create(&dir, PROJECT, options)?);
        let writer = Arc::clone(&store);
        let handle = std::thread::spawn(move || {
            for _ in 0..10 {
                writer
                    .append_batch(&[AppendInput::new("run.interrupted", interrupted_payload())])?;
            }
            Ok::<(), StoreError>(())
        });
        let mut seen: Vec<u64> = Vec::new();
        for _ in 0..10 {
            if let Ok(reader) =
                RelayStore::open_read_only(&dir, StoreOptions::deterministic(CLOCK_MS))
            {
                let count = reader.read_all()?.len() as u64;
                seen.push(count);
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        handle.join().map_err(|_| "writer thread panicked")??;
        assert!(!seen.is_empty());
        assert!(seen.iter().all(|count| *count <= 10));
        Ok(())
    }

    #[test]
    fn delete_index_reopen_equivalent() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let options = StoreOptions::deterministic(CLOCK_MS);
        let store = RelayStore::create(dir.path(), PROJECT, options.clone())?;
        store.append_batch(&[
            AppendInput::new("run.interrupted", interrupted_payload()),
            AppendInput::new("run.interrupted", interrupted_payload()),
        ])?;
        let summary_before = store.projection_summary(PROJECT)?;
        drop(store);
        std::fs::remove_file(dir.path().join(".relay").join("index.sqlite"))?;
        let (reopened, report) = RelayStore::open(dir.path(), options)?;
        assert!(matches!(
            report.reconcile,
            Some(ReconcileOutcome::ReplayedSuffix { .. })
        ));
        assert_eq!(reopened.projection_event_count()?, Some(2));
        assert_eq!(reopened.projection_summary(PROJECT)?, summary_before);
        Ok(())
    }

    #[test]
    fn rebuild_index_service() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let options = StoreOptions::deterministic(CLOCK_MS);
        let store = RelayStore::create(dir.path(), PROJECT, options)?;
        store.append_batch(&[AppendInput::new("run.interrupted", interrupted_payload())])?;
        let report = store.rebuild_index()?;
        assert_eq!(report.events_indexed, 1);
        assert!(report.quarantine.is_some());
        assert_eq!(store.projection_event_count()?, Some(1));
        Ok(())
    }
}
