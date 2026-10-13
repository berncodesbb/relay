//! The fact-store orchestrator: fact log + writer lock + projection
//! (`docs/plans/fact-format-proposal.md`).
//!
//! [`FactStore`] executes the append protocol end to end: acquire the
//! writer lock, re-derive the tail, seal and validate the whole batch,
//! append one buffered write, fsync (in durable mode), rotate when
//! thresholds are crossed, fold the projection, release the lock. The
//! user-visible success boundary is the durable JSONL append: a projection
//! failure after it leaves the fact durable and the projection lagging,
//! and the next open replays the missing suffix.
//!
//! The lock is held only for storage operations — never across model calls
//! or long-running tools. The domain enters through two injected seams:
//! a [`FactValidator`] (type/version acceptance) and a [`FactFold`]
//! (projection fold); storage itself knows no domain types.

use std::sync::Arc;
use std::sync::Mutex;

use crate::clock::Clock;
use crate::clock::FixedClock;
use crate::clock::SystemClock;
use crate::error::Result;
use crate::error::StoreError;
use crate::fact::log::FactLog;
use crate::fact::log::FactTail;
use crate::fact::paths::FactPaths;
use crate::fact::projection::FactFold;
use crate::fact::projection::FactProjection;
use crate::fact::projection::FactReconcileOutcome;
use crate::fact::projection::NoFold;
use crate::fact::record::FactEntry;
use crate::fact::record::FactInput;
use crate::fact::record::FactRecord;
use crate::fact::record::Integrity;
use crate::fact::validator::AcceptAllFacts;
use crate::fact::validator::FactValidator;
use crate::fault::FaultInjector;
use crate::fault::FaultPoint;
use crate::fault::NoFaults;
use crate::lock;
use crate::log::Durability;
use crate::log::OpenMode;
use crate::log::RecoveredTail;

/// Fact-store behavior configuration. Clocks and fault injection are
/// injected boundary data so replay and tests stay deterministic.
#[derive(Clone)]
pub struct FactStoreOptions {
    /// Append durability mode.
    pub durability: Durability,
    /// Writer-lock acquisition timeout.
    pub lock_timeout: std::time::Duration,
    /// Time source.
    pub clock: Arc<dyn Clock>,
    /// Crash-injection hooks for tests.
    pub fault: Arc<dyn FaultInjector>,
}

impl Default for FactStoreOptions {
    fn default() -> FactStoreOptions {
        FactStoreOptions {
            durability: Durability::Durable,
            lock_timeout: crate::lock::DEFAULT_LOCK_TIMEOUT,
            clock: Arc::new(SystemClock),
            fault: Arc::new(NoFaults),
        }
    }
}

impl FactStoreOptions {
    /// Options for deterministic tests: a fixed clock at `epoch_millis`.
    pub fn deterministic(epoch_millis: i64) -> FactStoreOptions {
        FactStoreOptions {
            clock: Arc::new(FixedClock::new(epoch_millis)),
            ..FactStoreOptions::default()
        }
    }
}

/// What an append did.
#[derive(Debug, Clone)]
pub struct FactAppendOutcome {
    /// The appended records, exactly as a later read validates them.
    pub records: Vec<FactRecord>,
    /// Whether the projection could not be updated (the facts are durable;
    /// the next open replays the suffix).
    pub projection_lagging: bool,
    /// Whether the append triggered a segment rotation.
    pub rotated: bool,
}

/// What opening a fact store did.
#[derive(Debug, Clone, Default)]
pub struct FactOpenReport {
    /// Tail recoveries performed.
    pub recovered_tails: Vec<RecoveredTail>,
    /// Projection reconciliation outcome.
    pub reconcile: Option<FactReconcileOutcome>,
}

/// Result of a projection rebuild.
#[derive(Debug, Clone)]
pub struct FactRebuildReport {
    /// Facts replayed into the fresh projection.
    pub facts_indexed: u64,
    /// Where the previous database was quarantined, when it existed.
    pub quarantine: Option<std::path::PathBuf>,
}

