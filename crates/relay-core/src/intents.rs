//! Intent contracts: classification of natural-language input into validated
//! proposals, ambiguity detection, and slash-command parsing. Ported from
//! `src/core/intents.ts`.

use crate::artifacts::ArtifactKind;
use crate::modes::{AutonomyBound, WorkflowMode, zod_option_list};
use serde::{Deserialize, Serialize};

/// All intent kinds, in canonical order.
pub const INTENT_KINDS: [IntentKind; 11] = [
    IntentKind::ProvideGoal,
    IntentKind::Advance,
    IntentKind::ReviseArtifact,
    IntentKind::ChangeMode,
    IntentKind::SetAutonomy,
    IntentKind::AnswerQuestion,
    IntentKind::RequestStatus,
    IntentKind::RequestExplanation,
    IntentKind::InspectArtifact,
    IntentKind::Pause,
    IntentKind::Unknown,
];

/// One classified intent kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum IntentKind {
    /// The user provided the product goal.
    #[serde(rename = "provide-goal")]
    ProvideGoal,
    /// Advance the workflow.
    #[serde(rename = "advance")]
    Advance,
    /// Revise an existing artifact.
    #[serde(rename = "revise-artifact")]
    ReviseArtifact,
    /// Change the workflow mode.
    #[serde(rename = "change-mode")]
    ChangeMode,
    /// Change the autonomy bound.
    #[serde(rename = "set-autonomy")]
    SetAutonomy,
    /// Answer a pending question.
    #[serde(rename = "answer-question")]
    AnswerQuestion,
    /// Report workflow status.
    #[serde(rename = "request-status")]
    RequestStatus,
    /// Explain the last decision.
    #[serde(rename = "request-explanation")]
    RequestExplanation,
    /// Show an artifact.
    #[serde(rename = "inspect-artifact")]
    InspectArtifact,
    /// Pause the workflow.
    #[serde(rename = "pause")]
    Pause,
    /// The input could not be classified.
    #[serde(rename = "unknown")]
    Unknown,
}

impl IntentKind {
    /// The wire spelling of this intent kind.
    pub fn as_str(self) -> &'static str {
        match self {
            IntentKind::ProvideGoal => "provide-goal",
            IntentKind::Advance => "advance",
            IntentKind::ReviseArtifact => "revise-artifact",
            IntentKind::ChangeMode => "change-mode",
            IntentKind::SetAutonomy => "set-autonomy",
            IntentKind::AnswerQuestion => "answer-question",
            IntentKind::RequestStatus => "request-status",
            IntentKind::RequestExplanation => "request-explanation",
            IntentKind::InspectArtifact => "inspect-artifact",
            IntentKind::Pause => "pause",
            IntentKind::Unknown => "unknown",
        }
    }

    /// Parse the wire spelling of an intent kind.
    pub fn parse(value: &str) -> Option<Self> {
        INTENT_KINDS
            .iter()
            .copied()
            .find(|kind| kind.as_str() == value)
    }

    /// The enum options as a Zod-style quoted list for validation messages.
    pub fn zod_options() -> String {
        zod_option_list(INTENT_KINDS.iter().map(|kind| kind.as_str()))
    }
}

/// Intent kinds that change workflow direction or touch approved artifacts.
pub const CONSEQUENTIAL_INTENT_KINDS: [IntentKind; 3] = [
    IntentKind::ReviseArtifact,
    IntentKind::ChangeMode,
    IntentKind::SetAutonomy,
];

/// Where an intent came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum IntentSource {
    /// Natural-language input classified by a model.
    #[serde(rename = "natural-language")]
    NaturalLanguage,
    /// An explicit slash command.
    #[serde(rename = "command")]
    Command,
}

impl IntentSource {
    /// The wire spelling of this source.
    pub fn as_str(self) -> &'static str {
        match self {
            IntentSource::NaturalLanguage => "natural-language",
            IntentSource::Command => "command",
        }
    }

    /// The enum options as a Zod-style quoted list for validation messages.
    pub fn zod_options() -> String {
        zod_option_list(["natural-language", "command"].into_iter())
    }
}

