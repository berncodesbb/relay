//! SQLite projection over the fact log
//! (`docs/plans/fact-format-proposal.md`).
//!
//! `projection.sqlite` is derived, disposable state: the JSONL fact log is
//! the sole authority. This module provides the **rebuild primitive**:
//!
//! - storage owns a generic `facts` table (every record, canonical payload
//!   text, checksum) and a one-row `checkpoint` (last indexed sequence +
//!   checksum);
//! - domain-specific folds are plugged in through [`FactFold`] and run
//!   **inside the same transaction** as the storage rows and the
//!   checkpoint, so every commit leaves the database describing exactly
//!   one prefix of the log;
//! - the database can be deleted (or quarantined) at any time and rebuilt
//!   purely by replaying the log through the fold.
//!
//! Storage never depends on domain types: the fold receives the opaque
//! [`FactRecord`] and a `rusqlite` transaction, and owns its own tables.

use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

use rusqlite::Connection;
use rusqlite::OptionalExtension;
use rusqlite::params;

use crate::clock::Clock;
use crate::error::Result;
use crate::error::StoreError;
use crate::fact::canonical;
use crate::fact::log::FactTail;
use crate::fact::record::FactEntry;
use crate::fact::record::FactRecord;
use crate::fault::FaultInjector;
use crate::fault::FaultPoint;

/// Storage-owned projection schema migrations, forward-only. Each runs once,
/// recorded in `fact_schema_migrations` inside the same transaction. The
/// projection schema version is independent of the fact format version.
const MIGRATIONS: &[(i64, &str, &str)] = &[(
    1,
    "initial-fact-projections",
    r#"
CREATE TABLE facts (
  sequence INTEGER PRIMARY KEY,
  type TEXT NOT NULL,
  fact_version INTEGER NOT NULL,
  recorded_at TEXT NOT NULL,
  previous TEXT,
  checksum TEXT NOT NULL,
  payload TEXT NOT NULL
);
CREATE INDEX facts_type ON facts (type);
CREATE INDEX facts_recorded_at ON facts (recorded_at);

CREATE TABLE fact_checkpoint (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  last_sequence INTEGER NOT NULL,
  last_checksum TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
"#,
)];

/// Records applied per transaction while replaying or rebuilding.
const CHUNK: usize = 500;

/// The projection checkpoint: the sequence + checksum of the last indexed
/// fact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactCheckpoint {
    /// Sequence of the last indexed fact.
    pub last_sequence: u64,
    /// Checksum of that fact.
    pub last_checksum: String,
}

/// A domain-specific projection fold. Implementations are stateless — all
/// state lives in the database — and must be expressible as a pure function
/// of the fact stream so a rebuild from JSONL converges to the same rows.
pub trait FactFold: Send + Sync {
    /// Create the fold's tables and indexes if they do not exist. Runs once
    /// per connection before the first apply, inside a transaction.
    fn prepare(&self, tx: &rusqlite::Transaction<'_>) -> Result<()> {
        let _ = tx;
        Ok(())
    }

    /// Fold one record into the domain tables. Runs inside the same
    /// transaction as the storage-owned `facts` row and the checkpoint, so
    /// a crash either applies the whole record or nothing.
    fn apply(&self, tx: &rusqlite::Transaction<'_>, record: &FactRecord) -> Result<()>;
}

/// A fold that adds nothing beyond the storage-owned `facts` table.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoFold;

impl FactFold for NoFold {
    fn apply(&self, _tx: &rusqlite::Transaction<'_>, _record: &FactRecord) -> Result<()> {
        Ok(())
    }
}

/// What open-time reconciliation did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FactReconcileOutcome {
    /// The checkpoint matches the log tail; nothing to do.
    Matched,
    /// The checkpoint was behind; the missing suffix was replayed.
    ReplayedSuffix {
        /// First replayed sequence.
        from: u64,
        /// Last replayed sequence.
        to: u64,
    },
    /// The database was missing, corrupt, ahead, divergent, or on a newer
    /// schema; it was quarantined into `recovery/` (when it existed) and
    /// rebuilt from the fact log.
    Rebuilt {
        /// Why.
        reason: String,
        /// Where the old database was moved, when it existed.
        quarantine: Option<PathBuf>,
    },
}

