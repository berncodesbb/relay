//! Pure conversation projection: durable chat state folded from facts.
//!
//! Conversations are workspace-scoped exchanges that exist independently of
//! the work graph: a conversation may start — and informational turns may
//! complete — before any goal is declared and with no work at all. The
//! types here are folded into [`crate::graph::ProjectGraph`] by the same
//! deterministic fold as the work graph; nothing in this module performs
//! IO, reads clocks, or generates randomness. Every `at_ms` value is
//! caller-supplied data and its ordering is deliberately not validated.
//!
//! Turn shape: a [`UserMessageRecorded`](crate::facts::DomainFact::UserMessageRecorded)
//! fact starts a turn (exactly one user message per turn), and exactly one
//! terminal fact — an assistant message, a failure, or a cancellation —
//! closes it. A started turn with no terminal outcome is *interrupted*:
//! the restart derivation never invents an assistant response for it.
//!
//! Related ids on assistant messages are annotations, not references the
//! fold checks: conversation facts may be appended before or without the
//! attempts, decisions, and work they name.

use std::collections::BTreeMap;

use crate::ids::{AttemptId, DecisionId, MessageId, TurnId, WorkItemId};

/// The maximum length of a durable message text, counted in [`char`]s.
/// Applies to user messages and assistant messages; enforcement is part of
/// the graph fold (fail closed).
pub const MESSAGE_TEXT_MAX_CHARS: usize = 16_384;

/// The maximum number of entries per related-id list on an assistant
/// message (`related_attempts`, `related_decisions`, `related_work`).
pub const TURN_RELATED_MAX: usize = 32;

/// Who spoke a transcript entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Speaker {
    /// The user.
    User,
    /// The assistant.
    Assistant,
}

/// A durable recorded message: its identity, bounded text, and
/// caller-supplied timestamp.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedMessage {
    /// The message identity (`msg_…`), unique across conversations.
    pub message: MessageId,
    /// The bounded message text.
    pub text: String,
    /// Wall-clock milliseconds when the message was recorded.
    pub at_ms: i64,
}

/// Work-graph ids an assistant message annotated itself with. Annotations
/// only: the fold does not require these ids to exist.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TurnReferences {
    /// Related attempts.
    pub attempts: Vec<AttemptId>,
    /// Related decisions.
    pub decisions: Vec<DecisionId>,
    /// Related work items.
    pub work: Vec<WorkItemId>,
}

/// The terminal outcome of a conversation turn. Exactly one per turn,
/// enforced by the fold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnOutcome {
    /// An assistant message completed the turn.
    Completed {
        /// The assistant message identity.
        message: MessageId,
        /// The bounded final message text.
        text: String,
        /// Wall-clock milliseconds when the message was recorded.
        at_ms: i64,
        /// The annotated work-graph ids.
        related: TurnReferences,
    },
    /// The turn failed with a safe (redacted) message.
    Failed {
        /// The safe failure message shown in the transcript.
        safe_message: String,
        /// Wall-clock milliseconds when the failure was recorded.
        at_ms: i64,
    },
    /// The turn was cancelled.
    Cancelled {
        /// Why it was cancelled.
        reason: String,
        /// Wall-clock milliseconds when the cancellation was recorded.
        at_ms: i64,
    },
}

/// One conversation turn: its user message (always present once the turn
/// exists) and its optional terminal outcome.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TurnNode {
    /// The user message that started this turn.
    pub user: Option<RecordedMessage>,
    /// The terminal outcome, once one was recorded. `None` means the turn
    /// is in flight — *interrupted* after a restart.
    pub outcome: Option<TurnOutcome>,
}

impl TurnNode {
    /// Whether the turn started but recorded no terminal outcome.
    pub fn is_interrupted(&self) -> bool {
        self.user.is_some() && self.outcome.is_none()
    }
}

/// One conversation and its folded turns in deterministic (turn id) order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationNode {
    /// Wall-clock milliseconds when the conversation started
    /// (caller-supplied).
    pub started_at_ms: i64,
    /// The conversation's turns, keyed by turn id.
    pub turns: BTreeMap<TurnId, TurnNode>,
}

impl ConversationNode {
    /// Turns that started (user message recorded) but recorded no terminal
    /// outcome: the interrupted set, in deterministic order.
    pub fn interrupted_turns(&self) -> Vec<&TurnId> {
        self.turns
            .iter()
            .filter(|(_, turn)| turn.is_interrupted())
            .map(|(id, _)| id)
            .collect()
    }
}

