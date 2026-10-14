//! Work-graph TUI tests: TestBackend snapshots of the five screens from a
//! fixed fact stream, typed keymap behavior, terminal restoration on
//! normal exit / error / panic, narrow-width rendering, and no-TTY
//! independence (everything here renders without a terminal).

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::ok_expect,
    clippy::err_expect
)] // test-only conveniences, house style

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use relay_app::RuntimeWiring;
use relay_app::commands as app;
use relay_app::session::{ProjectSession, SessionOptions};
use relay_domain::{
    ArtifactDigest, AttemptOutcome, AttemptReceipt, ExecutorClass, OutputProposal, Usage,
};
use relay_executor::spec::{AttemptSpec, RunContext};
use relay_executor::{
    Executor, ExecutorSet, ModelClient, ModelClientError, ModelTurnRequest, ModelTurnResponse,
};
use relay_providers::contract::CancelToken;
use relay_ui::work::keymap::{Key, Keymap, TuiAction};
use relay_ui::work::screens::draw;
use relay_ui::work::terminal::TerminalGuard;
use relay_ui::work::{
    Screen, TuiOutcome, TuiRequest, ViewerState, WorkSnapshot, WorkTui, snapshot_from_session,
};
use relay_workflows::StructuredDoc;

const EPOCH_MS: i64 = 1_791_878_400_000;

// ---------------------------------------------------------------------------
// A fixed fact stream: init → clarification → discovery → approval → round
// ---------------------------------------------------------------------------

/// A model-class executor that publishes the opportunity brief for
/// discovery work (offline and deterministic).
struct DiscoveryExecutor;

impl Executor for DiscoveryExecutor {
    fn class(&self) -> ExecutorClass {
        ExecutorClass::Model
    }

    fn run(&self, spec: &AttemptSpec, context: &RunContext) -> AttemptReceipt {
        let doc = relay_workflows::BriefDoc {
            problem: "team reports are stuck inside the tool".to_string(),
            audience: "analysts".to_string(),
            outcome: "one-click CSV export".to_string(),
            open_questions: Vec::new(),
        };
        AttemptReceipt {
            attempt: spec.attempt.clone(),
            work: spec.work.clone(),
            outcome: AttemptOutcome::Completed,
            completed_at_ms: context.now_ms,
            outputs: vec![OutputProposal {
                artifact: "art_brief".parse().ok().unwrap(),
                title: Some("Opportunity brief".to_string()),
                content: doc.to_content(),
                digest: ArtifactDigest::parse(&format!("{:064x}", 0x00ff))
                    .ok()
                    .unwrap(),
            }],
            evidence: Vec::new(),
            graph_changes: Vec::new(),
            usage: Usage {
                input_tokens: 10,
                output_tokens: 20,
                tool_calls: 0,
                duration_ms: 5,
            },
        }
    }
}

struct EchoModel;

impl ModelClient for EchoModel {
    fn complete(
        &self,
        _route: &relay_domain::ModelRoute,
        request: &ModelTurnRequest,
        _signal: &CancelToken,
    ) -> Result<ModelTurnResponse, ModelClientError> {
        Ok(ModelTurnResponse {
            text: format!("echo: {}", request.prompt),
            input_tokens: 1,
            output_tokens: 1,
            routes_attempted: Vec::new(),
        })
    }
}

fn wiring() -> RuntimeWiring {
    RuntimeWiring::scripted(
        ExecutorSet::new().with(Arc::new(DiscoveryExecutor)),
        Arc::new(EchoModel),
    )
}

fn options() -> SessionOptions {
    SessionOptions::deterministic(EPOCH_MS)
}

/// After `init`: a clarification awaits a human; discovery is blocked
/// behind it.
fn early_state() -> (tempfile::TempDir, ProjectSession) {
    let dir = tempfile::tempdir().ok().unwrap();
    app::init(dir.path(), "export team reports as CSV", options())
        .ok()
        .unwrap();
    let session = ProjectSession::open(dir.path(), options()).ok().unwrap();
    (dir, session)
}

