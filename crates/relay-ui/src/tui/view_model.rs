//! Event-driven view model for the interactive terminal interface. Ported
//! from `src/tui/view-model.ts` (retired TypeScript tree, removed from the
//! repository 2026-10-14): a pure, terminal-free fold over controller
//! UI events and local UI actions. Components never orchestrate workflow
//! work; they render [`TuiViewModel`] snapshots and dispatch [`TuiAction`]s.
//! Folding stays deterministic and unit-testable without a terminal.
//!
//! Controller events arrive as values (the runtime pushes them through a
//! feed; see [`crate::tui::feed`]) and the fold still applies the TypeScript
//! implementation's defensive fallbacks — a missing mode renders as
//! `discovery`, a missing artifact version as `1`, and an unrecognized
//! outcome payload falls back to `NEEDS_USER` — so a drifted event degrades
//! gracefully instead of crashing the interface.

/// One controller UI event, mirroring the `ControllerUiEvent` union of the
/// retired `src/runtime/controller.ts` (removed from the tree 2026-10-14).
/// Field-level optionality matches the
/// TypeScript payloads; the fold fills defaults defensively.
#[derive(Debug, Clone, PartialEq)]
pub enum ControllerEvent {
    /// A session lifecycle event (carries no transcript text).
    Session {
        /// The session that started.
        session_id: String,
    },
    /// A workflow mode change.
    Mode {
        /// The mode entered.
        mode: String,
        /// The mode left, when known.
        previous: Option<String>,
        /// Why the transition happened.
        reason: String,
    },
    /// A streamed text delta.
    Delta {
        /// The delta text.
        text: String,
    },
    /// A tool execution phase change.
    Tool {
        /// Tool name.
        tool: String,
        /// Side-effect class, when provided.
        capability: Option<String>,
        /// Start or end of the execution.
        phase: ToolPhase,
        /// Whether the execution succeeded (end phase).
        ok: Option<bool>,
        /// Redacted summary (end phase).
        summary: Option<String>,
    },
    /// An artifact lifecycle change.
    Artifact {
        /// The artifact's id.
        artifact_id: String,
        /// The artifact's kind.
        artifact_kind: String,
        /// The artifact's version.
        version: Option<i64>,
        /// The artifact's status.
        status: String,
    },
    /// A step outcome. The payload is kept loose so malformed or drifted
    /// outcomes exercise the same `NEEDS_USER` fallback as the reference.
    Outcome {
        /// The raw outcome payload (missing, a bare label, or fields).
        outcome: OutcomeValue,
    },
    /// A free-form notice.
    Notice {
        /// The notice text.
        message: String,
    },
    /// A controller error.
    Error {
        /// The error text.
        message: String,
    },
}

/// Start/end phase of a [`ControllerEvent::Tool`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolPhase {
    /// The tool started executing.
    Start,
    /// The tool finished executing.
    End,
}

/// The loose outcome payload of a [`ControllerEvent::Outcome`], mirroring the
/// TypeScript fold's `pickString(payload, [...])` probing.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum OutcomeValue {
    /// The payload was absent.
    #[default]
    Missing,
    /// The payload was a bare (non-object) value.
    Text(String),
    /// The payload was an object with the probed fields.
    Fields {
        /// `kind` / `label` / `outcome`.
        kind: Option<String>,
        /// `reason` / `message` / `detail`.
        reason: Option<String>,
    },
}

/// One rendered line of conversation or activity.
#[derive(Debug, Clone, PartialEq)]
pub enum TranscriptEntry {
    /// The user typed something.
    User {
        /// The submitted text (trimmed).
        text: String,
    },
    /// Model output (streaming deltas merge into the previous entry).
    Assistant {
        /// The accumulated text.
        text: String,
    },
    /// A tool execution.
    Tool {
        /// Tool name.
        tool: String,
        /// Start or end of the execution.
        phase: ToolPhase,
        /// Whether the execution succeeded, when reported.
        ok: Option<bool>,
        /// Redacted summary, when reported.
        summary: Option<String>,
    },
    /// An artifact lifecycle change.
    Artifact {
        /// The artifact's id.
        artifact_id: String,
        /// The artifact's kind.
        artifact_kind: String,
        /// The artifact's version.
        version: i64,
        /// The artifact's status.
        status: String,
    },
    /// A workflow mode change.
    Mode {
        /// The mode entered.
        mode: String,
        /// The mode left (`None` renders as `start`).
        previous: Option<String>,
        /// Why the transition happened.
        reason: String,
    },
    /// A free-form notice.
    Notice {
        /// The notice text.
        message: String,
    },
    /// An error line.
    Error {
        /// The error text.
        message: String,
    },
    /// A step outcome.
    Outcome {
        /// Normalized label (one of the four documented kinds).
        label: String,
        /// Why the outcome was chosen.
        reason: String,
    },
}

