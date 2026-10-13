//! Generic fact persistence layer for the intent-first work graph
//! (`docs/plans/fact-format-proposal.md`).
//!
//! This layer is the storage side of the new authority order:
//!
//! ```text
//! canonical JSONL facts -> deterministic replay -> work-graph state
//!                     -> SQLite projections -> efficient queries
//! ```
//!
//! It reuses the hardened infrastructure of the legacy event store —
//! segment naming/discovery/tiling/rotation ([`crate::segments`]), atomic
//! manifest writes, the cross-process writer lock ([`crate::lock`]),
//! conservative crash-tail recovery, and the fault-injection hooks
//! ([`crate::fault`]) — but is **parameterized over an opaque record**: a
//! sequence number, a fact type (an open string the domain owns), a
//! per-type fact version, a JSON payload, and the envelope timestamp. It
//! has no knowledge of relay-core event types or any domain fact catalog;
//! type/version acceptance and projection folds are injected by the domain
//! through [`FactValidator`] and [`FactFold`].
//!
//! The legacy event path (`envelope`, `log::EventLog`, `projection`,
//! `store::RelayStore`) is unchanged and remains the authority for the
//! frozen compatibility surface until its documented retirement.
//!
//! # Module map
//!
//! - [`canonical`]: the fact format's canonical JSON serializer, local to
//!   this crate (the legacy path keeps using relay-core's).
//! - [`record`]: the fact record, integrity levels, sealing/checksums.
//! - [`validator`]: the domain-supplied type/version acceptance gate.
//! - [`paths`]: the `.relay-graph` layout.
//! - [`manifest`]: the fact-store manifest.
//! - [`log`]: the [`FactLog`] (open/validate/recover, append, rotate,
//!   read).
//! - [`projection`]: the [`FactProjection`] rebuild primitive (storage
//!   tables, checkpoint, reconcile, fold callback).
//! - [`store`]: the [`FactStore`] orchestrator and append protocol.
//! - [`domain`]: the typed seam to `relay-domain` — the
//!   [`DomainFactValidator`] gate and the [`GraphFactStore`] wrapper.
//! - [`graph`]: the [`GraphFold`] projection fold and its lookup queries.
//! - [`doctor`]: read-only `check` and `rebuild_projection` services.

pub mod canonical;
pub mod doctor;
pub mod domain;
pub mod graph;
pub mod log;
pub mod manifest;
pub mod paths;
pub mod projection;
pub mod record;
pub mod store;
pub mod validator;

pub use canonical::{parse as parse_json, serialize as serialize_json};
pub use doctor::{FactFinding, FactSeverity, check, rebuild_projection};
pub use domain::{
    DomainFactValidator, GraphAppendOutcome, GraphFactStore, TimelineFact, from_record, to_input,
};
pub use graph::{
    ArtifactVersionRow, AttemptRow, DecisionFilter, DecisionRow, DependencyRow, EvidenceRow,
    GraphFold, GraphMeta, InvalidationRow, WorkFilter, WorkRow, artifact_versions, attempts,
    decisions, dependencies, evidence, graph_meta, invalidations, work, works,
};
pub use log::{FactLog, FactTail};
pub use manifest::{FACT_MIN_READER_VERSION, FACT_STORE_VERSION, FactManifest};
pub use paths::{FACT_STORE_DIR, FactPaths};
pub use projection::{
    FactCheckpoint, FactFold, FactProjection, FactReconcileOutcome, FactRow, NoFold,
};
pub use record::{FactEntry, FactInput, FactRecord, Integrity};
pub use store::{
    FactAppendOutcome, FactOpenReport, FactRebuildReport, FactStore, FactStoreOptions,
};
pub use validator::{AcceptAllFacts, FactValidator, KnownFactVersions};
