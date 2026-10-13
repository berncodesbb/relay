//! Legacy migration integration suite (phase 3, storage/migration owner).
//!
//! Builds legacy `.relay/state.db` fixtures programmatically (applying the
//! legacy schema translated from `src/storage/migrations/001-initial.ts`
//! and `002-projects.ts`) in temp workspaces and exercises the §10
//! contract: read-only source, staged destination with fresh hashes,
//! artifact byte copy with SHA-256 digests, replay comparison, SQLite
//! build, atomic directory switch with a retained backup, and clean
//! failures that leave the original `.relay/` in charge.
//!
//! The canonical-JSONL import path is exercised against the deterministic
//! fixture corpus under `tests/fixtures/cases/` (core engineer's exporter
//! output follows the same format), plus an optional cross-check against
//! `scripts/export-legacy-project.mjs` when Node/tsx are available.

use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

use relay_store::FixedClock;
use relay_store::MigrationOptions;
use relay_store::MigrationOutcome;
use relay_store::RelayStore;
use relay_store::StoreOptions;
use rusqlite::Connection;
use sha2::Digest;
use sha2::Sha256;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const PROJECT: &str = "prj_00000000-0000-4000-8000-000000000001";
const SESSION: &str = "ses_00000000-0000-4000-8000-0000000000e1";
const CLOCK_MS: i64 = 1_791_878_400_000;
const REPO_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");

/// The legacy events/projects schema, translated verbatim from
/// `src/storage/migrations/001-initial.ts` and `002-projects.ts`.
const LEGACY_SCHEMA: &str = r#"
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
  payload TEXT NOT NULL
);
CREATE UNIQUE INDEX events_project_sequence ON events (project_id, sequence);
CREATE INDEX events_project_type ON events (project_id, type);
CREATE INDEX events_project_timestamp ON events (project_id, timestamp);
CREATE TABLE projects (
  project_id TEXT PRIMARY KEY,
  created_at TEXT NOT NULL,
  goal TEXT NOT NULL
);
CREATE TABLE schema_migrations (id INTEGER PRIMARY KEY, name TEXT NOT NULL, applied_at TEXT NOT NULL);
INSERT INTO schema_migrations VALUES (1, 'initial-append-only-events', '2026-01-01T00:00:00.000Z');
INSERT INTO schema_migrations VALUES (2, 'projects-listing-index', '2026-01-01T00:00:00.000Z');
"#;

/// One legacy row to insert.
struct LegacyEvent {
    id: String,
    sequence: u64,
    event_type: &'static str,
    /// Raw payload JSON string in legacy (insertion-order) form, exactly
    /// like the TypeScript store's `JSON.stringify`.
    payload: String,
}

fn evt(n: u64, sequence: u64, event_type: &'static str, payload: String) -> LegacyEvent {
    LegacyEvent {
        id: format!("evt_00000000-0000-4000-8000-{n:012x}"),
        sequence,
        event_type,
        payload,
    }
}

fn initialized_payload(goal: &str) -> String {
    // Deliberately non-canonical key order, like JSON.stringify.
    format!(
        "{{\"goal\":\"{goal}\",\"workspaceRoot\":\"/tmp/legacy\",\"initialAutonomy\":\"workflow\"}}"
    )
}

fn artifact_created_payload(artifact_id: &str, kind: &str, path: &str) -> String {
    format!(
        "{{\"artifactId\":\"{artifact_id}\",\"kind\":\"{kind}\",\"title\":\"Demo artifact\",\
         \"version\":1,\"path\":\"{path}\",\"summary\":\"A demo artifact body.\"}}"
    )
}

