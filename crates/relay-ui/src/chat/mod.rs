//! The chat-first terminal interface: a Unicode-safe multiline prompt
//! editor, a typed keymap, bounded transcript rendering, and the
//! presentation-only chat state machine.
//!
//! Nothing here decides domain policy. [`ChatUi`] holds presentation
//! state — the latest immutable [`ChatView`] and work snapshot, the
//! editor buffer and history, transcript scroll, overlay focus, busy and
//! cancelling flags, and the ephemeral stream/activity tails — and
//! translates decoded keys and submitted text into typed
//! [`ChatUiRequest`]s the CLI driver maps onto the application
//! conversation service. It never classifies intent, resolves decisions,
//! or touches the store. Drawing lives in [`screens`] and targets a
//! ratatui backend (`TestBackend` in tests), so no TTY is ever required.
//!
//! - [`editor`]: the multiline prompt buffer — char-based (multibyte-safe
//!   by construction), with a cursor, column-intent-preserving vertical
//!   movement, local history with draft stashing, and an explicit size
//!   bound.
//! - [`keymap`]: the typed keymap — decoded [`keymap::ChatKey`]s to
//!   [`keymap::ChatAction`]s under an editor-focus or overlay-focus mode.
//! - [`transcript`]: pure rendering helpers — char-safe wrapping, speaker
//!   and activity line formatting, transcript bounding with an ellipsis
//!   header, and ephemeral-vs-durable reconciliation.
//! - [`screens`]: the ratatui chat layout, overlays, and narrow/short
//!   degradation.

pub mod editor;
pub mod keymap;
pub mod screens;
pub mod transcript;

use relay_app::conversation::ConversationEvent;
use relay_app::conversation_views::ChatEntryKind;
use relay_app::conversation_views::ChatView;

use crate::chat::editor::Editor;
use crate::chat::keymap::{ChatAction, ChatKey, ChatKeymap, KeymapMode};
use crate::chat::transcript::{
    CANCELLED_MARKER, DECISION_MARKER, FAILED_MARKER, INTERRUPTED_MARKER, activity_block,
    assistant_block, marker_line, reconcile_ephemeral, user_block,
};
use crate::work::WorkSnapshot;

/// The maximum remembered ephemeral activity lines (oldest dropped).
pub const ACTIVITY_MAX_LINES: usize = 200;

/// The maximum chars of the ephemeral streaming tail (newest kept).
pub const STREAM_TAIL_MAX_CHARS: usize = 4_096;

/// The maximum lines of the ephemeral streaming tail (newest kept).
pub const STREAM_TAIL_MAX_LINES: usize = 64;

/// How many transcript lines one scroll action moves. The state machine
/// has no render height, so scrolling uses a fixed step; the renderer
/// clamps the offset against the visible content.
pub const SCROLL_STEP_LINES: usize = 10;

/// The saturating upper bound for the scroll offset ("jump to top").
pub const SCROLL_MAX: usize = 1_000_000;

/// The status shown when submission is refused because a turn is running.
const WAIT_STATUS: &str = "wait: a turn is running (Ctrl-C cancels)";

/// The status shown when the draft cannot grow further.
const DRAFT_LIMIT_STATUS: &str = "draft limit reached (16,384 chars); input was not accepted";

/// The transcript placeholder shown before any message exists.
const INVITE_LINE: &str =
    "(no messages yet — ask a question or describe an outcome; /help lists commands)";

/// A typed request the driver maps onto the application service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatUiRequest {
    /// Submit a prompt (one conversational turn).
    SubmitPrompt {
        /// The prompt text (non-empty, not slash-prefixed).
        text: String,
    },
    /// Cancel the running turn.
    CancelTurn,
    /// Refresh and show workspace/conversation status.
    SlashStatus,
    /// Refresh and show ready and blocked work.
    SlashWork,
    /// Refresh and show open decisions.
    SlashDecisions,
    /// Inspect one work item.
    SlashInspect {
        /// The work id.
        id: String,
    },
    /// Leave the interface.
    Quit,
}

/// What handling a key or submission produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatUiOutcome {
    /// Keep running.
    Continue,
    /// The driver should run this request (then refresh the views).
    Request(ChatUiRequest),
    /// The user asked to quit.
    Quit,
}

