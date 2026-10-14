//! The chat keymap: decoded keys to typed interface actions.
//!
//! Backend-neutral — the driver decodes its event stream into
//! [`ChatKey`] and asks [`ChatKeymap`] for the action; the chat state
//! machine applies actions to the editor, transcript scroll, and
//! overlays. Two keys never pass through the table: [`ChatKey::Paste`]
//! (multi-char text is inserted directly so it cannot map onto
//! single-char actions — the table reports [`ChatAction::Other`]) and
//! [`ChatKey::Esc`] (closes the open overlay or is a no-op — also
//! `Other`; the state machine decides, because "closes the overlay"
//! depends on whether one is open).
//!
//! The pinned bindings (pinned by the tests below and by the chat UI
//! tests): `Enter` submits; `Ctrl-J` inserts a newline (the one
//! unambiguous multiline binding); `Ctrl-C` cancels the running turn or
//! exits when idle; `PageUp`/`PageDown` scroll the transcript; `↑`/`↓`
//! walk history in a single-line editor with history and move the cursor
//! otherwise; `Esc` closes an overlay; `?` toggles help when the editor
//! is empty (a state-dependent refinement the state machine applies to
//! `TypeChar('?')`).

/// A decoded key.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ChatKey {
    /// A character key.
    Char(char),
    /// A control-key combination (the modifier letter, e.g. `'j'` for
    /// Ctrl-J).
    Ctrl(char),
    /// Enter / return.
    Enter,
    /// The up arrow.
    Up,
    /// The down arrow.
    Down,
    /// The left arrow.
    Left,
    /// The right arrow.
    Right,
    /// Home.
    Home,
    /// End.
    End,
    /// Backspace.
    Backspace,
    /// Delete.
    Delete,
    /// Escape.
    Esc,
    /// Page up.
    PageUp,
    /// Page down.
    PageDown,
    /// A bracketed paste payload (plain text, possibly multiline).
    Paste(String),
    /// Anything else.
    Other,
}

/// What a key asks the chat interface to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatAction {
    /// Type one char into the editor.
    TypeChar(char),
    /// Insert a line break in the editor (Ctrl-J).
    Newline,
    /// Submit the editor text as a prompt.
    Submit,
    /// Delete the char before the cursor.
    Backspace,
    /// Delete the char at the cursor.
    Delete,
    /// Move the cursor one char left.
    CursorLeft,
    /// Move the cursor one char right.
    CursorRight,
    /// Move the cursor one line up.
    CursorUp,
    /// Move the cursor one line down.
    CursorDown,
    /// Move the cursor to the line start.
    LineHome,
    /// Move the cursor to the line end.
    LineEnd,
    /// Scroll the transcript up.
    ScrollUp,
    /// Scroll the transcript down.
    ScrollDown,
    /// Scroll the transcript to the top.
    ScrollTop,
    /// Scroll the transcript to the bottom.
    ScrollBottom,
    /// Walk to the previous (older) local history entry.
    HistoryPrev,
    /// Walk to the next (newer) local history entry.
    HistoryNext,
    /// Cancel the running turn, or exit when idle (Ctrl-C).
    CancelOrExit,
    /// Toggle the help overlay.
    ToggleHelp,
    /// Cycle to the next overlay.
    NextOverlay,
    /// Cycle to the previous overlay.
    PrevOverlay,
    /// Nothing to do.
    Other,
}

/// Where keypresses go: the editor or an open overlay. Under
/// [`KeymapMode::Overlay`] the arrows scroll the transcript and cycle
/// overlays instead of moving the editor cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeymapMode {
    /// The editor has focus. `single_line_with_history` is the classic
    /// shell condition under which `↑`/`↓` walk local history instead of
    /// moving the cursor: the buffer is a single line and history is
    /// non-empty. (The state machine computes the flag from editor state
    /// on every keypress.)
    Editor {
        /// Whether Up/Down should walk history rather than move the
        /// cursor.
        single_line_with_history: bool,
    },
    /// An overlay has focus.
    Overlay,
}

/// The chat keymap: a pure table, no state.
#[derive(Debug, Clone, Copy, Default)]
pub struct ChatKeymap;

