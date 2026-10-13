//! Canonical event storage for Relay.
//!
//! This crate implements the storage layer specified by
//! `docs/event-format.md` (store format version 1): a segmented,
//! hash-chained, append-only JSONL event log as the sole authority for
//! workflow state; a disposable SQLite projection with a sequence/hash
//! checkpoint for efficient queries; conservative crash-tail recovery; a
//! cross-platform single-writer lock; read-only `doctor` services; and a
//! rollback-safe importer for the legacy `.relay/state.db` TypeScript
//! store.
//!
//! # Authority order
//!
//! ```text
//! canonical JSONL events -> deterministic replay -> current project state
//!                        -> SQLite projections -> efficient queries
//! ```
//!
//! If JSONL and SQLite disagree, JSONL wins and the index is repaired or
//! rebuilt. Deleting `index.sqlite` between runs always produces an
//! equivalent database.
//!
//! # Module map
//!
//! - [`canonical`]: canonical JSON value + serializer (relay-core
//!   integration seam).
//! - [`envelope`]: event envelope, validation, hash chain (relay-core
//!   integration seam).
//! - [`clock`]: injected clocks and id generation.
//! - [`paths`]: `.relay` layout.
//! - [`segments`]: naming, discovery, tiling, rotation thresholds.
//! - [`manifest`]: atomic manifest read/write.
//! - [`lock`]: writer lock with stale-PID detection.
//! - [`log`]: the [`log::EventLog`] (open/validate/recover, append,
//!   rotate, read).
//! - [`projection`]: the [`projection::SqliteProjection`] (schema,
//!   folds, checkpoint, reconcile, rebuild).
//! - [`store`]: the [`store::RelayStore`] orchestrator and append
//!   protocol.
//! - [`fault`]: crash-injection hooks for tests.
//! - [`doctor`]: read-only `check` and `rebuild_index` services.
//! - [`legacy`]: legacy `state.db` and JSONL-directory import.
//! - [`fact`]: the generic fact-persistence layer for the intent-first
//!   work graph (`docs/plans/fact-format-proposal.md`) — a segmented JSONL
//!   fact log, disposable SQLite projection with a fold callback, and the
//!   [`fact::FactStore`] orchestrator, all parameterized over an opaque
//!   record and independent of relay-core.
//!
//! # relay-core integration (complete)
//!
//! Canonical byte serialization comes from
//! [`relay_core::canonical::canonical_json`] (via [`canonical::Json`]), and
//! envelope rules, the event-type list, payload schema versions, typed
//! payload validation, and hash computation come from
//! [`relay_core::events`] (via [`envelope`]). The store adds only the
//! log-layer chain rules (`previousHash` null exactly at sequence 1, stored
//! `hash` must match) and storage diagnostics.
//!
//! The [`fact`] layer shares the storage-owned [`canonical::Json`] **value
//! type** but serializes through its own module ([`fact::canonical`]),
//! which delegates to the one shared canonicalizer in
//! `relay_artifacts::canonical`; it never calls into relay-core.

pub mod canonical;
pub mod clock;
pub mod doctor;
pub mod envelope;
pub mod error;
pub mod fact;
pub mod fault;
pub mod legacy;
pub mod lock;
pub mod log;
pub mod manifest;
pub mod migrate;
pub mod paths;
pub mod projection;
pub mod segments;
pub mod store;

pub use canonical::Json;
pub use clock::Clock;
pub use clock::FixedClock;
pub use clock::IdGen;
pub use clock::SequentialIds;
pub use clock::SystemClock;
pub use clock::UuidIdGen;
pub use envelope::EventEnvelope;
pub use error::Result;
pub use error::StoreError;
pub use fault::CrashAt;
pub use fault::FaultInjector;
pub use fault::FaultPoint;
pub use fault::NoFaults;
pub use legacy::ArtifactDigest;
pub use legacy::MigrationOptions;
pub use legacy::MigrationOutcome;
pub use legacy::MigrationReport;
pub use legacy::import_jsonl_directory;
pub use legacy::migrate_legacy_state_db;
pub use log::Durability;
pub use log::EventLog;
pub use log::EventRecord;
pub use log::OpenMode;
pub use log::RecoveredTail;
pub use log::TailState;
pub use manifest::Manifest;
pub use paths::RelayPaths;
pub use projection::Checkpoint;
pub use projection::EventRow;
pub use projection::ProjectSummaryRow;
pub use projection::ReconcileOutcome;
pub use projection::SqliteProjection;
pub use segments::SegmentListing;
pub use segments::SegmentName;
pub use store::AppendInput;
pub use store::AppendOutcome;
pub use store::OpenReport;
pub use store::RebuildReport;
pub use store::RelayStore;
pub use store::StoreOptions;

// Generic fact-persistence layer (intent-first work graph). Re-exports the
// primary surface so downstream crates can `use relay_store::...` uniformly;
// everything also remains reachable through `relay_store::fact::...`.
pub use fact::AcceptAllFacts;
pub use fact::FACT_STORE_DIR;
pub use fact::FactAppendOutcome;
pub use fact::FactEntry;
pub use fact::FactFold;
pub use fact::FactInput;
pub use fact::FactLog;
pub use fact::FactManifest;
pub use fact::FactOpenReport;
pub use fact::FactPaths;
pub use fact::FactProjection;
pub use fact::FactRebuildReport;
pub use fact::FactReconcileOutcome;
pub use fact::FactRecord;
pub use fact::FactStore;
pub use fact::FactStoreOptions;
pub use fact::FactTail;
pub use fact::FactValidator;
pub use fact::Integrity;
pub use fact::KnownFactVersions;
pub use fact::NoFold;
pub use fact::parse_json;
pub use fact::serialize_json;
// Domain seam + graph projection + doctor (wave 2).
pub use fact::doctor::{FactFinding, FactSeverity, check as fact_doctor_check};
pub use fact::{
    ArtifactVersionRow, AttemptRow, DecisionFilter, DecisionRow, DependencyRow,
    DomainFactValidator, EvidenceRow, GraphAppendOutcome, GraphFactStore, GraphFold, GraphMeta,
    InvalidationRow, TimelineFact, WorkFilter, WorkRow, artifact_versions, attempts, decisions,
    dependencies, evidence, from_record, graph_meta, invalidations, to_input, work, works,
};
