//! Semantic migration: legacy Relay projects → intent-first fact stores.
//!
//! Phase 4 of the intent-first plan (`docs/plans/2026-10-13-intent-first-
//! rust-work-graph.md`): extract durable product meaning from an existing
//! legacy project — a TypeScript `.relay/state.db` or an old Rust JSONL
//! event store — into a new `.relay-graph` fact store, without reproducing
//! every legacy internal event.
//!
//! Guarantees:
//!
//! - the legacy source is opened **read-only** and never modified;
//! - nothing is fabricated: coarse legacy relationships stay coarse and are
//!   surfaced in the report, and legacy kinds with no durable meaning are
//!   reported as unmapped;
//! - the staged fact stream must fold cleanly through the domain graph and
//!   pass the fact doctor before the switch is allowed;
//! - the switch is two atomic renames with the previous store (if any)
//!   retained; any failure before the switch leaves the workspace exactly
//!   as it was, and [`rollback`] restores a completed migration on request.
//!
//! Module map: [`legacy_source`] (read-only evidence readers),
//! [`mod@extract`] (the pure meaning mapping), [`stage`] (staging,
//! validation gate, atomic switch, rollback).

pub mod extract;
pub mod legacy_source;
pub mod stage;

use std::path::Path;

use crate::error::Result;
pub use crate::migrate::legacy_source::LegacySource;

pub use extract::{
    ExtractionReport, Inference, LEGACY_DOCUMENT_SCHEMA, MappingEntry, ReviewNeeded, UnmappedKind,
    extract, sha256_hex,
};
pub use legacy_source::{LegacyEvent, LegacyEvidence};
pub use stage::{STAGING_DIR, SemanticMigrationOptions, SemanticMigrationOutcome, rollback};

/// Migrate the legacy project in `workspace_root` into a new
/// `.relay-graph` fact store.
///
/// `source` says where the legacy evidence lives (usually
/// [`default_source`]). The workflow: read and validate the legacy
/// evidence → extract durable meaning → stage into
/// `.relay-graph-migration-staging` → validate (graph fold + doctor) →
/// atomically switch, retaining any previous fact store as
/// `.relay-graph-pre-migration-<stamp>`. The legacy source itself is never
/// touched.
pub fn migrate(
    workspace_root: &Path,
    source: &LegacySource,
    options: &SemanticMigrationOptions,
) -> Result<SemanticMigrationOutcome> {
    let evidence = source.read()?;
    let (facts, report) = extract::extract(&evidence)?;

    let staging_root = workspace_root.join(stage::STAGING_DIR);
    let _paths = stage::stage(&staging_root, &evidence, &facts, options)?;

    let stamp: String = options
        .store
        .clock
        .now_iso()
        .chars()
        .filter(|c| *c != '-' && *c != ':' && *c != '.')
        .collect();
    let previous = stage::switch(workspace_root, &staging_root, &stamp)?;

    Ok(SemanticMigrationOutcome {
        report,
        store_root: workspace_root.join(crate::fact::paths::FACT_STORE_DIR),
        previous_store_backup: previous,
        facts: facts.len() as u64,
    })
}

/// The conventional legacy source of a workspace: `.relay/state.db` when it
/// exists, else the `.relay` JSONL event store, with artifacts read from
/// `.relay/artifacts/`.
pub fn default_source(workspace_root: &Path) -> Option<LegacySource> {
    let relay = workspace_root.join(".relay");
    let artifacts_dir = relay.join("artifacts");
    let state_db = relay.join("state.db");
    if state_db.is_file() {
        return Some(LegacySource::StateDb {
            db: state_db,
            artifacts_dir,
        });
    }
    if relay.join("events").is_dir() || relay.join("events.jsonl").is_file() {
        return Some(LegacySource::JsonlStore {
            root: relay,
            artifacts_dir,
        });
    }
    None
}

/// Extract only, without staging or switching: useful for previews and
/// reports (`relay migrate --dry-run`).
pub fn plan(source: &LegacySource) -> Result<(Vec<relay_domain::DomainFact>, ExtractionReport)> {
    let evidence = source.read()?;
    extract::extract(&evidence)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_source_detects_state_db_and_jsonl() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        assert!(default_source(dir.path()).is_none());

        let relay = dir.path().join(".relay");
        std::fs::create_dir_all(relay.join("artifacts"))?;
        std::fs::write(relay.join("state.db"), b"placeholder")?;
        assert!(matches!(
            default_source(dir.path()),
            Some(LegacySource::StateDb { .. })
        ));
        std::fs::remove_file(relay.join("state.db"))?;
        assert!(default_source(dir.path()).is_none());
        std::fs::create_dir_all(relay.join("events"))?;
        assert!(matches!(
            default_source(dir.path()),
            Some(LegacySource::JsonlStore { .. })
        ));
        Ok(())
    }
}
