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
//! - [`conversation`]: the conversational turn service — typed proposals,
//!   bounded context, provider streaming, proposal application.
//! - [`conversation_views`]: stable chat transcript views over the durable
//!   conversation facts.
//! - [`runner`]: orchestrator composition for `run` (which executors, which
//!   model client, how jobs spawn).
//! - [`routing`]: the catalog-phase → routing-purpose mapping grants consult.
//! - [`runtime_routing`]: configuration-driven routing for runs and
//!   conversational turns (registry, plan, turn seam).
//! - [`views`]: serde view models — the single shape JSON and human
//!   presentations render.
//! - [`markdown`]: deterministic Markdown views over published artifact
//!   versions.
//! - [`ids`]: application-owned deterministic id minting.
//! - [`jobs`]: deterministic system jobs.

pub mod commands;
pub mod conversation;
pub mod conversation_views;
pub mod error;
pub mod ids;
pub mod jobs;
pub mod markdown;
pub mod routing;
pub mod runner;
pub mod runtime_routing;
pub mod session;
pub mod sink;
pub mod views;

pub use error::AppError;
pub use runner::RuntimeWiring;
pub use runtime_routing::{RuntimeRouting, TurnRouting};
pub use session::{ProjectSession, SessionOpenReport, SessionOptions, SessionSnapshot};
pub use sink::StoreFactSink;
