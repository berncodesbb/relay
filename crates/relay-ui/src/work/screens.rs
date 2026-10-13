//! The five work-graph screens plus the Markdown viewer, rendered with
//! ratatui.
//!
//! Every screen is a pure function of the [`WorkTui`] state: an immutable
//! snapshot of application view models plus a cursor. Layout: a two-line
//! header (goal, project/revision), the screen body, and a one-line footer
//! with the keymap hint. Long bodies cap with an explicit marker instead of
//! scrolling silently; long lines truncate with an ellipsis — a narrow
//! terminal degrades, never panics.

use ratatui::Terminal;
use ratatui::backend::Backend;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};

use crate::tui::components::{cap_lines, truncate_text};
use crate::work::keymap::{FOOTER, HELP_TEXT};
use crate::work::{Screen, WorkTui};

/// The maximum body lines before the truncation marker.
pub const MAX_BODY_LINES: usize = 40;

/// Render one frame of the interface onto any backend.
pub fn draw<B: Backend>(terminal: &mut Terminal<B>, tui: &WorkTui) -> std::io::Result<()> {
    terminal
        .draw(|frame| {
            let area = frame.area();
            draw_area(frame, area, tui);
        })
        .map(|_| ())
}

/// Render the interface into one area (usable by composite layouts too).
pub fn draw_area(frame: &mut ratatui::Frame, area: Rect, tui: &WorkTui) {
    let width = area.width.max(1) as usize;
    let chunks = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .split(area);

    // Header.
    let snapshot = tui.snapshot();
    let goal = snapshot
        .summary
        .goal
        .as_ref()
        .map(|goal| goal.outcome.as_str())
        .unwrap_or("(no goal declared)");
    let header = format!(
        "relay · {} · rev {} · {}",
        snapshot
            .summary
            .project_id
            .as_deref()
            .unwrap_or("(no project)"),
        snapshot.summary.revision,
        snapshot.summary.completion
    );
    let header_lines = vec![
        Line::styled(
            truncate_text(&header, width),
            Style::new().add_modifier(Modifier::BOLD),
        ),
        Line::styled(
            truncate_text(goal, width.saturating_sub(2)),
            Style::new().add_modifier(Modifier::DIM),
        ),
    ];
    frame.render_widget(Paragraph::new(header_lines), chunks[0]);

    // Body.
    let body_lines = if tui.help_visible() {
        help_lines()
    } else {
        body_lines(tui, width)
    };
    let capped = cap_lines(&body_lines, MAX_BODY_LINES);
    let title = if tui.help_visible() {
        " help "
    } else {
        tui.screen().title()
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title(format!(" {title} "));
    let content: Vec<Line> = capped
        .into_iter()
        .map(|line| {
            if line.is_empty() {
                Line::raw(" ")
            } else {
                Line::raw(line)
            }
        })
        .collect();
    frame.render_widget(Paragraph::new(content).block(block), chunks[1]);

    // Footer: the keymap hint, then the status line when it fits.
    let status = tui.status();
    let footer = if width > FOOTER.len() + 4 && !status.is_empty() {
        format!("{FOOTER} · {status}")
    } else {
        FOOTER.to_string()
    };
    frame.render_widget(
        Paragraph::new(Line::styled(
            truncate_text(&footer, width),
            Style::new().add_modifier(Modifier::DIM),
        )),
        chunks[2],
    );
}

fn help_lines() -> Vec<String> {
    HELP_TEXT.lines().map(str::to_string).collect()
}

fn body_lines(tui: &WorkTui, width: usize) -> Vec<String> {
    let snapshot = tui.snapshot();
    let width = width.saturating_sub(4);
    match tui.screen() {
        Screen::Summary => summary_lines(snapshot, width),
        Screen::Ready => list_lines(&snapshot.ready.works, tui.cursor(), width, true),
        Screen::Blocked => list_lines(&snapshot.blocked.works, tui.cursor(), width, false),
        Screen::Decisions => decisions_lines(tui, width),
        Screen::Detail => detail_lines(snapshot, width),
        Screen::Viewer => viewer_lines(snapshot, width),
    }
}

/// Truncate one body line to the available body width.
fn bound(width: usize, line: impl AsRef<str>) -> String {
    truncate_text(line.as_ref(), width)
}

fn summary_lines(snapshot: &crate::work::WorkSnapshot, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for phase in &snapshot.summary.phases {
        lines.push(bound(
            width,
            format!(
                "{:<14} {}/{} done",
                phase.phase, phase.completed, phase.total
            ),
        ));
    }
    let counts = &snapshot.summary.counts;
    lines.push(String::new());
    lines.push(bound(
        width,
        format!(
            "ready {} · blocked {} · awaiting you {} · active {} · completed {} · invalid {}",
            counts.ready,
            counts.blocked,
            counts.awaiting_human,
            counts.active_attempts,
            counts.completed,
            counts.invalidated
        ),
    ));
    if !snapshot.summary.open_decisions.is_empty() {
        lines.push(bound(
            width,
            format!(
                "open decisions: {}",
                snapshot.summary.open_decisions.join(", ")
            ),
        ));
    }
    lines.push(String::new());
    lines.push(bound(
        width,
        format!(
            "recommendation: {}",
            snapshot.summary.recommendation.rationale
        ),
    ));
    lines.push(bound(
        width,
        format!("next: {}", snapshot.summary.recommendation.next.detail),
    ));
    lines
}

fn list_lines(
    works: &[relay_app::views::WorkEntryView],
    cursor: usize,
    width: usize,
    with_explanation: bool,
) -> Vec<String> {
    let mut lines = Vec::new();
    if works.is_empty() {
        lines.push("(nothing here)".to_string());
        return lines;
    }
    for (index, work) in works.iter().enumerate() {
        let marker = if index == cursor { "> " } else { "  " };
        lines.push(bound(
            width,
            format!(
                "{marker}{} [{}] {} — {}",
                work.id, work.phase, work.kind, work.status
            ),
        ));
        lines.push(bound(width, format!("     {}", work.objective)));
        if with_explanation {
            lines.push(bound(width, format!("     {}", work.explanation)));
        }
    }
    lines
}

fn decisions_lines(tui: &WorkTui, width: usize) -> Vec<String> {
    let snapshot = tui.snapshot();
    let mut lines = Vec::new();
    if snapshot.decisions.decisions.is_empty() {
        lines.push("(no decisions)".to_string());
        return lines;
    }
    let rows = tui.decision_rows();
    for decision in &snapshot.decisions.decisions {
        lines.push(bound(
            width,
            format!(
                "{} [{}] {}",
                decision.id, decision.status, decision.question
            ),
        ));
        if decision.options.is_empty() {
            lines.push(bound(
                width,
                "     (free-form: resolve with `relay decisions resolve`)",
            ));
        }
        for option in &decision.options {
            let selected = rows
                .get(tui.cursor())
                .is_some_and(|row| row.decision == decision.id && row.choice == option.label);
            let marker = if selected { "> " } else { "  " };
            lines.push(bound(
                width,
                format!("   {marker}{} — {}", option.label, option.description),
            ));
        }
        if let Some(choice) = &decision.choice {
            lines.push(bound(width, format!("     resolved: {choice}")));
        }
    }
    lines
}

fn detail_lines(snapshot: &crate::work::WorkSnapshot, width: usize) -> Vec<String> {
    let Some(detail) = &snapshot.detail else {
        return vec!["(no work item open; press enter on ready or blocked work)".to_string()];
    };
    let entry = &detail.entry;
    let mut lines = Vec::new();
    lines.push(bound(
        width,
        format!(
            "{} [{}] {} · {} · {}",
            entry.id, entry.phase, entry.kind, entry.executor, entry.status
        ),
    ));
    lines.push(bound(width, format!("objective: {}", entry.objective)));
    lines.push(bound(width, format!("completion: {}", detail.completion)));
    if !detail.dependencies.is_empty() {
        lines.push(bound(width, "depends on:"));
        for dependency in &detail.dependencies {
            lines.push(bound(
                width,
                format!(
                    "  {} ({}) — {}",
                    dependency.to, dependency.kind, dependency.status
                ),
            ));
        }
    }
    if !detail.dependents.is_empty() {
        lines.push(bound(
            width,
            format!("dependents: {}", detail.dependents.join(", ")),
        ));
    }
    if !detail.inputs.is_empty() {
        lines.push(bound(
            width,
            format!("inputs: {}", detail.inputs.join(", ")),
        ));
    }
    if !detail.attempts.is_empty() {
        lines.push(bound(width, "attempts:"));
        for attempt in &detail.attempts {
            match &attempt.detail {
                Some(text) => lines.push(bound(
                    width,
                    format!("  {} {} — {}", attempt.id, attempt.conclusion, text),
                )),
                None => lines.push(bound(
                    width,
                    format!("  {} {}", attempt.id, attempt.conclusion),
                )),
            }
        }
    }
    if !detail.evidence.is_empty() {
        lines.push(bound(width, "evidence:"));
        for evidence in &detail.evidence {
            lines.push(bound(
                width,
                format!(
                    "  {} [{} · {}] {}",
                    evidence.id, evidence.kind, evidence.outcome, evidence.summary
                ),
            ));
        }
    }
    lines.push(String::new());
    lines.push(bound(width, format!("why: {}", entry.explanation)));
    lines
}

fn viewer_lines(snapshot: &crate::work::WorkSnapshot, width: usize) -> Vec<String> {
    let Some(viewer) = &snapshot.viewer else {
        return vec!["(no view open)".to_string()];
    };
    let mut lines = vec![bound(width, viewer.title.clone()), String::new()];
    lines.extend(viewer.markdown.lines().map(|line| bound(width, line)));
    lines
}