/// A fact store: segmented canonical JSONL guarded by a writer lock, plus a
/// disposable SQLite projection folded by a domain-supplied callback. Share
/// via `Arc`; internal mutability is behind mutexes, and cross-process
/// writers serialize through the file lock.
pub struct FactStore {
    paths: FactPaths,
    log: Mutex<FactLog>,
    projection: Mutex<Option<FactProjection>>,
    options: FactStoreOptions,
}

impl FactStore {
    /// Create a new fact store for `stream_id` under
    /// `<workspace_root>/.relay-graph` with the given integrity level.
    pub fn create(
        workspace_root: &std::path::Path,
        stream_id: &str,
        integrity: Integrity,
        options: FactStoreOptions,
    ) -> Result<FactStore> {
        let paths = FactPaths::new(workspace_root);
        FactStore::create_at(&paths, stream_id, integrity, options)
    }

    /// Create a new fact store at an explicit root (migration staging).
    pub fn create_at(
        paths: &FactPaths,
        stream_id: &str,
        integrity: Integrity,
        options: FactStoreOptions,
    ) -> Result<FactStore> {
        let log = FactLog::init(paths, stream_id, integrity, options.clock.as_ref())?;
        let projection = FactProjection::open(
            &paths.projection_db,
            &paths.recovery_dir,
            options.clock.as_ref(),
        )?;
        Ok(FactStore {
            paths: paths.clone(),
            log: Mutex::new(log),
            projection: Mutex::new(Some(projection)),
            options,
        })
    }

    /// Open an existing fact store (writer mode): validate everything,
    /// recover a crash tail, reconcile the projection — all under the
    /// writer lock.
    pub fn open(
        workspace_root: &std::path::Path,
        options: FactStoreOptions,
        validator: &dyn FactValidator,
        fold: &dyn FactFold,
    ) -> Result<(FactStore, FactOpenReport)> {
        let paths = FactPaths::new(workspace_root);
        FactStore::open_at(&paths, options, validator, fold)
    }

    /// Open an existing fact store at an explicit root (migration staging).
    pub fn open_at(
        paths: &FactPaths,
        options: FactStoreOptions,
        validator: &dyn FactValidator,
        fold: &dyn FactFold,
    ) -> Result<(FactStore, FactOpenReport)> {
        let _guard = lock::acquire(
            &paths.writer_lock(),
            &paths.recovery_dir,
            options.lock_timeout,
            options.clock.as_ref(),
        )?;
        let (log, recovered_tails) =
            FactLog::open(paths, OpenMode::Writer, options.clock.as_ref(), validator)?;
        let entries = log.read_all(validator)?;
        let tail = log.tail().clone();
        let mut projection = FactProjection::open(
            &paths.projection_db,
            &paths.recovery_dir,
            options.clock.as_ref(),
        )?;
        let reconcile = projection.reconcile(
            &entries,
            &tail,
            options.clock.as_ref(),
            options.fault.as_ref(),
            fold,
        )?;
        drop(_guard);
        Ok((
            FactStore {
                paths: paths.clone(),
                log: Mutex::new(log),
                projection: Mutex::new(Some(projection)),
                options,
            },
            FactOpenReport {
                recovered_tails,
                reconcile: Some(reconcile),
            },
        ))
    }

    /// Open read-only: validate the log without writing. An incomplete
    /// active tail (an append in progress) is ignored; the projection is
    /// not opened (JSONL is authoritative).
    pub fn open_read_only(
        workspace_root: &std::path::Path,
        options: FactStoreOptions,
        validator: &dyn FactValidator,
    ) -> Result<FactStore> {
        let paths = FactPaths::new(workspace_root);
        let (log, _recoveries) =
            FactLog::open(&paths, OpenMode::Reader, options.clock.as_ref(), validator)?;
        Ok(FactStore {
            paths,
            log: Mutex::new(log),
            projection: Mutex::new(None),
            options,
        })
    }

    /// Append a batch with the full protocol. Every input passes the domain
    /// gate before anything is written; an invalid member aborts with the
    /// store untouched.
    pub fn append_facts(
        &self,
        inputs: &[FactInput],
        validator: &dyn FactValidator,
        fold: &dyn FactFold,
    ) -> Result<FactAppendOutcome> {
        self.append_facts_at(None, inputs, validator, fold)
    }