/// An overlay pane rendered over the chat screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatOverlay {
    /// The keymap and slash-command help.
    Help,
    /// Workspace and conversation status.
    Status,
    /// Ready and blocked work rows.
    Work,
    /// Open decisions with their options.
    Decisions,
    /// One work item's detail (`String` is the work id; the content comes
    /// from the work snapshot's `detail`, delivered by [`ChatUi::set_work`]).
    Detail(String),
    /// One artifact's rendered view (`String` is the reference/title; the
    /// content comes from the work snapshot's `viewer`, delivered by
    /// [`ChatUi::set_work`]).
    Artifact(String),
}

/// The chat interface state machine. Presentation only: no policy, no
/// IO, no clocks — durable state arrives through [`ChatUi::set_view`] and
/// [`ChatUi::set_work`], ephemeral turn progress through
/// [`ChatUi::push_event`].
pub struct ChatUi {
    view: ChatView,
    work: WorkSnapshot,
    workspace_path: String,
    editor: Editor,
    keymap: ChatKeymap,
    busy: bool,
    cancelling: bool,
    status: String,
    scroll: usize,
    stream_tail: String,
    activity: Vec<String>,
    /// The running turn (`TurnId` as a string; kept as text so this crate
    /// needs no `relay-domain` dependency), used to reconcile the
    /// ephemeral stream against the durable view.
    active_turn: Option<String>,
    overlay: Option<ChatOverlay>,
}

impl ChatUi {
    /// A new interface over an initial view, work snapshot, and workspace
    /// path. Starts at the transcript bottom with no overlay.
    pub fn new(view: ChatView, work: WorkSnapshot, workspace_path: String) -> Self {
        ChatUi {
            view,
            work,
            workspace_path,
            editor: Editor::new(),
            keymap: ChatKeymap,
            busy: false,
            cancelling: false,
            status: String::new(),
            scroll: 0,
            stream_tail: String::new(),
            activity: Vec::new(),
            active_turn: None,
            overlay: None,
        }
    }

    /// Replace the durable chat view, reconciling the ephemeral state:
    /// the streaming tail is cleared when the view brings the terminal
    /// entry (assistant, failed, cancelled, or interrupted) for the
    /// active turn, and activity lines that have not become durable
    /// survive (so a mid-turn refresh never blinks live activity away).
    pub fn set_view(&mut self, view: ChatView) {
        let terminal_landed = self.active_turn.as_deref().is_some_and(|active| {
            view.entries.iter().any(|entry| {
                entry
                    .turn
                    .as_ref()
                    .is_some_and(|turn| turn.as_str() == active)
                    && matches!(
                        entry.kind,
                        ChatEntryKind::Assistant
                            | ChatEntryKind::Failed
                            | ChatEntryKind::Cancelled
                            | ChatEntryKind::Interrupted
                    )
            })
        });
        if terminal_landed {
            self.stream_tail.clear();
            self.active_turn = None;
            self.activity.clear();
        } else {
            let durable_texts: Vec<String> = view
                .entries
                .iter()
                .map(|entry| entry.text.clone())
                .collect();
            self.activity = reconcile_ephemeral(&self.activity, &durable_texts);
        }
        self.view = view;
    }

    /// Replace the work snapshot. A snapshot carrying a work detail or
    /// artifact viewer opens the corresponding overlay; a snapshot with
    /// neither closes a detail/artifact overlay (mirroring the work
    /// interface's snapshot rules).
    pub fn set_work(&mut self, work: WorkSnapshot) {
        if let Some(viewer) = &work.viewer {
            self.overlay = Some(ChatOverlay::Artifact(viewer.title.clone()));
        } else if let Some(detail) = &work.detail {
            self.overlay = Some(ChatOverlay::Detail(detail.entry.id.clone()));
        } else if matches!(
            self.overlay,
            Some(ChatOverlay::Detail(_)) | Some(ChatOverlay::Artifact(_))
        ) {
            self.overlay = None;
        }
        self.work = work;
    }

    /// Set the busy flag: while busy, submission is explicitly disabled
    /// (a refused submit sets a status and never drops the draft). Clearing
    /// busy also clears the cancelling flag.
    pub fn set_busy(&mut self, busy: bool) {
        self.busy = busy;
        if !busy {
            self.cancelling = false;
        }
    }

    /// Set the status line (the driver reports request outcomes here).
    pub fn set_status(&mut self, status: impl Into<String>) {
        self.status = status.into();
    }

