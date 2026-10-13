//! TestBackend snapshot tests for the terminal interface: narrow-terminal
//! handling (80 and 40 columns), the pause overlay, the focused panels, and
//! long-transcript truncation. Rendering is exercised through the same
//! [`super::components::draw`] the CLI runner uses — no TTY required.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use super::components::{RenderInputs, draw};
use super::view_model::{ControllerEvent, Panel, ToolPhase, TuiAction, ViewModel};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use relay_core::fakes::{full_pipeline_approved, replay_events};

/// Flatten a rendered frame to trimmed text lines.
fn frame_text(terminal: &Terminal<TestBackend>) -> String {
    let buffer = terminal.backend().buffer();
    let width = buffer.area.width as usize;
    buffer
        .content
        .chunks(width)
        .map(|row| {
            row.iter()
                .map(ratatui::buffer::Cell::symbol)
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<String>>()
        .join("\n")
}

fn render(width: u16, height: u16, model: &ViewModel, inputs: &RenderInputs) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| draw(frame, model.state(), inputs))
        .unwrap();
    frame_text(&terminal)
}

fn pipeline_inputs<'a>(
    snapshot: &'a relay_core::ProjectState,
    outcome_seen_at: Option<&'a str>,
) -> RenderInputs<'a> {
    RenderInputs {
        snapshot,
        uninitialized: snapshot.project_id().is_empty(),
        outcome_seen_at,
        input_value: "",
    }
}

#[test]
fn renders_header_transcript_prompt_and_footer_at_80_columns() {
    let mut model = ViewModel::new();
    model.dispatch(TuiAction::ControllerEvent(ControllerEvent::Delta {
        text: "Drafting the brief.".to_string(),
    }));
    model.dispatch(TuiAction::ControllerEvent(ControllerEvent::Mode {
        mode: "specification".to_string(),
        previous: Some("discovery".to_string()),
        reason: "brief approved".to_string(),
    }));
    let events = full_pipeline_approved();
    let state = replay_events(&events);
    let text = render(80, 24, &model, &pipeline_inputs(&state, None));
    assert!(text.contains("relay — Delivery [7/7] · auto:workflow"));
    assert!(text.contains("Ship a workflow CLI that preserves product intent"));
    assert!(text.contains("relay> Drafting the brief."));
    assert!(text.contains("mode discovery -> specification — brief approved"));
    assert!(text.contains("›"));
    assert!(text.contains("/help for commands · enter to send · ctrl-c exits"));
}

#[test]
fn truncates_the_goal_instead_of_crashing_at_40_columns() {
    let mut model = ViewModel::new();
    let events = vec![relay_core::fakes::initialized_default()];
    let state = replay_events(&events);
    model.dispatch(TuiAction::Resize(40));
    let text = render(40, 24, &model, &pipeline_inputs(&state, None));
    // The 39-column header fits at 40 columns; the 47-character goal line
    // truncates to columns - 4 = 36 (35 kept characters plus ellipsis).
    assert!(text.contains("relay — Discovery [1/7] · auto:workflow"));
    assert!(text.contains("Ship a workflow CLI that preserves …"));
    assert!(!text.contains("Ship a workflow CLI that preserves product intent"));
}

/// Replay a committed fixture case by name (the paused-state overlay test
/// needs a real `workflow.paused` fold).
fn replay_fixture_case(name: &str) -> relay_core::ProjectState {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/cases")
        .join(name)
        .join("events.jsonl");
    let text = std::fs::read_to_string(path).unwrap();
    let mut events = Vec::new();
    for line in text.split('\n').filter(|line| !line.is_empty()) {
        let envelope: relay_core::EventEnvelope = serde_json::from_str(line).unwrap();
        events.push(relay_core::events::validate_stored_event(&envelope).unwrap());
    }
    relay_core::replay_project(&events).unwrap()
}

#[test]
fn shows_no_pause_overlay_when_the_workflow_is_running() {
    let events = relay_core::fakes::full_pipeline_approved();
    let state = replay_events(&events);
    let mut model = ViewModel::new();
    model.dispatch(TuiAction::Resize(80));
    let text = render(80, 30, &model, &pipeline_inputs(&state, None));
    assert!(!text.contains("relay is paused"));
    assert!(!text.contains("relay needs your decision"));
}

#[test]
fn pause_overlay_renders_for_a_paused_state() {
    let state = replay_fixture_case("workflow-paused-resumed");
    // The fixture resumes at the end; fold the resume away by replaying only
    // the prefix before the workflow.resumed event.
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/cases/workflow-paused-resumed/events.jsonl");
    let text = std::fs::read_to_string(path).unwrap();
    let mut prefix = Vec::new();
    for line in text.split('\n').filter(|line| !line.is_empty()) {
        let envelope: relay_core::EventEnvelope = serde_json::from_str(line).unwrap();
        if envelope.event_type == "workflow.resumed" {
            break;
        }
        prefix.push(relay_core::events::validate_stored_event(&envelope).unwrap());
    }
    let paused = relay_core::replay_project(&prefix).unwrap();
    assert!(paused.paused().is_some());
    assert!(!state.goal().is_empty());

    let mut model = ViewModel::new();
    model.dispatch(TuiAction::Resize(80));
    let text = render(80, 34, &model, &pipeline_inputs(&paused, None));
    assert!(text.contains("! relay is paused"));
    assert!(text.contains("type a command or an answer and press enter"));
    assert!(text.contains("/resume"));
}