/// The SQLite projection over one fact store.
#[derive(Debug)]
pub struct FactProjection {
    conn: Option<Connection>,
    path: PathBuf,
    recovery_dir: PathBuf,
    fold_prepared: bool,
}

impl FactProjection {
    /// Open (creating if necessary) the projection database, applying
    /// durability pragmas and forward-only migrations. A database that is
    /// unreadable or on a newer schema is quarantined into `recovery/` and
    /// recreated empty; reconciliation decides what to replay.
    pub fn open(path: &Path, recovery_dir: &Path, clock: &dyn Clock) -> Result<FactProjection> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| StoreError::Io {
                context: "creating the projection directory".to_string(),
                path: parent.to_path_buf(),
                source: error,
            })?;
        }
        let mut projection = FactProjection {
            conn: None,
            path: path.to_path_buf(),
            recovery_dir: recovery_dir.to_path_buf(),
            fold_prepared: false,
        };
        projection.connect()?;
        if let Err(reason) = projection.probe(clock) {
            // Unreadable or newer-schema database: quarantine and recreate.
            let _ = projection.conn.take();
            let _ = quarantine_projection_files(&projection.path, &projection.recovery_dir, clock);
            projection.connect()?;
            projection
                .probe(clock)
                .map_err(|error| StoreError::Projection {
                    reason: format!(
                        "the projection database at {} could not be initialized after quarantining \
                     the previous file ({reason}; retry error: {error})",
                        path.display()
                    ),
                    source: None,
                })?;
        }
        Ok(projection)
    }

    fn connect(&mut self) -> Result<()> {
        let conn = Connection::open(&self.path)?;
        conn.busy_timeout(Duration::from_millis(5000))?;
        let _ = conn.pragma_update(None, "journal_mode", "WAL");
        let _ = conn.pragma_update(None, "foreign_keys", "ON");
        let _ = conn.pragma_update(None, "synchronous", "NORMAL");
        self.conn = Some(conn);
        self.fold_prepared = false;
        Ok(())
    }

    /// Integrity + schema probe and migration application.
    fn probe(&mut self, clock: &dyn Clock) -> Result<()> {
        let conn = self.connection()?;
        let integrity: String = conn
            .query_row("PRAGMA integrity_check;", [], |row| row.get(0))
            .map_err(|error| StoreError::Projection {
                reason: "running PRAGMA integrity_check".to_string(),
                source: Some(error),
            })?;
        if integrity != "ok" {
            return Err(StoreError::Projection {
                reason: format!("integrity_check reported: {integrity}"),
                source: None,
            });
        }
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS fact_schema_migrations (
              id INTEGER PRIMARY KEY,
              name TEXT NOT NULL,
              applied_at TEXT NOT NULL
            );",
        )?;
        let known_max = MIGRATIONS.iter().map(|m| m.0).max().unwrap_or(0);
        let mut applied: Vec<i64> = Vec::new();
        {
            let mut statement =
                conn.prepare("SELECT id FROM fact_schema_migrations ORDER BY id;")?;
            let rows = statement.query_map([], |row| row.get::<_, i64>(0))?;
            for row in rows {
                applied.push(row?);
            }
        }
        if let Some(newest) = applied.iter().filter(|id| **id > known_max).max() {
            return Err(StoreError::Projection {
                reason: format!(
                    "projection schema version {newest} is newer than this build understands \
                     ({known_max})"
                ),
                source: None,
            });
        }
        for (id, name, up) in MIGRATIONS {
            if applied.iter().any(|applied_id| applied_id == id) {
                continue;
            }
            let tx = conn.unchecked_transaction()?;
            tx.execute_batch(up)?;
            tx.execute(
                "INSERT INTO fact_schema_migrations (id, name, applied_at) VALUES (?, ?, ?);",
                params![id, name, clock.now_iso()],
            )?;
            tx.commit()?;
        }
        Ok(())
    }

    fn connection(&self) -> Result<&Connection> {
        self.conn.as_ref().ok_or_else(|| StoreError::Projection {
            reason: "the projection connection is closed".to_string(),
            source: None,
        })
    }

    /// The current checkpoint, when the database has one.
    pub fn checkpoint(&self) -> Result<Option<FactCheckpoint>> {
        let conn = self.connection()?;
        conn.query_row(
            "SELECT last_sequence, last_checksum FROM fact_checkpoint WHERE id = 1;",
            [],
            |row| {
                Ok(FactCheckpoint {
                    last_sequence: row.get::<_, i64>(0)? as u64,
                    last_checksum: row.get(1)?,
                })
            },
        )
        .optional()
        .map_err(|error| StoreError::Projection {
            reason: "reading the fact checkpoint".to_string(),
            source: Some(error),
        })
    }

    /// Reconcile against the fact-log tail: match, catch up a suffix, or
    /// quarantine-and-rebuild. `entries` is the full validated log in
    /// sequence order.
    pub fn reconcile(
        &mut self,
        entries: &[FactEntry],
        tail: &FactTail,
        clock: &dyn Clock,
        fault: &dyn FaultInjector,
        fold: &dyn FactFold,
    ) -> Result<FactReconcileOutcome> {
        let checkpoint = self.checkpoint()?;
        let (cp_sequence, cp_checksum) = match &checkpoint {
            Some(checkpoint) => (checkpoint.last_sequence, checkpoint.last_checksum.clone()),
            None => (0, String::new()),
        };
        let tail_checksum = tail.last_checksum.clone().unwrap_or_default();
        if cp_sequence == tail.last_sequence
            && (tail.last_sequence == 0 || cp_checksum == tail_checksum)
        {
            return Ok(FactReconcileOutcome::Matched);
        }
        if cp_sequence > tail.last_sequence {
            let quarantine = self.quarantine_and_recreate(clock)?;
            self.replay_all(entries, clock, fault, fold)?;
            return Ok(FactReconcileOutcome::Rebuilt {
                reason: format!(
                    "checkpoint sequence {cp_sequence} is ahead of the fact-log tail {last}; \
                     SQLite is never authoritative",
                    last = tail.last_sequence
                ),
                quarantine,
            });
        }
        if cp_sequence == tail.last_sequence && cp_checksum != tail_checksum {
            let quarantine = self.quarantine_and_recreate(clock)?;
            self.replay_all(entries, clock, fault, fold)?;
            return Ok(FactReconcileOutcome::Rebuilt {
                reason: "checkpoint checksum diverges from the fact-log tail".to_string(),
                quarantine,
            });
        }
        // Behind (or empty): replay the missing suffix. Chunks commit the
        // fold, the storage rows, and the checkpoint atomically, so resuming
        // from the committed checkpoint converges without deletions.
        let suffix: Vec<FactRecord> = entries
            .iter()
            .filter(|entry| entry.record.sequence > cp_sequence)
            .map(|entry| entry.record.clone())
            .collect();
        self.apply_records(&suffix, clock, fault, fold)?;
        Ok(FactReconcileOutcome::ReplayedSuffix {
            from: cp_sequence + 1,
            to: tail.last_sequence,
        })
    }

    /// Apply newly appended records, chunked so every commit is
    /// self-consistent: storage rows, domain fold, and checkpoint commit
    /// together.
    pub fn apply_records(
        &mut self,
        records: &[FactRecord],
        clock: &dyn Clock,
        fault: &dyn FaultInjector,
        fold: &dyn FactFold,
    ) -> Result<()> {
        self.ensure_fold_tables(fold)?;
        for chunk in records.chunks(CHUNK) {
            let conn = self.connection()?;
            let tx = conn.unchecked_transaction()?;
            for record in chunk {
                insert_fact_row(&tx, record)?;
                fold.apply(&tx, record)?;
            }
            if let Some(last) = chunk.last() {
                upsert_checkpoint(&tx, last.sequence, &last.checksum, clock)?;
            }
            tx.commit()?;
            if fault.should_crash(FaultPoint::ProjectionChunkCommitted) {
                return Err(StoreError::SimulatedCrash {
                    point: FaultPoint::ProjectionChunkCommitted.name().to_string(),
                });
            }
        }
        if fault.should_crash(FaultPoint::ProjectionCommitted) {
            return Err(StoreError::SimulatedCrash {
                point: FaultPoint::ProjectionCommitted.name().to_string(),
            });
        }
        Ok(())
    }

    /// Run the fold's `prepare` once per connection.
    fn ensure_fold_tables(&mut self, fold: &dyn FactFold) -> Result<()> {
        if self.fold_prepared {
            return Ok(());
        }
        let conn = self.connection()?;
        let tx = conn.unchecked_transaction()?;
        fold.prepare(&tx)?;
        tx.commit()?;
        self.fold_prepared = true;
        Ok(())
    }

    /// Quarantine the current database (if any) into `recovery/`, recreate
    /// an empty one. Returns the quarantine path when a file was moved.
    pub fn quarantine_and_recreate(&mut self, clock: &dyn Clock) -> Result<Option<PathBuf>> {
        // Closing first releases the WAL handles so renames succeed.
        if let Some(conn) = self.conn.take() {
            let _ = conn.close();
        }
        let quarantine = quarantine_projection_files(&self.path, &self.recovery_dir, clock)?;
        self.connect()?;
        self.probe(clock)?;
        Ok(quarantine)
    }

    /// Replay everything from the fact log into the (fresh) database.
    pub fn replay_all(
        &mut self,
        entries: &[FactEntry],
        clock: &dyn Clock,
        fault: &dyn FaultInjector,
        fold: &dyn FactFold,
    ) -> Result<()> {
        let records: Vec<FactRecord> = entries.iter().map(|e| e.record.clone()).collect();
        self.apply_records(&records, clock, fault, fold)
    }

    /// Number of indexed facts.
    pub fn record_count(&self) -> Result<u64> {
        let conn = self.connection()?;
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM facts;", [], |row| row.get(0))?;
        Ok(count as u64)
    }

    /// Run a caller-supplied query against the connection (the escape hatch
    /// for domain-specific lookup queries over the fold's tables). The
    /// connection is shared — keep queries bounded and read-only.
    pub fn query<F, T>(&self, query: F) -> Result<T>
    where
        F: FnOnce(&Connection) -> Result<T>,
    {
        let conn = self.connection()?;
        query(conn)
    }
}

