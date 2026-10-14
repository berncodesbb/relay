//! The interactive chat driver: the only place the CLI touches the
//! terminal.
//!
//! crossterm supplies the event stream and the alternate screen; ratatui
//! renders `relay-ui`'s chat screens; every domain action is a typed
//! [`ChatUiRequest`] the driver maps onto the application conversation
//! service. A [`TerminalGuard`] restores the terminal on normal exit, on
//! error, and through the panic hook.
//!
//! Architecture:
//!
//! - **Lazy session** — nothing is opened (let alone created) before the
//!   first submitted prompt. A fresh workspace renders a welcome view
//!   hand-built from an empty [`ChatView`]; the first prompt runs
//!   `ProjectSession::open_or_create` and the turn service appends the
//!   project/conversation/user-message facts in one batch. An existing
//!   workspace (detected by the store manifest, never by opening) resumes
//!   its latest conversation immediately.
//! - **Worker thread + channels** — each turn runs on its own thread,
//!   entirely outside the event loop; ephemeral `ConversationEvent`s and
//!   the terminal result arrive over `std::sync::mpsc` channels the loop
//!   pumps every tick. No async runtime, no extra dependencies.
//! - **Cancellation** — the UI's Ctrl-C handling asks the driver to cancel
//!   the running turn's [`CancelToken`]; a second Ctrl-C force-quits,
//!   giving the worker a short grace window to land its terminal fact
//!   before the terminal is restored regardless.
//!
//! The decode (`chat_key_of`) and request planning (`plan_request`) layers
//! are pure and tested in-process, so the raw event loop stays a thin shell
//! that never needs a live terminal in tests.

use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::RecvTimeoutError;
use std::sync::mpsc::TryRecvError;
use std::sync::mpsc::channel;
use std::thread::JoinHandle;
use std::time::Duration;
use std::time::Instant;

use crossterm::event::DisableBracketedPaste;
use crossterm::event::EnableBracketedPaste;
use crossterm::event::Event as TermEvent;
use crossterm::execute;
use crossterm::terminal::EnterAlternateScreen;
use crossterm::terminal::LeaveAlternateScreen;
use crossterm::terminal::disable_raw_mode;
use crossterm::terminal::enable_raw_mode;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use relay_app::AppError;
use relay_app::commands as app;
use relay_app::conversation::ConversationEvent;
use relay_app::conversation::ConversationLimits;
use relay_app::conversation::ConversationTurnResult;
use relay_app::conversation::TurnStatus;
use relay_app::conversation::start_turn;
use relay_app::conversation_views::ChatView;
use relay_app::conversation_views::chat_view;
use relay_app::session::ProjectSession;
use relay_app::session::SessionOptions;
use relay_providers::contract::CancelToken;
use relay_providers::contract::ModelProvider;
use relay_ui::chat::ChatUi;
use relay_ui::chat::ChatUiOutcome;
use relay_ui::chat::ChatUiRequest;
use relay_ui::chat::keymap::ChatKey;
use relay_ui::chat::screens;
use relay_ui::work::WorkSnapshot;
use relay_ui::work::terminal::RestoreFn;
use relay_ui::work::terminal::TerminalGuard;

use crate::dispatch::CliDeps;
use crate::dispatch::conversational_provider;
use crate::dispatch::{EXIT_FAILURE, EXIT_OK};
use crate::output::OutputWriter;

/// How many transcript entries the durable chat view carries.
const VIEW_ENTRY_LIMIT: usize = 200;

/// Terminal event poll slice while a turn runs: keeps streamed text
/// responsive without busy-spinning.
const POLL_WHILE_RUNNING: Duration = Duration::from_millis(40);

/// Terminal event poll slice while idle (nothing streams; keys stay snappy).
const POLL_WHILE_IDLE: Duration = Duration::from_millis(200);

/// How long a forced quit waits for the running turn to land its terminal
/// fact before restoring the terminal anyway (the worker is then detached;
/// the store's crash recovery owns any torn tail).
const FORCE_QUIT_GRACE: Duration = Duration::from_secs(2);

/// The fact store's manifest, relative to the workspace root. Probing it
/// (rather than opening the store) is how the driver detects an existing
/// project without creating anything on a fresh workspace. Mirrors
/// `relay-store`'s documented `.relay-graph` layout.
const FACT_STORE_MANIFEST: &str = ".relay-graph/manifest.json";