    /// Absorb one ephemeral conversation event. Assistant deltas append
    /// to the bounded streaming tail (cleared by [`ChatUi::set_view`]
    /// when the durable terminal entry lands — `TurnFinished` itself
    /// changes nothing, the view refresh that follows reconciles);
    /// activity and proposal outcomes append to the bounded activity
    /// lines.
    pub fn push_event(&mut self, event: &ConversationEvent) {
        match event {
            ConversationEvent::TurnStarted { turn, .. } => {
                self.active_turn = Some(turn.as_str().to_string());
                self.stream_tail.clear();
            }
            ConversationEvent::AssistantDelta { text } => self.append_stream(text),
            ConversationEvent::ProposalDecided {
                index,
                accepted,
                detail,
            } => {
                let verdict = if *accepted { "accepted" } else { "rejected" };
                self.push_activity(format!("proposal {}: {verdict}: {detail}", index + 1));
            }
            ConversationEvent::Activity { text } => self.push_activity(text.clone()),
            ConversationEvent::TurnFinished { .. } => {}
        }
    }

    /// Handle one decoded key. Paste payloads are inserted directly and
    /// Escape closes the open overlay (or is a no-op); everything else
    /// passes through the keymap under the current focus mode.
    pub fn handle(&mut self, key: ChatKey) -> ChatUiOutcome {
        match key {
            ChatKey::Paste(text) => {
                if !self.editor.insert_paste(&text) {
                    self.status = format!("{DRAFT_LIMIT_STATUS} (paste truncated)");
                }
                ChatUiOutcome::Continue
            }
            ChatKey::Esc => {
                if self.overlay.is_some() {
                    self.overlay = None;
                }
                ChatUiOutcome::Continue
            }
            key => {
                let mode = self.keymap_mode();
                self.apply(self.keymap.action(key, mode))
            }
        }
    }

    /// Handle a submitted text directly (the prompt-form CLI path and
    /// tests). Parses slash commands exactly like an editor submission;
    /// a non-slash text is requested as [`ChatUiRequest::SubmitPrompt`],
    /// remembered in local history, and — if it is the editor's current
    /// draft — the draft is cleared. While busy the submission is
    /// refused with a status and nothing is dropped or cleared.
    pub fn handle_submit(&mut self, text: String) -> ChatUiOutcome {
        if text.trim().is_empty() {
            return ChatUiOutcome::Continue;
        }
        if self.busy {
            self.status = WAIT_STATUS.to_string();
            return ChatUiOutcome::Continue;
        }
        if let Some(command) = text.split_whitespace().next()
            && command.starts_with('/')
        {
            let argument = text.split_whitespace().nth(1);
            return match (command, argument) {
                ("/status", _) => {
                    self.overlay = Some(ChatOverlay::Status);
                    ChatUiOutcome::Request(ChatUiRequest::SlashStatus)
                }
                ("/work", _) => {
                    self.overlay = Some(ChatOverlay::Work);
                    ChatUiOutcome::Request(ChatUiRequest::SlashWork)
                }
                ("/decisions", _) => {
                    self.overlay = Some(ChatOverlay::Decisions);
                    ChatUiOutcome::Request(ChatUiRequest::SlashDecisions)
                }
                ("/inspect", Some(id)) => {
                    ChatUiOutcome::Request(ChatUiRequest::SlashInspect { id: id.to_string() })
                }
                ("/inspect", None) => {
                    self.status = "usage: /inspect <work-id> (see /work)".to_string();
                    ChatUiOutcome::Continue
                }
                ("/help", _) => {
                    self.overlay = Some(ChatOverlay::Help);
                    ChatUiOutcome::Continue
                }
                ("/quit", _) => ChatUiOutcome::Request(ChatUiRequest::Quit),
                _ => {
                    self.status = format!(
                        "unknown command {command} — commands: /status /work /decisions /inspect <id> /help /quit"
                    );
                    ChatUiOutcome::Continue
                }
            };
        }
        self.editor.push_history(&text);
        if self.editor.text() == text {
            self.editor.clear();
        }
        ChatUiOutcome::Request(ChatUiRequest::SubmitPrompt { text })
    }

    /// The current editor draft (for draft-preservation checks).
    pub fn editor_text(&self) -> String {
        self.editor.text()
    }

    /// The transcript scroll offset in lines from the bottom (0 is pinned
    /// to the newest line; for scroll-preservation checks).
    pub fn transcript_offset(&self) -> usize {
        self.scroll
    }

    /// The open overlay, if any.
    pub fn overlay(&self) -> Option<ChatOverlay> {
        self.overlay.clone()
    }

    // -- pub(crate) surface for the screens (not part of the driver
    //    contract) --------------------------------------------------