/// One entry of a bounded chronological transcript view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptEntry {
    /// The turn the entry belongs to.
    pub turn: TurnId,
    /// Who spoke.
    pub speaker: Speaker,
    /// The bounded message text.
    pub text: String,
    /// Wall-clock milliseconds when the message was recorded.
    pub at_ms: i64,
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::ok_expect,
    clippy::err_expect
)] // test-only conveniences, house style
mod tests {
    use super::*;
    use crate::error::FactError;
    use crate::facts::DomainFact;
    use crate::graph::ProjectGraph;
    use crate::ids::{ConversationId, ProjectId};

    fn ids<T: std::str::FromStr>(value: &str) -> T {
        value.parse::<T>().ok().expect("valid id literal in test")
    }

    fn conversation() -> ConversationId {
        ids("cnv_chat")
    }

    fn started(conversation: &ConversationId, at_ms: i64) -> DomainFact {
        DomainFact::ConversationStarted {
            conversation: conversation.clone(),
            started_at_ms: at_ms,
        }
    }

    fn user(conversation: &ConversationId, n: u8, text: &str, at_ms: i64) -> DomainFact {
        DomainFact::UserMessageRecorded {
            conversation: conversation.clone(),
            turn: ids(&format!("turn_{n}")),
            message: ids(&format!("msg_u{n}")),
            text: text.to_string(),
            at_ms,
        }
    }

    fn assistant(
        conversation: &ConversationId,
        n: u8,
        text: &str,
        at_ms: i64,
        related: TurnReferences,
    ) -> DomainFact {
        DomainFact::AssistantMessageRecorded {
            conversation: conversation.clone(),
            turn: ids(&format!("turn_{n}")),
            message: ids(&format!("msg_a{n}")),
            text: text.to_string(),
            at_ms,
            related_attempts: related.attempts,
            related_decisions: related.decisions,
            related_work: related.work,
        }
    }

    fn base(conversation: &ConversationId) -> Vec<DomainFact> {
        vec![
            DomainFact::ProjectCreated {
                project: ids::<ProjectId>("prj_demo"),
            },
            started(conversation, 1_000),
        ]
    }

    fn fold_error(facts: &[DomainFact]) -> FactError {
        ProjectGraph::from_facts(facts).err().expect("must fail")
    }

    #[test]
    fn conversation_folds_without_goal_or_work() {
        let cnv = conversation();
        let mut facts = base(&cnv);
        facts.push(user(&cnv, 1, "Explain this repository", 1_001));
        facts.push(assistant(
            &cnv,
            1,
            "It is a work-graph harness",
            1_002,
            TurnReferences::default(),
        ));
        let graph = ProjectGraph::from_facts(&facts)
            .ok()
            .expect("no goal or work needed");
        assert!(graph.goal().is_none());
        assert_eq!(graph.works().count(), 0);
        assert_eq!(graph.conversations().count(), 1);
        assert_eq!(graph.latest_conversation(), Some(cnv.clone()));
        let node = graph.conversation(&cnv).expect("conversation folded");
        assert_eq!(node.started_at_ms, 1_000);
        assert_eq!(node.turns.len(), 1);
        assert_eq!(node.interrupted_turns(), Vec::<&TurnId>::new());
        assert!(graph.interrupted_turns(&cnv).is_empty());
    }

    #[test]
    fn replay_is_deterministic() {
        let cnv = conversation();
        let mut facts = base(&cnv);
        facts.push(user(&cnv, 1, "one", 1_001));
        facts.push(assistant(&cnv, 1, "two", 1_002, TurnReferences::default()));
        facts.push(user(&cnv, 2, "three", 1_003));
        let first = ProjectGraph::from_facts(&facts).ok().expect("folds");
        let second = ProjectGraph::from_facts(&facts).ok().expect("folds");
        assert_eq!(first, second);
        assert_eq!(
            first.transcript_tail(&cnv, 10),
            second.transcript_tail(&cnv, 10)
        );
    }