    /// Append a batch that expects the log to currently end at
    /// `expected_last_sequence`. A stale expectation fails with an
    /// actionable error instead of racing another writer.
    pub fn append_facts_at(
        &self,
        expected_last_sequence: Option<u64>,
        inputs: &[FactInput],
        validator: &dyn FactValidator,
        fold: &dyn FactFold,
    ) -> Result<FactAppendOutcome> {
        if inputs.is_empty() {
            return Ok(FactAppendOutcome {
                records: vec![],
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
            reason: "the fact log mutex was poisoned by a panic in another thread".to_string(),
        })?;
        log.refresh(self.options.clock.as_ref(), validator)?;
        if let Some(expected) = expected_last_sequence {
            let current = log.tail().last_sequence;
            if current != expected {
                return Err(StoreError::Append {
                    reason: format!(
                        "stale expected revision: this writer expected the stream at sequence \
                         {expected}, but the log has advanced to sequence {current} (another \
                         writer appended). Reload the state and retry the operation."
                    ),
                });
            }
        }

        // Seal and validate the whole batch before writing anything: an
        // invalid member aborts with the store untouched.
        let chained = log.integrity() == Integrity::Chained;
        let mut records = Vec::with_capacity(inputs.len());
        let first_sequence = log.tail().last_sequence + 1;
        let mut previous = log.tail().last_checksum.clone();
        for (offset, input) in inputs.iter().enumerate() {
            validator
                .validate_input(input)
                .map_err(|reason| StoreError::Append { reason })?;
            let sequence = first_sequence + offset as u64;
            let recorded_at = input
                .recorded_at
                .clone()
                .unwrap_or_else(|| self.options.clock.now_iso());
            let link = if chained { previous.clone() } else { None };
            let record = FactRecord::seal(input, sequence, &recorded_at, link)?;
            crate::fact::validator::check_append(validator, &record)?;
            record.validate()?;
            if chained {
                previous = Some(record.checksum.clone());
            }
            records.push(record);
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
            &records,
            self.options.durability,
            self.options.fault.as_ref(),
        )?;
        drop(log);

        // Projection. The durable JSONL append above is the success
        // boundary; a real projection failure is reported as a lagging
        // index, while a simulated crash propagates so tests can observe
        // recovery from exactly that point.
        let mut projection_slot = self.projection.lock().map_err(|_| StoreError::Invalid {
            reason: "the projection mutex was poisoned by a panic in another thread".to_string(),
        })?;
        let mut projection_lagging = false;
        if let Some(projection) = projection_slot.as_mut() {
            if let Err(error) = projection.apply_records(
                &records,
                self.options.clock.as_ref(),
                self.options.fault.as_ref(),
                fold,
            ) {
                if matches!(error, StoreError::SimulatedCrash { .. }) {
                    return Err(error);
                }
                projection_lagging = true;
            }
        } else {
            projection_lagging = true;
        }
        Ok(FactAppendOutcome {
            records,
            projection_lagging,
            rotated,
        })
    }

    /// Read every validated record from JSONL (authoritative).
    pub fn read_all(&self, validator: &dyn FactValidator) -> Result<Vec<FactEntry>> {
        let log = self.log.lock().map_err(|_| StoreError::Invalid {
            reason: "the fact log mutex was poisoned by a panic in another thread".to_string(),
        })?;
        log.read_all(validator)
    }

    /// The current tail (as of the last open/refresh/append in this
    /// process).
    pub fn tail(&self) -> Result<FactTail> {
        let log = self.log.lock().map_err(|_| StoreError::Invalid {
            reason: "the fact log mutex was poisoned by a panic in another thread".to_string(),
        })?;
        Ok(log.tail().clone())
    }

    /// The opaque stream id this store records.
    pub fn stream_id(&self) -> Result<String> {
        let log = self.log.lock().map_err(|_| StoreError::Invalid {
            reason: "the fact log mutex was poisoned by a panic in another thread".to_string(),
        })?;
        Ok(log.manifest().stream_id.clone())
    }

    /// The integrity level of this stream.
    pub fn integrity(&self) -> Result<Integrity> {
        let log = self.log.lock().map_err(|_| StoreError::Invalid {
            reason: "the fact log mutex was poisoned by a panic in another thread".to_string(),
        })?;
        Ok(log.integrity())
    }

    /// Projection checkpoint, when the projection is open.
    pub fn projection_checkpoint(&self) -> Result<Option<crate::fact::projection::FactCheckpoint>> {
        let projection = self.projection.lock().map_err(|_| StoreError::Invalid {
            reason: "the projection mutex was poisoned by a panic in another thread".to_string(),
        })?;
        match projection.as_ref() {
            Some(projection) => projection.checkpoint(),
            None => Ok(None),
        }
    }

    /// Number of indexed facts in the projection.
    pub fn projection_record_count(&self) -> Result<Option<u64>> {
        let projection = self.projection.lock().map_err(|_| StoreError::Invalid {
            reason: "the projection mutex was poisoned by a panic in another thread".to_string(),
        })?;
        match projection.as_ref() {
            Some(projection) => projection.record_count().map(Some),
            None => Ok(None),
        }
    }

    /// Run a query against the projection's SQLite connection. This is the
    /// escape hatch for domain-specific lookup queries (see
    /// [`crate::fact::graph`]): storage owns the connection, callers own
    /// the SQL they write against the fold's tables. The projection mutex
    /// is held for the duration of the query — keep queries bounded.
    /// Fails when the projection is not open (read-only stores).
    pub fn projection_query<F, T>(&self, query: F) -> Result<T>
    where
        F: FnOnce(&rusqlite::Connection) -> Result<T>,
    {
        let projection = self.projection.lock().map_err(|_| StoreError::Invalid {
            reason: "the projection mutex was poisoned by a panic in another thread".to_string(),
        })?;
        match projection.as_ref() {
            Some(projection) => projection.query(query),
            None => Err(StoreError::Invalid {
                reason: "the projection is not open in this store (read-only stores have no \
                         projection; derive state from the fact log instead)"
                    .to_string(),
            }),
        }
    }

    /// Quarantine the current projection and rebuild it purely from the
    /// fact log. Never touches the JSONL log.
    pub fn rebuild_projection(
        &self,
        validator: &dyn FactValidator,
        fold: &dyn FactFold,
    ) -> Result<FactRebuildReport> {
        let _guard = lock::acquire(
            &self.paths.writer_lock(),
            &self.paths.recovery_dir,
            self.options.lock_timeout,
            self.options.clock.as_ref(),
        )?;
        let entries = self.read_all(validator)?;
        let count = entries.len() as u64;
        let mut projection_slot = self.projection.lock().map_err(|_| StoreError::Invalid {
            reason: "the projection mutex was poisoned by a panic in another thread".to_string(),
        })?;
        let existing = projection_slot
            .as_mut()
            .ok_or_else(|| StoreError::Invalid {
                reason: "the projection is not open in this store".to_string(),
            })?;
        let quarantine = existing.quarantine_and_recreate(self.options.clock.as_ref())?;
        existing.replay_all(
            &entries,
            self.options.clock.as_ref(),
            self.options.fault.as_ref(),
            fold,
        )?;
        Ok(FactRebuildReport {
            facts_indexed: count,
            quarantine,
        })
    }
}

impl FactStore {
    /// Convenience: open or create with the accepting validator and no
    /// domain fold (generic use and tests).
    pub fn open_or_create_generic(
        workspace_root: &std::path::Path,
        stream_id: &str,
        integrity: Integrity,
        options: FactStoreOptions,
    ) -> Result<FactStore> {
        match FactStore::open(workspace_root, options.clone(), &AcceptAllFacts, &NoFold) {
            Ok((store, _)) => Ok(store),
            Err(StoreError::Manifest { .. }) => {
                FactStore::create(workspace_root, stream_id, integrity, options)
            }
            Err(other) => Err(other),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canonical::Json;

    const STREAM: &str = "prj_00000000-0000-4000-8000-000000000001";
    const CLOCK_MS: i64 = 1_791_878_400_000;

    fn goal_input(n: u64) -> FactInput {
        FactInput::new(
            "goal.declared",
            1,
            Json::object([("n", Json::UInt(n)), ("goal", Json::string("Ship it"))]),
        )
    }

    #[test]
    fn create_append_reopen_cycle() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let options = FactStoreOptions::deterministic(CLOCK_MS);
        let store = FactStore::create(dir.path(), STREAM, Integrity::Checksums, options.clone())?;
        let outcome = store.append_facts(&[goal_input(1)], &AcceptAllFacts, &NoFold)?;
        assert_eq!(outcome.records.len(), 1);
        assert!(!outcome.projection_lagging);
        drop(store);
        let (reopened, report) = FactStore::open(dir.path(), options, &AcceptAllFacts, &NoFold)?;
        assert!(report.recovered_tails.is_empty());
        assert_eq!(
            report.reconcile,
            Some(FactReconcileOutcome::Matched),
            "projection must match after clean close"
        );
        assert_eq!(reopened.tail()?.last_sequence, 1);
        assert_eq!(reopened.projection_record_count()?, Some(1));
        assert_eq!(reopened.stream_id()?, STREAM);
        Ok(())
    }

    #[test]
    fn invalid_batch_leaves_store_untouched() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let options = FactStoreOptions::deterministic(CLOCK_MS);
        let store = FactStore::create(dir.path(), STREAM, Integrity::Checksums, options)?;
        let catalog =
            crate::fact::validator::KnownFactVersions::new([("goal.declared".to_string(), 1)]);
        let bad = FactInput::new("made.up.fact", 1, Json::Object(Default::default()));
        let error = store
            .append_facts(&[goal_input(1), bad], &catalog, &NoFold)
            .err()
            .ok_or("expected rejection")?;
        assert!(error.to_string().contains("made.up.fact"), "{error}");
        assert_eq!(store.tail()?.last_sequence, 0);
        assert_eq!(store.read_all(&AcceptAllFacts)?.len(), 0);
        assert_eq!(store.projection_record_count()?, Some(0));
        Ok(())
    }

