//! Stable views over the durable conversation.
//!
//! The chat transcript is a *view* over the folded graph, never a separate
//! record: user/assistant pairs come from conversation message facts,
//! decision prompts and interrupted turns are derived from graph state, and
//! everything rendered is bounded and redacted. The interface crates (chat
//! UI, `--print`) consume these view models and own no policy.

use relay_domain::ConversationId;
use relay_domain::DecisionId;
use relay_domain::TurnId;
use relay_domain::TurnOutcome;

use crate::conversation::sanitize_for_record;
use crate::session::ProjectSession;

/// How one transcript entry classifies. Rendering keys on the kind, never
/// on parsing the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatEntryKind {
    /// A message the user sent.
    User,
    /// A recorded assistant message.
    Assistant,
    /// Compact work activity (bounded, one line).
    Activity,
    /// An open decision surfaced into the transcript.
    Decision,
    /// A turn that failed.
    Failed,
    /// A turn the user cancelled.
    Cancelled,
    /// A started turn with no terminal fact (crash or restart).
    Interrupted,
}

impl ChatEntryKind {
    /// The stable rendering label for the kind.
    pub fn label(self) -> &'static str {
        match self {
            ChatEntryKind::User => "You",
            ChatEntryKind::Assistant => "Relay",
            ChatEntryKind::Activity => "Relay",
            ChatEntryKind::Decision => "Decision",
            ChatEntryKind::Failed => "Failed",
            ChatEntryKind::Cancelled => "Cancelled",
            ChatEntryKind::Interrupted => "Interrupted",
        }
    }
}

/// Character cap for one rendered chat entry. Entries derive from bounded
/// durable text, but views enforce their own bound so a future input path
/// can never render unbounded text.
pub const ENTRY_TEXT_CAP_CHARS: usize = 16_384;

/// Bound and redact one entry text for rendering. Views never display a
/// secret a durable fact was protected from.
pub fn entry_text(text: &str) -> String {
    sanitize_for_record(text, ENTRY_TEXT_CAP_CHARS)
}

/// Render one open decision as a transcript line the user can answer:
/// `dec_x: which database? (options: sqlite, postgres)`.
pub fn decision_line(decision: &str, question: &str, options: &[String]) -> String {
    let mut line = format!("{decision}: {question}");
    if !options.is_empty() {
        line.push_str(&format!(" (options: {})", options.join(", ")));
    }
    entry_text(&line)
}

/// Render one failed-turn line: the safe, redacted failure message.
pub fn failure_line(message: &str) -> String {
    entry_text(&format!("turn failed: {message}"))
}

/// Render one cancelled-turn line.
pub fn cancelled_line(reason: &str) -> String {
    entry_text(&format!("turn cancelled: {reason}"))
}

/// Render the interrupted-turn line for a started turn with no terminal
/// fact. Recovery must never invent an assistant reply.
pub fn interrupted_line(turn: &str) -> String {
    entry_text(&format!(
        "turn {turn} started but never finished (interrupted by a restart); no reply was recorded"
    ))
}

/// Take the chronological tail of `entries`, bounded to `limit`; a zero
/// limit yields no entries.
pub fn bounded_tail<T>(mut entries: Vec<T>, limit: usize) -> Vec<T> {
    if entries.len() <= limit {
        return entries;
    }
    let drop = entries.len() - limit;
    entries.drain(..drop);
    entries
}

/// One rendered transcript entry.
#[derive(Debug, Clone, PartialEq)]
pub struct ChatEntry {
    /// How the entry classifies.
    pub kind: ChatEntryKind,
    /// The bounded, redacted entry text.
    pub text: String,
    /// The turn the entry belongs to, when it belongs to one (synthetic
    /// decision entries carry none).
    pub turn: Option<TurnId>,
    /// Wall-clock milliseconds when the underlying fact was recorded.
    pub at_ms: i64,
}

