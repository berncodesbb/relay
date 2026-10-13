//! Terminal components: ratatui widgets over the view model, faithful to
//! `src/tui/components/*.tsx` (labels, key hints, truncation, pause overlay,
//! panel caps). Drawing targets a [`ratatui`] frame — nothing here prints or
//! owns a terminal; the CLI runner supplies the backend (or a
//! `ratatui::backend::TestBackend` in tests).

use crate::tui::view_model::{
    TranscriptEntry, TuiViewModel, entry_is_dim, entry_prefix, entry_text,
};
use ratatui::Frame;
use ratatui::layout::Constraint;
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};
use relay_core::modes::WORKFLOW_MODES;
use relay_core::project_state::{PauseState, ProjectState};

/// Maximum rendered lines per panel before the truncation marker.
pub const MAX_PANEL_LINES: usize = 14;

/// Decision commands offered by the pause overlay while a policy decision is
/// pending (`DECISION_COMMANDS`).
const DECISION_COMMANDS: [(&str, &str); 4] = [
    ("/approve", "approve this artifact and continue"),
    ("/revise prd <why>", "send it back with your reason"),
    (
        "/auto workflow",
        "let relay decide and continue automatically",
    ),
    ("/why", "inspect the evidence behind this pause"),
];

/// Commands offered by the pause overlay for a plain user pause
/// (`PAUSE_COMMANDS`).
const PAUSE_COMMANDS: [(&str, &str); 4] = [
    ("/resume", "continue the workflow"),
    ("/status", "see where things stand"),
    ("/why", "inspect the last decision"),
    ("/auto <bound>", "change the autonomy bound"),
];

/// Truncates text to `max` columns, appending an ellipsis when it does not
/// fit (the limit never drops below four columns).
pub fn truncate_text(text: &str, max: usize) -> String {
    let limit = max.max(4);
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= limit {
        return text.to_string();
    }
    let kept: String = chars[..limit - 1].iter().collect();
    format!("{kept}…")
}

/// Caps rendered lines, replacing the overflow with an explicit marker
/// pointing at `relay inspect`.
pub fn cap_lines(lines: &[String], max: usize) -> Vec<String> {
    if lines.len() <= max {
        return lines.to_vec();
    }
    let hidden = lines.len() - max;
    let mut capped = lines[..max].to_vec();
    let plural = if hidden == 1 { "" } else { "s" };
    capped.push(format!("… ({hidden} more line{plural}, see relay inspect)"));
    capped
}

/// The pause overlay data: why relay stopped plus the offered commands.
#[derive(Debug, Clone, PartialEq)]
pub struct PauseOverlay {
    /// Why relay stopped.
    pub reason: String,
    /// Focused question to answer, when the controller provided one.
    pub question: Option<String>,
    /// Options offered, numbered.
    pub options: Vec<String>,
    /// True when this pause awaits a decision (vs. a plain user pause).
    pub decision: bool,
}

impl PauseOverlay {
    /// The overlay's rendered lines (inside its border).
    pub fn lines(&self) -> Vec<String> {
        let commands = if self.decision {
            DECISION_COMMANDS
        } else {
            PAUSE_COMMANDS
        };
        let command_column = commands
            .iter()
            .map(|(command, _)| command.chars().count())
            .max()
            .unwrap_or(0);
        let mut lines = Vec::new();
        lines.push(
            if self.decision {
                "! relay needs your decision"
            } else {
                "! relay is paused"
            }
            .to_string(),
        );
        if !self.reason.is_empty() {
            lines.push(format!("  reason: {}", self.reason));
        }
        if let Some(question) = self.question.as_ref().filter(|q| !q.is_empty()) {
            lines.push(format!("  {question}"));
        }
        for (index, option) in self.options.iter().enumerate() {
            lines.push(format!("  {}) {option}", index + 1));
        }
        lines.push("  what you can do:".to_string());
        for (command, description) in commands {
            lines.push(format!(
                "    {:<width$}  {description}",
                command,
                width = command_column
            ));
        }
        lines.push("  type a command or an answer and press enter".to_string());
        lines
    }
}

/// Compute the pause overlay from a state snapshot, mirroring the App's
/// visibility rule: the overlay shows while the workflow is paused or a
/// `NEEDS_USER` outcome has not been acknowledged (`outcome_seen_at` is the
/// timestamp of the outcome the user already answered).
pub fn pause_overlay_for(
    snapshot: &ProjectState,
    outcome_seen_at: Option<&str>,
) -> Option<PauseOverlay> {
    let paused: Option<&PauseState> = snapshot.paused();
    let needs_user = snapshot
        .last_outcome()
        .filter(|outcome| {
            outcome.outcome == relay_core::OutcomeKind::NeedsUser
                && Some(outcome.at.as_str()) != outcome_seen_at
        })
        .map(|_| ());
    if paused.is_none() && needs_user.is_none() {
        return None;
    }
    match paused {
        Some(paused) => Some(PauseOverlay {
            reason: paused.reason.clone(),
            question: paused.question.clone(),
            options: paused.options.clone(),
            decision: paused.by.as_str() == "policy",
        }),
        None => {
            let outcome = snapshot.last_outcome()?;
            Some(PauseOverlay {
                reason: outcome.reason.clone(),
                question: None,
                options: Vec::new(),
                decision: true,
            })
        }
    }
}