/// A validated intent proposal produced by model classification. Everything
/// the controller reads from a model passes through
/// [`parse_intent_proposal`](crate::payloads) (inside the `intent.proposed`
/// payload decoder) before use.
#[derive(Debug, Clone, PartialEq)]
pub struct IntentProposal {
    /// The classified intent kind.
    pub kind: IntentKind,
    /// Classification confidence in [0, 1].
    pub confidence: f64,
    /// Why the model classified the input this way.
    pub rationale: String,
    /// Target mode, when relevant.
    pub mode: Option<WorkflowMode>,
    /// Target autonomy bound, when relevant.
    pub autonomy: Option<AutonomyBound>,
    /// Target artifact kind, when relevant.
    pub artifact_kind: Option<ArtifactKind>,
    /// Focused question to ask when the intent is ambiguous.
    pub question: Option<String>,
    /// Cleaned-up restatement of user input to carry forward.
    pub free_text: Option<String>,
}

impl Default for IntentProposal {
    fn default() -> Self {
        IntentProposal {
            kind: IntentKind::Unknown,
            confidence: 0.0,
            rationale: String::new(),
            mode: None,
            autonomy: None,
            artifact_kind: None,
            question: None,
            free_text: None,
        }
    }
}

/// Confidence below this threshold requires a focused question before acting.
pub const INTENT_CONFIDENCE_FLOOR: f64 = 0.6;

/// Whether a proposal is too ambiguous to act on directly.
pub fn is_intent_ambiguous(proposal: &IntentProposal) -> bool {
    if proposal.kind == IntentKind::Unknown {
        return true;
    }
    if proposal.confidence < INTENT_CONFIDENCE_FLOOR {
        return true;
    }
    let consequential = CONSEQUENTIAL_INTENT_KINDS.contains(&proposal.kind);
    consequential && proposal.confidence < 0.8
}

/// All slash commands, in canonical order.
pub const SLASH_COMMANDS: [&str; 9] = [
    "/status", "/why", "/inspect", "/mode", "/auto", "/pause", "/resume", "/approve", "/revise",
];

/// A parsed slash command (the subset of intents expressible without a model).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedCommand {
    /// `/status`
    Status,
    /// `/why`
    Why,
    /// `/inspect <target>`
    Inspect {
        /// The artifact id or path to inspect.
        target: String,
    },
    /// `/mode <mode>`
    Mode {
        /// The mode to enter.
        mode: WorkflowMode,
    },
    /// `/auto [bound]`
    Auto {
        /// The autonomy bound (defaults to `workflow` when omitted).
        bound: AutonomyBound,
    },
    /// `/pause [reason]`
    Pause {
        /// Why the workflow is paused.
        reason: String,
    },
    /// `/resume`
    Resume,
    /// `/approve [artifact]`
    Approve {
        /// The artifact to approve, or the current one when `None`.
        artifact_id: Option<String>,
    },
    /// `/revise <kind> [reason]`
    Revise {
        /// The kind of artifact to revise.
        artifact_kind: ArtifactKind,
        /// Why the artifact is revised.
        reason: String,
    },
}