/// Run the interactive chat interface over `workspace`.
///
/// Interactive only — dispatch guarantees a fully interactive terminal
/// before calling. Returns the process exit code; every path (quit,
/// provider failure, cancellation, forced exit, panic) restores the
/// terminal through the [`TerminalGuard`].
pub fn run_chat(workspace: &Path, deps: &CliDeps, initial_prompt: Option<&str>) -> i32 {
    // Resolve the provider before any terminal setup so a routing failure
    // is a plain stderr message, never a raw-mode surprise.
    let (provider, model) = match conversational_provider(&deps.runtime) {
        Ok(resolved) => resolved,
        Err(error) => {
            let mut writer = OutputWriter::std(false);
            writer.error(&format!("relay: {}", error.message()));
            return EXIT_FAILURE;
        }
    };
    // An existing project resumes its latest conversation; a fresh
    // workspace stays untouched until the first prompt (the manifest probe
    // never opens or creates the store).
    let session: Option<Arc<ProjectSession>> = if workspace.join(FACT_STORE_MANIFEST).is_file() {
        match ProjectSession::open(workspace, deps.session_options.clone()) {
            Ok(session) => Some(Arc::new(session)),
            Err(error) => {
                let mut writer = OutputWriter::std(false);
                writer.error(&format!("relay: {}", error.message()));
                if matches!(error, AppError::NotInitialized) {
                    writer.error("Initialize the project first: `relay init --goal <text>`.");
                }
                return EXIT_FAILURE;
            }
        }
    } else {
        None
    };
    let (view, work) = match &session {
        Some(session) => (
            chat_view(session, VIEW_ENTRY_LIMIT),
            relay_ui::work::snapshot_from_session(session),
        ),
        None => (welcome_view(), empty_work_snapshot()),
    };
    let mut ui = ChatUi::new(view, work.clone(), workspace.display().to_string());
    let mut state = DriverState {
        workspace: workspace.to_path_buf(),
        session_options: deps.session_options.clone(),
        session,
        provider,
        model,
        running: None,
        last_work: work,
    };
    // The prompt-form invocation submits once, before the first draw.
    if let Some(prompt) = initial_prompt
        && let ChatUiOutcome::Request(request) = ui.handle_submit(prompt.to_string())
        && apply_request(&mut state, &mut ui, request)
    {
        return EXIT_OK;
    }
    match run_loop(&mut state, &mut ui) {
        Ok(()) => EXIT_OK,
        Err(error) => {
            let mut writer = OutputWriter::std(false);
            writer.error(&format!("relay: terminal interface failed: {error}"));
            EXIT_FAILURE
        }
    }
}

/// The welcome view for a workspace with no conversation yet: an empty,
/// fully hand-built snapshot — no session, no store, no files.
fn welcome_view() -> ChatView {
    ChatView {
        conversation: None,
        entries: Vec::new(),
        open_decisions: Vec::new(),
        interrupted_turns: 0,
        goal: None,
    }
}

/// The work snapshot for a workspace with no project: derived from an
/// empty graph, exactly like a session snapshot but without a session.
fn empty_work_snapshot() -> WorkSnapshot {
    let graph = relay_domain::ProjectGraph::default();
    let evaluation = relay_engine::evaluate(&graph);
    let catalog = relay_workflows::WorkflowCatalog::builtin();
    WorkSnapshot::new(
        relay_app::views::summary_view(&graph, &evaluation, &catalog),
        relay_app::views::ready_view(&graph, &evaluation, &catalog),
        relay_app::views::blocked_view(&graph, &evaluation, &catalog),
        relay_app::views::decision_list_view(&graph),
    )
}

/// What the driver does with one interface request. Pure translation —
/// the mapping is pinned by tests, the executor functions own all IO.
#[derive(Debug, Clone, PartialEq, Eq)]
enum DriverAction {
    /// Re-read the graph and refresh both views.
    RefreshViews,
    /// Load one work item's detail into the work snapshot.
    InspectWork(String),
    /// Start one conversational turn.
    Submit(String),
    /// Cancel the running turn.
    CancelRunning,
    /// Leave the interface.
    Quit,
}

