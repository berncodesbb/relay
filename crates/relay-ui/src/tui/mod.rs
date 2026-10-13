//! The interactive terminal interface: the view-model fold, the
//! controller-feed seam, defensive view-to-text rendering, and the ratatui
//! components. Ported from `src/tui/**`. Nothing here prints or owns a
//! terminal; the `relay-cli` binary supplies the crossterm backend and the
//! terminal-restore lifecycle.

pub mod components;
pub mod feed;
pub mod view_model;
pub mod view_text;

#[cfg(test)]
mod snapshots;

pub use components::{
    MAX_PANEL_LINES, PauseOverlay, RenderInputs, cap_lines, draw, pause_overlay_for, truncate_text,
};
pub use feed::{ControllerFeed, feed_is_uninitialized, read_feed_state};
pub use view_model::{
    ControllerEvent, DEFAULT_COLUMNS, OUTCOME_LABELS, OutcomeValue, Panel, TRANSCRIPT_CAP,
    ToolPhase, TranscriptEntry, TuiAction, TuiViewModel, ViewModel, entry_is_dim, entry_prefix,
    entry_text,
};
pub use view_text::{
    HELP_TEXT, INSPECT_KEY_ORDER, Loose, STATUS_KEY_ORDER, WHY_KEY_ORDER, loose_inspect,
    loose_status, loose_why, render_inspect_body, render_view_text,
};