/// Focused side panel, or none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Panel {
    /// No panel focused.
    #[default]
    None,
    /// The `/status` view.
    Status,
    /// The `/why` view.
    Why,
    /// The `/inspect` view.
    Inspect,
    /// The `/help` reference.
    PauseHelp,
}

/// The render model consumed by the terminal components.
#[derive(Debug, Clone, PartialEq)]
pub struct TuiViewModel {
    /// Completed and in-flight entries, capped at the last
    /// [`TRANSCRIPT_CAP`].
    pub transcript: Vec<TranscriptEntry>,
    /// The focused panel.
    pub panel: Panel,
    /// Rendered text for the active panel.
    pub panel_content: String,
    /// A controller operation is in flight.
    pub busy: bool,
    /// The most recent error, if any.
    pub last_error: Option<String>,
    /// The user asked to leave the interface.
    pub exit_requested: bool,
    /// Terminal width in columns.
    pub columns: usize,
}

/// A UI action dispatched into the view model.
#[derive(Debug, Clone, PartialEq)]
pub enum TuiAction {
    /// A controller UI event.
    ControllerEvent(ControllerEvent),
    /// The user submitted input (already recorded in the transcript).
    UserInput {
        /// The submitted text.
        text: String,
    },
    /// Focus a panel (with optional rendered content).
    Panel {
        /// The panel to focus.
        panel: Panel,
        /// Rendered panel content; `None` clears it.
        content: Option<String>,
    },
    /// A controller operation started or finished.
    Busy(bool),
    /// Set (or clear with `None`/empty) the error state.
    Error(Option<String>),
    /// The terminal changed width.
    Resize(usize),
    /// The user asked to exit.
    Exit,
}

/// Transcript keeps at most the last 500 entries.
pub const TRANSCRIPT_CAP: usize = 500;

/// The four documented outcome labels.
pub const OUTCOME_LABELS: [&str; 4] = ["CONTINUE", "ADVANCE", "NEEDS_USER", "REVISE"];

/// Default terminal width before a resize is reported.
pub const DEFAULT_COLUMNS: usize = 80;

/// Minimum terminal width the view model clamps to.
const MIN_COLUMNS: usize = 20;

impl Default for ViewModel {
    fn default() -> ViewModel {
        ViewModel::new()
    }
}

/// An isolated view model. Dispatch mutates an internal draft;
/// [`ViewModel::state`] returns a snapshot callers treat as read-only.
#[derive(Debug)]
pub struct ViewModel {
    draft: TuiViewModel,
}

impl ViewModel {
    /// A fresh view model with the default state.
    pub fn new() -> ViewModel {
        ViewModel {
            draft: TuiViewModel {
                transcript: Vec::new(),
                panel: Panel::None,
                panel_content: String::new(),
                busy: false,
                last_error: None,
                exit_requested: false,
                columns: DEFAULT_COLUMNS,
            },
        }
    }

    /// The current state snapshot.
    pub fn state(&self) -> &TuiViewModel {
        &self.draft
    }

    /// Apply one action.
    pub fn dispatch(&mut self, action: TuiAction) {
        match action {
            TuiAction::ControllerEvent(event) => fold_controller_event(&mut self.draft, event),
            TuiAction::UserInput { text } => {
                let trimmed = text.trim().to_string();
                if !trimmed.is_empty() {
                    push_entry(&mut self.draft, TranscriptEntry::User { text: trimmed });
                }
            }
            TuiAction::Panel { panel, content } => {
                self.draft.panel = panel;
                self.draft.panel_content = content.unwrap_or_default();
            }
            TuiAction::Busy(busy) => self.draft.busy = busy,
            TuiAction::Error(message) => match message {
                None => self.draft.last_error = None,
                Some(text) if text.is_empty() => self.draft.last_error = None,
                Some(text) => {
                    self.draft.last_error = Some(text.clone());
                    push_entry(&mut self.draft, TranscriptEntry::Error { message: text });
                }
            },
            TuiAction::Resize(columns) => {
                self.draft.columns = columns.max(MIN_COLUMNS);
            }
            TuiAction::Exit => self.draft.exit_requested = true,
        }
    }
}