    #[test]
    fn duplicate_conversation_turn_and_message_ids_rejected() {
        let cnv = conversation();
        // Duplicate conversation start.
        let mut facts = base(&cnv);
        facts.push(started(&cnv, 1_005));
        assert!(matches!(
            fold_error(&facts),
            FactError::Duplicate {
                fact: "conversation_started",
                ..
            }
        ));

        // A second user message for the same turn.
        let mut facts = base(&cnv);
        facts.push(user(&cnv, 1, "one", 1_001));
        facts.push(user(&cnv, 1, "again", 1_002));
        assert!(matches!(
            fold_error(&facts),
            FactError::Duplicate {
                fact: "user_message_recorded",
                ..
            }
        ));

        // A duplicate message id, fresh turn.
        let mut facts = base(&cnv);
        facts.push(user(&cnv, 1, "one", 1_001));
        facts.push(DomainFact::UserMessageRecorded {
            conversation: cnv.clone(),
            turn: ids("turn_2"),
            message: ids("msg_u1"),
            text: "reused message id".to_string(),
            at_ms: 1_002,
        });
        assert!(matches!(
            fold_error(&facts),
            FactError::Duplicate {
                fact: "user_message_recorded",
                ..
            }
        ));

        // A duplicate assistant message id across turns.
        let mut facts = base(&cnv);
        facts.push(user(&cnv, 1, "one", 1_001));
        facts.push(user(&cnv, 2, "two", 1_002));
        facts.push(assistant(
            &cnv,
            1,
            "answer",
            1_003,
            TurnReferences::default(),
        ));
        let mut reused = assistant(&cnv, 2, "answer", 1_004, TurnReferences::default());
        if let DomainFact::AssistantMessageRecorded { message, .. } = &mut reused {
            *message = ids("msg_a1"); // reuse the first assistant's id
        }
        facts.push(reused);
        assert!(matches!(
            fold_error(&facts),
            FactError::Duplicate {
                fact: "assistant_message_recorded",
                ..
            }
        ));
    }

    #[test]
    fn assistant_without_started_turn_rejected() {
        let cnv = conversation();
        let facts = vec![
            DomainFact::ProjectCreated {
                project: ids::<ProjectId>("prj_demo"),
            },
            started(&cnv, 1_000),
            assistant(&cnv, 1, "ghost answer", 1_001, TurnReferences::default()),
        ];
        let error = fold_error(&facts);
        assert!(matches!(
            error,
            FactError::InvalidState {
                fact: "assistant_message_recorded",
                ..
            }
        ));
        assert!(error.to_string().contains("without a started turn"));

        // Same for failure and cancellation terminals.
        let mut facts = base(&cnv);
        facts.push(DomainFact::ConversationTurnFailed {
            conversation: cnv.clone(),
            turn: ids("turn_missing"),
            safe_message: "boom".to_string(),
            at_ms: 1_001,
        });
        assert!(matches!(
            fold_error(&facts),
            FactError::InvalidState {
                fact: "conversation_turn_failed",
                ..
            }
        ));
        let mut facts = base(&cnv);
        facts.push(DomainFact::ConversationTurnCancelled {
            conversation: cnv,
            turn: ids("turn_missing"),
            reason: "ctrl-c".to_string(),
            at_ms: 1_001,
        });
        assert!(matches!(
            fold_error(&facts),
            FactError::InvalidState {
                fact: "conversation_turn_cancelled",
                ..
            }
        ));
    }

    #[test]
    fn double_terminal_outcome_rejected() {
        let cnv = conversation();
        for terminal in [
            assistant(&cnv, 1, "again", 1_002, TurnReferences::default()),
            DomainFact::ConversationTurnFailed {
                conversation: cnv.clone(),
                turn: ids("turn_1"),
                safe_message: "late failure".to_string(),
                at_ms: 1_002,
            },
            DomainFact::ConversationTurnCancelled {
                conversation: cnv.clone(),
                turn: ids("turn_1"),
                reason: "late cancel".to_string(),
                at_ms: 1_002,
            },
        ] {
            let mut facts = base(&cnv);
            facts.push(user(&cnv, 1, "one", 1_001));
            facts.push(assistant(
                &cnv,
                1,
                "answer",
                1_002,
                TurnReferences::default(),
            ));
            facts.push(terminal);
            let error = fold_error(&facts);
            assert!(
                matches!(
                    error,
                    FactError::InvalidState { fact, .. }
                        if fact == "assistant_message_recorded"
                            || fact == "conversation_turn_failed"
                            || fact == "conversation_turn_cancelled"
                ),
                "{error}"
            );
            assert!(error.to_string().contains("terminal outcome"));
        }
    }