/// Map one typed interface request onto the driver's plan.
fn plan_request(request: &ChatUiRequest) -> DriverAction {
    match request {
        ChatUiRequest::SubmitPrompt { text } => DriverAction::Submit(text.clone()),
        ChatUiRequest::CancelTurn => DriverAction::CancelRunning,
        ChatUiRequest::SlashStatus | ChatUiRequest::SlashWork | ChatUiRequest::SlashDecisions => {
            DriverAction::RefreshViews
        }
        ChatUiRequest::SlashInspect { id } => DriverAction::InspectWork(id.clone()),
        ChatUiRequest::Quit => DriverAction::Quit,
    }
}

/// One conversational turn running on its own thread.
struct RunningTurn {
    /// Ephemeral turn events; the sender drops when the turn ends.
    events: Receiver<ConversationEvent>,
    /// The turn's terminal result, sent exactly once.
    done: Receiver<Result<ConversationTurnResult, AppError>>,
    /// Cancellation, shared with the worker.
    cancel: CancelToken,
    /// The worker: joined on normal completion, detached after a forced
    /// quit's grace window.
    worker: Option<JoinHandle<()>>,
}

impl RunningTurn {
    /// Spawn one turn on a worker thread. The turn — provider call,
    /// proposal application, fact appends — runs entirely outside the
    /// event loop; its events and result arrive over the channels.
    fn spawn(
        session: Arc<ProjectSession>,
        provider: Arc<dyn ModelProvider>,
        model: String,
        prompt: String,
    ) -> io::Result<RunningTurn> {
        let (event_tx, event_rx) = channel::<ConversationEvent>();
        let (done_tx, done_rx) = channel::<Result<ConversationTurnResult, AppError>>();
        let cancel = CancelToken::new();
        let worker_cancel = cancel.clone();
        let worker = std::thread::Builder::new()
            .name("relay-chat-turn".to_string())
            .spawn(move || {
                let result = start_turn(
                    &session,
                    provider,
                    &model,
                    &prompt,
                    &ConversationLimits::default(),
                    worker_cancel,
                    event_tx,
                );
                let _ = done_tx.send(result);
            })?;
        Ok(RunningTurn {
            events: event_rx,
            done: done_rx,
            cancel,
            worker: Some(worker),
        })
    }

    /// Request cancellation of the turn (idempotent).
    fn cancel(&self) {
        self.cancel.cancel("Ctrl-C in the chat interface");
    }