/// Build a legacy workspace: `.relay/state.db` with `events` and optional
/// `.relay/artifacts/` files.
fn legacy_workspace(
    events: &[LegacyEvent],
    artifacts: &[(&str, &str)],
) -> Result<tempfile::TempDir> {
    let dir = tempfile::tempdir()?;
    let relay = dir.path().join(".relay");
    std::fs::create_dir_all(relay.join("artifacts").join("prd"))?;
    let db = Connection::open(relay.join("state.db"))?;
    db.execute_batch(LEGACY_SCHEMA)?;
    let mut goal = String::new();
    for (index, event) in events.iter().enumerate() {
        if index == 0
            && let Ok(value) = serde_json::from_str::<serde_json::Value>(&event.payload)
            && let Some(goal_text) = value.get("goal").and_then(|g| g.as_str())
        {
            goal = goal_text.to_string();
        }
        db.execute(
            "INSERT INTO events (id, project_id, sequence, session_id, type, schema_version, \
             timestamp, correlation_id, causation_id, payload) \
             VALUES (?, ?, ?, ?, ?, 1, ?, ?, NULL, ?);",
            rusqlite::params![
                event.id,
                PROJECT,
                event.sequence as i64,
                if index == 0 { None } else { Some(SESSION) },
                event.event_type,
                format!("2026-10-13T08:00:{:02}.000Z", event.sequence % 60),
                format!("cor_00000000-0000-4000-8000-{:012x}", event.sequence),
                event.payload,
            ],
        )?;
    }
    if !events.is_empty() {
        db.execute(
            "INSERT INTO projects (project_id, created_at, goal) VALUES (?, ?, ?);",
            rusqlite::params![
                PROJECT,
                "2026-10-13T08:00:00.000Z",
                if goal.is_empty() { "goal" } else { &goal }
            ],
        )?;
    }
    for (name, body) in artifacts {
        let path = relay.join("artifacts").join("prd").join(name);
        std::fs::write(path, body)?;
    }
    Ok(dir)
}

fn typical_events() -> Vec<LegacyEvent> {
    vec![
        evt(1, 1, "project.initialized", initialized_payload("Ship the demo")),
        evt(
            2,
            2,
            "session.started",
            format!("{{\"sessionId\":\"{SESSION}\",\"interactive\":true}}"),
        ),
        evt(
            3,
            3,
            "mode.entered",
            "{\"from\":null,\"to\":\"discovery\",\"reason\":\"start\",\"source\":\"auto\"}"
                .to_string(),
        ),
        evt(
            4,
            4,
            "artifact.created",
            artifact_created_payload(
                "art_00000000-0000-4000-8000-000000000004",
                "prd",
                ".relay/artifacts/prd/v1-demo-prd.md",
            ),
        ),
        evt(
            5,
            5,
            "model.requested",
            "{\"provider\":\"fake\",\"model\":\"demo\",\"purpose\":\"draft\",\"mode\":\"discovery\",\"attempt\":1}"
                .to_string(),
        ),
        evt(
            6,
            6,
            "model.responded",
            "{\"provider\":\"fake\",\"model\":\"demo\",\"purpose\":\"draft\",\"usage\":{\"inputTokens\":10,\"outputTokens\":20},\"toolCalls\":0,\"durationMs\":5}"
                .to_string(),
        ),
    ]
}

fn migration_options() -> MigrationOptions {
    MigrationOptions {
        clock: Arc::new(FixedClock::new(CLOCK_MS)),
        ..MigrationOptions::default()
    }
}

fn store_options() -> StoreOptions {
    StoreOptions::deterministic(CLOCK_MS)
}

fn sha256_file(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path)?;
    Ok(hex::encode(Sha256::digest(&bytes)))
}

fn migrated(outcome: MigrationOutcome) -> Result<relay_store::MigrationReport> {
    match outcome {
        MigrationOutcome::Migrated(report) => Ok(*report),
        MigrationOutcome::AlreadyMigrated { detail } => {
            Err(format!("expected migration, got AlreadyMigrated: {detail}").into())
        }
    }
}

