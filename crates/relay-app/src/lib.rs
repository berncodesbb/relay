//! Relay application service.
//!
//! Small command handlers composed over engine, store, and executor ports:
//! submit intents, query projections, schedule ready work, accept human
//! decisions, cancel, and explain policy results. Deliberately not a
//! monolithic controller — each handler is a focused unit that validates a
//! command, persists accepted facts through the checked store, and returns
//! a typed view. Presentation (CLI/TUI) consumes the returned view models
//! and owns no domain policy.
//!
//! Module map:
//!
//! - [`session`]: [`ProjectSession`] — open/create, cached graph +
//!   evaluation, refresh after every append.
//! - [`sink`]: [`StoreFactSink`] — the executor `FactSink` port over the
//!   graph fact store (lock per append, never across executor work).
//! - [`commands`]: the command handlers (init, expand, resolve, cancel,
//!   import, publish, doctor, queries).
//! - [`runner`]: orchestrator composition for `run` (which executors, which
//!   model client, how jobs spawn).
//! - [`providers`]: provider-name resolution for runs (preview wiring).
//! - [`views`]: serde view models — the single shape JSON and human
//!   presentations render.
//! - [`markdown`]: deterministic Markdown views over published artifact
//!   versions.
//! - [`ids`]: application-owned deterministic id minting.
//! - [`jobs`]: deterministic system jobs.

pub mod commands;
pub mod error;
pub mod ids;
pub mod jobs;
pub mod markdown;
pub mod providers;
pub mod runner;
pub mod session;
pub mod sink;
pub mod views;

pub use error::AppError;
pub use runner::RuntimeWiring;
pub use session::{ProjectSession, SessionOpenReport, SessionOptions, SessionSnapshot};
pub use sink::StoreFactSink;