    /// A forced quit: request cancellation, then wait a bounded grace for
    /// the terminal fact. The worker is joined only when it finished in
    /// time; otherwise it is detached and the store's crash recovery owns
    /// the tail.
    fn force_quit(mut self) {
        self.cancel();
        let deadline = Instant::now() + FORCE_QUIT_GRACE;
        let mut finished = false;
        while Instant::now() < deadline && !finished {
            match self.done.recv_timeout(Duration::from_millis(50)) {
                Ok(_) | Err(RecvTimeoutError::Disconnected) => finished = true,
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
        if finished && let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        // A still-running worker stays detached; dropping the handle does
        // not kill the thread, and every append is a short locked
        // operation that crash recovery can reconcile.
    }
}

/// Driver-owned state: the lazily opened session, the resolved provider,
/// the running turn, and the last work snapshot (the base for detail
/// overlays — `ChatUi` does not expose its snapshot).
struct DriverState {
    workspace: PathBuf,
    session_options: SessionOptions,
    session: Option<Arc<ProjectSession>>,
    provider: Arc<dyn ModelProvider>,
    model: String,
    running: Option<RunningTurn>,
    last_work: WorkSnapshot,
}

impl DriverState {
    /// Open (or create) the session on first use. On failure the editor
    /// draft is preserved — the submit path already cleared it, so the
    /// text is pasted back in.
    fn ensure_session(&mut self, ui: &mut ChatUi, draft: &str) -> bool {
        if self.session.is_some() {
            return true;
        }
        match ProjectSession::open_or_create(&self.workspace, self.session_options.clone()) {
            Ok((session, _report)) => {
                self.session = Some(Arc::new(session));
                true
            }
            Err(error) => {
                ui.set_status(format!("could not open the workspace: {}", error.message()));
                if !draft.trim().is_empty() {
                    let _ = ui.handle(ChatKey::Paste(draft.to_string()));
                }
                false
            }
        }
    }

    /// Start one conversational turn on the worker thread.
    fn start_turn(&mut self, ui: &mut ChatUi, prompt: String) {
        if self.running.is_some() {
            ui.set_status("wait: a turn is running (Ctrl-C cancels)");
            return;
        }
        if !self.ensure_session(ui, &prompt) {
            return;
        }
        let Some(session) = self.session.clone() else {
            return;
        };
        ui.set_busy(true);
        ui.set_status("asking…");
        match RunningTurn::spawn(
            session,
            Arc::clone(&self.provider),
            self.model.clone(),
            prompt,
        ) {
            Ok(turn) => self.running = Some(turn),
            Err(error) => {
                ui.set_busy(false);
                ui.set_status(format!("could not start the turn worker: {error}"));
            }
        }
    }

    /// Absorb a finished turn: clear busy, report the truthful terminal
    /// status, refresh the durable views.
    fn complete_turn(&mut self, ui: &mut ChatUi, result: Result<ConversationTurnResult, AppError>) {
        ui.set_busy(false);
        match result {
            Ok(turn) => match turn.status {
                TurnStatus::Completed => {
                    let proposals = turn.proposals.len();
                    ui.set_status(if proposals == 0 {
                        "completed".to_string()
                    } else {
                        format!("completed — {proposals} proposal(s) decided")
                    });
                }
                TurnStatus::Failed { message } => {
                    ui.set_status(format!("failed: {message}"));
                }
                TurnStatus::Cancelled { reason } => {
                    ui.set_status(format!("cancelled: {reason}"));
                }
            },
            Err(error) => ui.set_status(format!("error: {}", error.message())),
        }
        self.refresh(ui);
    }

    /// Re-read the graph and replace both durable views.
    fn refresh(&mut self, ui: &mut ChatUi) {
        let Some(session) = &self.session else {
            return;
        };
        if let Err(error) = session.refresh() {
            ui.set_status(format!("could not refresh the views: {}", error.message()));
            return;
        }
        ui.set_view(chat_view(session, VIEW_ENTRY_LIMIT));
        let work = relay_ui::work::snapshot_from_session(session);
        self.last_work = work.clone();
        ui.set_work(work);
    }

    /// Load one work item's detail into the work snapshot (the UI opens
    /// the detail overlay from the snapshot).
    fn inspect(&mut self, ui: &mut ChatUi, id: &str) {
        let Some(session) = &self.session else {
            ui.set_status("no project yet — ask a question first");
            return;
        };
        match app::work_detail(session, id) {
            Ok(detail) => {
                let mut work = self.last_work.clone();
                work.detail = Some(detail);
                work.viewer = None;
                ui.set_status(format!("inspected {id}"));
                ui.set_work(work);
            }
            Err(error) => ui.set_status(format!("inspect failed: {}", error.message())),
        }
    }
}

/// Apply one interface request. Returns `true` when the interface should
/// quit.
fn apply_request(state: &mut DriverState, ui: &mut ChatUi, request: ChatUiRequest) -> bool {
    match plan_request(&request) {
        DriverAction::Quit => {
            if let Some(turn) = state.running.take() {
                turn.force_quit();
            }
            true
        }
        DriverAction::RefreshViews => {
            state.refresh(ui);
            false
        }
        DriverAction::InspectWork(id) => {
            state.inspect(ui, &id);
            false
        }
        DriverAction::CancelRunning => {
            if let Some(turn) = &state.running {
                turn.cancel();
            }
            false
        }
        DriverAction::Submit(text) => {
            state.start_turn(ui, text);
            false
        }
    }
}

/// Pump the running turn: drain pending events into the UI, then check
/// for completion. Returns the terminal result when the turn finished
/// (consuming the worker join).
fn pump_turn(
    turn: &mut RunningTurn,
    ui: &mut ChatUi,
) -> Option<Result<ConversationTurnResult, AppError>> {
    while let Ok(event) = turn.events.try_recv() {
        ui.push_event(&event);
    }
    match turn.done.try_recv() {
        Ok(result) => {
            if let Some(worker) = turn.worker.take() {
                let _ = worker.join();
            }
            Some(result)
        }
        Err(TryRecvError::Empty) => None,
        // The worker ended without reporting: a truthful error, never a
        // fabricated reply.
        Err(TryRecvError::Disconnected) => Some(Err(AppError::Rejected(
            "the turn worker ended without a result".to_string(),
        ))),
    }
}

/// Set up the terminal, run the event loop, restore. The guard's panic
/// hook restores even on a panic between setup and teardown.
fn run_loop(state: &mut DriverState, ui: &mut ChatUi) -> Result<(), Box<dyn std::error::Error>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableBracketedPaste)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.hide_cursor()?;

