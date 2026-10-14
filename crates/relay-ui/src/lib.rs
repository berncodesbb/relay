//! Stable Relay view models, human/JSON renderers, and the terminal
//! interface. This crate owns everything user-visible that is not the
//! executable itself:
//!
//! - [`work`]: the work-graph terminal interface — immutable snapshots of
//!   `relay-app` view models, a typed keymap, the ratatui screens, and the
//!   terminal-restoration guard. This is the primary surface.
//! - [`chat`]: the chat-first terminal interface — a Unicode-safe
//!   multiline prompt editor, a typed keymap, and bounded transcript
//!   rendering. The conversational surface; the chat state machine and
//!   screens land with the application conversation service.
//! - [`views`]: the `/status` and `/why` view models (`build_status_view` /
//!   `build_why_view`) over `relay-core` state and events, byte-compatible
//!   with the retired `src/runtime/controller-views.ts` (removed from the
//!   tree 2026-10-14) and pinned by the fixture goldens under
//!   `tests/fixtures/cases/*/expected-{status,why}.json`. Frozen reference
//!   material from the stopped compatibility port, kept building until the
//!   retirement gate.
//! - [`render`]: the human status/why renderers ported from the retired
//!   `src/cli/output.ts` (removed 2026-10-14; reference, same freeze as
//!   [`views`]).
//! - [`tui`]: the interactive view-model fold, the controller-feed seam, and
//!   the ratatui components ported from the retired `src/tui/**` (removed
//!   2026-10-14; reference; the truncation/capping helpers are shared with
//!   [`work`]).
//!
//! Library crates must not print: every renderer returns strings, and only
//! the `relay-cli` binary writes to stdout/stderr.

pub mod chat;
pub mod render;
pub mod tui;
pub mod views;
pub mod work;

pub use render::{render_status, render_why};
pub use tui::components::{
    MAX_PANEL_LINES, PauseOverlay, RenderInputs, cap_lines, draw, pause_overlay_for, truncate_text,
};
pub use tui::feed::{ControllerFeed, feed_is_uninitialized, read_feed_state};
pub use tui::view_model::{
    ControllerEvent, DEFAULT_COLUMNS, OUTCOME_LABELS, OutcomeValue, Panel, TRANSCRIPT_CAP,
    ToolPhase, TranscriptEntry, TuiAction, TuiViewModel, ViewModel, entry_is_dim, entry_prefix,
    entry_text,
};
pub use tui::view_text::{
    HELP_TEXT, INSPECT_KEY_ORDER, Loose, STATUS_KEY_ORDER, WHY_KEY_ORDER, loose_inspect,
    loose_status, loose_why, render_inspect_body, render_view_text,
};
pub use views::{
    BLOCKER_LIMIT, ROUTING_TAIL_EVENTS, StatusArtifact, StatusPause, StatusProgress, StatusStats,
    StatusView, TRANSITION_TAIL, WhyOutcome, WhyRejection, WhyRouting, WhyTransition, WhyView,
    build_status_view, build_why_view, changed_files_for_session, view_json,
};