    #[test]
    fn cross_conversation_references_rejected() {
        let cnv_a = conversation();
        let cnv_b: ConversationId = ids("cnv_other");
        let project = DomainFact::ProjectCreated {
            project: ids::<ProjectId>("prj_demo"),
        };

        // A turn id reused under a different conversation.
        let facts = vec![
            project.clone(),
            started(&cnv_a, 1_000),
            started(&cnv_b, 1_000),
            user(&cnv_a, 1, "one", 1_001),
            user(&cnv_b, 1, "impostor", 1_002),
        ];
        let error = fold_error(&facts);
        assert!(matches!(
            error,
            FactError::InvalidState {
                fact: "user_message_recorded",
                ..
            }
        ));
        assert!(error.to_string().contains("cross-conversation"));

        // A terminal fact naming a turn that belongs to another conversation.
        let facts = vec![
            project,
            started(&cnv_a, 1_000),
            started(&cnv_b, 1_000),
            user(&cnv_a, 1, "one", 1_001),
            assistant(
                &cnv_b,
                1,
                "impostor answer",
                1_002,
                TurnReferences::default(),
            ),
        ];
        let error = fold_error(&facts);
        assert!(matches!(
            error,
            FactError::InvalidState {
                fact: "assistant_message_recorded",
                ..
            }
        ));
        assert!(error.to_string().contains("cross-conversation"));
    }

    #[test]
    fn unknown_conversation_rejected_for_every_referencing_fact_kind() {
        let cnv = conversation();
        let missing: ConversationId = ids("cnv_missing");
        // `ConversationStarted` is the creator: it folds for any fresh id.
        // Every fact that *references* an existing conversation must fail
        // closed when it does not exist.
        let facts = [
            user(&missing, 1, "hello", 1_000),
            assistant(&missing, 1, "hi", 1_000, TurnReferences::default()),
            DomainFact::ConversationTurnFailed {
                conversation: missing.clone(),
                turn: ids("turn_1"),
                safe_message: "boom".to_string(),
                at_ms: 1_000,
            },
            DomainFact::ConversationTurnCancelled {
                conversation: missing,
                turn: ids("turn_1"),
                reason: "ctrl-c".to_string(),
                at_ms: 1_000,
            },
        ];
        for fact in facts {
            let history = vec![
                DomainFact::ProjectCreated {
                    project: ids::<ProjectId>("prj_demo"),
                },
                started(&cnv, 1_000),
                fact,
            ];
            let error = fold_error(&history);
            assert!(
                matches!(error, FactError::UnknownReference { .. }),
                "every referencing conversation fact requires the conversation: {error}"
            );
        }
    }

    #[test]
    fn text_and_related_bounds_are_enforced() {
        let cnv = conversation();
        let limit = "x".repeat(MESSAGE_TEXT_MAX_CHARS);

        // Empty user text.
        let mut facts = base(&cnv);
        facts.push(user(&cnv, 1, "", 1_001));
        assert!(matches!(
            fold_error(&facts),
            FactError::InvalidState {
                fact: "user_message_recorded",
                ..
            }
        ));

        // User text one char over the limit; exactly at the limit is legal.
        let mut facts = base(&cnv);
        facts.push(user(&cnv, 1, &format!("y{}", limit), 1_001));
        let error = fold_error(&facts);
        assert!(matches!(error, FactError::InvalidState { .. }));
        assert!(error.to_string().contains("maximum"));

        let mut facts = base(&cnv);
        facts.push(user(&cnv, 1, &limit, 1_001));
        assert!(ProjectGraph::from_facts(&facts).is_ok());

        // Assistant text over the limit (empty assistant text is allowed:
        // only the bound is enforced).
        let mut facts = base(&cnv);
        facts.push(user(&cnv, 1, "one", 1_001));
        facts.push(assistant(
            &cnv,
            1,
            &format!("y{}", limit),
            1_002,
            TurnReferences::default(),
        ));
        assert!(matches!(
            fold_error(&facts),
            FactError::InvalidState {
                fact: "assistant_message_recorded",
                ..
            }
        ));

        // One related list over TURN_RELATED_MAX.
        let over = TurnReferences {
            attempts: (0..=TURN_RELATED_MAX)
                .map(|n| ids(&format!("att_{n}")))
                .collect(),
            ..Default::default()
        };
        let mut facts = base(&cnv);
        facts.push(user(&cnv, 1, "one", 1_001));
        facts.push(assistant(&cnv, 1, "answer", 1_002, over));
        let error = fold_error(&facts);
        assert!(matches!(error, FactError::InvalidState { .. }));
        assert!(error.to_string().contains("related"));

        // Exactly at the limit folds, and the related ids need not exist.
        let at_limit = TurnReferences {
            decisions: (0..TURN_RELATED_MAX)
                .map(|n| ids(&format!("dec_ghost-{n}")))
                .collect(),
            work: vec![ids("work_ghost")],
            ..Default::default()
        };
        let mut facts = base(&cnv);
        facts.push(user(&cnv, 1, "one", 1_001));
        facts.push(assistant(&cnv, 1, "answer", 1_002, at_limit));
        let graph = ProjectGraph::from_facts(&facts)
            .ok()
            .expect("annotations fold");
        let node = graph.conversation(&cnv).expect("conversation");
        let turn = node.turns.get(&ids::<TurnId>("turn_1")).expect("turn");
        let Some(TurnOutcome::Completed { related, .. }) = &turn.outcome else {
            panic!("turn must be completed");
        };
        assert_eq!(related.decisions.len(), TURN_RELATED_MAX);
    }