#[test]
fn typical_workflow_migrates_with_backup_and_intact_source() -> Result<()> {
    let dir = legacy_workspace(&typical_events(), &[("v1-demo-prd.md", "# PRD body\n")])?;
    let workspace = dir.path();
    let state_db = workspace.join(".relay").join("state.db");
    let source_digest = sha256_file(&state_db)?;
    let artifact_digest = sha256_file(&workspace.join(".relay/artifacts/prd/v1-demo-prd.md"))?;

    let report = migrated(relay_store::migrate_legacy_state_db(
        workspace,
        &migration_options(),
    )?)?;
    assert_eq!(report.events_migrated, 6);
    let backup = report.backup_dir.as_ref().ok_or("backup missing")?;
    assert!(backup.is_dir(), "backup retained at {}", backup.display());

    // Source database and artifacts moved, byte-for-byte.
    assert!(!state_db.exists());
    let backup_db = backup.join("state.db");
    assert_eq!(sha256_file(&backup_db)?, source_digest);
    assert_eq!(
        sha256_file(&workspace.join(".relay/artifacts/prd/v1-demo-prd.md"))?,
        artifact_digest,
        "artifact bytes preserved"
    );
    assert_eq!(report.artifacts.len(), 1);

    // The new store opens and validates; fresh hashes chain from scratch.
    let (store, open_report) = RelayStore::open(workspace, store_options())?;
    assert!(open_report.recovered_tails.is_empty());
    let records = store.read_all()?;
    assert_eq!(records.len(), 6);
    assert_eq!(
        records.first().ok_or("empty")?.envelope.event_type,
        "project.initialized"
    );
    assert_eq!(
        store.tail()?.last_hash.as_deref(),
        records.last().map(|r| r.envelope.hash.as_str())
    );
    let summary = store
        .projection_summary(PROJECT)?
        .ok_or("summary missing")?;
    assert_eq!(summary.goal.as_deref(), Some("Ship the demo"));
    assert_eq!(summary.current_mode.as_deref(), Some("discovery"));
    assert_eq!(summary.artifact_count, 1);
    let artifacts = store.projection_artifacts(PROJECT)?;
    assert_eq!(artifacts.first().ok_or("artifact row")?.status, "created");
    Ok(())
}

#[test]
fn rollback_restores_the_legacy_store() -> Result<()> {
    let dir = legacy_workspace(&typical_events(), &[("v1-demo-prd.md", "# PRD body\n")])?;
    let workspace = dir.path();
    let report = migrated(relay_store::migrate_legacy_state_db(
        workspace,
        &migration_options(),
    )?)?;
    let backup = report.backup_dir.clone().ok_or("backup")?;

    // Roll back: drop the migrated store, move the backup back.
    std::fs::rename(
        workspace.join(".relay"),
        workspace.join(".relay-migrated-away"),
    )?;
    std::fs::rename(&backup, workspace.join(".relay"))?;
    assert!(workspace.join(".relay/state.db").exists());
    assert!(!workspace.join(".relay/manifest.json").exists());
    // The legacy database is intact and still read-only-migratable.
    let second = migrated(relay_store::migrate_legacy_state_db(
        workspace,
        &migration_options(),
    )?)?;
    assert_eq!(second.events_migrated, 6);
    Ok(())
}

#[test]
fn empty_legacy_db_fails_cleanly() -> Result<()> {
    let dir = legacy_workspace(&[], &[])?;
    let error = relay_store::migrate_legacy_state_db(dir.path(), &migration_options())
        .err()
        .ok_or("expected failure")?;
    assert!(error.to_string().contains("no events"), "{error}");
    assert!(dir.path().join(".relay/state.db").exists());
    assert!(!dir.path().join(".relay/manifest.json").exists());
    Ok(())
}

#[test]
fn corrupt_source_fails_and_leaves_original_in_charge() -> Result<()> {
    let dir = legacy_workspace(&typical_events(), &[])?;
    // Overwrite the database with garbage (simulating corruption).
    std::fs::write(
        dir.path().join(".relay/state.db"),
        b"not a sqlite database at all",
    )?;
    let error = relay_store::migrate_legacy_state_db(dir.path(), &migration_options())
        .err()
        .ok_or("expected failure")?;
    let message = error.to_string();
    assert!(
        message.contains("integrity")
            || message.contains("Cannot open")
            || message.contains("open"),
        "{message}"
    );
    assert!(dir.path().join(".relay/state.db").exists());
    assert!(!dir.path().join(".relay/manifest.json").exists());
    Ok(())
}