impl ChatKeymap {
    /// Map a key to its action under `mode`.
    pub fn action(&self, key: ChatKey, mode: KeymapMode) -> ChatAction {
        // Mode-independent bindings: editing keys always reach the editor
        // (type-ahead while an overlay is open is preserved, never
        // dropped), and the cancel binding always means cancel-or-exit.
        match key {
            ChatKey::Ctrl('c') | ChatKey::Ctrl('C') => return ChatAction::CancelOrExit,
            ChatKey::Ctrl('j') | ChatKey::Ctrl('J') => return ChatAction::Newline,
            ChatKey::Char(c) => return ChatAction::TypeChar(c),
            ChatKey::Backspace => return ChatAction::Backspace,
            ChatKey::Delete => return ChatAction::Delete,
            ChatKey::PageUp => return ChatAction::ScrollUp,
            ChatKey::PageDown => return ChatAction::ScrollDown,
            ChatKey::Paste(_) => return ChatAction::Other,
            ChatKey::Esc | ChatKey::Other => return ChatAction::Other,
            _ => {}
        }
        match mode {
            KeymapMode::Editor {
                single_line_with_history,
            } => match key {
                ChatKey::Enter => ChatAction::Submit,
                ChatKey::Up => {
                    if single_line_with_history {
                        ChatAction::HistoryPrev
                    } else {
                        ChatAction::CursorUp
                    }
                }
                ChatKey::Down => {
                    if single_line_with_history {
                        ChatAction::HistoryNext
                    } else {
                        ChatAction::CursorDown
                    }
                }
                ChatKey::Left => ChatAction::CursorLeft,
                ChatKey::Right => ChatAction::CursorRight,
                ChatKey::Home | ChatKey::Ctrl('a') | ChatKey::Ctrl('A') => ChatAction::LineHome,
                ChatKey::End | ChatKey::Ctrl('e') | ChatKey::Ctrl('E') => ChatAction::LineEnd,
                _ => ChatAction::Other,
            },
            KeymapMode::Overlay => match key {
                ChatKey::Enter => ChatAction::Other,
                ChatKey::Up => ChatAction::ScrollUp,
                ChatKey::Down => ChatAction::ScrollDown,
                ChatKey::Left => ChatAction::PrevOverlay,
                ChatKey::Right => ChatAction::NextOverlay,
                ChatKey::Home => ChatAction::ScrollTop,
                ChatKey::End => ChatAction::ScrollBottom,
                _ => ChatAction::Other,
            },
        }
    }
}

/// The footer hint rendered under the chat screen.
pub const FOOTER: &str = "/help · Enter send · Ctrl-J newline · Ctrl-C cancel/exit";

/// The help overlay text.
pub const HELP_TEXT: &str = "\
relay chat

  enter        send the prompt
  ctrl-j       newline (for multiline prompts)
  ctrl-c       cancel the running turn; exit when idle
  ← / →        move the cursor
  ↑ / ↓        previous/next prompt (single line); move the cursor (multiline)
  home / end   line start / line end · ctrl-a / ctrl-e
  pgup / pgdn  scroll the transcript
  esc          close the overlay
  with an overlay open: ↑ / ↓ scroll, ← / → cycle overlays, home / end top / bottom

slash commands
  /status        workspace and conversation status
  /work          ready and blocked work
  /decisions     open decisions
  /inspect <id>  inspect a work item
  /help          this help
  /quit          leave relay