/// Clarification answered, discovery completed, the approval gate open
/// with selectable options.
fn mid_state() -> (tempfile::TempDir, ProjectSession) {
    let (dir, session) = early_state();
    app::resolve_decision(
        &session,
        "dec_clarify-unknowns-1",
        "CSV export for analysts",
        Some("the concrete outcome"),
    )
    .ok()
    .unwrap();
    relay_app::runner::run_ticks(
        &session,
        &wiring(),
        relay_engine::SchedulingPolicy::default(),
        1,
    )
    .ok()
    .unwrap();
    app::expand(&session).ok().unwrap();
    let session = ProjectSession::open(dir.path(), options()).ok().unwrap();
    (dir, session)
}

/// Brief approved; the specification round (requirements + read-only
/// research) is ready.
fn late_state() -> (tempfile::TempDir, ProjectSession) {
    let (dir, session) = mid_state();
    let decisions = app::decisions(&session).ok().unwrap();
    let approval = decisions
        .decisions
        .iter()
        .find(|decision| decision.question.contains("Approve opportunity brief"))
        .unwrap();
    app::resolve_decision(&session, &approval.id, "approve", Some("it matches"))
        .ok()
        .unwrap();
    app::expand(&session).ok().unwrap();
    let session = ProjectSession::open(dir.path(), options()).ok().unwrap();
    (dir, session)
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// Flatten a rendered frame to trimmed text lines (the salvage snapshot
/// technique from the compatibility port's TUI tests).
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

fn render(tui: &WorkTui, width: u16, height: u16) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).ok().unwrap();
    draw(&mut terminal, tui).ok().unwrap();
    frame_text(&terminal)
}

fn on_screen(snapshot: WorkSnapshot, screen: Screen) -> WorkTui {
    let mut tui = WorkTui::new(snapshot);
    while tui.screen() != screen {
        tui.handle(TuiAction::NextScreen);
    }
    tui
}

#[test]
fn summary_screen_renders_goal_phases_and_recommendation() {
    let (_dir, session) = early_state();
    let tui = WorkTui::new(snapshot_from_session(&session));
    let text = render(&tui, 80, 24);
    assert!(text.contains("summary"), "{text}");
    assert!(text.contains("export team reports as CSV"), "{text}");
    assert!(text.contains("Discovery"), "{text}");
    assert!(text.contains("Delivery"), "{text}");
    assert!(text.contains("awaiting you 1"), "{text}");
    assert!(text.contains("next: resolve decision"), "{text}");
    assert!(text.contains("q quit"), "{text}");
}

#[test]
fn blocked_screen_lists_the_blocked_work() {
    let (_dir, session) = early_state();
    let tui = on_screen(snapshot_from_session(&session), Screen::Blocked);
    let text = render(&tui, 80, 24);
    assert!(text.contains("work_discover-opportunity-1"), "{text}");
    assert!(text.contains("blocked"), "{text}");
}

#[test]
fn ready_screen_shows_ready_work_with_explanations() {
    let (_dir, session) = late_state();
    let tui = on_screen(snapshot_from_session(&session), Screen::Ready);
    let text = render(&tui, 80, 40);
    assert!(text.contains("ready"), "{text}");
    assert!(
        text.contains("work_define-requirements-1") || text.contains("work_inspect-repository-1"),
        "the specification round renders: {text}"
    );
}

#[test]
fn decisions_screen_lists_open_decisions_and_marks_selection() {
    let (_dir, session) = mid_state();
    let mut tui = on_screen(snapshot_from_session(&session), Screen::Decisions);
    let text = render(&tui, 80, 24);
    assert!(text.contains("decisions"), "{text}");
    // The approval gate is optioned; the selected option is marked.
    assert!(text.contains("approve"), "{text}");
    assert!(text.contains("revise"), "{text}");
    assert!(text.contains("> approve"), "{text}");
    // Moving the cursor moves the marker.
    tui.handle(TuiAction::SelectNext);
    let text = render(&tui, 80, 24);
    assert!(text.contains("> revise"), "{text}");
    // The earlier free-form clarification is listed as CLI-resolvable.
    assert!(text.contains("free-form"), "{text}");
}

