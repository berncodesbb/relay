//! The chat prompt editor: a Unicode-safe multiline buffer with a cursor,
//! column-intent-preserving movement, local history, and an explicit size
//! bound.
//!
//! The buffer is `Vec<Vec<char>>` — one `Vec<char>` per line — so every
//! operation is char-based and multibyte-safe by construction: no index
//! ever splits a code point, and CJK or emoji content behaves exactly like
//! ASCII content. Display width is deliberately not computed here; the
//! renderer (ratatui) measures glyphs. There are no dependencies on
//! provider, application, or domain types — this is a plain text buffer.
//!
//! History follows the classic shell behavior: pressing
//! [`Editor::history_prev`] for the first time stashes the in-progress
//! draft, walking back loads older entries, and
//! [`Editor::history_next`] past the newest entry restores the stashed
//! draft, so history browsing never loses what was being typed. Any edit
//! made while browsing commits the shown entry as the new draft and leaves
//! history mode (the stash is dropped, as in readline).

/// The maximum draft size in chars, newlines included.
///
/// Mirrors `relay_domain`'s message text bound for conversational messages
/// (`MESSAGE_TEXT_MAX_CHARS`, 16,384 chars per the chat-first workspace
/// plan). The constant is local so this module stays dependency-free; when
/// the domain constant lands, the two must stay in sync — this is a
/// presentation-side pre-bound so an oversized draft is refused before it
/// ever reaches the application service.
pub const MAX_DRAFT_CHARS: usize = 16_384;

/// The maximum number of remembered prompts. Oldest entries are dropped.
pub const HISTORY_MAX_ENTRIES: usize = 500;

/// A rejected control character for direct insertion (paste normalizes
/// `\r\n` first; `\n` becomes a newline, `\t` is allowed).
fn is_rejected_control(c: char) -> bool {
    c == '\r' || (c.is_control() && c != '\t')
}

/// Split `text` into char lines, stopping at `max_chars` total (newlines
/// count). Always returns at least one line.
fn lines_from_text(text: &str, max_chars: usize) -> Vec<Vec<char>> {
    let mut lines: Vec<Vec<char>> = Vec::new();
    let mut current: Vec<char> = Vec::new();
    for (total, c) in text.chars().enumerate() {
        if total >= max_chars {
            break;
        }
        if c == '\n' {
            lines.push(std::mem::take(&mut current));
        } else {
            current.push(c);
        }
    }
    lines.push(current);
    lines
}

/// The multiline prompt editor.
///
/// Invariants (held by every operation): there is always at least one
/// line; the cursor `(row, col)` always satisfies `row < lines.len()` and
/// `col <= lines[row].len()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Editor {
    /// The buffer: one `Vec<char>` per line. Never empty.
    lines: Vec<Vec<char>>,
    /// The cursor row.
    row: usize,
    /// The cursor column (a char index into the row, one past the last
    /// char at line end).
    col: usize,
    /// The remembered column for vertical movement, so a short line does
    /// not collapse the column intent (`Some` only while moving
    /// vertically; any edit or horizontal move clears it).
    preferred_col: Option<usize>,
    /// Submitted prompts, oldest first.
    history: Vec<String>,
    /// The current history position while browsing (`Some(index)` into
    /// `history`); `None` when the buffer is the live draft.
    history_cursor: Option<usize>,
    /// The in-progress draft stashed on the first history move, restored
    /// when browsing runs past the newest entry.
    stashed_draft: Option<String>,
}

impl Editor {
    /// A new empty editor.
    pub fn new() -> Self {
        Editor {
            lines: vec![Vec::new()],
            row: 0,
            col: 0,
            preferred_col: None,
            history: Vec::new(),
            history_cursor: None,
            stashed_draft: None,
        }
    }

    /// The buffer text, lines joined with `\n`.
    pub fn text(&self) -> String {
        self.lines
            .iter()
            .map(|line| line.iter().collect::<String>())
            .collect::<Vec<String>>()
            .join("\n")
    }

    /// Replace the whole buffer (clamped to [`MAX_DRAFT_CHARS`]), placing
    /// the cursor at the end. Exits history mode.
    pub fn set_text(&mut self, text: &str) {
        self.replace_text(text);
        self.exit_history();
    }