    let restore: RestoreFn = Arc::new(move || {
        // A fresh handle per call keeps the closure `Fn` (idempotent anyway).
        let mut stdout = io::stdout();
        let _ = execute!(stdout, DisableBracketedPaste);
        let _ = disable_raw_mode();
        let _ = execute!(stdout, LeaveAlternateScreen);
        let _ = disable_raw_mode();
    });
    let guard = TerminalGuard::new(restore);

    let outcome = event_loop(&mut terminal, state, ui);
    let _ = terminal.show_cursor();
    guard.restore_now();
    outcome
}

/// The event loop: draw, poll terminal events, pump the running turn.
fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    state: &mut DriverState,
    ui: &mut ChatUi,
) -> Result<(), Box<dyn std::error::Error>> {
    loop {
        screens::draw(terminal, ui)?;
        let slice = if state.running.is_some() {
            POLL_WHILE_RUNNING
        } else {
            POLL_WHILE_IDLE
        };
        if crossterm::event::poll(slice)? {
            match crossterm::event::read() {
                // Resize redraws at the top of the loop.
                Ok(TermEvent::Resize(..)) => {}
                Ok(event) => match ui.handle(chat_key_of(event)) {
                    ChatUiOutcome::Continue => {}
                    ChatUiOutcome::Quit => {
                        if let Some(turn) = state.running.take() {
                            turn.force_quit();
                        }
                        return Ok(());
                    }
                    ChatUiOutcome::Request(request) => {
                        if apply_request(state, ui, request) {
                            return Ok(());
                        }
                    }
                },
                Err(error) => return Err(Box::new(error)),
            }
        }
        if let Some(mut turn) = state.running.take() {
            if let Some(result) = pump_turn(&mut turn, ui) {
                state.complete_turn(ui, result);
            } else {
                state.running = Some(turn);
            }
        }
    }
}