#[test]
fn detail_screen_renders_status_attempts_and_evidence() {
    let (_dir, session) = mid_state();
    let mut tui = WorkTui::new(snapshot_from_session(&session));
    let detail = app::work_detail(&session, "work_discover-opportunity-1")
        .ok()
        .unwrap();
    tui.set_snapshot_detail(detail);
    let text = render(&tui, 80, 40);
    assert!(text.contains("work_discover-opportunity-1"), "{text}");
    assert!(text.contains("completed"), "{text}");
    assert!(text.contains("attempts:"), "{text}");
    assert!(text.contains("accepted"), "{text}");
}

#[test]
fn viewer_screen_renders_markdown_and_esc_returns() {
    let (_dir, session) = mid_state();
    let mut tui = WorkTui::new(snapshot_from_session(&session));
    let artifact = app::artifact_detail(&session, "art_brief", None, true)
        .ok()
        .unwrap();
    let markdown = artifact.markdown.clone().unwrap_or_default();
    assert!(markdown.contains("# Opportunity Brief"), "{markdown}");
    tui.set_snapshot_viewer(ViewerState {
        title: "art_brief@1".to_string(),
        markdown,
    });
    let text = render(&tui, 80, 40);
    assert!(text.contains("art_brief@1"), "{text}");
    assert!(text.contains("Opportunity Brief"), "{text}");
    // esc returns from the viewer without quitting the interface.
    assert_eq!(tui.handle(TuiAction::Quit), TuiOutcome::Continue);
    assert_eq!(tui.screen(), Screen::Summary);
}

// ---------------------------------------------------------------------------
// Keymap and typed actions
// ---------------------------------------------------------------------------

#[test]
fn keymap_maps_typed_keys_to_actions() {
    let keymap = Keymap;
    assert_eq!(keymap.action(Key::Char('q')), Some(TuiAction::Quit));
    assert_eq!(keymap.action(Key::Esc), Some(TuiAction::Quit));
    assert_eq!(keymap.action(Key::Tab), Some(TuiAction::NextScreen));
    assert_eq!(keymap.action(Key::BackTab), Some(TuiAction::PrevScreen));
    assert_eq!(keymap.action(Key::Char('j')), Some(TuiAction::SelectNext));
    assert_eq!(keymap.action(Key::Up), Some(TuiAction::SelectPrev));
    assert_eq!(keymap.action(Key::Enter), Some(TuiAction::Activate));
    assert_eq!(keymap.action(Key::Char('?')), Some(TuiAction::Help));
    assert_eq!(keymap.action(Key::Char('z')), None);
}

#[test]
fn help_toggles_and_q_closes_help_before_quitting() {
    let (_dir, session) = early_state();
    let mut tui = WorkTui::new(snapshot_from_session(&session));
    tui.handle(TuiAction::Help);
    let text = render(&tui, 80, 40);
    assert!(text.contains("relay work-graph interface"), "{text}");
    assert!(text.contains("never answers"), "{text}");
    assert_eq!(tui.handle(TuiAction::Quit), TuiOutcome::Continue);
    assert!(!tui.help_visible());
    assert_eq!(tui.handle(TuiAction::Quit), TuiOutcome::Quit);
}

#[test]
fn ready_screen_activate_requests_inspection() {
    let (_dir, session) = late_state();
    let mut tui = on_screen(snapshot_from_session(&session), Screen::Ready);
    let rows = tui.rows();
    assert!(!rows.is_empty(), "ready work exists");
    match tui.handle(TuiAction::Activate) {
        TuiOutcome::Request(TuiRequest::InspectWork { work }) => {
            assert!(rows.contains(&work), "{work} not in {rows:?}");
        }
        other => panic!("expected an inspect request, got {other:?}"),
    }
}