/// One open decision the chat surfaces for an answer.
#[derive(Debug, Clone, PartialEq)]
pub struct OpenDecisionRow {
    /// The decision identity.
    pub decision: DecisionId,
    /// The question to answer.
    pub question: String,
    /// The selectable option labels (empty for free-form decisions).
    pub options: Vec<String>,
}

/// The immutable chat snapshot the interface renders: the bounded
/// chronological transcript tail of the latest conversation, the open
/// decisions, the interrupted-turn count, and the declared goal.
#[derive(Debug, Clone, PartialEq)]
pub struct ChatView {
    /// The latest conversation, when one exists.
    pub conversation: Option<ConversationId>,
    /// The bounded chronological transcript tail (`entry_limit` bounds it).
    pub entries: Vec<ChatEntry>,
    /// Open decisions, most recently opened first.
    pub open_decisions: Vec<OpenDecisionRow>,
    /// How many turns of this conversation started but never finished
    /// (interrupted by a restart; never answered).
    pub interrupted_turns: usize,
    /// The goal outcome text, when a goal is declared.
    pub goal: Option<String>,
}

/// Build the chat view over the latest conversation of `session`:
///
/// - transcript entries from the folded conversation (user/assistant pairs,
///   failure and cancellation outcomes), plus one synthetic `Interrupted`
///   entry per started turn with no terminal fact — recovery never invents
///   an assistant reply;
/// - one synthetic `Decision` entry per open decision, most recently opened
///   first;
/// - `entry_limit` bounds the assembled chronological tail.
pub fn chat_view(session: &ProjectSession, entry_limit: usize) -> ChatView {
    let graph = session.graph();
    let conversation = graph.latest_conversation();
    let goal = graph
        .goal()
        .map(|goal| sanitize_for_record(&goal.outcome, ENTRY_TEXT_CAP_CHARS));

    let open: Vec<&relay_domain::DecisionNode> = {
        let mut nodes: Vec<&relay_domain::DecisionNode> = graph
            .decisions()
            .filter(|(_, node)| node.is_open())
            .map(|(_, node)| node)
            .collect();
        nodes.sort_by_key(|node| std::cmp::Reverse(node.decision.opened_at_ms));
        nodes
    };
    let open_decisions: Vec<OpenDecisionRow> = open
        .iter()
        .map(|node| OpenDecisionRow {
            decision: node.decision.id.clone(),
            question: entry_text(&node.decision.question),
            options: node
                .decision
                .options
                .iter()
                .map(|option| option.label.clone())
                .collect(),
        })
        .collect();

    let mut entries: Vec<ChatEntry> = Vec::new();
    let mut interrupted = 0usize;
    if let Some(conversation_id) = &conversation
        && let Some(node) = graph.conversation(conversation_id)
    {
        interrupted = node.interrupted_turns().len();
        for (turn_id, turn) in &node.turns {
            if let Some(user) = &turn.user {
                entries.push(ChatEntry {
                    kind: ChatEntryKind::User,
                    text: entry_text(&user.text),
                    turn: Some(turn_id.clone()),
                    at_ms: user.at_ms,
                });
            }
            match &turn.outcome {
                Some(TurnOutcome::Completed { text, at_ms, .. }) => entries.push(ChatEntry {
                    kind: ChatEntryKind::Assistant,
                    text: entry_text(text),
                    turn: Some(turn_id.clone()),
                    at_ms: *at_ms,
                }),
                Some(TurnOutcome::Failed {
                    safe_message,
                    at_ms,
                }) => entries.push(ChatEntry {
                    kind: ChatEntryKind::Failed,
                    text: failure_line(safe_message),
                    turn: Some(turn_id.clone()),
                    at_ms: *at_ms,
                }),
                Some(TurnOutcome::Cancelled { reason, at_ms }) => entries.push(ChatEntry {
                    kind: ChatEntryKind::Cancelled,
                    text: cancelled_line(reason),
                    turn: Some(turn_id.clone()),
                    at_ms: *at_ms,
                }),
                None => entries.push(ChatEntry {
                    kind: ChatEntryKind::Interrupted,
                    text: interrupted_line(turn_id.as_str()),
                    turn: Some(turn_id.clone()),
                    at_ms: turn
                        .user
                        .as_ref()
                        .map(|user| user.at_ms)
                        .unwrap_or_default(),
                }),
            }
        }
    }

    // Open decisions surface even before any conversation exists.
    for node in &open {
        entries.push(ChatEntry {
            kind: ChatEntryKind::Decision,
            text: decision_line(
                node.decision.id.as_str(),
                &node.decision.question,
                &node
                    .decision
                    .options
                    .iter()
                    .map(|option| option.label.clone())
                    .collect::<Vec<_>>(),
            ),
            turn: None,
            at_ms: node.decision.opened_at_ms,
        });
    }
    // Chronological, with the stable sort preserving per-turn order and
    // most-recent-first decision order among equal timestamps.
    entries.sort_by_key(|entry| entry.at_ms);

    ChatView {
        conversation,
        entries: bounded_tail(entries, entry_limit),
        open_decisions,
        interrupted_turns: interrupted,
        goal,
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)] // test-only, house style
mod tests {
    use super::*;

