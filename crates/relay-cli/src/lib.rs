//! Relay native command-line executable library.
//!
//! The binary entry point lives in `src/main.rs`; this library exposes the
//! reusable pieces so integration tests drive argument parsing, command
//! dispatch, and output writing in-process over captured buffers.
//!
//! - [`args`]: the work-centric argument grammar plus the legacy-command
//!   recognition table (the frozen byte-compat grammar from the stopped
//!   compatibility port is the reference for what must be *recognized*, not
//!   reproduced).
//! - [`dispatch`]: argv → command handler → view model → exit code, and the
//!   legacy-command mapping.
//! - [`render`]: human-readable rendering of the application's view models.
//! - [`tui`]: the interactive terminal driver (the only terminal-touching
//!   path).
//! - [`output`]: the single stdout/stderr writer all commands flow through.
//! - [`errors`]: the frozen error-name → exit-code mapping from the
//!   compatibility port (reference for the 0/1/2 house convention).
//!
//! The old `ops.rs` controller-shaped query surface (`DirectStoreOps` over
//! `relay-core`) was retired with this restructuring: the work-graph
//! application service in `relay-app` replaces it, and nothing in this
//! crate touches `relay-core` anymore. The file is kept on disk at
//! `src/ops.rs` as **inert reference material** — it is not declared as a
//! module, so it is neither compiled, formatted, nor linted; restore it
//! with `pub mod ops;` (and the `relay-core`/`relay-store`/`serde_yaml`
//! dependencies) only for archaeology.

pub mod args;
pub mod dispatch;
pub mod errors;
pub mod output;
pub mod render;
pub mod tui;

pub use dispatch::{
    CliDeps, EXIT_FAILURE, EXIT_OK, EXIT_USAGE, RuntimeSelection, run_main, run_with,
};