    #[test]
    fn stale_expected_revision_fails() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let options = FactStoreOptions::deterministic(CLOCK_MS);
        let store = FactStore::create(dir.path(), STREAM, Integrity::Checksums, options)?;
        store.append_facts(&[goal_input(1)], &AcceptAllFacts, &NoFold)?;
        store.append_facts(&[goal_input(2)], &AcceptAllFacts, &NoFold)?;
        let error = store
            .append_facts_at(Some(1), &[goal_input(3)], &AcceptAllFacts, &NoFold)
            .err()
            .ok_or("expected stale failure")?;
        assert!(
            error.to_string().contains("stale expected revision"),
            "{error}"
        );
        Ok(())
    }

    #[test]
    fn crash_after_durable_append_keeps_fact() -> Result<(), Box<dyn std::error::Error>> {
        use crate::fault::CrashAt;
        let dir = tempfile::tempdir()?;
        let mut options = FactStoreOptions::deterministic(CLOCK_MS);
        options.fault = Arc::new(CrashAt::new(FaultPoint::AfterDurableAppend));
        let store = FactStore::create(dir.path(), STREAM, Integrity::Checksums, options)?;
        let outcome = store.append_facts(&[goal_input(1)], &AcceptAllFacts, &NoFold);
        assert!(matches!(outcome, Err(StoreError::SimulatedCrash { .. })));
        let (reopened, _) = FactStore::open(
            dir.path(),
            FactStoreOptions::deterministic(CLOCK_MS),
            &AcceptAllFacts,
            &NoFold,
        )?;
        assert_eq!(reopened.read_all(&AcceptAllFacts)?.len(), 1);
        assert_eq!(reopened.projection_record_count()?, Some(1));
        Ok(())
    }

