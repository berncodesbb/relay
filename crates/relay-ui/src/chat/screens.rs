//! The chat screens, rendered with ratatui.
//!
//! Every frame is a pure function of the [`ChatUi`] state: header
//! (workspace path + textual state word), the scrollable transcript
//! (durable entries, the ephemeral streaming tail, compact activity), the
//! prompt editor with a live cursor, and a footer with the keymap hint
//! and status. Overlays render as centered panes over the base. Narrow
//! terminals (≤ [`NARROW_WIDTH`] columns) hide the footer extras and
//! ephemeral activity; short terminals (below [`SHORT_HEIGHT`] rows) drop
//! the footer entirely — a small terminal degrades, never panics, and
//! state is never signaled by color alone.

use ratatui::Frame;
use ratatui::Terminal;
use ratatui::backend::Backend;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

use crate::chat::keymap::FOOTER;
use crate::chat::keymap::HELP_TEXT;
use crate::chat::transcript::bound_transcript;
use crate::chat::{ChatOverlay, ChatUi};

/// At or below this width the interface degrades: footer extras and
/// ephemeral activity lines are hidden.
pub const NARROW_WIDTH: usize = 60;

/// Below this height the footer is dropped; header, transcript, and
/// editor remain.
pub const SHORT_HEIGHT: u16 = 12;

/// The maximum rendered transcript lines before the ellipsis header
/// ("+ N earlier messages").
pub const TRANSCRIPT_MAX_LINES: usize = 200;

/// The maximum editor lines shown at once (older lines scroll under a
/// leading ellipsis marker).
pub const EDITOR_MAX_VISIBLE_LINES: usize = 6;

/// The maximum overlay pane width in columns.
pub const OVERLAY_MAX_WIDTH: usize = 72;

/// Render one frame of the chat interface onto any backend.
pub fn draw<B: Backend>(terminal: &mut Terminal<B>, ui: &ChatUi) -> std::io::Result<()> {
    terminal
        .draw(|frame| {
            let area = frame.area();
            draw_area(frame, area, ui);
        })
        .map(|_| ())
}

/// Render the chat interface into one area (usable by composite layouts
/// too).
pub fn draw_area(frame: &mut Frame, area: Rect, ui: &ChatUi) {
    let width = area.width.max(1) as usize;
    let narrow = width <= NARROW_WIDTH;
    let short = area.height < SHORT_HEIGHT;

    let editor_rows = ui.editor().line_count().clamp(1, EDITOR_MAX_VISIBLE_LINES) as u16;
    let footer_rows: u16 = u16::from(!short);
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(editor_rows + 2),
        Constraint::Length(footer_rows),
    ])
    .split(area);

    draw_header(frame, chunks[0], ui, width);
    draw_transcript(frame, chunks[1], ui, narrow);
    let editor_cursor = draw_editor(frame, chunks[2], ui);
    if !short {
        draw_footer(frame, chunks[3], ui, width, narrow);
    }

    if let Some(overlay) = ui.overlay() {
        draw_overlay(frame, area, &overlay, ui, width);
    } else if let Some((inner, cursor_y, cursor_x)) = editor_cursor {
        // Place the terminal cursor in the editor (approximate: the char
        // column, since the renderer does not measure glyph widths).
        let x = inner.x + cursor_x.min(inner.width.saturating_sub(1) as usize) as u16;
        let y = inner.y + cursor_y as u16;
        frame.set_cursor_position((x, y));
    }
}

/// Header: `Relay · <workspace path> · <state word>`; the path yields so
/// the state word always fits.
fn draw_header(frame: &mut Frame, area: Rect, ui: &ChatUi, width: usize) {
    let line = header_line(ui.workspace_path(), ui.state_label(), width);
    frame.render_widget(
        Paragraph::new(Line::styled(
            line,
            Style::new().add_modifier(Modifier::BOLD),
        )),
        area,
    );
}

