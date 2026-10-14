//! Bounded transcript rendering helpers: char-safe wrapping, speaker and
//! activity line formatting, transcript bounding, and ephemeral-vs-durable
//! reconciliation.
//!
//! Everything here is a pure function of text and width. Wrapping is
//! char-based (multibyte-safe by construction; display width is the
//! renderer's concern), never panics on zero or degenerate widths, and
//! prefers breaking at spaces while hard-breaking words that do not fit.
//! These helpers own no state and know nothing of providers, stores, or
//! the application service.

/// The label prefixing user entries.
pub const USER_LABEL: &str = "You";
/// The label prefixing assistant entries.
pub const ASSISTANT_LABEL: &str = "Relay";
/// The marker prefixing compact activity entries.
pub const ACTIVITY_MARKER: &str = "·";
/// The marker prefixing decision-request entries.
pub const DECISION_MARKER: &str = "? decision";
/// The marker prefixing failed-turn entries.
pub const FAILED_MARKER: &str = "failed";
/// The marker prefixing cancelled-turn entries.
pub const CANCELLED_MARKER: &str = "cancelled";
/// The marker prefixing interrupted-turn entries (a started turn that
/// never reached a terminal fact before a restart).
pub const INTERRUPTED_MARKER: &str = "interrupted";
/// The indent under a speaker label.
pub const SPEAKER_INDENT: &str = "  ";

/// Wrap `text` to `width` chars per line, breaking at spaces when
/// possible and hard-breaking words that do not fit. Existing newlines
/// are preserved as line breaks; a `width` of zero is clamped to one
/// (degenerate but safe — never a panic).
pub fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    for line in text.split('\n') {
        if line.is_empty() {
            lines.push(String::new());
            continue;
        }
        let chars: Vec<char> = line.chars().collect();
        let mut start = 0usize;
        while start < chars.len() {
            let end = (start + width).min(chars.len());
            let mut piece: String = chars[start..end].iter().collect();
            if end < chars.len() {
                // The line must break: prefer the last space inside the
                // piece, dropping that space; otherwise hard-break.
                if let Some(space) = piece.rfind(' ') {
                    if space > 0 {
                        piece.truncate(space);
                        start += space + 1;
                    } else {
                        start = end;
                    }
                } else {
                    start = end;
                }
            } else {
                start = end;
            }
            lines.push(piece);
        }
    }
    lines
}

/// Format a speaker entry: the label on its own line, the wrapped text
/// indented under it. Empty text renders the bare label.
pub fn speaker_block(speaker: &str, text: &str, width: usize) -> Vec<String> {
    let mut lines = vec![speaker.to_string()];
    if text.is_empty() {
        return lines;
    }
    let body_width = width.saturating_sub(SPEAKER_INDENT.chars().count()).max(1);
    for line in wrap_text(text, body_width) {
        if line.is_empty() {
            lines.push(String::new());
        } else {
            lines.push(format!("{SPEAKER_INDENT}{line}"));
        }
    }
    lines
}

/// Format a user entry (`You` + indented text).
pub fn user_block(text: &str, width: usize) -> Vec<String> {
    speaker_block(USER_LABEL, text, width)
}

/// Format an assistant entry (`Relay` + indented text).
pub fn assistant_block(text: &str, width: usize) -> Vec<String> {
    speaker_block(ASSISTANT_LABEL, text, width)
}

/// Format a marker entry (`· text`, `? decision text`, …) with
/// continuations aligned under the text. Empty text renders the bare
/// marker.
pub fn marker_line(marker: &str, text: &str, width: usize) -> Vec<String> {
    if text.is_empty() {
        return vec![marker.to_string()];
    }
    let prefix_width = marker.chars().count() + 1;
    let body_width = width.saturating_sub(prefix_width).max(1);
    let continuation = " ".repeat(prefix_width);
    let mut lines = Vec::new();
    for (index, line) in wrap_text(text, body_width).into_iter().enumerate() {
        if index == 0 {
            lines.push(format!("{marker} {line}"));
        } else if line.is_empty() {
            lines.push(String::new());
        } else {
            lines.push(format!("{continuation}{line}"));
        }
    }
    lines
}