/// Parse a slash command. Returns `None` for non-commands and malformed
/// invocations; behavior mirrors `parseSlashCommand` in the TypeScript core.
pub fn parse_slash_command(input: &str) -> Option<ParsedCommand> {
    let trimmed = input.trim();
    if !trimmed.starts_with('/') {
        return None;
    }
    let mut tokens = trimmed.split_whitespace();
    let raw_command = tokens.next()?;
    let rest: Vec<&str> = tokens.collect();
    let arg = rest.join(" ");
    match raw_command {
        "/status" => {
            if arg.is_empty() {
                Some(ParsedCommand::Status)
            } else {
                None
            }
        }
        "/why" => {
            if arg.is_empty() {
                Some(ParsedCommand::Why)
            } else {
                None
            }
        }
        "/inspect" => {
            if arg.is_empty() {
                None
            } else {
                Some(ParsedCommand::Inspect { target: arg })
            }
        }
        "/mode" => WorkflowMode::parse(arg.trim()).map(|mode| ParsedCommand::Mode { mode }),
        "/auto" => {
            if arg.is_empty() {
                return Some(ParsedCommand::Auto {
                    bound: AutonomyBound::Workflow,
                });
            }
            crate::modes::parse_autonomy_bound(arg.trim())
                .map(|bound| ParsedCommand::Auto { bound })
        }
        "/pause" => Some(ParsedCommand::Pause {
            reason: if arg.is_empty() {
                "requested by user".to_string()
            } else {
                arg
            },
        }),
        "/resume" => {
            if arg.is_empty() {
                Some(ParsedCommand::Resume)
            } else {
                None
            }
        }
        "/approve" => Some(ParsedCommand::Approve {
            artifact_id: if arg.is_empty() { None } else { Some(arg) },
        }),
        "/revise" => {
            let kind_arg = rest.first().copied()?;
            let artifact_kind = ArtifactKind::parse(kind_arg)?;
            let reason = rest[1..].join(" ");
            Some(ParsedCommand::Revise {
                artifact_kind,
                reason: if reason.is_empty() {
                    "user requested revision".to_string()
                } else {
                    reason
                },
            })
        }
        _ => None,
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn parses_slash_commands_and_rejects_unknown_ones() {
        assert_eq!(parse_slash_command("/status"), Some(ParsedCommand::Status));
        assert_eq!(
            parse_slash_command("/mode planning"),
            Some(ParsedCommand::Mode {
                mode: WorkflowMode::Planning
            })
        );
        assert_eq!(
            parse_slash_command("/auto until:verification"),
            Some(ParsedCommand::Auto {
                bound: AutonomyBound::UntilMode(WorkflowMode::Verification)
            })
        );
        assert_eq!(
            parse_slash_command("/revise prd tighten the scope"),
            Some(ParsedCommand::Revise {
                artifact_kind: ArtifactKind::Prd,
                reason: "tighten the scope".to_string()
            })
        );
        assert_eq!(parse_slash_command("/nope"), None);
        assert_eq!(parse_slash_command("plain text"), None);
        assert_eq!(parse_slash_command("/status extra"), None);
        assert_eq!(
            parse_slash_command("/pause"),
            Some(ParsedCommand::Pause {
                reason: "requested by user".to_string()
            })
        );
        assert_eq!(
            parse_slash_command("/approve art-1"),
            Some(ParsedCommand::Approve {
                artifact_id: Some("art-1".to_string())
            })
        );
    }

    #[test]
    fn flags_ambiguity_by_kind_and_confidence() {
        let ambiguous = IntentProposal {
            kind: IntentKind::ChangeMode,
            confidence: 0.5,
            rationale: "maybe switch modes".to_string(),
            mode: Some(WorkflowMode::Planning),
            ..Default::default()
        };
        assert!(is_intent_ambiguous(&ambiguous)); // consequential below 0.8

        let clear = IntentProposal {
            kind: IntentKind::RequestStatus,
            confidence: 0.95,
            rationale: "user asked for status".to_string(),
            ..Default::default()
        };
        assert!(!is_intent_ambiguous(&clear));

        let unknown = IntentProposal {
            kind: IntentKind::Unknown,
            confidence: 0.99,
            rationale: "no idea".to_string(),
            ..Default::default()
        };
        assert!(is_intent_ambiguous(&unknown));
    }

    #[test]
    fn exposes_option_lists_for_validation_messages() {
        assert!(IntentKind::zod_options().starts_with("\"provide-goal\""));
        assert_eq!(
            IntentSource::zod_options(),
            "\"natural-language\"|\"command\""
        );
    }
}