fn mode_title(mode: &str) -> String {
    for contract in relay_core::MODE_CONTRACTS.iter() {
        if contract.mode.as_str() == mode {
            return contract.title.to_string();
        }
    }
    // Unknown mode strings render verbatim, like the reference.
    mode.to_string()
}

fn stage_of(mode: &str) -> usize {
    WORKFLOW_MODES
        .iter()
        .position(|candidate| candidate.as_str() == mode)
        .map(|index| index + 1)
        .unwrap_or(1)
}

/// Everything the frame renderer needs beyond the view model.
#[derive(Debug, Clone)]
pub struct RenderInputs<'a> {
    /// The controller state snapshot (or the empty projection).
    pub snapshot: &'a ProjectState,
    /// Whether no project is initialized yet.
    pub uninitialized: bool,
    /// The acknowledged `NEEDS_USER` outcome timestamp (submitted input
    /// acknowledges the pending outcome).
    pub outcome_seen_at: Option<&'a str>,
    /// The current prompt-line value.
    pub input_value: &'a str,
}

/// Draw the full interface: header, transcript, pause overlay, focused
/// panel, input line, and footer hint.
pub fn draw(frame: &mut Frame, model: &TuiViewModel, inputs: &RenderInputs) {
    let area = frame.area();
    let columns = model.columns.max(area.width as usize);
    let snapshot = inputs.snapshot;
    let mode = snapshot.current_mode().as_str().to_string();

    // Header lines.
    let stage = stage_of(&mode);
    let title = mode_title(&mode);
    let mut parts = vec![
        format!("relay — {title} [{stage}/{}]", WORKFLOW_MODES.len()),
        format!("auto:{}", snapshot.autonomy().as_str()),
    ];
    if snapshot.paused().is_some() {
        parts.push("!paused".to_string());
    }
    let header_line = truncate_text(&parts.join(" · "), columns);
    let goal = if !snapshot.goal().is_empty() {
        snapshot.goal().to_string()
    } else if inputs.uninitialized {
        "no project yet — type a goal to start".to_string()
    } else {
        "(no goal set)".to_string()
    };
    let goal_line = truncate_text(&goal, columns.saturating_sub(4));

    // Transcript lines (the tail that fits).
    let transcript_lines: Vec<Line> = model
        .transcript
        .iter()
        .map(|entry| transcript_line(entry))
        .collect();

    // Pause overlay and panel contents.
    let overlay_lines: Vec<String> = pause_overlay_for(snapshot, inputs.outcome_seen_at)
        .map(|overlay| overlay.lines())
        .unwrap_or_default();
    let panel_lines: Vec<String> = if model.panel != crate::tui::view_model::Panel::None {
        cap_lines(
            &model
                .panel_content
                .split('\n')
                .map(str::to_string)
                .collect::<Vec<String>>(),
            MAX_PANEL_LINES,
        )
    } else {
        Vec::new()
    };
    let panel_title = match model.panel {
        crate::tui::view_model::Panel::None => "",
        crate::tui::view_model::Panel::Status => "status",
        crate::tui::view_model::Panel::Why => "why",
        crate::tui::view_model::Panel::Inspect => "inspect",
        crate::tui::view_model::Panel::PauseHelp => "help",
    };

    let input_line = if model.busy {
        "› working… (ctrl-c to cancel)".to_string()
    } else {
        format!("› {}", inputs.input_value)
    };
    let footer = "/help for commands · enter to send · ctrl-c exits";

    // Bottom-up height budgeting: footer(1) + input(1) + panel + overlay,
    // header(2), transcript takes the remainder (at least one row).
    let panel_height = panel_lines.len() + 2;
    let overlay_height = overlay_lines.len() + 2;
    let mut transcript_height = area.height as usize - 2 - 1 - 1 - panel_height - overlay_height;
    transcript_height = transcript_height.max(1);

    let chunks = ratatui::layout::Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(transcript_height as u16),
        Constraint::Length(overlay_height as u16),
        Constraint::Length(panel_height as u16),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(area);

    // Header.
    let header_widget = Paragraph::new(vec![
        Line::styled(header_line, Style::new().add_modifier(Modifier::BOLD)),
        Line::styled(goal_line, Style::new().add_modifier(Modifier::DIM)),
    ]);
    frame.render_widget(header_widget, chunks[0]);

    // Transcript: the latest entries that fit (the live tail; older activity
    // stays available through the log).
    let visible: Vec<&Line> = if transcript_lines.len() > transcript_height {
        transcript_lines[transcript_lines.len() - transcript_height..]
            .iter()
            .collect()
    } else {
        transcript_lines.iter().collect()
    };
    let transcript_widget = if visible.is_empty() {
        Paragraph::new(Line::styled(
            "(no activity yet)",
            Style::new().add_modifier(Modifier::DIM),
        ))
    } else {
        Paragraph::new(visible.into_iter().cloned().collect::<Vec<Line>>())
    };
    frame.render_widget(transcript_widget, chunks[1]);

    // Pause overlay.
    if !overlay_lines.is_empty() {
        let overlay_widget = bordered_block("", overlay_lines);
        frame.render_widget(overlay_widget, chunks[2]);
    }

    // Focused panel: a bounded-width bordered box.
    if !panel_lines.is_empty() {
        let width = (columns.saturating_sub(2)).clamp(20, 120);
        let panel_area = ratatui::layout::Layout::horizontal([Constraint::Length(width as u16)])
            .split(chunks[3])[0];
        let panel_widget = bordered_block(panel_title, panel_lines);
        frame.render_widget(panel_widget, panel_area);
    }

    // Input line and footer.
    frame.render_widget(Paragraph::new(input_line), chunks[4]);
    frame.render_widget(
        Paragraph::new(Line::styled(
            footer,
            Style::new().add_modifier(Modifier::DIM),
        )),
        chunks[5],
    );
}