fn insert_fact_row(tx: &rusqlite::Transaction<'_>, record: &FactRecord) -> Result<()> {
    let payload = canonical::serialize(&record.payload)?;
    tx.execute(
        "INSERT OR REPLACE INTO facts (
          sequence, type, fact_version, recorded_at, previous, checksum, payload
        ) VALUES (?, ?, ?, ?, ?, ?, ?);",
        params![
            record.sequence as i64,
            record.fact_type,
            record.fact_version as i64,
            record.recorded_at,
            record.previous,
            record.checksum,
            payload
        ],
    )?;
    Ok(())
}

fn upsert_checkpoint(
    tx: &rusqlite::Transaction<'_>,
    sequence: u64,
    checksum: &str,
    clock: &dyn Clock,
) -> Result<()> {
    tx.execute(
        "INSERT INTO fact_checkpoint (id, last_sequence, last_checksum, updated_at)
         VALUES (1, ?, ?, ?)
         ON CONFLICT(id) DO UPDATE SET
           last_sequence = excluded.last_sequence,
           last_checksum = excluded.last_checksum,
           updated_at = excluded.updated_at;",
        params![sequence as i64, checksum, clock.now_iso()],
    )?;
    Ok(())
}

/// Move a divergent projection database (and its WAL sidecars) into
/// `recovery/`, parameterized over the database file name.
fn quarantine_projection_files(
    path: &Path,
    recovery_dir: &Path,
    clock: &dyn Clock,
) -> Result<Option<PathBuf>> {
    if !path.exists() {
        return Ok(None);
    }
    std::fs::create_dir_all(recovery_dir).map_err(|error| StoreError::Io {
        context: "creating the recovery directory".to_string(),
        path: recovery_dir.to_path_buf(),
        source: error,
    })?;
    let stamp: String = clock
        .now_iso()
        .chars()
        .filter(|c| *c != '-' && *c != ':' && *c != '.')
        .collect();
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "projection.sqlite".to_string());
    let moved = recovery_dir.join(format!("{file_name}-{stamp}Z.quarantine"));
    std::fs::rename(path, &moved).map_err(|error| StoreError::Io {
        context: "quarantining a divergent projection database".to_string(),
        path: path.to_path_buf(),
        source: error,
    })?;
    for suffix in ["-wal", "-shm"] {
        let sidecar = path.with_file_name(format!("{file_name}{suffix}"));
        if sidecar.exists() {
            let target = recovery_dir.join(format!(
                "{}-{stamp}Z.quarantine{suffix}",
                file_name.strip_suffix(".sqlite").unwrap_or(&file_name)
            ));
            let _ = std::fs::rename(&sidecar, target);
        }
    }
    Ok(Some(moved))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canonical::Json;
    use crate::clock::FixedClock;
    use crate::fact::log::FactLog;
    use crate::fact::paths::FactPaths;
    use crate::fact::record::FactInput;
    use crate::fact::record::Integrity;
    use crate::fact::validator::AcceptAllFacts;
    use crate::fault::NoFaults;
    use crate::log::Durability;

    const STREAM: &str = "prj_00000000-0000-4000-8000-000000000001";
    const CLOCK_MS: i64 = 1_791_878_400_000;

    fn clock() -> FixedClock {
        FixedClock::new(CLOCK_MS)
    }

    /// A toy domain fold: counts records per type in its own table. This is
    /// the shape a real domain projection plugs in with.
    struct TypeCounts;

    impl FactFold for TypeCounts {
        fn prepare(&self, tx: &rusqlite::Transaction<'_>) -> Result<()> {
            tx.execute_batch(
                "CREATE TABLE IF NOT EXISTS type_counts (
                  type TEXT PRIMARY KEY,
                  count INTEGER NOT NULL
                );",
            )?;
            Ok(())
        }

        fn apply(&self, tx: &rusqlite::Transaction<'_>, record: &FactRecord) -> Result<()> {
            tx.execute(
                "INSERT INTO type_counts (type, count) VALUES (?, 1)
                 ON CONFLICT(type) DO UPDATE SET count = count + 1;",
                params![record.fact_type],
            )?;
            Ok(())
        }
    }

    fn sample_input(kind: &str, n: u64) -> FactInput {
        FactInput::new(kind, 1, Json::object([("n", Json::UInt(n))]))
    }

    #[test]
    fn delete_and_rebuild_yields_equivalent_state() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let paths = FactPaths::new(dir.path());
        let mut log = FactLog::init(&paths, STREAM, Integrity::Checksums, &clock())?;
        let inputs = [
            sample_input("goal.declared", 1),
            sample_input("work.completed", 2),
            sample_input("goal.declared", 3),
        ];
        let mut records = Vec::new();
        let first_sequence = log.tail().last_sequence + 1;
        for (offset, input) in inputs.iter().enumerate() {
            let sequence = first_sequence + offset as u64;
            let record = crate::fact::record::FactRecord::seal(
                input,
                sequence,
                "2026-10-13T08:00:00.000Z",
                None,
            )?;
            records.push(record);
        }
        log.append(&records, Durability::Durable, &NoFaults)?;

        let mut projection =
            FactProjection::open(&paths.projection_db, &paths.recovery_dir, &clock())?;
        let entries = log.read_all(&AcceptAllFacts)?;
        projection.replay_all(&entries, &clock(), &NoFaults, &TypeCounts)?;
        let counts_before = type_counts(&projection)?;
        assert_eq!(
            counts_before,
            vec![
                ("goal.declared".to_string(), 2),
                ("work.completed".to_string(), 1),
            ]
        );

        // Delete the projection entirely and rebuild from the fact log.
        drop(projection);
        std::fs::remove_file(&paths.projection_db)?;
        let mut rebuilt =
            FactProjection::open(&paths.projection_db, &paths.recovery_dir, &clock())?;
        rebuilt.replay_all(&entries, &clock(), &NoFaults, &TypeCounts)?;
        assert_eq!(type_counts(&rebuilt)?, counts_before);
        assert_eq!(rebuilt.record_count()?, 3);
        assert_eq!(rebuilt.checkpoint()?.map(|c| c.last_sequence), Some(3));
        Ok(())
    }

    fn type_counts(projection: &FactProjection) -> Result<Vec<(String, i64)>> {
        let conn = projection.connection()?;
        let mut statement = conn.prepare("SELECT type, count FROM type_counts ORDER BY type;")?;
        let rows = statement.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get(1)?)))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    #[test]
    fn reconcile_catches_up_a_suffix() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let paths = FactPaths::new(dir.path());
        let mut log = FactLog::init(&paths, STREAM, Integrity::Checksums, &clock())?;
        let first = vec![crate::fact::record::FactRecord::seal(
            &sample_input("goal.declared", 1),
            1,
            "2026-10-13T08:00:00.000Z",
            None,
        )?];
        log.append(&first, Durability::Durable, &NoFaults)?;

        let mut projection =
            FactProjection::open(&paths.projection_db, &paths.recovery_dir, &clock())?;
        let entries = log.read_all(&AcceptAllFacts)?;
        projection.replay_all(&entries, &clock(), &NoFaults, &TypeCounts)?;
        assert!(matches!(
            projection.reconcile(
                entries.as_slice(),
                log.tail(),
                &clock(),
                &NoFaults,
                &TypeCounts
            )?,
            FactReconcileOutcome::Matched
        ));

        // Append more; the projection is behind and catches up.
        let second = vec![crate::fact::record::FactRecord::seal(
            &sample_input("work.completed", 2),
            2,
            "2026-10-13T08:00:01.000Z",
            None,
        )?];
        log.append(&second, Durability::Durable, &NoFaults)?;
        let entries = log.read_all(&AcceptAllFacts)?;
        assert!(matches!(
            projection.reconcile(
                entries.as_slice(),
                log.tail(),
                &clock(),
                &NoFaults,
                &TypeCounts
            )?,
            FactReconcileOutcome::ReplayedSuffix { from: 2, to: 2 }
        ));
        assert_eq!(projection.record_count()?, 2);
        Ok(())
    }

    #[test]
    fn ahead_checkpoint_quarantines_and_rebuilds() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let paths = FactPaths::new(dir.path());
        let mut log = FactLog::init(&paths, STREAM, Integrity::Checksums, &clock())?;
        let record = crate::fact::record::FactRecord::seal(
            &sample_input("goal.declared", 1),
            1,
            "2026-10-13T08:00:00.000Z",
            None,
        )?;
        log.append(&[record], Durability::Durable, &NoFaults)?;
        let mut projection =
            FactProjection::open(&paths.projection_db, &paths.recovery_dir, &clock())?;
        let entries = log.read_all(&AcceptAllFacts)?;
        projection.replay_all(&entries, &clock(), &NoFaults, &TypeCounts)?;
        // Doctor the checkpoint ahead of the log tail.
        {
            let conn = projection.connection()?;
            conn.execute(
                "UPDATE fact_checkpoint SET last_sequence = 99 WHERE id = 1;",
                [],
            )?;
        }
        assert!(matches!(
            projection.reconcile(
                entries.as_slice(),
                log.tail(),
                &clock(),
                &NoFaults,
                &TypeCounts
            )?,
            FactReconcileOutcome::Rebuilt { .. }
        ));
        assert_eq!(projection.record_count()?, 1);
        Ok(())
    }

    #[test]
    fn chunked_crash_leaves_a_consistent_prefix() -> Result<(), Box<dyn std::error::Error>> {
        use crate::fault::CrashAt;
        let dir = tempfile::tempdir()?;
        let paths = FactPaths::new(dir.path());
        let mut log = FactLog::init(&paths, STREAM, Integrity::Checksums, &clock())?;
        let mut records = Vec::new();
        let first_sequence = log.tail().last_sequence + 1;
        for n in 0..600 {
            let sequence = first_sequence + n;
            records.push(crate::fact::record::FactRecord::seal(
                &sample_input("goal.declared", n),
                sequence,
                "2026-10-13T08:00:00.000Z",
                None,
            )?);
        }
        log.append(&records, Durability::Durable, &NoFaults)?;
        let entries = log.read_all(&AcceptAllFacts)?;

        let mut projection =
            FactProjection::open(&paths.projection_db, &paths.recovery_dir, &clock())?;
        let crash = CrashAt::new(FaultPoint::ProjectionChunkCommitted);
        let outcome = projection.replay_all(&entries, &clock(), &crash, &TypeCounts);
        assert!(matches!(outcome, Err(StoreError::SimulatedCrash { .. })));
        // The committed prefix is self-consistent: count matches checkpoint.
        let checkpoint = projection.checkpoint()?.ok_or("no checkpoint")?;
        assert_eq!(projection.record_count()?, checkpoint.last_sequence);
        // Resuming converges.
        projection.reconcile(
            entries.as_slice(),
            log.tail(),
            &clock(),
            &NoFaults,
            &TypeCounts,
        )?;
        assert_eq!(projection.record_count()?, 600);
        assert_eq!(projection.checkpoint()?.map(|c| c.last_sequence), Some(600));
        Ok(())
    }
}