#[test]
fn renders_the_status_panel_bounded_to_fourteen_lines() {
    let events = full_pipeline_approved();
    let state = replay_events(&events);
    let view = crate::views::build_status_view(&state, &events, None);
    let content = crate::tui::view_text::render_view_text(
        &crate::tui::view_text::loose_status(&view),
        &crate::tui::view_text::STATUS_KEY_ORDER,
    );
    let mut model = ViewModel::new();
    model.dispatch(TuiAction::Panel {
        panel: Panel::Status,
        content: Some(content),
    });
    model.dispatch(TuiAction::Resize(80));
    let text = render(80, 40, &model, &pipeline_inputs(&state, None));
    assert!(text.contains("status"));
    assert!(text.contains("Goal: Ship a workflow CLI that preserves product intent"));
    assert!(text.contains("Mode: delivery"));
    // The artifact catalog overflows the 14-line panel cap; the marker points
    // at `relay inspect` exactly like the reference.
    assert!(text.contains("more lines, see relay inspect"));
}

#[test]
fn renders_the_help_panel_with_the_command_reference() {
    let events = vec![relay_core::fakes::initialized_default()];
    let state = replay_events(&events);
    let mut model = ViewModel::new();
    model.dispatch(TuiAction::Panel {
        panel: Panel::PauseHelp,
        content: Some(crate::tui::view_text::HELP_TEXT.to_string()),
    });
    model.dispatch(TuiAction::Resize(80));
    let text = render(80, 40, &model, &pipeline_inputs(&state, None));
    assert!(text.contains("help"));
    assert!(text.contains("/inspect <artifact>"));
    assert!(text.contains("/revise <kind> <why>"));
    assert!(text.contains("/quit"));
}

#[test]
fn shows_working_while_a_controller_operation_is_in_flight() {
    let events = vec![relay_core::fakes::initialized_default()];
    let state = replay_events(&events);
    let mut model = ViewModel::new();
    model.dispatch(TuiAction::Busy(true));
    let text = render(80, 24, &model, &pipeline_inputs(&state, None));
    assert!(text.contains("› working… (ctrl-c to cancel)"));
    model.dispatch(TuiAction::Busy(false));
    let text = render(80, 24, &model, &pipeline_inputs(&state, None));
    assert!(!text.contains("working…"));
}

#[test]
fn truncates_a_long_transcript_to_the_visible_tail() {
    let events = vec![relay_core::fakes::initialized_default()];
    let state = replay_events(&events);
    let mut model = ViewModel::new();
    for index in 0..80 {
        model.dispatch(TuiAction::ControllerEvent(ControllerEvent::Notice {
            message: format!("notice {index:03}"),
        }));
    }
    let text = render(80, 24, &model, &pipeline_inputs(&state, None));
    // The oldest notices scrolled out of the fixed-height transcript.
    assert!(!text.contains("notice 000"));
    assert!(!text.contains("notice 010"));
    assert!(text.contains("notice 079"));
}

#[test]
fn shows_the_uninitialized_hint_when_no_project_exists() {
    let model = ViewModel::new();
    let empty = relay_core::create_empty_project_state();
    let inputs = RenderInputs {
        snapshot: &empty,
        uninitialized: true,
        outcome_seen_at: None,
        input_value: "",
    };
    let text = render(80, 24, &model, &inputs);
    assert!(text.contains("no project yet — type a goal to start"));
    assert!(text.contains("(no activity yet)"));
}

#[test]
fn renders_tool_entry_prefixes_in_the_transcript() {
    let mut model = ViewModel::new();
    model.dispatch(TuiAction::ControllerEvent(ControllerEvent::Tool {
        tool: "read-file".to_string(),
        capability: Some("read".to_string()),
        phase: ToolPhase::Start,
        ok: None,
        summary: None,
    }));
    model.dispatch(TuiAction::ControllerEvent(ControllerEvent::Tool {
        tool: "read-file".to_string(),
        capability: Some("read".to_string()),
        phase: ToolPhase::End,
        ok: Some(true),
        summary: Some("CLAUDE.md".to_string()),
    }));
    model.dispatch(TuiAction::ControllerEvent(ControllerEvent::Tool {
        tool: "run-command".to_string(),
        capability: Some("execute".to_string()),
        phase: ToolPhase::End,
        ok: Some(false),
        summary: Some("exit 1".to_string()),
    }));
    let events = vec![relay_core::fakes::initialized_default()];
    let state = replay_events(&events);
    let text = render(80, 24, &model, &pipeline_inputs(&state, None));
    assert!(text.contains("tool+ read-file"));
    assert!(text.contains("tool✔ read-file — CLAUDE.md"));
    assert!(text.contains("tool✘ run-command — exit 1"));
}