    #[test]
    fn crash_after_partial_write_recovers_tail() -> Result<(), Box<dyn std::error::Error>> {
        use crate::fault::CrashAt;
        let dir = tempfile::tempdir()?;
        let mut options = FactStoreOptions::deterministic(CLOCK_MS);
        options.fault = Arc::new(CrashAt::new(FaultPoint::AfterPartialWrite));
        let store = FactStore::create(dir.path(), STREAM, Integrity::Checksums, options)?;
        let outcome = store.append_facts(&[goal_input(1)], &AcceptAllFacts, &NoFold);
        assert!(matches!(outcome, Err(StoreError::SimulatedCrash { .. })));
        // Nothing was reported durable, so recovery drops the partial tail.
        let (reopened, report) = FactStore::open(
            dir.path(),
            FactStoreOptions::deterministic(CLOCK_MS),
            &AcceptAllFacts,
            &NoFold,
        )?;
        assert_eq!(report.recovered_tails.len(), 1);
        assert_eq!(reopened.read_all(&AcceptAllFacts)?.len(), 0);
        // The store remains usable.
        reopened.append_facts(&[goal_input(1)], &AcceptAllFacts, &NoFold)?;
        assert_eq!(reopened.tail()?.last_sequence, 1);
        Ok(())
    }