fn header_line(path: &str, state: &str, width: usize) -> String {
    let full = format!("Relay · {path} · {state}");
    if full.chars().count() <= width {
        return full;
    }
    // Shrink the path, keeping the state visible.
    let tail = format!("… · {state}");
    let prefix = "Relay · ";
    let budget = width.saturating_sub(prefix.chars().count() + tail.chars().count());
    let path_chars: Vec<char> = path.chars().collect();
    let kept: String = path_chars[..budget.min(path_chars.len())].iter().collect();
    let composed = format!("{prefix}{kept}{tail}");
    let composed_chars: Vec<char> = composed.chars().collect();
    if composed_chars.len() <= width {
        composed
    } else {
        composed_chars[composed_chars.len() - width..]
            .iter()
            .collect()
    }
}

/// Transcript: durable entries plus ephemeral stream and activity,
/// scrolled by the offset (lines above the bottom), bounded by
/// [`TRANSCRIPT_MAX_LINES`].
fn draw_transcript(frame: &mut Frame, area: Rect, ui: &ChatUi, narrow: bool) {
    if area.height == 0 {
        return;
    }
    let inner = Block::bordered().inner(area);
    // one padding column inside the left border
    let body_width = inner.width.saturating_sub(1).max(1) as usize;
    let content = bound_transcript(
        &ui.transcript_lines(body_width, !narrow),
        TRANSCRIPT_MAX_LINES,
    );
    let total = content.len();
    let visible = inner.height as usize;
    let offset = ui.transcript_offset().min(total.saturating_sub(visible));
    let start = total.saturating_sub(visible + offset);
    let end = (start + visible).min(total);
    let title = if offset > 0 {
        format!(" transcript · ↑ {offset} ")
    } else {
        " transcript ".to_string()
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title(title);
    let lines: Vec<Line> = content[start..end]
        .iter()
        .map(|line| Line::raw(format!(" {line}")))
        .collect();
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// Editor: the `> ` prompt with the draft and a live cursor. Returns the
/// editor's inner area plus the cursor's inner-relative `(x, y)` (for
/// cursor placement) when it rendered.
fn draw_editor(frame: &mut Frame, area: Rect, ui: &ChatUi) -> Option<(Rect, usize, usize)> {
    if area.height == 0 {
        return None;
    }
    let title = if ui.cancelling() {
        " prompt — cancelling… (Ctrl-C again to force exit) "
    } else if ui.busy() {
        " prompt — working (Ctrl-C cancels) "
    } else {
        " prompt "
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title(title);
    let inner = block.inner(area);
    // one padding column plus the two-column prompt prefix
    let body_width = inner.width.saturating_sub(3).max(1) as usize;
    let (row, col) = ui.editor().cursor();
    let line_count = ui.editor().line_count();
    let lines = ui.editor().lines();
    let window = editor_window(line_count, row);
    let ellipsis = usize::from(window > 0);
    let text_rows = inner.height as usize - ellipsis.min(inner.height as usize);
    let mut rendered: Vec<Line> = Vec::new();
    if window > 0 {
        rendered.push(Line::styled(" …", Style::new().add_modifier(Modifier::DIM)));
    }
    for (index, line) in lines.iter().enumerate().skip(window).take(text_rows) {
        let prefix = if index == 0 { "> " } else { "  " };
        let text: String = line.iter().collect();
        rendered.push(Line::raw(format!(
            " {prefix}{}",
            bound_chars(&text, body_width)
        )));
    }
    frame.render_widget(Paragraph::new(rendered).block(block), area);
    // The char column approximates the display column (glyph widths are
    // not measured here): padding + prompt prefix + column.
    let cursor_x = 1 + 2 + col;
    let cursor_y = ellipsis + (row - window);
    Some((inner, cursor_x, cursor_y))
}

/// The first draft line to show so the cursor line stays visible within
/// the cap; older lines are hidden under a leading ellipsis.
fn editor_window(line_count: usize, cursor_row: usize) -> usize {
    if line_count <= EDITOR_MAX_VISIBLE_LINES {
        return 0;
    }
    let show = EDITOR_MAX_VISIBLE_LINES - 1; // one row goes to the ellipsis
    let start = line_count.saturating_sub(show);
    start.min(cursor_row)
}

/// Footer: the keymap hint plus status; when both do not fit, the
/// actionable status wins over the static hint; narrow terminals keep
/// only the status (or a minimal `/help` pointer).
fn draw_footer(frame: &mut Frame, area: Rect, ui: &ChatUi, width: usize, narrow: bool) {
    if area.height == 0 {
        return;
    }
    let status = ui.status();
    let combined = format!("{FOOTER} · {status}");
    let text = if narrow {
        if status.is_empty() {
            "/help".to_string()
        } else {
            status.to_string()
        }
    } else if status.is_empty() {
        FOOTER.to_string()
    } else if combined.chars().count() <= width {
        combined
    } else {
        // the status is actionable; the hint is static — the status wins
        status.to_string()
    };
    frame.render_widget(
        Paragraph::new(Line::styled(
            bound_chars(&text, width),
            Style::new().add_modifier(Modifier::DIM),
        )),
        area,
    );
}

/// Overlays: a centered, bordered pane over a cleared base.
fn draw_overlay(frame: &mut Frame, area: Rect, overlay: &ChatOverlay, ui: &ChatUi, width: usize) {
    let (title, lines) = overlay_lines(overlay, ui, width);
    let pane_width = (width.saturating_sub(4)).clamp(8, OVERLAY_MAX_WIDTH) as u16;
    let pane_height = (lines.len() as u16 + 2).min(area.height.saturating_sub(1).max(3));
    let pane = centered(area, pane_width, pane_height);
    // one padding column inside the left border
    let inner_width = pane.width.saturating_sub(3).max(1) as usize;
    let inner_height = pane.height.saturating_sub(2) as usize;
    let mut content = lines;
    if content.len() > inner_height {
        let hidden = content.len() - (inner_height - 1);
        content.truncate(inner_height - 1);
        content.push(format!("… ({hidden} more lines)"));
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title(format!(" {title} "));
    let rendered: Vec<Line> = content
        .iter()
        .map(|line| Line::raw(format!(" {}", bound_chars(line, inner_width))))
        .collect();
    frame.render_widget(Clear, pane);
    frame.render_widget(Paragraph::new(rendered).block(block), pane);
}

/// The overlay's title and content lines (unbounded; the pane caps).
fn overlay_lines(overlay: &ChatOverlay, ui: &ChatUi, width: usize) -> (&'static str, Vec<String>) {
    match overlay {
        ChatOverlay::Help => ("help", HELP_TEXT.lines().map(str::to_string).collect()),
        ChatOverlay::Status => ("status", status_lines(ui, width)),
        ChatOverlay::Work => ("work", work_lines(ui, width)),
        ChatOverlay::Decisions => ("decisions", decisions_lines(ui, width)),
        ChatOverlay::Detail(id) => ("work detail", detail_lines(ui, id, width)),
        ChatOverlay::Artifact(reference) => ("artifact", artifact_lines(ui, reference, width)),
    }
}

fn status_lines(ui: &ChatUi, width: usize) -> Vec<String> {
    let view = ui.view();
    let summary = &ui.work().summary;
    let conversation = view
        .conversation
        .as_ref()
        .map(|id| id.to_string())
        .unwrap_or_else(|| "(none yet — the first prompt starts one)".to_string());
    let goal = view
        .goal
        .clone()
        .unwrap_or_else(|| "(none declared)".to_string());
    let counts = &summary.counts;
    vec![
        bound_chars(&format!("workspace: {}", ui.workspace_path()), width),
        format!("conversation: {conversation}"),
        format!("goal: {goal}"),
        format!("transcript entries: {}", view.entries.len()),
        format!("interrupted turns: {}", view.interrupted_turns),
        format!("open decisions: {}", view.open_decisions.len()),
        format!(
            "work: ready {} · blocked {} · active {} · completed {}",
            counts.ready, counts.blocked, counts.active_attempts, counts.completed
        ),
    ]
}

fn work_lines(ui: &ChatUi, width: usize) -> Vec<String> {
    let snapshot = ui.work();
    let mut lines = vec!["ready:".to_string()];
    if snapshot.ready.works.is_empty() {
        lines.push("  (no ready work)".to_string());
    }
    for work in &snapshot.ready.works {
        lines.push(bound_chars(
            &format!("  {} [{}] {}", work.id, work.kind, work.status),
            width,
        ));
        lines.push(bound_chars(&format!("      {}", work.objective), width));
    }
    lines.push("blocked:".to_string());
    if snapshot.blocked.works.is_empty() {
        lines.push("  (no blocked work)".to_string());
    }
    for work in &snapshot.blocked.works {
        lines.push(bound_chars(
            &format!(
                "  {} [{}] {} — {}",
                work.id, work.kind, work.status, work.explanation
            ),
            width,
        ));
    }
    lines
}

fn decisions_lines(ui: &ChatUi, width: usize) -> Vec<String> {
    let decisions = &ui.view().open_decisions;
    if decisions.is_empty() {
        return vec!["(no open decisions)".to_string()];
    }
    let mut lines = Vec::new();
    for decision in decisions {
        lines.push(bound_chars(
            &format!("{}: {}", decision.decision, decision.question),
            width,
        ));
        if decision.options.is_empty() {
            lines.push("  (free-form: answer in the prompt)".to_string());
        }
        for option in &decision.options {
            lines.push(bound_chars(&format!("  - {option}"), width));
        }
    }
    lines
}

fn detail_lines(ui: &ChatUi, id: &str, width: usize) -> Vec<String> {
    let Some(detail) = ui.work().detail.as_ref() else {
        return vec![format!(
            "(detail for {id} is not loaded — it arrives with the next work refresh)"
        )];
    };
    if detail.entry.id != id {
        return vec![format!(
            "(detail for {id} is not loaded — showing {} is not available)",
            detail.entry.id
        )];
    }
    let entry = &detail.entry;
    let mut lines = vec![bound_chars(
        &format!(
            "{} [{}] {} · {} · {}",
            entry.id, entry.phase, entry.kind, entry.executor, entry.status
        ),
        width,
    )];
    lines.push(bound_chars(
        &format!("objective: {}", entry.objective),
        width,
    ));
    lines.push(bound_chars(
        &format!("completion: {}", detail.completion),
        width,
    ));
    if !detail.dependencies.is_empty() {
        lines.push("depends on:".to_string());
        for dependency in &detail.dependencies {
            lines.push(bound_chars(
                &format!(
                    "  {} ({}) — {}",
                    dependency.to, dependency.kind, dependency.status
                ),
                width,
            ));
        }
    }
    if !detail.dependents.is_empty() {
        lines.push(bound_chars(
            &format!("dependents: {}", detail.dependents.join(", ")),
            width,
        ));
    }
    if !detail.inputs.is_empty() {
        lines.push(bound_chars(
            &format!("inputs: {}", detail.inputs.join(", ")),
            width,
        ));
    }
    if !detail.attempts.is_empty() {
        lines.push("attempts:".to_string());
        for attempt in &detail.attempts {
            match &attempt.detail {
                Some(text) => lines.push(bound_chars(
                    &format!("  {} {} — {}", attempt.id, attempt.conclusion, text),
                    width,
                )),
                None => lines.push(bound_chars(
                    &format!("  {} {}", attempt.id, attempt.conclusion),
                    width,
                )),
            }
        }
    }
    if !detail.evidence.is_empty() {
        lines.push("evidence:".to_string());
        for evidence in &detail.evidence {
            lines.push(bound_chars(
                &format!(
                    "  [{} · {}] {}",
                    evidence.kind, evidence.outcome, evidence.summary
                ),
                width,
            ));
        }
    }
    lines.push(String::new());
    lines.push(bound_chars(&format!("why: {}", entry.explanation), width));
    lines
}

fn artifact_lines(ui: &ChatUi, reference: &str, width: usize) -> Vec<String> {
    let Some(viewer) = ui.work().viewer.as_ref() else {
        return vec![format!(
            "(artifact {reference} is not loaded — it arrives with the next work refresh)"
        )];
    };
    let mut lines = vec![viewer.title.clone(), String::new()];
    lines.extend(viewer.markdown.lines().map(|line| bound_chars(line, width)));
    lines
}

/// A centered rect of at most `width` × `height` inside `area`.
fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
}

/// Truncate one line to `max` chars (char-safe), with an ellipsis.
fn bound_chars(text: &str, max: usize) -> String {
    let limit = max.max(4);
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= limit {
        return text.to_string();
    }
    let kept: String = chars[..limit - 1].iter().collect();
    format!("{kept}…")
}