    /// The buffer lines, for rendering.
    pub fn lines(&self) -> &[Vec<char>] {
        &self.lines
    }

    /// The cursor as `(row, col)` char indexes.
    pub fn cursor(&self) -> (usize, usize) {
        (self.row, self.col)
    }

    /// The number of lines (always at least one).
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// The total char count, newlines included.
    pub fn char_count(&self) -> usize {
        self.lines.iter().map(|line| line.len()).sum::<usize>() + self.lines.len() - 1
    }

    /// Whether the buffer has no chars (an empty single line).
    pub fn is_empty(&self) -> bool {
        self.lines.len() == 1 && self.lines[0].is_empty()
    }

    /// Whether the buffer is a single line (empty or not) — the condition
    /// under which Up/Down walk history instead of moving the cursor.
    pub fn is_single_line(&self) -> bool {
        self.lines.len() == 1
    }

    /// Insert one char at the cursor. `\n` inserts a line break; other
    /// control characters are refused. Returns `false` (changing nothing)
    /// when the char is refused or the draft is at [`MAX_DRAFT_CHARS`] —
    /// the caller reports it, input is never silently accepted.
    pub fn insert_char(&mut self, c: char) -> bool {
        if c == '\n' {
            return self.insert_newline();
        }
        if is_rejected_control(c) {
            return false;
        }
        if self.char_count() + 1 > MAX_DRAFT_CHARS {
            return false;
        }
        self.exit_history();
        self.preferred_col = None;
        self.lines[self.row].insert(self.col, c);
        self.col += 1;
        true
    }

    /// Split the current line at the cursor. Returns `false` (changing
    /// nothing) when the draft is at [`MAX_DRAFT_CHARS`].
    pub fn insert_newline(&mut self) -> bool {
        if self.char_count() + 1 > MAX_DRAFT_CHARS {
            return false;
        }
        self.exit_history();
        self.preferred_col = None;
        let tail = self.lines[self.row].split_off(self.col);
        self.lines.insert(self.row + 1, tail);
        self.row += 1;
        self.col = 0;
        true
    }

    /// Insert pasted text at the cursor, treated as plain chars: `\r\n`
    /// and lone `\r` are normalized to line breaks, everything else
    /// (including a trailing newline) is inserted literally. Insertion
    /// stops at [`MAX_DRAFT_CHARS`]; returns `false` when the paste was
    /// truncated, `true` when it was applied in full.
    pub fn insert_paste(&mut self, text: &str) -> bool {
        let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
        let mut complete = true;
        for c in normalized.chars() {
            if !self.insert_char(c) {
                complete = false;
                break;
            }
        }
        complete
    }

    /// Delete the char before the cursor, or join to the previous line at
    /// a line start. A no-op at the very start.
    pub fn backspace(&mut self) {
        if self.col > 0 {
            self.exit_history();
            self.preferred_col = None;
            self.lines[self.row].remove(self.col - 1);
            self.col -= 1;
        } else if self.row > 0 {
            self.exit_history();
            self.preferred_col = None;
            let join_col = self.lines[self.row - 1].len();
            let tail = self.lines.remove(self.row);
            self.row -= 1;
            self.lines[self.row].extend(tail);
            self.col = join_col;
        }
    }

    /// Delete the char at the cursor, or join the next line at a line
    /// end. A no-op at the very end.
    pub fn delete(&mut self) {
        if self.col < self.lines[self.row].len() {
            self.exit_history();
            self.preferred_col = None;
            self.lines[self.row].remove(self.col);
        } else if self.row + 1 < self.lines.len() {
            self.exit_history();
            self.preferred_col = None;
            let tail = self.lines.remove(self.row + 1);
            self.lines[self.row].extend(tail);
        }
    }

    /// Move left one char, stepping to the end of the previous line at a
    /// line start. A no-op at the very start.
    pub fn cursor_left(&mut self) {
        self.preferred_col = None;
        if self.col > 0 {
            self.col -= 1;
        } else if self.row > 0 {
            self.row -= 1;
            self.col = self.lines[self.row].len();
        }
    }

    /// Move right one char, stepping to the start of the next line at a
    /// line end. A no-op at the very end.
    pub fn cursor_right(&mut self) {
        self.preferred_col = None;
        if self.col < self.lines[self.row].len() {
            self.col += 1;
        } else if self.row + 1 < self.lines.len() {
            self.row += 1;
            self.col = 0;
        }
    }