fn transcript_line(entry: &TranscriptEntry) -> Line<'_> {
    let line = format!("{}{}", entry_prefix(entry), entry_text(entry));
    if entry_is_dim(entry) {
        Line::styled(line, Style::new().add_modifier(Modifier::DIM))
    } else {
        Line::raw(line)
    }
}

/// A bordered, padded text block (the shared `PanelFrame` body). Empty lines
/// render as a single space so ratatui keeps the row.
fn bordered_block(title: &str, lines: Vec<String>) -> Paragraph<'_> {
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .padding(ratatui::widgets::Padding::horizontal(1));
    if !title.is_empty() {
        block = block.title(format!(" {title} "));
    }
    let content: Vec<Line> = lines
        .into_iter()
        .map(|line| {
            if line.is_empty() {
                Line::raw(" ")
            } else {
                Line::raw(line)
            }
        })
        .collect();
    Paragraph::new(content).block(block)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn truncates_with_an_ellipsis_and_never_below_four_columns() {
        assert_eq!(truncate_text("abc", 10), "abc");
        assert_eq!(truncate_text("abcdefgh", 5), "abcd…");
        assert_eq!(truncate_text("abcdefgh", 2), "abc…");
    }

    #[test]
    fn caps_lines_with_an_explicit_marker() {
        let lines: Vec<String> = (0..20).map(|i| format!("line {i}")).collect();
        let capped = cap_lines(&lines, MAX_PANEL_LINES);
        assert_eq!(capped.len(), MAX_PANEL_LINES + 1);
        assert_eq!(
            capped.last().unwrap(),
            "… (6 more lines, see relay inspect)"
        );
        let single = cap_lines(&["only".to_string(), "two".to_string()], MAX_PANEL_LINES);
        assert_eq!(single.len(), 2);
        let one_over = cap_lines(&lines[..15], MAX_PANEL_LINES);
        assert_eq!(
            one_over.last().unwrap(),
            "… (1 more line, see relay inspect)"
        );
    }

    #[test]
    fn pause_overlay_lists_question_options_and_decision_commands() {
        let overlay = PauseOverlay {
            reason: "PRD is a contractual artifact".to_string(),
            question: Some("Approve the PRD as the contract?".to_string()),
            options: vec!["approve it".to_string(), "request changes".to_string()],
            decision: true,
        };
        let lines = overlay.lines();
        assert!(lines.contains(&"! relay needs your decision".to_string()));
        assert!(lines.contains(&"  reason: PRD is a contractual artifact".to_string()));
        assert!(lines.contains(&"  Approve the PRD as the contract?".to_string()));
        assert!(lines.contains(&"  1) approve it".to_string()));
        assert!(lines.contains(&"  2) request changes".to_string()));
        assert!(lines.contains(&"  what you can do:".to_string()));
        assert!(lines.iter().any(|l| l.contains("/approve")));
        assert!(lines.iter().any(|l| l.contains("/revise prd <why>")));
        assert!(lines.iter().any(|l| l.contains("/auto workflow")));
        assert!(lines.contains(&"  type a command or an answer and press enter".to_string()));
    }

    #[test]
    fn user_pause_lists_resume_commands() {
        let overlay = PauseOverlay {
            reason: "reviewing".to_string(),
            question: None,
            options: vec![],
            decision: false,
        };
        let lines = overlay.lines();
        assert!(lines.contains(&"! relay is paused".to_string()));
        assert!(lines.iter().any(|l| l.contains("/resume")));
        assert!(!lines.iter().any(|l| l.contains("/approve this")));
    }
}