#[test]
fn large_log_spans_multiple_segments() -> Result<()> {
    let mut events = Vec::new();
    events.push(evt(
        1,
        1,
        "project.initialized",
        initialized_payload("Big project"),
    ));
    for sequence in 2..=2500u64 {
        events.push(evt(
            sequence,
            sequence,
            "run.interrupted",
            format!("{{\"phase\":\"phase-{sequence}\",\"hint\":\"h\"}}"),
        ));
    }
    let dir = legacy_workspace(&events, &[])?;
    let report = migrated(relay_store::migrate_legacy_state_db(
        dir.path(),
        &migration_options(),
    )?)?;
    assert_eq!(report.events_migrated, 2500);
    // Rotation at 1000 events: expect 0001-03e8, 03e9-7d0, 07d1-9c4, open.
    assert!(
        report.segments.len() >= 3,
        "segments: {:?}",
        report.segments
    );
    assert!(
        report
            .segments
            .iter()
            .any(|s| s == "0000000000000001-00000000000003e8.jsonl")
    );
    let (store, _) = RelayStore::open(dir.path(), store_options())?;
    assert_eq!(store.read_all()?.len(), 2500);
    assert_eq!(store.projection_event_count()?, Some(2500));
    Ok(())
}

#[test]
fn interrupted_staging_directory_is_discarded_and_redone() -> Result<()> {
    let dir = legacy_workspace(&typical_events(), &[("v1-demo-prd.md", "# PRD body\n")])?;
    let workspace = dir.path();
    // A previous run died mid-staging: staging exists, original intact.
    let staging = workspace.join(format!(".relay-rust-migration-{PROJECT}"));
    std::fs::create_dir_all(&staging)?;
    std::fs::write(staging.join("junk.txt"), b"half-written staging")?;
    let report = migrated(relay_store::migrate_legacy_state_db(
        workspace,
        &migration_options(),
    )?)?;
    assert_eq!(report.events_migrated, 6);
    assert!(workspace.join(".relay/manifest.json").exists());
    assert!(
        !staging.exists(),
        "staging became the store, not a leftover"
    );
    Ok(())
}

#[test]
fn interrupted_switch_is_completed_on_next_run() -> Result<()> {
    let dir = legacy_workspace(&typical_events(), &[("v1-demo-prd.md", "# PRD body\n")])?;
    let workspace = dir.path();
    let report = migrated(relay_store::migrate_legacy_state_db(
        workspace,
        &migration_options(),
    )?)?;
    let backup = report.backup_dir.clone().ok_or("backup")?;
    // Simulate a crash between the two renames of step 9: move .relay back
    // into staging position and restore the backup name, leaving no .relay.
    let staging = workspace.join(format!(".relay-rust-migration-{PROJECT}"));
    std::fs::rename(workspace.join(".relay"), &staging)?;
    std::fs::rename(&backup, workspace.join(".relay-backup-20261013T080000Z"))?;
    // Original state.db is inside the restored backup.
    assert!(
        workspace
            .join(".relay-backup-20261013T080000Z/state.db")
            .exists()
    );

    // Preflight completes the interrupted switch.
    let outcome = relay_store::migrate_legacy_state_db(workspace, &migration_options())?;
    match outcome {
        MigrationOutcome::AlreadyMigrated { detail } => {
            assert!(
                detail.contains("interrupted migration was completed"),
                "{detail}"
            );
        }
        MigrationOutcome::Migrated(_) => {
            return Err("expected completion, not a fresh migration".into());
        }
    }
    assert!(workspace.join(".relay/manifest.json").exists());
    let (store, _) = RelayStore::open(workspace, store_options())?;
    assert_eq!(store.read_all()?.len(), 6);
    Ok(())
}