/// Format a compact activity entry (`· text`).
pub fn activity_block(text: &str, width: usize) -> Vec<String> {
    marker_line(ACTIVITY_MARKER, text, width)
}

/// Bound rendered transcript lines to `max_lines`, dropping from the top
/// and acknowledging what was hidden with an ellipsis header
/// (`+ N earlier messages`). A `max_lines` of zero returns nothing;
/// lines that already fit are returned unchanged.
pub fn bound_transcript(lines: &[String], max_lines: usize) -> Vec<String> {
    if lines.len() <= max_lines {
        return lines.to_vec();
    }
    if max_lines == 0 {
        return Vec::new();
    }
    let hidden = lines.len() - (max_lines - 1);
    let mut bounded = Vec::with_capacity(max_lines);
    bounded.push(hidden_label(hidden));
    bounded.extend(lines[lines.len() + 1 - max_lines..].iter().cloned());
    bounded
}

/// The ellipsis header for `hidden` dropped lines.
fn hidden_label(hidden: usize) -> String {
    let plural = if hidden == 1 { "" } else { "s" };
    format!("+ {hidden} earlier message{plural}")
}

/// Reconcile ephemeral lines against a refreshed durable render: keep the
/// ephemeral lines the durable render does not already show (exact line
/// match), so absorbing a new view never duplicates an entry that became
/// durable. The chat state machine applies this on every view refresh —
/// live activity that has not landed in the view survives, streamed and
/// activity lines that have are absorbed.
pub fn reconcile_ephemeral(ephemeral: &[String], durable: &[String]) -> Vec<String> {
    ephemeral
        .iter()
        .filter(|line| !durable.contains(line))
        .cloned()
        .collect()
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn wrap_breaks_at_spaces() {
        assert_eq!(
            wrap_text("the quick brown fox", 10),
            vec!["the quick".to_string(), "brown fox".to_string()]
        );
    }

    #[test]
    fn wrap_hard_breaks_long_words() {
        assert_eq!(
            wrap_text("aaaaaaaaaaaa", 5),
            vec!["aaaaa".to_string(), "aaaaa".to_string(), "aa".to_string()]
        );
    }

    #[test]
    fn wrap_preserves_newlines_and_empty_lines() {
        assert_eq!(
            wrap_text("ab\n\ncd", 10),
            vec!["ab".to_string(), String::new(), "cd".to_string()]
        );
    }

    #[test]
    fn wrap_short_text_is_unchanged() {
        assert_eq!(wrap_text("hello", 10), vec!["hello".to_string()]);
        assert_eq!(wrap_text("", 10), vec![String::new()]);
    }

    #[test]
    fn wrap_is_char_based_not_byte_based() {
        // 6 CJK chars, width 4: two display groups, no code point split
        assert_eq!(
            wrap_text("你好世界测试", 4),
            vec!["你好世界".to_string(), "测试".to_string()]
        );
        // an emoji family (a ZWJ sequence) wraps as whole chars, never
        // byte fragments (a sequence may still split across lines at
        // widths below its char count — that is inherent to char-based
        // wrapping)
        assert_eq!(wrap_text("👨‍👩‍👧x", 5), vec!["👨‍👩‍👧".to_string(), "x".to_string()]);
    }

    #[test]
    fn wrap_never_panics_on_zero_or_degenerate_widths() {
        let lines = wrap_text("ab cd", 0);
        assert!(!lines.is_empty());
        assert!(lines.iter().all(|line| line.chars().count() <= 1));
        let single = wrap_text("a", 1);
        assert_eq!(single, vec!["a".to_string()]);
        let empty = wrap_text("", 0);
        assert_eq!(empty, vec![String::new()]);
    }

    #[test]
    fn user_and_assistant_blocks_indent_under_the_label() {
        assert_eq!(
            user_block("hello world", 12),
            vec![
                "You".to_string(),
                "  hello".to_string(),
                "  world".to_string()
            ]
        );
        assert_eq!(
            assistant_block("I am tracing the checkout and retry paths.", 24),
            vec![
                "Relay".to_string(),
                "  I am tracing the".to_string(),
                "  checkout and retry".to_string(),
                "  paths.".to_string()
            ]
        );
    }

    #[test]
    fn speaker_block_keeps_internal_blank_lines() {
        assert_eq!(
            user_block("a\n\nb", 10),
            vec![
                "You".to_string(),
                "  a".to_string(),
                String::new(),
                "  b".to_string()
            ]
        );
    }

    #[test]
    fn speaker_block_with_empty_text_is_the_bare_label() {
        assert_eq!(user_block("", 10), vec!["You".to_string()]);
        assert_eq!(assistant_block("", 10), vec!["Relay".to_string()]);
    }

    #[test]
    fn activity_lines_carry_the_marker_with_aligned_continuations() {
        assert_eq!(
            activity_block("running tests now", 12),
            vec!["· running".to_string(), "  tests now".to_string()]
        );
        assert_eq!(
            marker_line("? decision", "choose the idempotency boundary", 26),
            vec![
                "? decision choose the".to_string(),
                "           idempotency".to_string(),
                "           boundary".to_string()
            ]
        );
    }

    #[test]
    fn marker_line_with_empty_text_is_the_bare_marker() {
        assert_eq!(activity_block("", 10), vec!["·".to_string()]);
        assert_eq!(
            marker_line(FAILED_MARKER, "", 10),
            vec!["failed".to_string()]
        );
    }

    #[test]
    fn unicode_text_renders_without_panicking() {
        let lines = user_block("修复结账重复下单的问题，然后运行测试。", 10);
        assert_eq!(lines.first().map(String::as_str), Some("You"));
        assert!(lines.len() > 1);
        assert!(lines[1..].iter().all(|line| line.starts_with("  ")));
    }

    #[test]
    fn bound_transcript_returns_fitting_lines_unchanged() {
        let lines: Vec<String> = (0..3).map(|i| i.to_string()).collect();
        assert_eq!(bound_transcript(&lines, 3), lines);
        assert_eq!(bound_transcript(&lines, 10), lines);
        assert_eq!(bound_transcript(&[], 3), Vec::<String>::new());
    }

    #[test]
    fn bound_transcript_drops_from_the_top_with_an_ellipsis_header() {
        let lines: Vec<String> = (0..5).map(|i| i.to_string()).collect();
        assert_eq!(
            bound_transcript(&lines, 3),
            vec![
                "+ 3 earlier messages".to_string(),
                "3".to_string(),
                "4".to_string()
            ]
        );
    }

    #[test]
    fn bound_transcript_header_always_acknowledges_the_hidden_count() {
        // with a positive maximum, at least two lines are always hidden
        let lines: Vec<String> = (0..3).map(|i| i.to_string()).collect();
        assert_eq!(
            bound_transcript(&lines, 2),
            vec!["+ 2 earlier messages".to_string(), "2".to_string()]
        );
    }

    #[test]
    fn bound_transcript_degenerate_maximums_never_panic() {
        let lines: Vec<String> = (0..4).map(|i| i.to_string()).collect();
        assert_eq!(bound_transcript(&lines, 0), Vec::<String>::new());
        assert_eq!(
            bound_transcript(&lines, 1),
            vec!["+ 4 earlier messages".to_string()]
        );
    }

    #[test]
    fn reconcile_keeps_only_lines_the_durable_render_lacks() {
        let ephemeral = vec![
            "· running tests".to_string(),
            "Relay".to_string(),
            "  I am still streaming…".to_string(),
        ];
        let durable = vec![
            "You".to_string(),
            "  fix the tests".to_string(),
            "Relay".to_string(),
            "· running tests".to_string(),
        ];
        assert_eq!(
            reconcile_ephemeral(&ephemeral, &durable),
            vec!["  I am still streaming…".to_string()]
        );
    }

    #[test]
    fn reconcile_with_no_overlap_keeps_everything_and_with_full_overlap_nothing() {
        let durable = vec!["Relay".to_string(), "  done".to_string()];
        let fresh = vec!["· started".to_string()];
        assert_eq!(reconcile_ephemeral(&fresh, &durable), fresh);
        let absorbed = vec!["Relay".to_string()];
        assert!(reconcile_ephemeral(&absorbed, &durable).is_empty());
        assert!(reconcile_ephemeral(&[], &durable).is_empty());
    }
}