fn cap_transcript(entries: &[TranscriptEntry]) -> Vec<TranscriptEntry> {
    if entries.len() <= TRANSCRIPT_CAP {
        return entries.to_vec();
    }
    entries[entries.len() - TRANSCRIPT_CAP..].to_vec()
}

fn push_entry(draft: &mut TuiViewModel, entry: TranscriptEntry) {
    let mut transcript = draft.transcript.clone();
    transcript.push(entry);
    draft.transcript = cap_transcript(&transcript);
}

/// Normalizes an outcome label to the four documented kinds
/// (`NEEDS_USER` for anything unrecognized).
fn outcome_label(value: &OutcomeValue) -> String {
    let raw = match value {
        OutcomeValue::Text(text) => text.clone(),
        OutcomeValue::Fields { kind, .. } => kind.clone().unwrap_or_default(),
        OutcomeValue::Missing => String::new(),
    };
    let mut normalized = String::new();
    for ch in raw.to_uppercase().chars() {
        if ch.is_ascii_whitespace() || ch == '-' {
            if !normalized.ends_with('_') && !normalized.is_empty() {
                normalized.push('_');
            }
        } else {
            normalized.push(ch);
        }
    }
    // The reference replaces each run of whitespace/hyphens with one `_`;
    // collapse any accidental doubles introduced above.
    while normalized.contains("__") {
        normalized = normalized.replace("__", "_");
    }
    if OUTCOME_LABELS.contains(&normalized.as_str()) {
        normalized
    } else {
        "NEEDS_USER".to_string()
    }
}

fn fold_controller_event(draft: &mut TuiViewModel, event: ControllerEvent) {
    match event {
        ControllerEvent::Session { .. } => {
            // Session events carry no transcript text (the reference probes
            // message/text/detail keys that never exist on them).
        }
        ControllerEvent::Mode {
            mode,
            previous,
            reason,
        } => {
            let mode = if mode.is_empty() {
                "discovery".to_string()
            } else {
                mode
            };
            let previous = previous.filter(|value| !value.is_empty());
            push_entry(
                draft,
                TranscriptEntry::Mode {
                    mode,
                    previous,
                    reason,
                },
            );
        }
        ControllerEvent::Delta { text } => {
            if text.is_empty() {
                return;
            }
            if let Some(TranscriptEntry::Assistant { text: last }) = draft.transcript.last_mut() {
                last.push_str(&text);
                return;
            }
            push_entry(draft, TranscriptEntry::Assistant { text });
        }
        ControllerEvent::Tool {
            tool,
            phase: ToolPhase::Start,
            ..
        } => {
            let tool = if tool.is_empty() {
                "tool".to_string()
            } else {
                tool
            };
            push_entry(
                draft,
                TranscriptEntry::Tool {
                    tool,
                    phase: ToolPhase::Start,
                    ok: None,
                    summary: None,
                },
            );
        }
        ControllerEvent::Tool {
            tool,
            phase: ToolPhase::End,
            ok,
            summary,
            ..
        } => {
            let tool = if tool.is_empty() {
                "tool".to_string()
            } else {
                tool
            };
            push_entry(
                draft,
                TranscriptEntry::Tool {
                    tool,
                    phase: ToolPhase::End,
                    ok,
                    summary: summary.filter(|text| !text.is_empty()),
                },
            );
        }
        ControllerEvent::Artifact {
            artifact_id,
            artifact_kind,
            version,
            status,
        } => {
            let artifact_kind = if artifact_kind.is_empty() {
                "artifact".to_string()
            } else {
                artifact_kind
            };
            let version = version.filter(|value| *value != 0).unwrap_or(1);
            let status = if status.is_empty() {
                "draft".to_string()
            } else {
                status
            };
            push_entry(
                draft,
                TranscriptEntry::Artifact {
                    artifact_id,
                    artifact_kind,
                    version,
                    status,
                },
            );
        }
        ControllerEvent::Outcome { outcome } => {
            let label = outcome_label(&outcome);
            let reason = match &outcome {
                OutcomeValue::Fields { reason, .. } => reason.clone(),
                _ => None,
            }
            .unwrap_or_default();
            push_entry(draft, TranscriptEntry::Outcome { label, reason });
        }
        ControllerEvent::Notice { message } => {
            if !message.is_empty() {
                push_entry(draft, TranscriptEntry::Notice { message });
            }
        }
        ControllerEvent::Error { message } => {
            let message = if message.is_empty() {
                "unknown controller error".to_string()
            } else {
                message
            };
            draft.last_error = Some(message.clone());
            push_entry(draft, TranscriptEntry::Error { message });
        }
    }
}

