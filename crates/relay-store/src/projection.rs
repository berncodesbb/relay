//! SQLite projections (`docs/event-format.md` §9).
//!
//! `index.sqlite` is derived, disposable state: the JSONL log is the sole
//! authority. This module owns the projection schema (forward-only
//! migrations), incremental event folding into query tables, the
//! sequence/hash checkpoint, and open-time reconciliation (match / catch up
//! a suffix / quarantine-and-rebuild). Every committed transaction leaves
//! the database self-consistent: event rows, derived rows, and the
//! checkpoint always describe exactly the same prefix of the log, so a
//! crash at any commit boundary converges on the next open.

use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

use rusqlite::Connection;
use rusqlite::OptionalExtension;
use rusqlite::params;

use crate::canonical::Json;
use crate::clock::Clock;
use crate::envelope::EventEnvelope;
use crate::error::Result;
use crate::error::StoreError;
use crate::fault::FaultInjector;
use crate::fault::FaultPoint;
use crate::fault::NoFaults;
use crate::log::EventRecord;
use crate::log::TailState;

/// Projection schema migrations, forward-only. Each runs once, recorded in
/// `relay_schema_migrations` inside the same transaction. The projection
/// schema version is independent of the JSONL store version.
const MIGRATIONS: &[(i64, &str, &str)] = &[(
    1,
    "initial-projections",
    r#"
CREATE TABLE events (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL,
  sequence INTEGER NOT NULL,
  session_id TEXT,
  type TEXT NOT NULL,
  schema_version INTEGER NOT NULL,
  timestamp TEXT NOT NULL,
  correlation_id TEXT NOT NULL,
  causation_id TEXT,
  payload TEXT NOT NULL,
  hash TEXT NOT NULL,
  UNIQUE (project_id, sequence)
);
CREATE INDEX events_project_type ON events (project_id, type);
CREATE INDEX events_project_timestamp ON events (project_id, timestamp);
CREATE INDEX events_project_session ON events (project_id, session_id);
CREATE INDEX events_project_correlation ON events (project_id, correlation_id);

CREATE TABLE projects_summary (
  project_id TEXT PRIMARY KEY,
  created_at TEXT,
  goal TEXT,
  current_mode TEXT,
  autonomy TEXT,
  paused INTEGER NOT NULL DEFAULT 0,
  artifact_count INTEGER NOT NULL DEFAULT 0,
  verification_passed INTEGER,
  last_event_sequence INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE sessions (
  project_id TEXT NOT NULL,
  session_id TEXT NOT NULL,
  started_at TEXT,
  interactive INTEGER,
  event_count INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (project_id, session_id)
);

CREATE TABLE artifacts (
  project_id TEXT NOT NULL,
  artifact_id TEXT NOT NULL,
  kind TEXT NOT NULL,
  version INTEGER NOT NULL,
  status TEXT NOT NULL,
  path TEXT,
  title TEXT,
  PRIMARY KEY (project_id, artifact_id)
);

CREATE TABLE model_calls (
  project_id TEXT NOT NULL,
  event_id TEXT NOT NULL,
  timestamp TEXT NOT NULL,
  phase TEXT NOT NULL,
  provider TEXT NOT NULL,
  model TEXT NOT NULL,
  purpose TEXT NOT NULL,
  role TEXT,
  input_tokens INTEGER,
  output_tokens INTEGER,
  tool_calls INTEGER,
  error_kind TEXT,
  message TEXT,
  attempts INTEGER,
  duration_ms INTEGER,
  PRIMARY KEY (project_id, event_id)
);

CREATE TABLE tool_calls (
  project_id TEXT NOT NULL,
  event_id TEXT NOT NULL,
  timestamp TEXT NOT NULL,
  tool TEXT NOT NULL,
  capability TEXT NOT NULL,
  mode TEXT,
  outcome TEXT NOT NULL,
  duration_ms INTEGER,
  summary TEXT,
  reason TEXT,
  PRIMARY KEY (project_id, event_id)
);

CREATE TABLE checkpoint (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  last_sequence INTEGER NOT NULL,
  last_hash TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
"#,
)];

/// Events applied per transaction while replaying or rebuilding.
const CHUNK: usize = 500;

/// The projection checkpoint: one row, sequence + hash of the last indexed
/// event (§9).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkpoint {
    /// Sequence of the last indexed event.
    pub last_sequence: u64,
    /// Hash of that event.
    pub last_hash: String,
}

/// What open-time reconciliation did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReconcileOutcome {
    /// The checkpoint matches the JSONL tail; nothing to do.
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
    /// rebuilt from JSONL.
    Rebuilt {
        /// Why.
        reason: String,
        /// Where the old database was moved, when it existed.
        quarantine: Option<PathBuf>,
    },
}

/// One indexed event row (query view).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventRow {
    /// Event id.
    pub id: String,
    /// Sequence.
    pub sequence: u64,
    /// Event type.
    pub event_type: String,
    /// Timestamp.
    pub timestamp: String,
    /// Canonical payload JSON.
    pub payload: String,
    /// Chain hash.
    pub hash: String,
}