    /// The durable chat view.
    pub(crate) fn view(&self) -> &ChatView {
        &self.view
    }

    /// The work snapshot.
    pub(crate) fn work(&self) -> &WorkSnapshot {
        &self.work
    }

    /// The workspace path shown in the header.
    pub(crate) fn workspace_path(&self) -> &str {
        &self.workspace_path
    }

    /// The editor.
    pub(crate) fn editor(&self) -> &Editor {
        &self.editor
    }

    /// Whether a turn is running.
    pub(crate) fn busy(&self) -> bool {
        self.busy
    }

    /// Whether cancellation of the running turn was requested.
    pub(crate) fn cancelling(&self) -> bool {
        self.cancelling
    }

    /// The status line.
    pub(crate) fn status(&self) -> &str {
        &self.status
    }

    /// The header state word: cancelling, working, awaiting you (open
    /// decisions), interrupted (unrecovered turns), or ready. Textual,
    /// never color-only.
    pub(crate) fn state_label(&self) -> &'static str {
        if self.busy && self.cancelling {
            "cancelling"
        } else if self.busy {
            "working"
        } else if !self.view.open_decisions.is_empty() {
            "awaiting you"
        } else if self.view.interrupted_turns > 0 {
            "interrupted"
        } else {
            "ready"
        }
    }

    /// Render the transcript content at `width`: durable entries, then
    /// the streaming tail, then (when `with_activity`) the ephemeral
    /// activity lines. Empty content renders the invite placeholder.
    pub(crate) fn transcript_lines(&self, width: usize, with_activity: bool) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();
        let push_block = |block: Vec<String>, lines: &mut Vec<String>| {
            if block.is_empty() {
                return;
            }
            if !lines.is_empty() && lines.last().is_some_and(|last| !last.is_empty()) {
                lines.push(String::new());
            }
            lines.extend(block);
        };
        for entry in &self.view.entries {
            let block = match entry.kind {
                ChatEntryKind::User => user_block(&entry.text, width),
                ChatEntryKind::Assistant => assistant_block(&entry.text, width),
                ChatEntryKind::Activity => activity_block(&entry.text, width),
                ChatEntryKind::Decision => marker_line(DECISION_MARKER, &entry.text, width),
                ChatEntryKind::Failed => marker_line(FAILED_MARKER, &entry.text, width),
                ChatEntryKind::Cancelled => marker_line(CANCELLED_MARKER, &entry.text, width),
                ChatEntryKind::Interrupted => marker_line(INTERRUPTED_MARKER, &entry.text, width),
            };
            push_block(block, &mut lines);
        }
        if !self.stream_tail.is_empty() {
            push_block(assistant_block(&self.stream_tail, width), &mut lines);
        }
        if with_activity {
            for line in &self.activity {
                push_block(activity_block(line, width), &mut lines);
            }
        }
        if lines.is_empty() {
            lines.push(INVITE_LINE.to_string());
        }
        lines
    }

    // -- internals ----------------------------------------------------

    /// The keymap focus mode: overlay focus when an overlay is open;
    /// otherwise editor focus with the shell-classic history condition.
    fn keymap_mode(&self) -> KeymapMode {
        if self.overlay.is_some() {
            KeymapMode::Overlay
        } else {
            KeymapMode::Editor {
                single_line_with_history: self.editor.is_single_line() && self.editor.has_history(),
            }
        }
    }

    /// Apply one keymap action.
    fn apply(&mut self, action: ChatAction) -> ChatUiOutcome {
        match action {
            ChatAction::TypeChar(c) => {
                if c == '?' && self.overlay.is_none() && self.editor.is_empty() {
                    self.overlay = Some(ChatOverlay::Help);
                } else if !self.editor.insert_char(c) {
                    self.status = DRAFT_LIMIT_STATUS.to_string();
                }
                ChatUiOutcome::Continue
            }
            ChatAction::Newline => {
                if !self.editor.insert_newline() {
                    self.status = DRAFT_LIMIT_STATUS.to_string();
                }
                ChatUiOutcome::Continue
            }
            ChatAction::Submit => {
                if self.busy {
                    self.status = WAIT_STATUS.to_string();
                    return ChatUiOutcome::Continue;
                }
                let text = self.editor.text();
                self.handle_submit(text)
            }
            ChatAction::Backspace => {
                self.editor.backspace();
                ChatUiOutcome::Continue
            }
            ChatAction::Delete => {
                self.editor.delete();
                ChatUiOutcome::Continue
            }
            ChatAction::CursorLeft => {
                self.editor.cursor_left();
                ChatUiOutcome::Continue
            }
            ChatAction::CursorRight => {
                self.editor.cursor_right();
                ChatUiOutcome::Continue
            }
            ChatAction::CursorUp => {
                self.editor.cursor_up();
                ChatUiOutcome::Continue
            }
            ChatAction::CursorDown => {
                self.editor.cursor_down();
                ChatUiOutcome::Continue
            }
            ChatAction::LineHome => {
                self.editor.line_home();
                ChatUiOutcome::Continue
            }
            ChatAction::LineEnd => {
                self.editor.line_end();
                ChatUiOutcome::Continue
            }
            ChatAction::HistoryPrev => {
                self.editor.history_prev();
                ChatUiOutcome::Continue
            }
            ChatAction::HistoryNext => {
                self.editor.history_next();
                ChatUiOutcome::Continue
            }
            ChatAction::ScrollUp => {
                self.scroll = (self.scroll + SCROLL_STEP_LINES).min(SCROLL_MAX);
                ChatUiOutcome::Continue
            }
            ChatAction::ScrollDown => {
                self.scroll = self.scroll.saturating_sub(SCROLL_STEP_LINES);
                ChatUiOutcome::Continue
            }
            ChatAction::ScrollTop => {
                self.scroll = SCROLL_MAX;
                ChatUiOutcome::Continue
            }
            ChatAction::ScrollBottom => {
                self.scroll = 0;
                ChatUiOutcome::Continue
            }
            ChatAction::CancelOrExit => {
                if self.busy {
                    if self.cancelling {
                        // A second Ctrl-C force-exits.
                        ChatUiOutcome::Quit
                    } else {
                        self.cancelling = true;
                        self.status =
                            "cancelling the running turn… (Ctrl-C again to force exit)".to_string();
                        ChatUiOutcome::Request(ChatUiRequest::CancelTurn)
                    }
                } else {
                    ChatUiOutcome::Quit
                }
            }
            ChatAction::ToggleHelp => {
                self.overlay = if self.overlay == Some(ChatOverlay::Help) {
                    None
                } else {
                    Some(ChatOverlay::Help)
                };
                ChatUiOutcome::Continue
            }
            ChatAction::NextOverlay => {
                self.cycle_overlay(1);
                ChatUiOutcome::Continue
            }
            ChatAction::PrevOverlay => {
                self.cycle_overlay(-1);
                ChatUiOutcome::Continue
            }
            ChatAction::Other => ChatUiOutcome::Continue,
        }
    }

    /// Cycle among the static overlays (help, status, work, decisions);
    /// detail and artifact overlays cycle from the start of the ring.
    fn cycle_overlay(&mut self, step: i32) {
        const ORDER: [ChatOverlay; 4] = [
            ChatOverlay::Help,
            ChatOverlay::Status,
            ChatOverlay::Work,
            ChatOverlay::Decisions,
        ];
        let next = match ORDER
            .iter()
            .position(|overlay| self.overlay.as_ref() == Some(overlay))
        {
            Some(index) => (index as i32 + step).rem_euclid(ORDER.len() as i32) as usize,
            None => {
                if step > 0 {
                    0
                } else {
                    ORDER.len() - 1
                }
            }
        };
        self.overlay = Some(ORDER[next].clone());
    }

    /// Append to the streaming tail, enforcing the char and line caps
    /// (newest kept).
    fn append_stream(&mut self, text: &str) {
        self.stream_tail.push_str(text);
        let lines: Vec<&str> = self.stream_tail.split('\n').collect();
        if lines.len() > STREAM_TAIL_MAX_LINES {
            let kept = lines[lines.len() - STREAM_TAIL_MAX_LINES..].join("\n");
            self.stream_tail = kept;
        }
        let chars: Vec<char> = self.stream_tail.chars().collect();
        if chars.len() > STREAM_TAIL_MAX_CHARS {
            let kept: String = chars[chars.len() - STREAM_TAIL_MAX_CHARS..]
                .iter()
                .collect();
            self.stream_tail = kept;
        }
    }

    /// Append one activity line, enforcing the cap (oldest dropped).
    fn push_activity(&mut self, line: String) {
        if line.trim().is_empty() {
            return;
        }
        self.activity.push(line);
        if self.activity.len() > ACTIVITY_MAX_LINES {
            let overflow = self.activity.len() - ACTIVITY_MAX_LINES;
            self.activity.drain(0..overflow);
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn empty_editor_and_history_mode_flag_follow_editor_state() {
        let mut ui = ChatUi::new(empty_view(), empty_snapshot(), "/w".to_string());
        assert!(matches!(
            ui.keymap_mode(),
            KeymapMode::Editor {
                single_line_with_history: false
            }
        ));
        ui.handle_submit("first".to_string());
        assert!(matches!(
            ui.keymap_mode(),
            KeymapMode::Editor {
                single_line_with_history: true
            }
        ));
        // a multiline draft moves the arrows back to the cursor
        ui.handle(ChatKey::Ctrl('j'));
        assert!(matches!(
            ui.keymap_mode(),
            KeymapMode::Editor {
                single_line_with_history: false
            }
        ));
        ui.overlay = Some(ChatOverlay::Help);
        assert_eq!(ui.keymap_mode(), KeymapMode::Overlay);
    }

    #[test]
    fn stream_tail_caps_chars_and_lines() {
        let mut ui = ChatUi::new(empty_view(), empty_snapshot(), "/w".to_string());
        ui.push_event(&ConversationEvent::TurnStarted {
            conversation: "cnv_a".parse().unwrap(),
            turn: "turn_1".parse().unwrap(),
            user_text: "go".to_string(),
        });
        for _ in 0..(STREAM_TAIL_MAX_LINES + 10) {
            ui.push_event(&ConversationEvent::AssistantDelta {
                text: "line\n".to_string(),
            });
        }
        assert!(ui.stream_tail.lines().count() <= STREAM_TAIL_MAX_LINES);
        let huge = "x".repeat(STREAM_TAIL_MAX_CHARS + 1);
        ui.push_event(&ConversationEvent::AssistantDelta { text: huge });
        assert!(ui.stream_tail.chars().count() <= STREAM_TAIL_MAX_CHARS);
    }

    #[test]
    fn activity_caps_at_the_limit() {
        let mut ui = ChatUi::new(empty_view(), empty_snapshot(), "/w".to_string());
        for i in 0..(ACTIVITY_MAX_LINES + 25) {
            ui.push_event(&ConversationEvent::Activity {
                text: format!("activity {i}"),
            });
        }
        assert_eq!(ui.activity.len(), ACTIVITY_MAX_LINES);
        assert_eq!(ui.activity.first().map(String::as_str), Some("activity 25"));
    }

    #[test]
    fn proposal_decisions_become_activity_lines() {
        let mut ui = ChatUi::new(empty_view(), empty_snapshot(), "/w".to_string());
        ui.push_event(&ConversationEvent::ProposalDecided {
            index: 1,
            accepted: false,
            detail: "needs a goal first".to_string(),
        });
        assert_eq!(
            ui.activity,
            vec!["proposal 2: rejected: needs a goal first".to_string()]
        );
    }

    // -- helpers ------------------------------------------------------

    fn empty_view() -> ChatView {
        ChatView {
            conversation: None,
            entries: Vec::new(),
            open_decisions: Vec::new(),
            interrupted_turns: 0,
            goal: None,
        }
    }

    fn empty_snapshot() -> WorkSnapshot {
        use relay_app::views::{
            DecisionListView, NextActionView, RecommendationView, StatusCountsView, SummaryView,
            WorkListView,
        };
        WorkSnapshot::new(
            SummaryView {
                project_id: Some("prj_test".to_string()),
                goal: None,
                revision: 1,
                completion: "in-progress".to_string(),
                open_required: Vec::new(),
                open_decisions: Vec::new(),
                counts: StatusCountsView {
                    ready: 0,
                    blocked: 0,
                    awaiting_human: 0,
                    active_attempts: 0,
                    invalidated: 0,
                    completed: 0,
                    canceled: 0,
                    superseded: 0,
                },
                phases: Vec::new(),
                recommendation: RecommendationView {
                    autonomy: "default".to_string(),
                    parallel: Vec::new(),
                    serialized: Vec::new(),
                    deferred: Vec::new(),
                    rationale: "nothing to run".to_string(),
                    next: NextActionView {
                        kind: "none".to_string(),
                        target: None,
                        detail: "declare a goal or ask a question".to_string(),
                    },
                },
            },
            WorkListView {
                works: Vec::new(),
                revision: 1,
            },
            WorkListView {
                works: Vec::new(),
                revision: 1,
            },
            DecisionListView {
                decisions: Vec::new(),
                revision: 1,
            },
        )
    }
}