/// The stable textual prefix of one transcript entry (never color-only, so
/// every line stays identifiable without color support).
pub fn entry_prefix(entry: &TranscriptEntry) -> &'static str {
    match entry {
        TranscriptEntry::User { .. } => "you> ",
        TranscriptEntry::Assistant { .. } => "relay> ",
        TranscriptEntry::Tool { phase, ok, .. } => match (phase, ok) {
            (ToolPhase::Start, _) => "tool+ ",
            (ToolPhase::End, Some(false)) => "tool✘ ",
            (ToolPhase::End, Some(true)) => "tool✔ ",
            (ToolPhase::End, None) => "tool· ",
        },
        TranscriptEntry::Artifact { .. } => "art ",
        TranscriptEntry::Mode { .. } => "mode ",
        TranscriptEntry::Notice { .. } => "note ",
        TranscriptEntry::Error { .. } => "!err ",
        TranscriptEntry::Outcome { .. } => "out ",
    }
}

/// The text of one transcript entry (without its prefix).
pub fn entry_text(entry: &TranscriptEntry) -> String {
    match entry {
        TranscriptEntry::User { text } | TranscriptEntry::Assistant { text } => text.clone(),
        TranscriptEntry::Tool {
            tool,
            phase: ToolPhase::Start,
            ..
        } => tool.clone(),
        TranscriptEntry::Tool {
            tool,
            phase: ToolPhase::End,
            summary,
            ..
        } => match summary {
            Some(summary) if !summary.is_empty() => format!("{tool} — {summary}"),
            _ => tool.clone(),
        },
        TranscriptEntry::Artifact {
            artifact_kind,
            version,
            status,
            artifact_id,
        } => format!("{artifact_kind} v{version} {status} ({artifact_id})"),
        TranscriptEntry::Mode {
            mode,
            previous,
            reason,
        } => {
            let from = previous.clone().unwrap_or_else(|| "start".to_string());
            if reason.is_empty() {
                format!("{from} -> {mode}")
            } else {
                format!("{from} -> {mode} — {reason}")
            }
        }
        TranscriptEntry::Notice { message } | TranscriptEntry::Error { message } => message.clone(),
        TranscriptEntry::Outcome { label, reason } => format!("{label} — {reason}"),
    }
}

