//! The interactive terminal driver: the only place the CLI touches the
//! terminal.
//!
//! crossterm supplies the event stream and the alternate screen; ratatui
//! renders `relay-ui`'s screens; every domain action is a typed
//! [`TuiRequest`] mapped onto an application handler. A
//! [`TerminalGuard`] restores the terminal on normal exit, on error, and
//! through the panic hook. Non-interactive commands never enter this
//! module, so they cannot put the terminal into raw mode.

use std::io;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use relay_app::commands as app;
use relay_app::session::{ProjectSession, SessionOptions};
use relay_ui::work::keymap::{Key, Keymap};
use relay_ui::work::screens;
use relay_ui::work::terminal::TerminalGuard;
use relay_ui::work::{TuiOutcome, TuiRequest, ViewerState, WorkSnapshot, WorkTui};

use crate::dispatch::{EXIT_FAILURE, EXIT_OK};
use crate::output::OutputWriter;

/// Run the interactive interface over the project in `workspace`.
pub fn run(workspace: &Path, options: SessionOptions) -> i32 {
    let session = match ProjectSession::open(workspace, options) {
        Ok(session) => session,
        Err(error) => {
            // Before any terminal setup: plain stderr, no raw mode.
            let mut writer = OutputWriter::std(false);
            writer.error(&format!("relay: {}", error.message()));
            if matches!(error, relay_app::AppError::NotInitialized) {
                writer.error("Initialize the project first: `relay init --goal <text>`.");
            }
            return EXIT_FAILURE;
        }
    };
    if !session.is_initialized() {
        let mut writer = OutputWriter::std(false);
        writer.error("relay: no Relay project in this workspace");
        writer.error("Initialize one first: `relay init --goal <text>`.");
        return EXIT_FAILURE;
    }
    match run_loop(&session) {
        Ok(()) => EXIT_OK,
        Err(error) => {
            let mut writer = OutputWriter::std(false);
            writer.error(&format!("relay: terminal interface failed: {error}"));
            EXIT_FAILURE
        }
    }
}

fn run_loop(session: &ProjectSession) -> Result<(), Box<dyn std::error::Error>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.hide_cursor()?;

    let restore: relay_ui::work::terminal::RestoreFn = Arc::new(move || {
        // A fresh handle per call keeps the closure `Fn` (idempotent anyway).
        let mut stdout = io::stdout();
        let _ = disable_raw_mode();
        let _ = execute!(stdout, LeaveAlternateScreen);
        let _ = disable_raw_mode();
    });
    let guard = TerminalGuard::new(restore);

    let keymap = Keymap;
    let mut interface = WorkTui::new(build_snapshot(session));
    let outcome = event_loop(&mut terminal, &mut interface, session, &keymap);
    let _ = terminal.show_cursor();
    guard.restore_now();
    outcome
}

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    interface: &mut WorkTui,
    session: &ProjectSession,
    keymap: &Keymap,
) -> Result<(), Box<dyn std::error::Error>> {
    loop {
        screens::draw(terminal, interface)?;
        // Poll so a future watchdog can break the wait; blocking read would
        // be fine for the same result.
        if !crossterm::event::poll(Duration::from_millis(250))? {
            continue;
        }
        let Some(key) = read_key() else {
            continue;
        };
        let Some(action) = keymap.action(key) else {
            continue;
        };
        match interface.handle(action) {
            TuiOutcome::Quit => return Ok(()),
            TuiOutcome::Continue => {}
            TuiOutcome::Request(request) => {
                apply_request(session, interface, request);
                interface.set_snapshot(build_snapshot(session));
            }
        }
    }
}

/// Run one typed request through the application handlers, reporting the
/// outcome on the interface's status line. Domain decisions stay in the
/// application layer; this only translates.
fn apply_request(session: &ProjectSession, interface: &mut WorkTui, request: TuiRequest) {
    match request {
        TuiRequest::Refresh => interface.set_status("refreshed"),
        TuiRequest::InspectWork { work } => match app::work_detail(session, &work) {
            Ok(detail) => {
                interface.set_snapshot_detail(detail);
                interface.set_status(format!("inspected {work}"));
            }
            Err(error) => interface.set_status(format!("inspect failed: {}", error.message())),
        },
        TuiRequest::ViewArtifact { reference } => {
            let (artifact, version) = match reference.split_once('@') {
                Some((artifact, version)) => (artifact.to_string(), version.parse::<u32>().ok()),
                None => (reference.clone(), None),
            };
            match app::artifact_detail(session, &artifact, version, true) {
                Ok(detail) => {
                    let title = format!("{}@{}", detail.artifact, detail.version);
                    let markdown = detail.markdown.unwrap_or_else(|| {
                        "(no rendered view for this schema; the structured content is authoritative)"
                            .to_string()
                    });
                    interface.set_snapshot_viewer(ViewerState { title, markdown });
                    interface.set_status("viewing the rendered Markdown");
                }
                Err(error) => interface.set_status(format!("view failed: {}", error.message())),
            }
        }
        TuiRequest::ResolveDecision { decision, choice } => {
            match app::resolve_decision(
                session,
                &decision,
                &choice,
                Some("resolved from the relay terminal interface"),
            ) {
                Ok(view) => {
                    let completed = view.completed_work.len();
                    interface.set_status(format!(
                        "resolved {decision} with \"{choice}\" ({completed} work completed)"
                    ));
                }
                Err(error) => interface.set_status(format!("resolve failed: {}", error.message())),
            }
        }
    }
}

/// Build a fresh snapshot from the session's current derivation.
fn build_snapshot(session: &ProjectSession) -> WorkSnapshot {
    relay_ui::work::snapshot_from_session(session)
}

fn read_key() -> Option<Key> {
    match crossterm::event::read() {
        Ok(Event::Key(event)) => Some(key_of(event)),
        _ => None,
    }
}

fn key_of(event: KeyEvent) -> Key {
    match event.code {
        KeyCode::Char(character) => Key::Char(character),
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        KeyCode::Enter => Key::Enter,
        KeyCode::Tab => {
            if event.modifiers.contains(KeyModifiers::SHIFT) {
                Key::BackTab
            } else {
                Key::Tab
            }
        }
        KeyCode::Esc => Key::Esc,
        _ => Key::Other,
    }
}