    /// Move up one line, preserving the column intent across shorter
    /// lines. Returns whether the cursor moved.
    pub fn cursor_up(&mut self) -> bool {
        if self.row == 0 {
            return false;
        }
        let intent = *self.preferred_col.get_or_insert(self.col);
        self.row -= 1;
        self.col = intent.min(self.lines[self.row].len());
        true
    }

    /// Move down one line, preserving the column intent across shorter
    /// lines. Returns whether the cursor moved.
    pub fn cursor_down(&mut self) -> bool {
        if self.row + 1 >= self.lines.len() {
            return false;
        }
        let intent = *self.preferred_col.get_or_insert(self.col);
        self.row += 1;
        self.col = intent.min(self.lines[self.row].len());
        true
    }

    /// Move to the start of the current line.
    pub fn line_home(&mut self) {
        self.preferred_col = None;
        self.col = 0;
    }

    /// Move to the end of the current line.
    pub fn line_end(&mut self) {
        self.preferred_col = None;
        self.col = self.lines[self.row].len();
    }

    /// Clear the buffer back to a single empty line (history untouched).
    pub fn clear(&mut self) {
        self.lines = vec![Vec::new()];
        self.row = 0;
        self.col = 0;
        self.preferred_col = None;
        self.exit_history();
    }

    /// Remember a submitted prompt. Empty or whitespace-only entries and
    /// immediate repeats of the newest entry are skipped; the oldest
    /// entry is dropped beyond [`HISTORY_MAX_ENTRIES`].
    pub fn push_history(&mut self, entry: &str) {
        if entry.trim().is_empty() {
            return;
        }
        if self.history.last().is_some_and(|last| last == entry) {
            return;
        }
        self.history.push(entry.to_string());
        if self.history.len() > HISTORY_MAX_ENTRIES {
            let overflow = self.history.len() - HISTORY_MAX_ENTRIES;
            self.history.drain(0..overflow);
        }
    }

    /// Walk to the previous (older) prompt. The first move stashes the
    /// in-progress draft; a no-op when there is no history or the oldest
    /// entry is already shown.
    pub fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }
        match self.history_cursor {
            Some(cursor) if cursor > 0 => {
                self.history_cursor = Some(cursor - 1);
                self.load_history_entry(cursor - 1);
            }
            Some(_) => {}
            None => {
                self.stashed_draft = Some(self.text());
                let newest = self.history.len() - 1;
                self.history_cursor = Some(newest);
                self.load_history_entry(newest);
            }
        }
    }

    /// Walk to the next (newer) prompt, restoring the stashed draft when
    /// moving past the newest entry. A no-op when not browsing history.
    pub fn history_next(&mut self) {
        let Some(cursor) = self.history_cursor else {
            return;
        };
        if cursor + 1 < self.history.len() {
            self.history_cursor = Some(cursor + 1);
            self.load_history_entry(cursor + 1);
        } else {
            self.history_cursor = None;
            if let Some(draft) = self.stashed_draft.take() {
                self.replace_text(&draft);
            }
        }
    }

    /// The number of remembered prompts.
    pub fn history_len(&self) -> usize {
        self.history.len()
    }

    /// Whether any prompt is remembered.
    pub fn has_history(&self) -> bool {
        !self.history.is_empty()
    }

    /// Whether the buffer currently shows a history entry (versus the
    /// live draft).
    pub fn is_navigating_history(&self) -> bool {
        self.history_cursor.is_some()
    }

    /// Replace the buffer and place the cursor at the end, without
    /// touching history state.
    fn replace_text(&mut self, text: &str) {
        self.lines = lines_from_text(text, MAX_DRAFT_CHARS);
        self.row = self.lines.len() - 1;
        self.col = self.lines[self.row].len();
        self.preferred_col = None;
    }

    /// Load `history[index]` into the buffer (clamped to the size bound).
    fn load_history_entry(&mut self, index: usize) {
        if let Some(entry) = self.history.get(index) {
            let entry = entry.clone();
            self.replace_text(&entry);
        }
    }

    /// Leave history mode: the buffer becomes the live draft and any
    /// stashed draft is dropped.
    fn exit_history(&mut self) {
        self.history_cursor = None;
        self.stashed_draft = None;
    }
}