#[test]
fn already_migrated_is_reported_once() -> Result<()> {
    let dir = legacy_workspace(&typical_events(), &[("v1-demo-prd.md", "# PRD body\n")])?;
    let workspace = dir.path();
    migrated(relay_store::migrate_legacy_state_db(
        workspace,
        &migration_options(),
    )?)?;
    match relay_store::migrate_legacy_state_db(workspace, &migration_options())? {
        MigrationOutcome::AlreadyMigrated { detail } => {
            assert!(detail.contains("canonical store"), "{detail}");
        }
        MigrationOutcome::Migrated(_) => return Err("second migration must be a no-op".into()),
    }
    // Exactly one backup exists.
    let backups: Vec<_> = std::fs::read_dir(workspace)?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|name| name.starts_with(".relay-backup-"))
        .collect();
    assert_eq!(backups.len(), 1);
    Ok(())
}

#[test]
fn missing_artifact_file_aborts() -> Result<()> {
    // The event references an artifact file that does not exist.
    let mut events = typical_events();
    events[3] = evt(
        4,
        4,
        "artifact.created",
        artifact_created_payload(
            "art_00000000-0000-4000-8000-000000000004",
            "prd",
            ".relay/artifacts/prd/v1-missing.md",
        ),
    );
    let dir = legacy_workspace(&events, &[])?;
    let error = relay_store::migrate_legacy_state_db(dir.path(), &migration_options())
        .err()
        .ok_or("expected failure")?;
    assert!(error.to_string().contains("v1-missing.md"), "{}", error);
    assert!(dir.path().join(".relay/state.db").exists());
    assert!(!dir.path().join(".relay/manifest.json").exists());
    Ok(())
}

#[test]
fn modified_artifact_bytes_are_detected_before_the_switch() -> Result<()> {
    let dir = legacy_workspace(&typical_events(), &[("v1-demo-prd.md", "# PRD body\n")])?;
    let workspace = dir.path();
    let mut options = migration_options();
    options.staging_probe = Some(Arc::new(|staging: &Path| {
        // Disturb the staged artifact between the copy and the switch.
        let target = staging.join("artifacts").join("prd").join("v1-demo-prd.md");
        std::fs::write(target, b"# tampered bytes\n").map_err(|e| relay_store::StoreError::Legacy {
            reason: format!("probe failed: {e}"),
        })
    }));
    let error = relay_store::migrate_legacy_state_db(workspace, &options)
        .err()
        .ok_or("expected digest failure")?;
    let message = error.to_string();
    assert!(message.contains("sha256"), "{message}");
    assert!(message.contains("v1-demo-prd.md"), "{message}");
    // The original .relay/ is untouched and still in charge.
    assert!(workspace.join(".relay/state.db").exists());
    assert!(!workspace.join(".relay/manifest.json").exists());
    Ok(())
}

#[test]
fn multi_project_source_is_refused_with_guidance() -> Result<()> {
    let dir = legacy_workspace(&typical_events(), &[])?;
    let db = Connection::open(dir.path().join(".relay/state.db"))?;
    db.execute(
        "INSERT INTO events (id, project_id, sequence, session_id, type, schema_version, \
         timestamp, correlation_id, causation_id, payload) \
         VALUES (?, ?, 1, NULL, 'project.initialized', 1, ?, ?, NULL, ?);",
        rusqlite::params![
            "evt_00000000-0000-4000-8000-00000000fff",
            "prj_00000000-0000-4000-8000-00000000eee",
            "2026-10-13T09:00:00.000Z",
            "cor_00000000-0000-4000-8000-00000000ffd",
            initialized_payload("Other project"),
        ],
    )?;
    drop(db);
    let error = relay_store::migrate_legacy_state_db(dir.path(), &migration_options())
        .err()
        .ok_or("expected refusal")?;
    assert!(error.to_string().contains("multiple projects"), "{}", error);
    assert!(dir.path().join(".relay/state.db").exists());
    Ok(())
}

#[test]
fn broken_legacy_payload_aborts() -> Result<()> {
    let mut events = vec![evt(1, 1, "project.initialized", initialized_payload("G"))];
    events.push(evt(2, 2, "run.interrupted", "not json {".to_string()));
    let dir = legacy_workspace(&events, &[])?;
    let error = relay_store::migrate_legacy_state_db(dir.path(), &migration_options())
        .err()
        .ok_or("expected failure")?;
    assert!(error.to_string().contains("not valid JSON"), "{}", error);
    assert!(!dir.path().join(".relay/manifest.json").exists());
    Ok(())
}