    #[test]
    fn every_kind_renders_under_its_expected_label() {
        let labels = [
            ChatEntryKind::User.label(),
            ChatEntryKind::Assistant.label(),
            ChatEntryKind::Activity.label(),
            ChatEntryKind::Decision.label(),
            ChatEntryKind::Failed.label(),
            ChatEntryKind::Cancelled.label(),
            ChatEntryKind::Interrupted.label(),
        ];
        assert_eq!(
            labels,
            [
                "You",
                "Relay",
                "Relay",
                "Decision",
                "Failed",
                "Cancelled",
                "Interrupted"
            ]
        );
    }

    #[test]
    fn decision_lines_carry_the_id_question_and_options() {
        let line = decision_line(
            "dec_1",
            "Which database?",
            &["sqlite".to_string(), "postgres".to_string()],
        );
        assert_eq!(line, "dec_1: Which database? (options: sqlite, postgres)");
        let free_form = decision_line("dec_2", "What scope?", &[]);
        assert_eq!(free_form, "dec_2: What scope?");
    }

    #[test]
    fn terminal_lines_are_prefixed_and_interrupted_turns_never_invent_replies() {
        assert!(failure_line("provider 500").starts_with("turn failed: provider 500"));
        assert!(cancelled_line("ctrl-c").starts_with("turn cancelled: ctrl-c"));
        let interrupted = interrupted_line("turn_1-2");
        assert!(interrupted.contains("turn_1-2"), "{interrupted}");
        assert!(
            interrupted.contains("no reply was recorded"),
            "{interrupted}"
        );
    }

    #[test]
    fn rendered_entries_are_redacted_and_bounded() {
        let dirty = "leaked sk-ant-0123456789abcdefghijklmnop";
        let clean = entry_text(dirty);
        assert!(
            !clean.contains("sk-ant-0123456789abcdefghijklmnop"),
            "{clean}"
        );
        assert!(clean.contains("[redacted:anthropic-key]"), "{clean}");
        let long = "x".repeat(ENTRY_TEXT_CAP_CHARS + 100);
        assert!(entry_text(&long).chars().count() <= ENTRY_TEXT_CAP_CHARS);
    }

    #[test]
    fn the_tail_keeps_the_newest_entries() {
        let entries: Vec<usize> = vec![1, 2, 3, 4, 5];
        assert_eq!(bounded_tail(entries.clone(), 2), vec![4, 5]);
        assert_eq!(bounded_tail(entries.clone(), 10), entries);
        assert_eq!(bounded_tail(entries, 0), Vec::<usize>::new());
    }
}