impl Default for Editor {
    fn default() -> Self {
        Editor::new()
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    /// Type a string char by char, asserting every insert applies.
    fn type_text(editor: &mut Editor, text: &str) {
        for c in text.chars() {
            assert!(editor.insert_char(c), "insert of {c:?} must apply");
        }
    }

    #[test]
    fn empty_editor_is_a_single_empty_line() {
        let editor = Editor::new();
        assert!(editor.is_empty());
        assert!(editor.is_single_line());
        assert_eq!(editor.text(), "");
        assert_eq!(editor.cursor(), (0, 0));
        assert_eq!(editor.char_count(), 0);
        assert_eq!(editor.line_count(), 1);
    }

    #[test]
    fn empty_editor_operations_are_no_ops() {
        let mut editor = Editor::new();
        editor.backspace();
        editor.delete();
        editor.cursor_left();
        editor.cursor_right();
        assert!(!editor.cursor_up());
        assert!(!editor.cursor_down());
        editor.line_home();
        editor.line_end();
        assert_eq!(editor.text(), "");
        assert_eq!(editor.cursor(), (0, 0));
        editor.history_prev();
        editor.history_next();
        assert_eq!(editor.text(), "");
    }

    #[test]
    fn typing_multibyte_content_is_char_safe() {
        let mut editor = Editor::new();
        type_text(&mut editor, "héllo 世界 👨‍👩‍👧");
        assert_eq!(editor.text(), "héllo 世界 👨‍👩‍👧");
        // cursor at end of the single line
        let (row, col) = editor.cursor();
        assert_eq!((row, col), (0, editor.lines()[0].len()));
        assert!(editor.char_count() > 0);
        // backspace removes the last char (a ZWJ-sequence member), not a byte fragment
        editor.backspace();
        let text = editor.text();
        assert_eq!(text.chars().count(), "héllo 世界 👨‍👩‍👧".chars().count() - 1);
    }

    #[test]
    fn newline_splits_the_line_at_the_cursor() {
        let mut editor = Editor::new();
        type_text(&mut editor, "abcd");
        editor.cursor_left();
        editor.cursor_left();
        assert!(editor.insert_newline());
        assert_eq!(editor.text(), "ab\ncd");
        assert_eq!(
            editor.cursor(),
            (1, 0),
            "cursor at the start of the new line"
        );
        assert_eq!(editor.line_count(), 2);
        assert!(!editor.is_single_line());
    }

    #[test]
    fn backspace_joins_lines_at_a_line_start() {
        let mut editor = Editor::new();
        editor.set_text("ab\ncd");
        editor.cursor_left(); // (1, 1)
        editor.cursor_left(); // (1, 0)
        editor.backspace();
        assert_eq!(editor.text(), "abcd");
        assert_eq!(editor.cursor(), (0, 2));
    }

    #[test]
    fn backspace_deletes_the_char_before_the_cursor() {
        let mut editor = Editor::new();
        editor.set_text("abc");
        editor.backspace();
        assert_eq!(editor.text(), "ab");
        assert_eq!(editor.cursor(), (0, 2));
    }

    #[test]
    fn delete_joins_lines_at_a_line_end() {
        let mut editor = Editor::new();
        editor.set_text("ab\ncd");
        editor.cursor_left(); // (1, 1)
        editor.cursor_left(); // (1, 0)
        editor.cursor_left(); // (0, 2) end of first line
        editor.delete();
        assert_eq!(editor.text(), "abcd");
        assert_eq!(editor.cursor(), (0, 2));
    }

    #[test]
    fn delete_removes_the_char_at_the_cursor() {
        let mut editor = Editor::new();
        editor.set_text("abc"); // cursor (0, 3)
        editor.cursor_left(); // (0, 2): on 'c'
        editor.delete();
        assert_eq!(editor.text(), "ab");
        assert_eq!(
            editor.cursor(),
            (0, 2),
            "the cursor does not move on delete"
        );
    }

    #[test]
    fn cursor_left_and_right_cross_line_ends() {
        let mut editor = Editor::new();
        editor.set_text("ab\ncd");
        editor.line_end(); // (1, 2)
        editor.cursor_left(); // (1, 1)
        editor.cursor_left(); // (1, 0)
        editor.cursor_left(); // (0, 2) — end of previous line
        assert_eq!(editor.cursor(), (0, 2));
        editor.cursor_right(); // (1, 0) — start of next line
        assert_eq!(editor.cursor(), (1, 0));
    }

    #[test]
    fn vertical_movement_preserves_column_intent() {
        let mut editor = Editor::new();
        editor.set_text("abcdef\nxy\nzzzzzzzz");
        // cursor at end: (2, 8)
        editor.cursor_up(); // intent 8 -> (1, min(8,2)=2)
        assert_eq!(editor.cursor(), (1, 2));
        editor.cursor_up(); // intent stays 8 -> (0, min(8,6)=6)
        assert_eq!(editor.cursor(), (0, 6));
        assert!(!editor.cursor_up()); // top row
        assert_eq!(editor.cursor(), (0, 6));
        editor.cursor_down(); // intent still 8 -> (1, 2)
        assert_eq!(editor.cursor(), (1, 2));
        editor.cursor_down(); // (2, 8)
        assert_eq!(editor.cursor(), (2, 8));
        assert!(!editor.cursor_down()); // bottom row
        // horizontal movement clears the intent
        editor.cursor_up(); // (1, 2), intent 8
        editor.cursor_left(); // (1, 1), intent cleared
        editor.cursor_down(); // fresh intent 1 -> (2, 1)
        assert_eq!(editor.cursor(), (2, 1));
    }

    #[test]
    fn home_and_end_move_within_the_line() {
        let mut editor = Editor::new();
        editor.set_text("ab\ncd");
        editor.line_home();
        assert_eq!(editor.cursor(), (1, 0));
        editor.line_end();
        assert_eq!(editor.cursor(), (1, 2));
    }

    #[test]
    fn clear_resets_to_a_single_empty_line() {
        let mut editor = Editor::new();
        editor.set_text("ab\ncd");
        editor.push_history("earlier");
        editor.clear();
        assert!(editor.is_empty());
        assert_eq!(editor.cursor(), (0, 0));
        assert_eq!(editor.history_len(), 1, "clear keeps history");
    }

    #[test]
    fn draft_bound_refuses_inserts_beyond_the_max() {
        let mut editor = Editor::new();
        for _ in 0..MAX_DRAFT_CHARS {
            assert!(editor.insert_char('x'));
        }
        assert_eq!(editor.char_count(), MAX_DRAFT_CHARS);
        assert!(
            !editor.insert_char('x'),
            "insert past the bound must refuse"
        );
        assert!(
            !editor.insert_newline(),
            "newline past the bound must refuse"
        );
        assert_eq!(editor.char_count(), MAX_DRAFT_CHARS);
        // backspace still works and frees room
        editor.backspace();
        assert!(editor.insert_char('y'));
        assert_eq!(editor.char_count(), MAX_DRAFT_CHARS);
    }

    #[test]
    fn draft_bound_counts_newlines() {
        let mut editor = Editor::new();
        editor.set_text(&"a".repeat(MAX_DRAFT_CHARS - 2));
        assert!(editor.insert_newline());
        assert!(editor.insert_char('b'));
        assert_eq!(editor.char_count(), MAX_DRAFT_CHARS);
        assert!(!editor.insert_char('c'));
    }

    #[test]
    fn set_text_clamps_to_the_bound() {
        let mut editor = Editor::new();
        editor.set_text(&"z".repeat(MAX_DRAFT_CHARS + 100));
        assert_eq!(editor.char_count(), MAX_DRAFT_CHARS);
        assert_eq!(editor.cursor(), (0, MAX_DRAFT_CHARS));
    }

    #[test]
    fn paste_is_plain_chars_including_a_trailing_newline() {
        let mut editor = Editor::new();
        assert!(editor.insert_paste("abc\n"));
        assert_eq!(editor.text(), "abc\n");
        assert_eq!(editor.line_count(), 2);
        assert_eq!(editor.cursor(), (1, 0), "cursor after the pasted newline");
    }

    #[test]
    fn paste_normalizes_carriage_returns_and_splits_lines() {
        let mut editor = Editor::new();
        editor.set_text("ab");
        assert!(editor.insert_paste("c\r\nd\r\re"));
        // \r\n -> one newline, a lone \r pair -> two newlines (blank line)
        assert_eq!(editor.text(), "abc\nd\n\ne");
    }

    #[test]
    fn paste_truncates_at_the_bound_and_reports_it() {
        let mut editor = Editor::new();
        let huge = "x".repeat(MAX_DRAFT_CHARS + 5_000);
        assert!(
            !editor.insert_paste(&huge),
            "oversized paste reports truncation"
        );
        assert_eq!(editor.char_count(), MAX_DRAFT_CHARS);
    }

    #[test]
    fn paste_into_multiline_text_keeps_the_tail() {
        let mut editor = Editor::new();
        editor.set_text("ab\ncd");
        // set_text leaves the cursor at the end (1, 2); walk back to
        // (0, 1): between a and b on the first line
        editor.cursor_left();
        editor.cursor_left();
        editor.cursor_left();
        editor.cursor_left();
        assert_eq!(editor.cursor(), (0, 1));
        assert!(editor.insert_paste("XY\nZ"));
        assert_eq!(editor.text(), "aXY\nZb\ncd");
    }

    #[test]
    fn history_walks_and_restores_the_stashed_draft() {
        let mut editor = Editor::new();
        editor.push_history("one");
        editor.push_history("two");
        type_text(&mut editor, "draft");
        editor.history_prev();
        assert_eq!(editor.text(), "two");
        assert!(editor.is_navigating_history());
        editor.history_prev();
        assert_eq!(editor.text(), "one");
        editor.history_prev(); // at the oldest entry: stays
        assert_eq!(editor.text(), "one");
        editor.history_next();
        assert_eq!(editor.text(), "two");
        editor.history_next(); // past the newest: restore the draft
        assert!(!editor.is_navigating_history());
        assert_eq!(
            editor.text(),
            "draft",
            "the in-progress draft survives history browsing"
        );
        editor.history_next(); // not browsing: no-op
        assert_eq!(editor.text(), "draft");
    }

    #[test]
    fn history_prev_without_history_is_a_no_op() {
        let mut editor = Editor::new();
        type_text(&mut editor, "draft");
        editor.history_prev();
        assert_eq!(editor.text(), "draft");
        assert!(!editor.is_navigating_history());
    }

    #[test]
    fn history_restores_multiline_entries() {
        let mut editor = Editor::new();
        editor.push_history("line one\nline two");
        editor.history_prev();
        assert_eq!(editor.text(), "line one\nline two");
        assert_eq!(editor.cursor(), (1, 8));
    }

    #[test]
    fn editing_a_history_entry_commits_it_as_the_draft() {
        let mut editor = Editor::new();
        editor.push_history("one");
        editor.push_history("two");
        type_text(&mut editor, "draft");
        editor.history_prev(); // "two"
        assert!(editor.insert_char('!'));
        assert_eq!(editor.text(), "two!");
        assert!(
            !editor.is_navigating_history(),
            "editing leaves history mode"
        );
        editor.history_next(); // no longer browsing: no-op
        assert_eq!(
            editor.text(),
            "two!",
            "the stash was dropped on edit (readline behavior)"
        );
    }

    #[test]
    fn push_history_skips_empty_and_immediate_repeats() {
        let mut editor = Editor::new();
        editor.push_history("");
        editor.push_history("   \n  ");
        assert_eq!(editor.history_len(), 0);
        editor.push_history("same");
        editor.push_history("same");
        assert_eq!(editor.history_len(), 1);
        editor.push_history("other");
        editor.push_history("same");
        assert_eq!(editor.history_len(), 3);
        assert!(editor.has_history());
    }

    #[test]
    fn history_is_bounded() {
        let mut editor = Editor::new();
        for i in 0..(HISTORY_MAX_ENTRIES + 10) {
            editor.push_history(&format!("prompt {i}"));
        }
        assert_eq!(editor.history_len(), HISTORY_MAX_ENTRIES);
        // the oldest entries were dropped, the newest kept
        editor.history_prev();
        assert_eq!(editor.text(), format!("prompt {}", HISTORY_MAX_ENTRIES + 9));
    }

    #[test]
    fn rejected_control_chars_change_nothing() {
        let mut editor = Editor::new();
        type_text(&mut editor, "ab");
        assert!(!editor.insert_char('\r'));
        assert!(!editor.insert_char('\u{7}'));
        assert_eq!(editor.text(), "ab");
    }
}