#[test]
fn decisions_activate_emits_a_typed_resolution_request_only_on_enter() {
    let (_dir, session) = mid_state();
    let snapshot = snapshot_from_session(&session);
    let mut tui = on_screen(snapshot.clone(), Screen::Decisions);
    let rows = tui.decision_rows();
    assert!(!rows.is_empty(), "the approval options are selectable");
    // Select the second option and activate: the request names both.
    tui.handle(TuiAction::SelectNext);
    match tui.handle(TuiAction::Activate) {
        TuiOutcome::Request(TuiRequest::ResolveDecision { decision, choice }) => {
            assert!(decision.starts_with("dec_"), "{decision}");
            assert!(!choice.is_empty(), "{choice}");
        }
        other => panic!("expected a resolve request, got {other:?}"),
    }
    // Navigation never resolves a decision by itself.
    let mut tui = on_screen(snapshot, Screen::Decisions);
    for _ in 0..5 {
        assert_eq!(tui.handle(TuiAction::SelectNext), TuiOutcome::Continue);
    }
}

#[test]
fn refresh_is_a_typed_request() {
    let (_dir, session) = early_state();
    let mut tui = WorkTui::new(snapshot_from_session(&session));
    assert_eq!(
        tui.handle(TuiAction::Refresh),
        TuiOutcome::Request(TuiRequest::Refresh)
    );
}

// ---------------------------------------------------------------------------
// Narrow width and no-TTY independence
// ---------------------------------------------------------------------------

#[test]
fn narrow_terminals_render_without_panicking() {
    let (_dir, session) = early_state();
    let snapshot = snapshot_from_session(&session);
    for screen in [
        Screen::Summary,
        Screen::Ready,
        Screen::Blocked,
        Screen::Decisions,
    ] {
        let tui = on_screen(snapshot.clone(), screen);
        let text = render(&tui, 40, 24);
        assert!(!text.is_empty(), "{screen:?} rendered nothing");
    }
    // Very narrow: still no panic, lines truncate instead of wrapping wide.
    let tui = WorkTui::new(snapshot);
    let text = render(&tui, 20, 24);
    assert!(text.lines().count() > 1, "{text}");
}

#[test]
fn rendering_never_touches_a_terminal() {
    // Everything the interface needs — snapshot construction, keymap,
    // screen state, and full rendering — runs against a TestBackend with no
    // tty, no raw mode, and no terminal guard. That is the no-TTY
    // independence guarantee: only `relay tui` ever constructs a guard or
    // enters raw mode, so non-interactive paths cannot touch the terminal.
    let (_dir, session) = early_state();
    let mut tui = WorkTui::new(snapshot_from_session(&session));
    tui.handle(TuiAction::NextScreen);
    let text = render(&tui, 80, 24);
    assert!(!text.is_empty());
}

// ---------------------------------------------------------------------------
// Terminal restoration
// ---------------------------------------------------------------------------

#[test]
fn terminal_restores_on_normal_exit() {
    let restored = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&restored);
    {
        let _guard = TerminalGuard::new(Arc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        }));
    }
    assert_eq!(restored.load(Ordering::SeqCst), 1);
}

#[test]
fn terminal_restores_after_an_error_path() {
    let restored = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&restored);
    let guard = TerminalGuard::new(Arc::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    }));
    // The driver's error path: explicit restore before returning nonzero.
    guard.restore_now();
    guard.restore_now();
    assert!(guard.restored());
    assert_eq!(restored.load(Ordering::SeqCst), 1);
}

#[test]
fn terminal_restores_on_panic() {
    let restored = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&restored);
    // Keep test output quiet around the deliberate probe panic.
    std::panic::set_hook(Box::new(|_| {}));
    let probe = std::panic::catch_unwind(|| {
        let _guard = TerminalGuard::new(Arc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        }));
        panic!("terminal probe");
    });
    drop(probe);
    assert_eq!(restored.load(Ordering::SeqCst), 1);
}