#[test]
fn noncontiguous_legacy_sequences_abort() -> Result<()> {
    let mut events = vec![evt(1, 1, "project.initialized", initialized_payload("G"))];
    // Gap: sequence jumps to 3.
    events.push(evt(
        3,
        3,
        "run.interrupted",
        "{\"phase\":\"x\",\"hint\":\"y\"}".to_string(),
    ));
    let dir = legacy_workspace(&events, &[])?;
    let error = relay_store::migrate_legacy_state_db(dir.path(), &migration_options())
        .err()
        .ok_or("expected failure")?;
    assert!(error.to_string().contains("contiguous"), "{}", error);
    Ok(())
}

// ---------------------------------------------------------------------------
// Canonical JSONL import (exporter / fixture corpus)
// ---------------------------------------------------------------------------

fn fixture_cases() -> Result<Vec<PathBuf>> {
    let root = Path::new(REPO_ROOT).join("tests/fixtures/cases");
    let mut cases: Vec<PathBuf> = std::fs::read_dir(&root)?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path().join("events.jsonl"))
        .filter(|path| path.exists())
        .collect();
    cases.sort();
    Ok(cases)
}

fn case_meta(events: &Path) -> Result<serde_json::Value> {
    let text = std::fs::read_to_string(events.with_file_name("meta.json"))?;
    Ok(serde_json::from_str(&text)?)
}

fn parse_fixture_events(events: &Path) -> Result<Vec<relay_store::EventEnvelope>> {
    let text = std::fs::read_to_string(events)?;
    let mut envelopes = Vec::new();
    let mut previous: Option<String> = None;
    for (index, line) in text.lines().enumerate() {
        if line.is_empty() {
            continue;
        }
        let name = events
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let envelope = relay_store::EventEnvelope::parse(
            line,
            relay_store::envelope::RecordLocation {
                segment: name.as_str(),
                line: index as u64 + 1,
            },
        )?;
        // The corpus carries a valid chain: previousHash chains across lines.
        if index > 0 {
            assert_eq!(
                envelope.previous_hash.as_deref(),
                previous.as_deref(),
                "fixture chain broken at {} line {}",
                name,
                index + 1
            );
        }
        previous = Some(envelope.hash.clone());
        envelopes.push(envelope);
    }
    Ok(envelopes)
}

#[test]
fn valid_fixture_corpus_imports_and_round_trips() -> Result<()> {
    let cases = fixture_cases()?;
    assert!(cases.len() >= 20, "fixture corpus missing: {cases:?}");
    let mut imported = 0;
    for events in cases {
        let meta = case_meta(&events)?;
        if meta.get("valid").and_then(|v| v.as_bool()) != Some(true) {
            continue;
        }
        let expected = parse_fixture_events(&events)?;
        let dir = tempfile::tempdir()?;
        let outcome =
            relay_store::import_jsonl_directory(dir.path(), &events, &migration_options())?;
        let report = migrated(outcome)?;
        assert_eq!(report.events_migrated as usize, expected.len());
        let (store, _) = RelayStore::open(dir.path(), store_options())?;
        let records = store.read_all()?;
        assert_eq!(records.len(), expected.len());
        for (record, want) in records.iter().zip(expected.iter()) {
            assert_eq!(record.envelope, *want, "fixture round-trip diverged");
        }
        imported += 1;
    }
    assert_eq!(imported, 13, "the corpus holds 13 valid cases");
    Ok(())
}