/// `projects_summary` row (query view).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProjectSummaryRow {
    /// Project id.
    pub project_id: String,
    /// First event timestamp.
    pub created_at: Option<String>,
    /// Goal text.
    pub goal: Option<String>,
    /// Latest `mode.entered` target.
    pub current_mode: Option<String>,
    /// Latest autonomy value.
    pub autonomy: Option<String>,
    /// Whether the workflow is paused.
    pub paused: bool,
    /// Distinct artifacts created.
    pub artifact_count: u64,
    /// Latest verification result (`None` = never run).
    pub verification_passed: Option<bool>,
    /// Last folded sequence.
    pub last_event_sequence: u64,
}

/// `sessions` row (query view).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRow {
    /// Session id.
    pub session_id: String,
    /// `session.started` timestamp.
    pub started_at: Option<String>,
    /// Interactive flag from `session.started`.
    pub interactive: Option<bool>,
    /// Events produced in this session.
    pub event_count: u64,
}

/// `artifacts` row (query view).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactRow {
    /// Artifact id.
    pub artifact_id: String,
    /// Artifact kind.
    pub kind: String,
    /// Current version.
    pub version: u64,
    /// Lifecycle status.
    pub status: String,
    /// Path relative to the workspace root.
    pub path: Option<String>,
    /// Title.
    pub title: Option<String>,
}

/// The SQLite projection over one store.
#[derive(Debug)]
pub struct SqliteProjection {
    conn: Option<Connection>,
    path: PathBuf,
    recovery_dir: PathBuf,
}