/// Decode one crossterm event into the chat interface's [`ChatKey`].
///
/// Pure — the translation is tested in-process against synthetic events,
/// so the raw event loop stays a thin shell. Only key *presses* decode
/// (release/repeat reports from extended keyboard protocols are ignored);
/// bracketed paste decodes to [`ChatKey::Paste`]; Ctrl combos decode to
/// [`ChatKey::Ctrl`] with the lowercase modifier letter; anything the
/// editor cannot use decodes to [`ChatKey::Other`].
pub(crate) fn chat_key_of(event: TermEvent) -> ChatKey {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyEventKind;
    use crossterm::event::KeyModifiers;
    let TermEvent::Key(KeyEvent {
        code,
        modifiers,
        kind,
        ..
    }) = event
    else {
        // Bracketed paste is the one non-key event the editor consumes.
        return match event {
            TermEvent::Paste(text) => ChatKey::Paste(text),
            _ => ChatKey::Other,
        };
    };
    if matches!(kind, KeyEventKind::Release) {
        return ChatKey::Other;
    }
    match code {
        KeyCode::Char(character) => {
            let foreign = KeyModifiers::ALT | KeyModifiers::SUPER | KeyModifiers::META;
            if modifiers.contains(KeyModifiers::CONTROL) && !modifiers.intersects(foreign) {
                ChatKey::Ctrl(character.to_ascii_lowercase())
            } else if modifiers.intersects(KeyModifiers::CONTROL | foreign) {
                ChatKey::Other
            } else {
                ChatKey::Char(character)
            }
        }
        KeyCode::Enter => ChatKey::Enter,
        KeyCode::Up => ChatKey::Up,
        KeyCode::Down => ChatKey::Down,
        KeyCode::Left => ChatKey::Left,
        KeyCode::Right => ChatKey::Right,
        KeyCode::Home => ChatKey::Home,
        KeyCode::End => ChatKey::End,
        KeyCode::Backspace => ChatKey::Backspace,
        KeyCode::Delete => ChatKey::Delete,
        KeyCode::Esc => ChatKey::Esc,
        KeyCode::PageUp => ChatKey::PageUp,
        KeyCode::PageDown => ChatKey::PageDown,
        _ => ChatKey::Other,
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crossterm::event::Event;
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyEventKind;
    use crossterm::event::KeyModifiers;
    use relay_ui::chat::ChatOverlay;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> Event {
        Event::Key(KeyEvent::new(code, modifiers))
    }

    fn test_state(root: &Path) -> DriverState {
        let (provider, model) =
            conversational_provider(&crate::dispatch::RuntimeSelection::Production {
                provider: "fake".to_string(),
            })
            .expect("the fake provider resolves");
        DriverState {
            workspace: root.to_path_buf(),
            session_options: SessionOptions::deterministic(0),
            session: None,
            provider,
            model,
            running: None,
            last_work: empty_work_snapshot(),
        }
    }

    fn fresh_ui() -> ChatUi {
        ChatUi::new(
            welcome_view(),
            empty_work_snapshot(),
            "/workspace".to_string(),
        )
    }

    /// Pump the running turn until it completes (bounded wait).
    fn run_to_completion(state: &mut DriverState, ui: &mut ChatUi) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if let Some(mut turn) = state.running.take() {
                if let Some(result) = pump_turn(&mut turn, ui) {
                    state.complete_turn(ui, result);
                    return;
                }
                state.running = Some(turn);
            } else {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("the turn did not complete in time");
    }

    #[test]
    fn decodes_keys_the_editor_uses() {
        assert_eq!(
            chat_key_of(key(KeyCode::Char('a'), KeyModifiers::NONE)),
            ChatKey::Char('a')
        );
        assert_eq!(
            chat_key_of(key(KeyCode::Char('A'), KeyModifiers::SHIFT)),
            ChatKey::Char('A')
        );
        assert_eq!(
            chat_key_of(key(KeyCode::Enter, KeyModifiers::NONE)),
            ChatKey::Enter
        );
        for (code, expected) in [
            (KeyCode::Up, ChatKey::Up),
            (KeyCode::Down, ChatKey::Down),
            (KeyCode::Left, ChatKey::Left),
            (KeyCode::Right, ChatKey::Right),
            (KeyCode::Home, ChatKey::Home),
            (KeyCode::End, ChatKey::End),
            (KeyCode::Backspace, ChatKey::Backspace),
            (KeyCode::Delete, ChatKey::Delete),
            (KeyCode::Esc, ChatKey::Esc),
            (KeyCode::PageUp, ChatKey::PageUp),
            (KeyCode::PageDown, ChatKey::PageDown),
        ] {
            assert_eq!(chat_key_of(key(code, KeyModifiers::NONE)), expected);
        }
    }

    #[test]
    fn decodes_control_combinations_with_the_lowercase_letter() {
        assert_eq!(
            chat_key_of(key(KeyCode::Char('j'), KeyModifiers::CONTROL)),
            ChatKey::Ctrl('j')
        );
        assert_eq!(
            chat_key_of(key(KeyCode::Char('C'), KeyModifiers::CONTROL)),
            ChatKey::Ctrl('c')
        );
    }

    #[test]
    fn decodes_bracketed_paste_and_ignores_everything_else() {
        assert_eq!(
            chat_key_of(Event::Paste("two\nlines".to_string())),
            ChatKey::Paste("two\nlines".to_string())
        );
        assert_eq!(
            chat_key_of(key(KeyCode::F(1), KeyModifiers::NONE)),
            ChatKey::Other
        );
        assert_eq!(
            chat_key_of(key(KeyCode::Tab, KeyModifiers::NONE)),
            ChatKey::Other
        );
        assert_eq!(
            chat_key_of(key(
                KeyCode::Char('a'),
                KeyModifiers::ALT | KeyModifiers::CONTROL
            )),
            ChatKey::Other
        );
        assert_eq!(
            chat_key_of(Event::Resize(80, 24)),
            ChatKey::Other,
            "resize is redrawn, never a key"
        );
        // Release reports (extended protocols) must not double-type.
        let release = Event::Key(KeyEvent::new_with_kind(
            KeyCode::Char('a'),
            KeyModifiers::NONE,
            KeyEventKind::Release,
        ));
        assert_eq!(chat_key_of(release), ChatKey::Other);
    }

    #[test]
    fn plans_every_ui_request_onto_a_driver_action() {
        assert_eq!(
            plan_request(&ChatUiRequest::SubmitPrompt {
                text: "fix the tests".to_string()
            }),
            DriverAction::Submit("fix the tests".to_string())
        );
        assert_eq!(
            plan_request(&ChatUiRequest::CancelTurn),
            DriverAction::CancelRunning
        );
        for request in [
            ChatUiRequest::SlashStatus,
            ChatUiRequest::SlashWork,
            ChatUiRequest::SlashDecisions,
        ] {
            assert_eq!(plan_request(&request), DriverAction::RefreshViews);
        }
        assert_eq!(
            plan_request(&ChatUiRequest::SlashInspect {
                id: "work_discover-1".to_string()
            }),
            DriverAction::InspectWork("work_discover-1".to_string())
        );
        assert_eq!(plan_request(&ChatUiRequest::Quit), DriverAction::Quit);
    }

    #[test]
    fn a_fresh_workspace_stays_untouched_until_the_first_prompt() {
        let dir = tempfile::tempdir().ok().unwrap();
        let root = dir.path();
        let mut state = test_state(root);
        let mut ui = fresh_ui();

        // Slash refreshes on a fresh workspace change nothing on disk.
        if let ChatUiOutcome::Request(request) = ui.handle_submit("/work".to_string()) {
            assert!(!apply_request(&mut state, &mut ui, request));
        }
        assert!(
            !root.join(".relay-graph").exists(),
            "no state before the first prompt"
        );
        assert_eq!(ui.overlay(), Some(ChatOverlay::Work));

        // The first prompt creates the project, conversation, and turn.
        if let ChatUiOutcome::Request(request) = ui.handle_submit("hello there".to_string()) {
            assert!(!apply_request(&mut state, &mut ui, request));
        }
        run_to_completion(&mut state, &mut ui);
        assert!(
            root.join(".relay-graph").is_dir(),
            "the turn recorded facts"
        );
        assert!(state.session.is_some());
        // The durable view carries the user message and the (echoed)
        // assistant reply.
        let session = state.session.as_ref().unwrap();
        let view = chat_view(session, VIEW_ENTRY_LIMIT);
        let texts: Vec<&str> = view
            .entries
            .iter()
            .map(|entry| entry.text.as_str())
            .collect();
        assert!(
            texts.iter().any(|text| text.contains("hello there")),
            "the user message is durable: {texts:?}"
        );
        assert_eq!(ui.transcript_offset(), 0);
    }

    #[test]
    fn a_completed_turn_clears_busy_and_accepts_the_next_prompt() {
        let dir = tempfile::tempdir().ok().unwrap();
        let root = dir.path();
        let mut state = test_state(root);
        let mut ui = fresh_ui();
        if let ChatUiOutcome::Request(request) = ui.handle_submit("one question".to_string()) {
            assert!(!apply_request(&mut state, &mut ui, request));
        }
        assert!(state.running.is_some(), "the turn runs outside the loop");
        run_to_completion(&mut state, &mut ui);
        assert!(state.running.is_none());
        // Not busy anymore: the next submission is requested, not refused.
        assert!(matches!(
            ui.handle_submit("next question".to_string()),
            ChatUiOutcome::Request(ChatUiRequest::SubmitPrompt { .. })
        ));
    }

    #[test]
    fn slash_inspect_on_a_fresh_workspace_reports_honestly() {
        let dir = tempfile::tempdir().ok().unwrap();
        let root = dir.path();
        let mut state = test_state(root);
        let mut ui = fresh_ui();
        if let ChatUiOutcome::Request(request) =
            ui.handle_submit("/inspect work_discover-1".to_string())
        {
            assert!(!apply_request(&mut state, &mut ui, request));
        }
        // No session was invented and no detail overlay opened.
        assert_eq!(ui.overlay(), None);
        assert!(
            !root.join(".relay-graph").exists(),
            "a refused inspect creates nothing"
        );
    }

    #[test]
    fn welcome_state_never_builds_a_session() {
        // The hand-built welcome pieces construct without any session.
        let ui = fresh_ui();
        assert_eq!(ui.editor_text(), "");
        assert_eq!(ui.overlay(), None);
    }
}