/// Whether an entry renders dimmed (activity rather than conversation).
pub fn entry_is_dim(entry: &TranscriptEntry) -> bool {
    matches!(
        entry,
        TranscriptEntry::Tool { .. }
            | TranscriptEntry::Notice { .. }
            | TranscriptEntry::Artifact { .. }
    )
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    fn fold(events: Vec<ControllerEvent>) -> TuiViewModel {
        let mut model = ViewModel::new();
        for event in events {
            model.dispatch(TuiAction::ControllerEvent(event));
        }
        model.state().clone()
    }

    #[test]
    fn merges_consecutive_deltas_into_a_single_assistant_entry() {
        let state = fold(vec![
            ControllerEvent::Delta {
                text: "Hello".to_string(),
            },
            ControllerEvent::Delta {
                text: " ".to_string(),
            },
            ControllerEvent::Delta {
                text: "world".to_string(),
            },
        ]);
        assert_eq!(state.transcript.len(), 1);
        assert_eq!(
            state.transcript[0],
            TranscriptEntry::Assistant {
                text: "Hello world".to_string()
            }
        );
    }

    #[test]
    fn starts_a_new_assistant_entry_after_another_entry_kind() {
        let state = fold(vec![
            ControllerEvent::Delta {
                text: "first".to_string(),
            },
            ControllerEvent::Notice {
                message: "saved".to_string(),
            },
            ControllerEvent::Delta {
                text: "second".to_string(),
            },
        ]);
        let kinds: Vec<&str> = state
            .transcript
            .iter()
            .map(|entry| match entry {
                TranscriptEntry::Assistant { .. } => "assistant",
                TranscriptEntry::Notice { .. } => "notice",
                _ => "other",
            })
            .collect();
        assert_eq!(kinds, vec!["assistant", "notice", "assistant"]);
    }

    #[test]
    fn folds_mode_tool_artifact_notice_and_error_events_into_prefixed_entries() {
        let state = fold(vec![
            ControllerEvent::Mode {
                mode: "specification".to_string(),
                previous: Some("discovery".to_string()),
                reason: "brief approved".to_string(),
            },
            ControllerEvent::Tool {
                tool: "read-file".to_string(),
                capability: Some("read".to_string()),
                phase: ToolPhase::Start,
                ok: None,
                summary: None,
            },
            ControllerEvent::Tool {
                tool: "read-file".to_string(),
                capability: Some("read".to_string()),
                phase: ToolPhase::End,
                ok: Some(true),
                summary: Some("CLAUDE.md".to_string()),
            },
            ControllerEvent::Tool {
                tool: "run-command".to_string(),
                capability: Some("execute".to_string()),
                phase: ToolPhase::End,
                ok: Some(false),
                summary: Some("exit 1".to_string()),
            },
            ControllerEvent::Artifact {
                artifact_id: "art_1".to_string(),
                artifact_kind: "prd".to_string(),
                version: Some(2),
                status: "approved".to_string(),
            },
            ControllerEvent::Notice {
                message: "persisted".to_string(),
            },
            ControllerEvent::Error {
                message: "provider unavailable".to_string(),
            },
        ]);
        assert_eq!(
            state.transcript,
            vec![
                TranscriptEntry::Mode {
                    mode: "specification".to_string(),
                    previous: Some("discovery".to_string()),
                    reason: "brief approved".to_string()
                },
                TranscriptEntry::Tool {
                    tool: "read-file".to_string(),
                    phase: ToolPhase::Start,
                    ok: None,
                    summary: None
                },
                TranscriptEntry::Tool {
                    tool: "read-file".to_string(),
                    phase: ToolPhase::End,
                    ok: Some(true),
                    summary: Some("CLAUDE.md".to_string())
                },
                TranscriptEntry::Tool {
                    tool: "run-command".to_string(),
                    phase: ToolPhase::End,
                    ok: Some(false),
                    summary: Some("exit 1".to_string())
                },
                TranscriptEntry::Artifact {
                    artifact_id: "art_1".to_string(),
                    artifact_kind: "prd".to_string(),
                    version: 2,
                    status: "approved".to_string()
                },
                TranscriptEntry::Notice {
                    message: "persisted".to_string()
                },
                TranscriptEntry::Error {
                    message: "provider unavailable".to_string()
                },
            ]
        );
        assert_eq!(state.last_error.as_deref(), Some("provider unavailable"));
    }

    #[test]
    fn labels_outcome_events_with_the_four_documented_kinds() {
        let state = fold(vec![
            ControllerEvent::Outcome {
                outcome: OutcomeValue::Fields {
                    kind: Some("CONTINUE".to_string()),
                    reason: Some("keep drafting".to_string()),
                },
            },
            ControllerEvent::Outcome {
                outcome: OutcomeValue::Fields {
                    kind: Some("ADVANCE".to_string()),
                    reason: Some("brief approved".to_string()),
                },
            },
            ControllerEvent::Outcome {
                outcome: OutcomeValue::Fields {
                    kind: Some("NEEDS_USER".to_string()),
                    reason: Some("contract approval".to_string()),
                },
            },
            ControllerEvent::Outcome {
                outcome: OutcomeValue::Fields {
                    kind: Some("REVISE".to_string()),
                    reason: Some("stale brief".to_string()),
                },
            },
        ]);
        let labels: Vec<String> = state
            .transcript
            .iter()
            .map(|entry| match entry {
                TranscriptEntry::Outcome { label, .. } => label.clone(),
                _ => String::new(),
            })
            .collect();
        assert_eq!(labels, vec!["CONTINUE", "ADVANCE", "NEEDS_USER", "REVISE"]);
        let reasons: Vec<String> = state
            .transcript
            .iter()
            .map(|entry| match entry {
                TranscriptEntry::Outcome { reason, .. } => reason.clone(),
                _ => String::new(),
            })
            .collect();
        assert_eq!(
            reasons,
            vec![
                "keep drafting",
                "brief approved",
                "contract approval",
                "stale brief"
            ]
        );
    }

    #[test]
    fn falls_back_to_needs_user_for_unrecognized_outcome_payloads() {
        let state = fold(vec![
            ControllerEvent::Outcome {
                outcome: OutcomeValue::Missing,
            },
            ControllerEvent::Outcome {
                outcome: OutcomeValue::Text("gibberish".to_string()),
            },
        ]);
        for entry in &state.transcript {
            match entry {
                TranscriptEntry::Outcome { label, .. } => assert_eq!(label, "NEEDS_USER"),
                other => panic!("unexpected entry {other:?}"),
            }
        }
    }

    #[test]
    fn ignores_session_events_which_carry_no_transcript_text() {
        let state = fold(vec![ControllerEvent::Session {
            session_id: "s_1".to_string(),
        }]);
        assert!(state.transcript.is_empty());
    }

    #[test]
    fn caps_the_transcript_at_the_last_500_entries() {
        let mut model = ViewModel::new();
        for i in 0..TRANSCRIPT_CAP + 60 {
            model.dispatch(TuiAction::ControllerEvent(ControllerEvent::Notice {
                message: format!("n{i}"),
            }));
        }
        let state = model.state();
        assert_eq!(state.transcript.len(), TRANSCRIPT_CAP);
        assert_eq!(
            state.transcript.first(),
            Some(&TranscriptEntry::Notice {
                message: "n60".to_string()
            })
        );
        assert_eq!(
            state.transcript.last(),
            Some(&TranscriptEntry::Notice {
                message: format!("n{}", TRANSCRIPT_CAP + 59)
            })
        );
    }

    #[test]
    fn records_trimmed_user_input_and_skips_empty_input() {
        let mut model = ViewModel::new();
        model.dispatch(TuiAction::UserInput {
            text: "  build a timer  ".to_string(),
        });
        model.dispatch(TuiAction::UserInput {
            text: "   ".to_string(),
        });
        assert_eq!(
            model.state().transcript,
            vec![TranscriptEntry::User {
                text: "build a timer".to_string()
            }]
        );
    }

    #[test]
    fn sets_the_panel_and_its_rendered_content() {
        let mut model = ViewModel::new();
        model.dispatch(TuiAction::Panel {
            panel: Panel::Status,
            content: Some("goal: ship it".to_string()),
        });
        assert_eq!(model.state().panel, Panel::Status);
        assert_eq!(model.state().panel_content, "goal: ship it");
        model.dispatch(TuiAction::Panel {
            panel: Panel::None,
            content: None,
        });
        assert_eq!(model.state().panel, Panel::None);
        assert_eq!(model.state().panel_content, "");
    }

    #[test]
    fn toggles_busy_and_exit_flags() {
        let mut model = ViewModel::new();
        model.dispatch(TuiAction::Busy(true));
        assert!(model.state().busy);
        model.dispatch(TuiAction::Busy(false));
        assert!(!model.state().busy);
        assert!(!model.state().exit_requested);
        model.dispatch(TuiAction::Exit);
        assert!(model.state().exit_requested);
    }

    #[test]
    fn shows_error_messages_in_the_transcript_and_clears_the_flag_with_none() {
        let mut model = ViewModel::new();
        model.dispatch(TuiAction::Error(Some("route failed".to_string())));
        assert_eq!(model.state().last_error.as_deref(), Some("route failed"));
        assert_eq!(
            model.state().transcript.last(),
            Some(&TranscriptEntry::Error {
                message: "route failed".to_string()
            })
        );
        model.dispatch(TuiAction::Error(None));
        assert_eq!(model.state().last_error, None);
        assert_eq!(model.state().transcript.len(), 1);
    }

    #[test]
    fn tracks_terminal_width_and_clamps_degenerate_values() {
        let mut model = ViewModel::new();
        assert_eq!(model.state().columns, 80);
        model.dispatch(TuiAction::Resize(40));
        assert_eq!(model.state().columns, 40);
        model.dispatch(TuiAction::Resize(1));
        assert_eq!(model.state().columns, 20);
    }
}