impl SqliteProjection {
    /// Open (creating if necessary) the projection database, applying
    /// durability pragmas and forward-only migrations. A database that is
    /// unreadable or on a newer schema is quarantined into `recovery/` and
    /// recreated empty; reconciliation decides what to replay.
    pub fn open(path: &Path, recovery_dir: &Path, clock: &dyn Clock) -> Result<SqliteProjection> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| StoreError::Io {
                context: "creating the index directory".to_string(),
                path: parent.to_path_buf(),
                source: error,
            })?;
        }
        let mut projection = SqliteProjection {
            conn: None,
            path: path.to_path_buf(),
            recovery_dir: recovery_dir.to_path_buf(),
        };
        projection.connect()?;
        let probe = projection.probe(clock);
        if let Err(reason) = probe {
            // Unreadable or newer-schema database: quarantine and recreate.
            let _ = projection.conn.take();
            let _ = quarantine_index_files(&projection.path, &projection.recovery_dir, clock);
            projection.connect()?;
            projection
                .probe(clock)
                .map_err(|error| StoreError::Projection {
                    reason: format!(
                        "the index database at {} could not be initialized after quarantining \
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
            "CREATE TABLE IF NOT EXISTS relay_schema_migrations (
              id INTEGER PRIMARY KEY,
              name TEXT NOT NULL,
              applied_at TEXT NOT NULL
            );",
        )?;
        let known_max = MIGRATIONS.iter().map(|m| m.0).max().unwrap_or(0);
        let mut applied: Vec<i64> = Vec::new();
        {
            let mut statement =
                conn.prepare("SELECT id FROM relay_schema_migrations ORDER BY id;")?;
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
                "INSERT INTO relay_schema_migrations (id, name, applied_at) VALUES (?, ?, ?);",
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
    pub fn checkpoint(&self) -> Result<Option<Checkpoint>> {
        let conn = self.connection()?;
        conn.query_row(
            "SELECT last_sequence, last_hash FROM checkpoint WHERE id = 1;",
            [],
            |row| {
                Ok(Checkpoint {
                    last_sequence: row.get::<_, i64>(0)? as u64,
                    last_hash: row.get(1)?,
                })
            },
        )
        .optional()
        .map_err(|error| StoreError::Projection {
            reason: "reading the checkpoint".to_string(),
            source: Some(error),
        })
    }

    /// Reconcile against the JSONL tail (§9): match, catch up a suffix, or
    /// quarantine-and-rebuild. `records` is the full validated log in
    /// sequence order.
    pub fn reconcile(
        &mut self,
        records: &[EventRecord],
        tail: &TailState,
        clock: &dyn Clock,
        fault: &dyn FaultInjector,
    ) -> Result<ReconcileOutcome> {
        let checkpoint = self.checkpoint()?;
        let (cp_sequence, cp_hash) = match &checkpoint {
            Some(checkpoint) => (checkpoint.last_sequence, checkpoint.last_hash.clone()),
            None => (0, String::new()),
        };
        let tail_hash = tail.last_hash.clone().unwrap_or_default();
        if cp_sequence == tail.last_sequence && (tail.last_sequence == 0 || cp_hash == tail_hash) {
            return Ok(ReconcileOutcome::Matched);
        }
        if cp_sequence > tail.last_sequence {
            let quarantine = self.quarantine_and_recreate(clock)?;
            self.replay_all(records, clock, fault)?;
            return Ok(ReconcileOutcome::Rebuilt {
                reason: format!(
                    "checkpoint sequence {cp_sequence} is ahead of the JSONL tail \
                     {last}; SQLite is never authoritative",
                    last = tail.last_sequence
                ),
                quarantine,
            });
        }
        if cp_sequence == tail.last_sequence && cp_hash != tail_hash {
            let quarantine = self.quarantine_and_recreate(clock)?;
            self.replay_all(records, clock, fault)?;
            return Ok(ReconcileOutcome::Rebuilt {
                reason: "checkpoint hash diverges from the JSONL tail".to_string(),
                quarantine,
            });
        }
        // Behind (or empty): replay the missing suffix idempotently.
        self.replay_suffix(records, cp_sequence, clock, fault)?;
        Ok(ReconcileOutcome::ReplayedSuffix {
            from: cp_sequence + 1,
            to: tail.last_sequence,
        })
    }

    /// Apply newly appended events (§6 step 8), chunked so every commit is
    /// self-consistent. Used by append (whole batch) and by suffix replay.
    pub fn apply_events(
        &mut self,
        envelopes: &[EventEnvelope],
        clock: &dyn Clock,
        fault: &dyn FaultInjector,
    ) -> Result<()> {
        for chunk in envelopes.chunks(CHUNK) {
            let conn = self.connection()?;
            let tx = conn.unchecked_transaction()?;
            for envelope in chunk {
                insert_event_row(&tx, envelope)?;
                fold_event(&tx, envelope)?;
            }
            if let Some(last) = chunk.last() {
                upsert_checkpoint(&tx, last.sequence, &last.hash, clock)?;
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

    /// Quarantine the current database (if any) into `recovery/`, recreate
    /// an empty one. Returns the quarantine path when a file was moved.
    pub fn quarantine_and_recreate(&mut self, clock: &dyn Clock) -> Result<Option<PathBuf>> {
        // Closing first releases the WAL handles so renames succeed.
        if let Some(conn) = self.conn.take() {
            let _ = conn.close();
        }
        let quarantine = quarantine_index_files(&self.path, &self.recovery_dir, clock)?;
        self.connect()?;
        self.probe(clock)?;
        Ok(quarantine)
    }

    /// Replay everything from JSONL into a fresh database.
    pub fn replay_all(
        &mut self,
        records: &[EventRecord],
        clock: &dyn Clock,
        fault: &dyn FaultInjector,
    ) -> Result<()> {
        let envelopes: Vec<EventEnvelope> = records.iter().map(|r| r.envelope.clone()).collect();
        self.apply_events(&envelopes, clock, fault)
    }

    /// Replay records after `checkpoint_sequence`, first deleting any rows
    /// beyond the checkpoint (idempotent convergence for half-committed
    /// states).
    fn replay_suffix(
        &mut self,
        records: &[EventRecord],
        checkpoint_sequence: u64,
        clock: &dyn Clock,
        fault: &dyn FaultInjector,
    ) -> Result<()> {
        {
            let conn = self.connection()?;
            conn.execute(
                "DELETE FROM events WHERE sequence > ?;",
                params![checkpoint_sequence as i64],
            )?;
        }
        let suffix: Vec<EventEnvelope> = records
            .iter()
            .filter(|record| record.envelope.sequence > checkpoint_sequence)
            .map(|record| record.envelope.clone())
            .collect();
        if suffix.is_empty() {
            // Nothing to replay: still refresh the checkpoint to the tail.
            if let Some(last) = records.last() {
                let conn = self.connection()?;
                let tx = conn.unchecked_transaction()?;
                upsert_checkpoint(&tx, last.envelope.sequence, &last.envelope.hash, clock)?;
                tx.commit()?;
            }
            return Ok(());
        }
        self.apply_events(&suffix, clock, fault)
    }

    /// Number of indexed events.
    pub fn event_count(&self) -> Result<u64> {
        let conn = self.connection()?;
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM events;", [], |row| row.get(0))?;
        Ok(count as u64)
    }

    /// Indexed events after `sequence`, ordered, at most `limit`.
    pub fn events_after(&self, sequence: u64, limit: u64) -> Result<Vec<EventRow>> {
        let conn = self.connection()?;
        let mut statement = conn
            .prepare(
                "SELECT id, sequence, type, timestamp, payload, hash FROM events
                 WHERE sequence > ? ORDER BY sequence ASC LIMIT ?;",
            )
            .map_err(|error| StoreError::Projection {
                reason: "preparing events_after".to_string(),
                source: Some(error),
            })?;
        let rows = statement
            .query_map(params![sequence as i64, limit as i64], |row| {
                Ok(EventRow {
                    id: row.get(0)?,
                    sequence: row.get::<_, i64>(1)? as u64,
                    event_type: row.get(2)?,
                    timestamp: row.get(3)?,
                    payload: row.get(4)?,
                    hash: row.get(5)?,
                })
            })
            .map_err(|error| StoreError::Projection {
                reason: "querying events_after".to_string(),
                source: Some(error),
            })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// The `projects_summary` row for `project_id`.
    pub fn project_summary(&self, project_id: &str) -> Result<Option<ProjectSummaryRow>> {
        let conn = self.connection()?;
        conn.query_row(
            "SELECT project_id, created_at, goal, current_mode, autonomy, paused, \
             artifact_count, verification_passed, last_event_sequence \
             FROM projects_summary WHERE project_id = ?;",
            params![project_id],
            |row| {
                Ok(ProjectSummaryRow {
                    project_id: row.get(0)?,
                    created_at: row.get(1)?,
                    goal: row.get(2)?,
                    current_mode: row.get(3)?,
                    autonomy: row.get(4)?,
                    paused: row.get::<_, i64>(5)? != 0,
                    artifact_count: row.get::<_, i64>(6)? as u64,
                    verification_passed: row.get::<_, Option<i64>>(7)?.map(|value| value != 0),
                    last_event_sequence: row.get::<_, i64>(8)? as u64,
                })
            },
        )
        .optional()
        .map_err(|error| StoreError::Projection {
            reason: "reading projects_summary".to_string(),
            source: Some(error),
        })
    }

    /// The `sessions` rows for `project_id`, ordered by session id.
    pub fn sessions(&self, project_id: &str) -> Result<Vec<SessionRow>> {
        let conn = self.connection()?;
        let mut statement = conn
            .prepare(
                "SELECT session_id, started_at, interactive, event_count FROM sessions
                 WHERE project_id = ? ORDER BY session_id ASC;",
            )
            .map_err(|error| StoreError::Projection {
                reason: "preparing sessions".to_string(),
                source: Some(error),
            })?;
        let rows = statement.query_map(params![project_id], |row| {
            Ok(SessionRow {
                session_id: row.get(0)?,
                started_at: row.get(1)?,
                interactive: row.get::<_, Option<i64>>(2)?.map(|v| v != 0),
                event_count: row.get::<_, i64>(3)? as u64,
            })
        });
        let mut out = Vec::new();
        match rows {
            Ok(rows) => {
                for row in rows {
                    out.push(row?);
                }
            }
            Err(error) => {
                return Err(StoreError::Projection {
                    reason: "querying sessions".to_string(),
                    source: Some(error),
                });
            }
        }
        Ok(out)
    }

    /// The `artifacts` rows for `project_id`, ordered by artifact id.
    pub fn artifacts(&self, project_id: &str) -> Result<Vec<ArtifactRow>> {
        let conn = self.connection()?;
        let mut statement = conn
            .prepare(
                "SELECT artifact_id, kind, version, status, path, title FROM artifacts
                 WHERE project_id = ? ORDER BY artifact_id ASC;",
            )
            .map_err(|error| StoreError::Projection {
                reason: "preparing artifacts".to_string(),
                source: Some(error),
            })?;
        let rows = statement.query_map(params![project_id], |row| {
            Ok(ArtifactRow {
                artifact_id: row.get(0)?,
                kind: row.get(1)?,
                version: row.get::<_, i64>(2)? as u64,
                status: row.get(3)?,
                path: row.get(4)?,
                title: row.get(5)?,
            })
        });
        let mut out = Vec::new();
        match rows {
            Ok(rows) => {
                for row in rows {
                    out.push(row?);
                }
            }
            Err(error) => {
                return Err(StoreError::Projection {
                    reason: "querying artifacts".to_string(),
                    source: Some(error),
                });
            }
        }
        Ok(out)
    }

    /// Count of `model_calls` rows.
    pub fn model_calls_count(&self) -> Result<u64> {
        let conn = self.connection()?;
        let count: i64 =
            conn.query_row("SELECT COUNT(*) FROM model_calls;", [], |row| row.get(0))?;
        Ok(count as u64)
    }

    /// Count of `tool_calls` rows.
    pub fn tool_calls_count(&self) -> Result<u64> {
        let conn = self.connection()?;
        let count: i64 =
            conn.query_row("SELECT COUNT(*) FROM tool_calls;", [], |row| row.get(0))?;
        Ok(count as u64)
    }
}

/// Quarantine `index.sqlite` (+ WAL sidecars) into `recovery/`; returns the
/// destination when a file was moved.
fn quarantine_index_files(
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
    let moved = recovery_dir.join(format!("index-{stamp}Z.sqlite"));
    std::fs::rename(path, &moved).map_err(|error| StoreError::Io {
        context: "quarantining a divergent index database".to_string(),
        path: path.to_path_buf(),
        source: error,
    })?;
    for suffix in ["-wal", "-shm"] {
        let sidecar = path.with_file_name(format!("index.sqlite{suffix}"));
        if sidecar.exists() {
            let target = recovery_dir.join(format!("index-{stamp}Z.sqlite{suffix}"));
            let _ = std::fs::rename(&sidecar, target);
        }
    }
    Ok(Some(moved))
}

fn insert_event_row(tx: &rusqlite::Transaction<'_>, envelope: &EventEnvelope) -> Result<()> {
    let payload_json = envelope.payload.to_canonical_string();
    tx.execute(
        "INSERT OR REPLACE INTO events (
          id, project_id, sequence, session_id, type, schema_version, timestamp,
          correlation_id, causation_id, payload, hash
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?);",
        params![
            envelope.id,
            envelope.project_id,
            envelope.sequence as i64,
            envelope.session_id,
            envelope.event_type,
            envelope.schema_version as i64,
            envelope.timestamp,
            envelope.correlation_id,
            envelope.causation_id,
            payload_json,
            envelope.hash,
        ],
    )?;
    Ok(())
}

fn upsert_checkpoint(
    tx: &rusqlite::Transaction<'_>,
    sequence: u64,
    hash: &str,
    clock: &dyn Clock,
) -> Result<()> {
    tx.execute(
        "INSERT INTO checkpoint (id, last_sequence, last_hash, updated_at)
         VALUES (1, ?, ?, ?)
         ON CONFLICT(id) DO UPDATE SET
           last_sequence = excluded.last_sequence,
           last_hash = excluded.last_hash,
           updated_at = excluded.updated_at;",
        params![sequence as i64, hash, clock.now_iso()],
    )?;
    Ok(())
}

/// Payload accessors that never fail: a projection fold must not error on
/// payload shapes the envelope layer accepted.
fn pstr(payload: &Json, key: &str) -> Option<String> {
    payload.get(key).and_then(Json::as_str).map(str::to_string)
}

fn pint(payload: &Json, key: &str) -> Option<i64> {
    payload.get(key).and_then(Json::as_i64)
}

fn pbool(payload: &Json, key: &str) -> Option<bool> {
    payload.get(key).and_then(Json::as_bool)
}

/// Fold one event into the derived tables. Incremental and order-dependent;
/// only ever called with events strictly after the current checkpoint.
fn fold_event(tx: &rusqlite::Transaction<'_>, envelope: &EventEnvelope) -> Result<()> {
    let project = envelope.project_id.as_str();
    tx.execute(
        "INSERT INTO projects_summary (project_id, last_event_sequence)
         VALUES (?, ?)
         ON CONFLICT(project_id) DO UPDATE SET
           last_event_sequence = excluded.last_event_sequence;",
        params![project, envelope.sequence as i64],
    )?;
    match envelope.event_type.as_str() {
        "project.initialized" => {
            let goal = pstr(&envelope.payload, "goal");
            let autonomy = pstr(&envelope.payload, "initialAutonomy");
            tx.execute(
                "UPDATE projects_summary SET created_at = ?, goal = ?, autonomy = \
                 COALESCE(?, autonomy) WHERE project_id = ?;",
                params![envelope.timestamp, goal, autonomy, project],
            )?;
        }
        "mode.entered" => {
            let to = pstr(&envelope.payload, "to");
            tx.execute(
                "UPDATE projects_summary SET current_mode = ? WHERE project_id = ?;",
                params![to, project],
            )?;
        }
        "autonomy.changed" => {
            let to = pstr(&envelope.payload, "to");
            tx.execute(
                "UPDATE projects_summary SET autonomy = ? WHERE project_id = ?;",
                params![to, project],
            )?;
        }
        "workflow.paused" => {
            tx.execute(
                "UPDATE projects_summary SET paused = 1 WHERE project_id = ?;",
                params![project],
            )?;
        }
        "workflow.resumed" => {
            tx.execute(
                "UPDATE projects_summary SET paused = 0 WHERE project_id = ?;",
                params![project],
            )?;
        }
        "verification.completed" => {
            if let Some(passed) = pbool(&envelope.payload, "passed") {
                tx.execute(
                    "UPDATE projects_summary SET verification_passed = ? WHERE project_id = ?;",
                    params![if passed { 1i64 } else { 0i64 }, project],
                )?;
            }
        }
        "session.started" => {
            if let Some(session) = pstr(&envelope.payload, "sessionId") {
                let interactive = pbool(&envelope.payload, "interactive");
                tx.execute(
                    "INSERT INTO sessions (project_id, session_id, started_at, interactive, event_count)
                     VALUES (?, ?, ?, ?, 0)
                     ON CONFLICT(project_id, session_id) DO UPDATE SET
                       started_at = excluded.started_at,
                       interactive = excluded.interactive;",
                    params![
                        project,
                        session,
                        envelope.timestamp,
                        interactive.map(|flag| if flag { 1i64 } else { 0i64 }),
                    ],
                )?;
            }
        }
        "artifact.created" => {
            let artifact_id = pstr(&envelope.payload, "artifactId");
            let kind = pstr(&envelope.payload, "kind");
            let version = pint(&envelope.payload, "version");
            let path = pstr(&envelope.payload, "path");
            let title = pstr(&envelope.payload, "title");
            if let (Some(artifact_id), Some(kind), Some(version)) = (artifact_id, kind, version) {
                let existing: Option<i64> = tx
                    .query_row(
                        "SELECT version FROM artifacts WHERE project_id = ? AND artifact_id = ?;",
                        params![project, artifact_id],
                        |row| row.get(0),
                    )
                    .optional()?;
                if existing.is_none() {
                    tx.execute(
                        "INSERT INTO artifacts (project_id, artifact_id, kind, version, status, path, title)
                         VALUES (?, ?, ?, ?, 'created', ?, ?);",
                        params![project, artifact_id, kind, version, path, title],
                    )?;
                    tx.execute(
                        "UPDATE projects_summary SET artifact_count = artifact_count + 1 \
                         WHERE project_id = ?;",
                        params![project],
                    )?;
                } else {
                    tx.execute(
                        "UPDATE artifacts SET kind = ?, version = ?, status = 'created', path = \
                         COALESCE(?, path), title = COALESCE(?, title)
                         WHERE project_id = ? AND artifact_id = ?;",
                        params![kind, version, path, title, project, artifact_id],
                    )?;
                }
            }
        }
        "artifact.revised" => {
            let artifact_id = pstr(&envelope.payload, "artifactId");
            let to_version = pint(&envelope.payload, "toVersion");
            let path = pstr(&envelope.payload, "path");
            if let (Some(artifact_id), Some(to_version)) = (artifact_id, to_version) {
                tx.execute(
                    "UPDATE artifacts SET version = ?, path = COALESCE(?, path), status = \
                     'created' WHERE project_id = ? AND artifact_id = ?;",
                    params![to_version, path, project, artifact_id],
                )?;
            }
        }
        "artifact.approval.requested" => {
            if let Some(artifact_id) = pstr(&envelope.payload, "artifactId") {
                tx.execute(
                    "UPDATE artifacts SET status = 'pending-approval' WHERE project_id = ? AND \
                     artifact_id = ?;",
                    params![project, artifact_id],
                )?;
            }
        }
        "artifact.approved" => {
            if let Some(artifact_id) = pstr(&envelope.payload, "artifactId") {
                let version = pint(&envelope.payload, "version");
                tx.execute(
                    "UPDATE artifacts SET status = 'approved', version = COALESCE(?, version) \
                     WHERE project_id = ? AND artifact_id = ?;",
                    params![version, project, artifact_id],
                )?;
            }
        }
        "artifact.invalidated" => {
            if let Some(ids) = envelope
                .payload
                .get("artifactIds")
                .and_then(|value| match value {
                    Json::Array(items) => Some(items.clone()),
                    _ => None,
                })
            {
                for id in ids {
                    if let Some(artifact_id) = id.as_str() {
                        tx.execute(
                            "UPDATE artifacts SET status = 'invalidated' WHERE project_id = ? \
                             AND artifact_id = ?;",
                            params![project, artifact_id],
                        )?;
                    }
                }
            }
        }
        "model.requested" | "model.responded" | "model.failed" => {
            let phase = match envelope.event_type.as_str() {
                "model.requested" => "requested",
                "model.responded" => "responded",
                _ => "failed",
            };
            let usage = envelope.payload.get("usage");
            let input_tokens = usage.and_then(|u| pint(u, "inputTokens"));
            let output_tokens = usage.and_then(|u| pint(u, "outputTokens"));
            let tool_calls = pint(&envelope.payload, "toolCalls");
            let error_kind = pstr(&envelope.payload, "errorKind");
            let message = pstr(&envelope.payload, "message");
            let attempts = pint(&envelope.payload, "attempts");
            let duration = pint(&envelope.payload, "durationMs");
            tx.execute(
                "INSERT OR REPLACE INTO model_calls (
                  project_id, event_id, timestamp, phase, provider, model, purpose, role,
                  input_tokens, output_tokens, tool_calls, error_kind, message, attempts,
                  duration_ms
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?);",
                params![
                    project,
                    envelope.id,
                    envelope.timestamp,
                    phase,
                    pstr(&envelope.payload, "provider"),
                    pstr(&envelope.payload, "model"),
                    pstr(&envelope.payload, "purpose"),
                    pstr(&envelope.payload, "role"),
                    input_tokens,
                    output_tokens,
                    tool_calls,
                    error_kind,
                    message,
                    attempts,
                    duration,
                ],
            )?;
        }
        "tool.executed" | "tool.denied" => {
            let outcome = if envelope.event_type == "tool.executed" {
                pstr(&envelope.payload, "outcome")
            } else {
                Some("denied".to_string())
            };
            tx.execute(
                "INSERT OR REPLACE INTO tool_calls (
                  project_id, event_id, timestamp, tool, capability, mode, outcome,
                  duration_ms, summary, reason
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?);",
                params![
                    project,
                    envelope.id,
                    envelope.timestamp,
                    pstr(&envelope.payload, "tool"),
                    pstr(&envelope.payload, "capability"),
                    pstr(&envelope.payload, "mode"),
                    outcome,
                    pint(&envelope.payload, "durationMs"),
                    pstr(&envelope.payload, "summary"),
                    pstr(&envelope.payload, "reason"),
                ],
            )?;
        }
        _ => {}
    }
    // Session event counts apply to every event carrying a session id.
    if let Some(session) = &envelope.session_id {
        tx.execute(
            "INSERT INTO sessions (project_id, session_id, event_count)
             VALUES (?, ?, 1)
             ON CONFLICT(project_id, session_id) DO UPDATE SET
               event_count = sessions.event_count + 1;",
            params![project, session],
        )?;
    }
    Ok(())
}

/// Convenience wrapper: quarantine the projection at `path` and rebuild it
/// from the given records in one shot.
pub fn rebuild_from_records(
    path: &Path,
    recovery_dir: &Path,
    records: &[EventRecord],
    clock: &dyn Clock,
) -> Result<(SqliteProjection, Option<PathBuf>)> {
    let mut projection = SqliteProjection::open(path, recovery_dir, clock)?;
    let quarantine = projection.quarantine_and_recreate(clock)?;
    projection.replay_all(records, clock, &NoFaults)?;
    Ok((projection, quarantine))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canonical::Json;
    use crate::clock::FixedClock;
    use crate::envelope::EventEnvelope;
    use crate::fault::NoFaults;
    use crate::log::TailState;

    const PROJECT: &str = "prj_00000000-0000-4000-8000-000000000001";
    const CLOCK_MS: i64 = 1_791_878_400_000;

    fn envelope(sequence: u64, previous: Option<String>, kind: &str) -> Result<EventEnvelope> {
        let payload = match kind {
            "project.initialized" => Json::object([
                ("goal", Json::string("Ship it")),
                ("workspaceRoot", Json::string("/w")),
                ("initialAutonomy", Json::string("default")),
            ]),
            "mode.entered" => Json::object([
                ("from", Json::Null),
                ("to", Json::string("discovery")),
                ("reason", Json::string("start")),
                ("source", Json::string("auto")),
            ]),
            "session.started" => Json::object([
                (
                    "sessionId",
                    Json::string("ses_00000000-0000-4000-8000-0000000000aa"),
                ),
                ("interactive", Json::Bool(true)),
            ]),
            _ => Json::object([("phase", Json::string("t")), ("hint", Json::string("h"))]),
        };
        EventEnvelope::build(crate::envelope::EnvelopeDraft {
            project_id: PROJECT.to_string(),
            sequence,
            session_id: Some("ses_00000000-0000-4000-8000-0000000000aa".to_string()),
            event_type: kind.to_string(),
            timestamp: format!("2026-10-13T08:00:{:02}.000Z", sequence % 60),
            correlation_id: format!("cor_00000000-0000-4000-8000-{sequence:012x}"),
            causation_id: None,
            payload,
            previous_hash: previous,
            id: format!("evt_00000000-0000-4000-8000-{sequence:012x}"),
        })
    }

    fn three_records() -> Result<Vec<EventRecord>> {
        let first = envelope(1, None, "project.initialized")?;
        let second = envelope(2, Some(first.hash.clone()), "session.started")?;
        let third = envelope(3, Some(second.hash.clone()), "mode.entered")?;
        Ok([first, second, third]
            .into_iter()
            .map(|e| EventRecord {
                envelope: e,
                segment: "0000000000000001-open.jsonl".to_string(),
                line: 1,
            })
            .collect())
    }

    #[test]
    fn apply_and_query_views() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let clock = FixedClock::new(CLOCK_MS);
        let mut projection = SqliteProjection::open(
            &dir.path().join("index.sqlite"),
            &dir.path().join("recovery"),
            &clock,
        )?;
        let records = three_records()?;
        let envelopes: Vec<EventEnvelope> = records.iter().map(|r| r.envelope.clone()).collect();
        projection.apply_events(&envelopes, &clock, &NoFaults)?;
        assert_eq!(projection.event_count()?, 3);
        let checkpoint = projection.checkpoint()?.ok_or("checkpoint missing")?;
        assert_eq!(checkpoint.last_sequence, 3);
        let summary = projection
            .project_summary(PROJECT)?
            .ok_or("summary missing")?;
        assert_eq!(summary.goal.as_deref(), Some("Ship it"));
        assert_eq!(summary.current_mode.as_deref(), Some("discovery"));
        assert_eq!(summary.last_event_sequence, 3);
        let sessions = projection.sessions(PROJECT)?;
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions.first().ok_or("session")?.event_count, 3);
        assert_eq!(sessions.first().ok_or("session")?.interactive, Some(true));
        let after = projection.events_after(1, 10)?;
        assert_eq!(after.len(), 2);
        assert_eq!(after.first().ok_or("row")?.event_type, "session.started");
        Ok(())
    }

    #[test]
    fn suffix_replay_from_checkpoint() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let clock = FixedClock::new(CLOCK_MS);
        let index = dir.path().join("index.sqlite");
        let records = three_records()?;
        {
            let mut projection = SqliteProjection::open(&index, &dir.path().join("r"), &clock)?;
            let envelopes: Vec<EventEnvelope> =
                records[..2].iter().map(|r| r.envelope.clone()).collect();
            projection.apply_events(&envelopes, &clock, &NoFaults)?;
        }
        let tail = TailState {
            last_sequence: 3,
            last_hash: records.last().map(|r| r.envelope.hash.clone()),
            active_events: 3,
            active_bytes: 0,
        };
        let mut projection = SqliteProjection::open(&index, &dir.path().join("r"), &clock)?;
        let outcome = projection.reconcile(&records, &tail, &clock, &NoFaults)?;
        assert_eq!(outcome, ReconcileOutcome::ReplayedSuffix { from: 3, to: 3 });
        assert_eq!(projection.event_count()?, 3);
        assert_eq!(projection.checkpoint()?.ok_or("cp")?.last_sequence, 3);
        // A second reconcile matches.
        let again = projection.reconcile(&records, &tail, &clock, &NoFaults)?;
        assert_eq!(again, ReconcileOutcome::Matched);
        Ok(())
    }

    #[test]
    fn ahead_checkpoint_rebuilds() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let clock = FixedClock::new(CLOCK_MS);
        let index = dir.path().join("index.sqlite");
        let records = three_records()?;
        // Build a database "from the future": 4 events, then reconcile
        // against a 3-event tail.
        let future = {
            let third = records.last().ok_or("r")?.envelope.clone();
            let fourth = envelope(4, Some(third.hash.clone()), "run.interrupted")?;
            let mut all = records.clone();
            all.push(EventRecord {
                envelope: fourth,
                segment: "s".to_string(),
                line: 4,
            });
            all
        };
        {
            let mut projection = SqliteProjection::open(&index, &dir.path().join("r"), &clock)?;
            let envelopes: Vec<EventEnvelope> = future.iter().map(|r| r.envelope.clone()).collect();
            projection.apply_events(&envelopes, &clock, &NoFaults)?;
        }
        let tail = TailState {
            last_sequence: 3,
            last_hash: records.last().map(|r| r.envelope.hash.clone()),
            active_events: 3,
            active_bytes: 0,
        };
        let mut projection = SqliteProjection::open(&index, &dir.path().join("r"), &clock)?;
        let outcome = projection.reconcile(&records, &tail, &clock, &NoFaults)?;
        match &outcome {
            ReconcileOutcome::Rebuilt { quarantine, .. } => {
                assert!(quarantine.is_some());
            }
            other => return Err(format!("expected rebuild, got {other:?}").into()),
        }
        assert_eq!(projection.event_count()?, 3);
        Ok(())
    }

    #[test]
    fn deleting_index_rebuilds_equivalent_views() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let clock = FixedClock::new(CLOCK_MS);
        let index = dir.path().join("index.sqlite");
        let records = three_records()?;
        {
            let mut projection = SqliteProjection::open(&index, &dir.path().join("r"), &clock)?;
            let envelopes: Vec<EventEnvelope> =
                records.iter().map(|r| r.envelope.clone()).collect();
            projection.apply_events(&envelopes, &clock, &NoFaults)?;
        }
        let before_summary = {
            let projection = SqliteProjection::open(&index, &dir.path().join("r"), &clock)?;
            projection.project_summary(PROJECT)?
        };
        std::fs::remove_file(&index)?;
        let tail = TailState {
            last_sequence: 3,
            last_hash: records.last().map(|r| r.envelope.hash.clone()),
            active_events: 3,
            active_bytes: 0,
        };
        let mut projection = SqliteProjection::open(&index, &dir.path().join("r"), &clock)?;
        let outcome = projection.reconcile(&records, &tail, &clock, &NoFaults)?;
        assert!(matches!(outcome, ReconcileOutcome::ReplayedSuffix { .. }));
        assert_eq!(projection.event_count()?, 3);
        let after_summary = projection.project_summary(PROJECT)?;
        assert_eq!(before_summary, after_summary);
        Ok(())
    }

    #[test]
    fn corrupt_database_is_quarantined_and_rebuilt() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let clock = FixedClock::new(CLOCK_MS);
        let index = dir.path().join("index.sqlite");
        std::fs::write(&index, b"this is definitely not sqlite")?;
        let mut projection = SqliteProjection::open(&index, &dir.path().join("r"), &clock)?;
        let records = three_records()?;
        let tail = TailState {
            last_sequence: 3,
            last_hash: records.last().map(|r| r.envelope.hash.clone()),
            active_events: 3,
            active_bytes: 0,
        };
        let outcome = projection.reconcile(&records, &tail, &clock, &NoFaults)?;
        assert!(matches!(outcome, ReconcileOutcome::ReplayedSuffix { .. }));
        assert_eq!(projection.event_count()?, 3);
        Ok(())
    }
}
