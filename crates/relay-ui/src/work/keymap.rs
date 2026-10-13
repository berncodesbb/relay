//! The typed keymap: input keys to interface actions.
//!
//! Backend-neutral — the driver decodes its event stream into [`Key`] and
//! asks the [`Keymap`] for the action. `q` always quits; `?` toggles help;
//! `tab`/`←`/`→` cycle screens; `j`/`k`/arrows move the cursor; `enter`
//! activates; `v` opens a view; `r` refreshes.

/// A decoded key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    /// A character key.
    Char(char),
    /// The up arrow.
    Up,
    /// The down arrow.
    Down,
    /// The left arrow.
    Left,
    /// The right arrow.
    Right,
    /// Enter / return.
    Enter,
    /// Tab.
    Tab,
    /// Shift-tab.
    BackTab,
    /// Escape.
    Esc,
    /// Anything else.
    Other,
}

/// What a key asks the interface to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TuiAction {
    /// Quit (or close help / the viewer).
    Quit,
    /// Cycle to the next screen.
    NextScreen,
    /// Cycle to the previous screen.
    PrevScreen,
    /// Move the cursor down.
    SelectNext,
    /// Move the cursor up.
    SelectPrev,
    /// Activate the selected row.
    Activate,
    /// Open the viewer for the detail's first input.
    View,
    /// Re-read the project.
    Refresh,
    /// Toggle help.
    Help,
}

/// The keymap.
#[derive(Debug, Clone, Copy, Default)]
pub struct Keymap;

impl Keymap {
    /// Map a key to its action, if it has one.
    pub fn action(&self, key: Key) -> Option<TuiAction> {
        match key {
            Key::Char('q') | Key::Char('Q') | Key::Esc => Some(TuiAction::Quit),
            Key::Char('j') | Key::Down => Some(TuiAction::SelectNext),
            Key::Char('k') | Key::Up => Some(TuiAction::SelectPrev),
            Key::Tab | Key::Right => Some(TuiAction::NextScreen),
            Key::BackTab | Key::Left => Some(TuiAction::PrevScreen),
            Key::Enter => Some(TuiAction::Activate),
            Key::Char('v') | Key::Char('V') => Some(TuiAction::View),
            Key::Char('r') | Key::Char('R') => Some(TuiAction::Refresh),
            Key::Char('?') => Some(TuiAction::Help),
            Key::Char(_) | Key::Other => None,
        }
    }
}

/// The footer hint rendered under every screen.
pub const FOOTER: &str =
    "tab screens · j/k move · enter select · v view · r refresh · ? help · q quit";

/// The help text.
pub const HELP_TEXT: &str = "\
relay work-graph interface

  tab / →      next screen (summary, ready, blocked, decisions)
  ⇧tab / ←     previous screen
  j / ↓        next row
  k / ↑        previous row
  enter        activate: inspect work, or resolve the selected decision option
  v            view the detail screen's first input as rendered Markdown
  r            refresh from the project graph
  ?            toggle this help
  q / esc      close the viewer, then quit

Decisions are resolved only by your enter key; the interface never answers
for you, and free-form decisions (no options) are resolved through
`relay decisions resolve <id> --choice <text>`.
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn q_quits_and_arrows_move() {
        let keymap = Keymap;
        assert_eq!(keymap.action(Key::Char('q')), Some(TuiAction::Quit));
        assert_eq!(keymap.action(Key::Esc), Some(TuiAction::Quit));
        assert_eq!(keymap.action(Key::Down), Some(TuiAction::SelectNext));
        assert_eq!(keymap.action(Key::Char('k')), Some(TuiAction::SelectPrev));
        assert_eq!(keymap.action(Key::Enter), Some(TuiAction::Activate));
        assert_eq!(keymap.action(Key::Tab), Some(TuiAction::NextScreen));
        assert_eq!(keymap.action(Key::Char('x')), None);
    }
}