";

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    const EDITOR_PLAIN: KeymapMode = KeymapMode::Editor {
        single_line_with_history: false,
    };
    const EDITOR_HISTORY: KeymapMode = KeymapMode::Editor {
        single_line_with_history: true,
    };

    #[test]
    fn enter_submits_in_the_editor() {
        let keymap = ChatKeymap;
        assert_eq!(
            keymap.action(ChatKey::Enter, EDITOR_PLAIN),
            ChatAction::Submit
        );
        assert_eq!(
            keymap.action(ChatKey::Enter, EDITOR_HISTORY),
            ChatAction::Submit
        );
    }

    #[test]
    fn ctrl_j_is_the_newline_binding() {
        let keymap = ChatKeymap;
        assert_eq!(
            keymap.action(ChatKey::Ctrl('j'), EDITOR_PLAIN),
            ChatAction::Newline
        );
        assert_eq!(
            keymap.action(ChatKey::Ctrl('J'), EDITOR_PLAIN),
            ChatAction::Newline
        );
        assert_eq!(
            keymap.action(ChatKey::Ctrl('j'), KeymapMode::Overlay),
            ChatAction::Newline
        );
    }

    #[test]
    fn ctrl_c_cancels_or_exits_in_both_modes() {
        let keymap = ChatKeymap;
        for mode in [EDITOR_PLAIN, EDITOR_HISTORY, KeymapMode::Overlay] {
            assert_eq!(
                keymap.action(ChatKey::Ctrl('c'), mode),
                ChatAction::CancelOrExit
            );
            assert_eq!(
                keymap.action(ChatKey::Ctrl('C'), mode),
                ChatAction::CancelOrExit
            );
        }
    }

    #[test]
    fn page_up_and_down_scroll_in_both_modes() {
        let keymap = ChatKeymap;
        for mode in [EDITOR_PLAIN, KeymapMode::Overlay] {
            assert_eq!(keymap.action(ChatKey::PageUp, mode), ChatAction::ScrollUp);
            assert_eq!(
                keymap.action(ChatKey::PageDown, mode),
                ChatAction::ScrollDown
            );
        }
    }

    #[test]
    fn up_down_walk_history_only_in_a_single_line_editor_with_history() {
        let keymap = ChatKeymap;
        assert_eq!(
            keymap.action(ChatKey::Up, EDITOR_HISTORY),
            ChatAction::HistoryPrev
        );
        assert_eq!(
            keymap.action(ChatKey::Down, EDITOR_HISTORY),
            ChatAction::HistoryNext
        );
        // multiline (or empty history): the arrows move the cursor
        assert_eq!(
            keymap.action(ChatKey::Up, EDITOR_PLAIN),
            ChatAction::CursorUp
        );
        assert_eq!(
            keymap.action(ChatKey::Down, EDITOR_PLAIN),
            ChatAction::CursorDown
        );
    }

    #[test]
    fn arrows_move_the_cursor_in_the_editor() {
        let keymap = ChatKeymap;
        assert_eq!(
            keymap.action(ChatKey::Left, EDITOR_PLAIN),
            ChatAction::CursorLeft
        );
        assert_eq!(
            keymap.action(ChatKey::Right, EDITOR_PLAIN),
            ChatAction::CursorRight
        );
        assert_eq!(
            keymap.action(ChatKey::Up, EDITOR_PLAIN),
            ChatAction::CursorUp
        );
        assert_eq!(
            keymap.action(ChatKey::Down, EDITOR_PLAIN),
            ChatAction::CursorDown
        );
    }

    #[test]
    fn home_end_bind_to_the_line_in_the_editor() {
        let keymap = ChatKeymap;
        for mode in [EDITOR_PLAIN, EDITOR_HISTORY] {
            assert_eq!(keymap.action(ChatKey::Home, mode), ChatAction::LineHome);
            assert_eq!(keymap.action(ChatKey::End, mode), ChatAction::LineEnd);
            assert_eq!(
                keymap.action(ChatKey::Ctrl('a'), mode),
                ChatAction::LineHome
            );
            assert_eq!(keymap.action(ChatKey::Ctrl('e'), mode), ChatAction::LineEnd);
        }
    }

    #[test]
    fn overlay_arrows_scroll_and_cycle_overlays() {
        let keymap = ChatKeymap;
        assert_eq!(
            keymap.action(ChatKey::Up, KeymapMode::Overlay),
            ChatAction::ScrollUp
        );
        assert_eq!(
            keymap.action(ChatKey::Down, KeymapMode::Overlay),
            ChatAction::ScrollDown
        );
        assert_eq!(
            keymap.action(ChatKey::Left, KeymapMode::Overlay),
            ChatAction::PrevOverlay
        );
        assert_eq!(
            keymap.action(ChatKey::Right, KeymapMode::Overlay),
            ChatAction::NextOverlay
        );
        assert_eq!(
            keymap.action(ChatKey::Home, KeymapMode::Overlay),
            ChatAction::ScrollTop
        );
        assert_eq!(
            keymap.action(ChatKey::End, KeymapMode::Overlay),
            ChatAction::ScrollBottom
        );
    }

    #[test]
    fn editing_keys_reach_the_editor_under_an_open_overlay() {
        let keymap = ChatKeymap;
        assert_eq!(
            keymap.action(ChatKey::Char('x'), KeymapMode::Overlay),
            ChatAction::TypeChar('x')
        );
        assert_eq!(
            keymap.action(ChatKey::Backspace, KeymapMode::Overlay),
            ChatAction::Backspace
        );
        assert_eq!(
            keymap.action(ChatKey::Delete, KeymapMode::Overlay),
            ChatAction::Delete
        );
    }

    #[test]
    fn chars_type_and_enter_does_not_submit_under_an_overlay() {
        let keymap = ChatKeymap;
        assert_eq!(
            keymap.action(ChatKey::Char('?'), EDITOR_PLAIN),
            ChatAction::TypeChar('?')
        );
        assert_eq!(
            keymap.action(ChatKey::Char('/'), KeymapMode::Overlay),
            ChatAction::TypeChar('/')
        );
        assert_eq!(
            keymap.action(ChatKey::Enter, KeymapMode::Overlay),
            ChatAction::Other
        );
    }

    #[test]
    fn esc_paste_and_unknown_keys_are_other() {
        let keymap = ChatKeymap;
        for mode in [EDITOR_PLAIN, KeymapMode::Overlay] {
            assert_eq!(keymap.action(ChatKey::Esc, mode), ChatAction::Other);
            assert_eq!(
                keymap.action(ChatKey::Paste("text".to_string()), mode),
                ChatAction::Other
            );
            assert_eq!(keymap.action(ChatKey::Other, mode), ChatAction::Other);
        }
    }

    #[test]
    fn unknown_control_keys_map_to_other() {
        let keymap = ChatKeymap;
        assert_eq!(
            keymap.action(ChatKey::Ctrl('q'), EDITOR_PLAIN),
            ChatAction::Other
        );
        assert_eq!(
            keymap.action(ChatKey::Ctrl('x'), KeymapMode::Overlay),
            ChatAction::Other
        );
    }
}