    #[test]
    fn chained_stream_appends_through_the_store() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let options = FactStoreOptions::deterministic(CLOCK_MS);
        let store = FactStore::create(dir.path(), STREAM, Integrity::Chained, options.clone())?;
        store.append_facts(&[goal_input(1), goal_input(2)], &AcceptAllFacts, &NoFold)?;
        store.append_facts(&[goal_input(3)], &AcceptAllFacts, &NoFold)?;
        let (reopened, _) = FactStore::open(dir.path(), options, &AcceptAllFacts, &NoFold)?;
        let entries = reopened.read_all(&AcceptAllFacts)?;
        assert_eq!(entries.len(), 3);
        for window in entries.windows(2) {
            assert_eq!(
                window[1].record.previous.as_deref(),
                Some(window[0].record.checksum.as_str())
            );
        }
        Ok(())
    }

    #[test]
    fn rebuild_projection_service() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let options = FactStoreOptions::deterministic(CLOCK_MS);
        let store = FactStore::create(dir.path(), STREAM, Integrity::Checksums, options)?;
        store.append_facts(&[goal_input(1), goal_input(2)], &AcceptAllFacts, &NoFold)?;
        let report = store.rebuild_projection(&AcceptAllFacts, &NoFold)?;
        assert_eq!(report.facts_indexed, 2);
        assert!(report.quarantine.is_some());
        assert_eq!(store.projection_record_count()?, Some(2));
        Ok(())
    }

    #[test]
    fn open_or_create_generic_creates_then_reopens() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let store = FactStore::open_or_create_generic(
            dir.path(),
            STREAM,
            Integrity::Checksums,
            FactStoreOptions::deterministic(CLOCK_MS),
        )?;
        assert_eq!(store.tail()?.last_sequence, 0);
        store.append_facts(&[goal_input(1)], &AcceptAllFacts, &NoFold)?;
        drop(store);
        // The second call opens the existing store rather than failing.
        let reopened = FactStore::open_or_create_generic(
            dir.path(),
            STREAM,
            Integrity::Checksums,
            FactStoreOptions::deterministic(CLOCK_MS),
        )?;
        assert_eq!(reopened.tail()?.last_sequence, 1);
        assert_eq!(reopened.stream_id()?, STREAM);
        Ok(())
    }

    #[test]
    fn two_threaded_writers_serialize() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let dir = dir.keep();
        let options = FactStoreOptions::deterministic(CLOCK_MS);
        let store = Arc::new(FactStore::create(
            &dir,
            STREAM,
            Integrity::Checksums,
            options,
        )?);
        let store_b = Arc::clone(&store);
        let handle_a = std::thread::spawn(move || {
            for n in 0..5 {
                store.append_facts(&[goal_input(n)], &AcceptAllFacts, &NoFold)?;
            }
            Ok::<(), StoreError>(())
        });
        let handle_b = std::thread::spawn(move || {
            for n in 5..10 {
                store_b.append_facts(&[goal_input(n)], &AcceptAllFacts, &NoFold)?;
            }
            Ok::<(), StoreError>(())
        });
        handle_a.join().map_err(|_| "writer thread panicked")??;
        handle_b.join().map_err(|_| "writer thread panicked")??;
        let (reopened, _) = FactStore::open(
            &dir,
            FactStoreOptions::deterministic(CLOCK_MS),
            &AcceptAllFacts,
            &NoFold,
        )?;
        assert_eq!(reopened.read_all(&AcceptAllFacts)?.len(), 10);
        assert_eq!(reopened.tail()?.last_sequence, 10);
        Ok(())
    }
}