    #[test]
    fn interrupted_turns_identified_for_turns_without_a_terminal() {
        let cnv = conversation();
        let mut facts = base(&cnv);
        facts.push(user(&cnv, 1, "one", 1_001));
        facts.push(assistant(
            &cnv,
            1,
            "answer",
            1_002,
            TurnReferences::default(),
        ));
        facts.push(user(&cnv, 2, "two", 1_003));
        facts.push(DomainFact::ConversationTurnFailed {
            conversation: cnv.clone(),
            turn: ids("turn_2"),
            safe_message: "provider died".to_string(),
            at_ms: 1_004,
        });
        facts.push(user(&cnv, 3, "three", 1_005)); // interrupted
        let graph = ProjectGraph::from_facts(&facts).ok().expect("folds");
        assert_eq!(graph.interrupted_turns(&cnv), vec![ids::<TurnId>("turn_3")]);
        // Unknown conversations have no interrupted turns.
        assert!(
            graph
                .interrupted_turns(&ids::<ConversationId>("cnv_none"))
                .is_empty()
        );
    }

    #[test]
    fn transcript_tail_is_chronological_bounded_and_skips_in_flight_assistants() {
        let cnv = conversation();
        let mut facts = base(&cnv);
        // Timestamps deliberately out of turn-id order: ordering is by
        // caller-supplied time, not by fold order or turn id.
        facts.push(user(&cnv, 3, "third question", 3_000));
        facts.push(assistant(
            &cnv,
            3,
            "third answer",
            3_100,
            TurnReferences::default(),
        ));
        facts.push(user(&cnv, 1, "first question", 1_000));
        facts.push(assistant(
            &cnv,
            1,
            "first answer",
            1_100,
            TurnReferences::default(),
        ));
        facts.push(user(&cnv, 2, "second question", 2_000)); // in flight
        let graph = ProjectGraph::from_facts(&facts).ok().expect("folds");

        let speakers = |entries: &[TranscriptEntry]| -> Vec<(Speaker, String)> {
            entries
                .iter()
                .map(|entry| (entry.speaker, entry.text.clone()))
                .collect()
        };
        let tail = graph.transcript_tail(&cnv, 10);
        assert_eq!(
            speakers(&tail),
            vec![
                (Speaker::User, "first question".to_string()),
                (Speaker::Assistant, "first answer".to_string()),
                (Speaker::User, "second question".to_string()),
                (Speaker::User, "third question".to_string()),
                (Speaker::Assistant, "third answer".to_string()),
            ],
            "chronological order; the in-flight turn contributes its user side only"
        );
        assert_eq!(tail[0].turn, ids::<TurnId>("turn_1"));
        assert_eq!(tail[0].at_ms, 1_000);

        // The tail keeps the most recent entries, oldest first.
        let tail = graph.transcript_tail(&cnv, 2);
        assert_eq!(
            speakers(&tail),
            vec![
                (Speaker::User, "third question".to_string()),
                (Speaker::Assistant, "third answer".to_string()),
            ]
        );
        // Degenerate bounds and unknown conversations.
        assert!(graph.transcript_tail(&cnv, 0).is_empty());
        assert!(
            graph
                .transcript_tail(&ids::<ConversationId>("cnv_none"), 10)
                .is_empty()
        );
    }

    #[test]
    fn latest_conversation_tracks_most_recently_applied_start() {
        let cnv_a = conversation();
        let cnv_b: ConversationId = ids("cnv_b");
        let cnv_c: ConversationId = ids("cnv_c");
        let facts = vec![
            DomainFact::ProjectCreated {
                project: ids::<ProjectId>("prj_demo"),
            },
            started(&cnv_a, 1_000),
            started(&cnv_b, 900), // earlier clock, later application
            started(&cnv_c, 1_000),
        ];
        let graph = ProjectGraph::from_facts(&facts).ok().expect("folds");
        assert_eq!(graph.latest_conversation(), Some(cnv_c));
        assert_eq!(graph.conversations().count(), 3);
    }
}
