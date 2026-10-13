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
//!
//! # relay-core integration (pending)
//!
//! The envelope type, event-type list, payload schemas, and canonical
//! serialization will be owned by `relay-core`. Until that crate publishes
//! them, [`canonical`] and [`envelope`] carry store-local implementations
//! exactly per the normative spec, and payload validation is an injectable
//! [`envelope::PayloadValidator`] hook. When relay-core lands: bind the
//! hook to its schemas, re-export its serializer from [`canonical`], and
//! replace the mirrored type list — no other module needs to change.

pub mod canonical;
pub mod clock;
pub mod doctor;
pub mod envelope;
pub mod error;
pub mod fault;
pub mod legacy;
pub mod lock;
pub mod log;
pub mod manifest;
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
pub use envelope::PayloadValidator;
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