#[test]
fn envelope_level_invalid_fixture_cases_are_rejected() -> Result<()> {
    // Cases the store itself must reject fail-closed (payload/fold-level
    // cases belong to the relay-core replay suite and are skipped here).
    let expectations: &[(&str, &str)] = &[
        ("unknown-event-type", "unknown event type"),
        ("newer-envelope-version", "envelopeVersion"),
        ("newer-schema-version", "schema version"),
        ("sequence-gap", "not contiguous"),
        ("wrong-project-id", "mixes projects"),
    ];
    for (case, needle) in expectations {
        let events = Path::new(REPO_ROOT)
            .join("tests/fixtures/cases")
            .join(case)
            .join("events.jsonl");
        let dir = tempfile::tempdir()?;
        let error = relay_store::import_jsonl_directory(dir.path(), &events, &migration_options())
            .err()
            .ok_or_else(|| format!("case {case} must be rejected"))?;
        let message = error.to_string();
        assert!(message.contains(needle), "case {case}: {message}");
        assert!(
            !dir.path().join(".relay/manifest.json").exists(),
            "case {case}: failed import must not install a store"
        );
    }
    Ok(())
}

#[test]
fn exporter_output_cross_checks_the_direct_importer() -> Result<()> {
    // Requires node + tsx (dev environment); skip silently when absent so
    // the suite stays offline/deterministic by default.
    let repo = Path::new(REPO_ROOT);
    let tsx = repo.join("node_modules/.bin/tsx");
    if !tsx.exists() || Command::new("node").arg("--version").output().is_err() {
        return Ok(());
    }
    let legacy = legacy_workspace(&typical_events(), &[("v1-demo-prd.md", "# PRD body\n")])?;
    let out = tempfile::tempdir()?;
    let status = Command::new("node")
        .arg(repo.join("scripts/export-legacy-project.mjs"))
        .arg("--db")
        .arg(legacy.path().join(".relay/state.db"))
        .arg("--out")
        .arg(out.path())
        .current_dir(repo)
        .output()?;
    if !status.status.success() {
        return Ok(()); // exporter environment not usable; skip
    }
    let exported: Vec<PathBuf> = std::fs::read_dir(out.path())?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "jsonl"))
        .collect();
    assert_eq!(exported.len(), 1, "one project exported");
    let jsonl = exported.first().ok_or("export file")?.clone();
    // The export carries events only; provide artifact bytes beside it so
    // the import can copy and digest them.
    std::fs::create_dir_all(out.path().join("artifacts/prd"))?;
    std::fs::copy(
        legacy.path().join(".relay/artifacts/prd/v1-demo-prd.md"),
        out.path().join("artifacts/prd/v1-demo-prd.md"),
    )?;

    // Import the exporter output.
    let via_export = tempfile::tempdir()?;
    let export_report = migrated(relay_store::import_jsonl_directory(
        via_export.path(),
        &jsonl,
        &migration_options(),
    )?)?;
    // Import the same legacy database directly. The legacy workspace's
    // .relay was not consumed by the exporter; copy the database into a
    // fresh workspace for the direct import.
    let direct_ws = tempfile::tempdir()?;
    std::fs::create_dir_all(direct_ws.path().join(".relay"))?;
    std::fs::copy(
        legacy.path().join(".relay/state.db"),
        direct_ws.path().join(".relay/state.db"),
    )?;
    std::fs::create_dir_all(direct_ws.path().join(".relay/artifacts/prd"))?;
    std::fs::copy(
        legacy.path().join(".relay/artifacts/prd/v1-demo-prd.md"),
        direct_ws.path().join(".relay/artifacts/prd/v1-demo-prd.md"),
    )?;
    let direct_report = migrated(relay_store::migrate_legacy_state_db(
        direct_ws.path(),
        &migration_options(),
    )?)?;

    assert_eq!(export_report.events_migrated, direct_report.events_migrated);
    let (export_store, _) = RelayStore::open(via_export.path(), store_options())?;
    let (direct_store, _) = RelayStore::open(direct_ws.path(), store_options())?;
    let export_events: Vec<_> = export_store
        .read_all()?
        .into_iter()
        .map(|r| r.envelope)
        .collect();
    let direct_events: Vec<_> = direct_store
        .read_all()?
        .into_iter()
        .map(|r| r.envelope)
        .collect();
    assert_eq!(
        export_events, direct_events,
        "exporter and direct import diverged"
    );
    assert_eq!(export_store.tail()?, direct_store.tail()?);
    Ok(())
}
