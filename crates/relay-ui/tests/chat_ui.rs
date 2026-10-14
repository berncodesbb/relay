//! Chat UI tests: TestBackend snapshots of the chat screen across the
//! required state matrix — fresh/resumed, busy streaming, cancelling,
//! failed/cancelled/interrupted entries, decisions, long transcripts and
//! lines, Unicode, narrow/short terminals, overlays (help, status, work,
//! decisions, detail, artifact), paste, history, scrolling, and draft
//! preservation. Everything renders without a terminal.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::ok_expect,
    clippy::err_expect
)] // test-only conveniences, house style

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use relay_app::conversation::ConversationEvent;
use relay_app::conversation_views::{ChatEntry, ChatEntryKind, ChatView, OpenDecisionRow};
use relay_app::views::{
    DecisionListView, NextActionView, RecommendationView, StatusCountsView, SummaryView,
    WorkDetailView, WorkEntryView, WorkListView,
};
use relay_domain::{ConversationId, DecisionId, TurnId};
use relay_ui::chat::keymap::ChatKey;
use relay_ui::chat::screens::draw;
use relay_ui::chat::{
    ChatOverlay, ChatUi, ChatUiOutcome, ChatUiRequest, SCROLL_MAX, SCROLL_STEP_LINES,
};

const WORKSPACE: &str = "/tmp/demo-workspace";

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn turn(id: &str) -> TurnId {
    id.parse().unwrap()
}

fn entry(kind: ChatEntryKind, text: &str, turn_id: Option<&str>) -> ChatEntry {
    ChatEntry {
        kind,
        text: text.to_string(),
        turn: turn_id.map(turn),
        at_ms: 0,
    }
}

fn decision_row(id: &str, question: &str, options: &[&str]) -> OpenDecisionRow {
    OpenDecisionRow {
        decision: DecisionId::parse(id).unwrap(),
        question: question.to_string(),
        options: options.iter().map(|option| option.to_string()).collect(),
    }
}

fn view(
    entries: Vec<ChatEntry>,
    open_decisions: Vec<OpenDecisionRow>,
    interrupted_turns: usize,
    goal: Option<&str>,
) -> ChatView {
    ChatView {
        conversation: (entries.is_empty() && goal.is_none())
            .then(|| ConversationId::parse("cnv_chat").unwrap()),
        entries,
        open_decisions,
        interrupted_turns,
        goal: goal.map(str::to_string),
    }
}

fn summary(ready: usize) -> SummaryView {
    SummaryView {
        project_id: Some("prj_test".to_string()),
        goal: None,
        revision: 1,
        completion: "in-progress".to_string(),
        open_required: Vec::new(),
        open_decisions: Vec::new(),
        counts: StatusCountsView {
            ready,
            blocked: 0,
            awaiting_human: 0,
            active_attempts: 0,
            invalidated: 0,
            completed: 0,
            canceled: 0,
            superseded: 0,
        },
        phases: Vec::new(),
        recommendation: RecommendationView {
            autonomy: "default".to_string(),
            parallel: Vec::new(),
            serialized: Vec::new(),
            deferred: Vec::new(),
            rationale: "nothing to run yet".to_string(),
            next: NextActionView {
                kind: "none".to_string(),
                target: None,
                detail: "ask a question or declare a goal".to_string(),
            },
        },
    }
}

fn empty_snapshot() -> relay_ui::work::WorkSnapshot {
    relay_ui::work::WorkSnapshot::new(
        summary(0),
        WorkListView {
            works: Vec::new(),
            revision: 1,
        },
        WorkListView {
            works: Vec::new(),
            revision: 1,
        },
        DecisionListView {
            decisions: Vec::new(),
            revision: 1,
        },
    )
}

fn work_entry(id: &str, status: &str, objective: &str) -> WorkEntryView {
    WorkEntryView {
        id: id.to_string(),
        kind: "model".to_string(),
        phase: "Discovery".to_string(),
        executor: "model".to_string(),
        objective: objective.to_string(),
        required: true,
        status: status.to_string(),
        explanation: "ready at the default policy".to_string(),
    }
}

fn snapshot_with_ready_work() -> relay_ui::work::WorkSnapshot {
    let mut snapshot = empty_snapshot();
    snapshot.ready = WorkListView {
        works: vec![work_entry("work_trace", "ready", "Trace the checkout path")],
        revision: 2,
    };
    snapshot.summary.counts.ready = 1;
    snapshot
}

fn snapshot_with_detail() -> relay_ui::work::WorkSnapshot {
    let mut snapshot = empty_snapshot();
    snapshot.detail = Some(WorkDetailView {
        entry: work_entry("work_trace", "ready", "Trace the checkout path"),
        dependencies: Vec::new(),
        dependents: vec!["work_fix".to_string()],
        inputs: vec!["art_brief@1".to_string()],
        expected_outputs: Vec::new(),
        read_scopes: vec!["repo".to_string()],
        write_scopes: Vec::new(),
        completion: "receipt".to_string(),
        attempts: Vec::new(),
        evidence: Vec::new(),
    });
    snapshot
}

fn snapshot_with_viewer() -> relay_ui::work::WorkSnapshot {
    let mut snapshot = empty_snapshot();
    snapshot.viewer = Some(relay_ui::work::ViewerState {
        title: "art_brief@1".to_string(),
        markdown: "# Opportunity brief\n\nTeam reports are stuck.".to_string(),
    });
    snapshot
}

fn ui_with(view: ChatView) -> ChatUi {
    ChatUi::new(view, empty_snapshot(), WORKSPACE.to_string())
}

/// Render one frame and return the per-line buffer content (trimmed).
fn render(ui: &ChatUi, width: u16, height: u16) -> Vec<String> {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    draw(&mut terminal, ui).unwrap();
    let buffer = terminal.backend().buffer();
    let mut lines = Vec::new();
    for y in 0..height {
        let mut line = String::new();
        for x in 0..width {
            line.push_str(buffer[(x, y)].symbol());
        }
        lines.push(line.trim_end().to_string());
    }
    lines
}

fn screen(ui: &ChatUi, width: u16, height: u16) -> String {
    render(ui, width, height).join("\n")
}

fn type_text(ui: &mut ChatUi, text: &str) {
    for c in text.chars() {
        ui.handle(ChatKey::Char(c));
    }
}

// ---------------------------------------------------------------------------
// fresh / resumed / editing
// ---------------------------------------------------------------------------

#[test]
fn fresh_workspace_renders_invite_header_and_footer() {
    let ui = ui_with(view(Vec::new(), Vec::new(), 0, None));
    let screen = screen(&ui, 80, 24);
    assert!(
        screen.contains("Relay · /tmp/demo-workspace · ready"),
        "header"
    );
    assert!(screen.contains("(no messages yet"), "invite line");
    assert!(screen.contains("/help · Enter send · Ctrl-J newline · Ctrl-C cancel/exit"));
    assert!(screen.contains("> "), "prompt marker");
}

#[test]
fn resumed_transcript_renders_user_and_assistant_entries() {
    let ui = ui_with(view(
        vec![
            entry(
                ChatEntryKind::User,
                "Find why checkout duplicates an order.",
                None,
            ),
            entry(
                ChatEntryKind::Assistant,
                "I am tracing the checkout and retry paths.",
                Some("turn_1"),
            ),
        ],
        Vec::new(),
        0,
        Some("one-click CSV export"),
    ));
    let screen = screen(&ui, 80, 24);
    assert!(screen.contains("You"));
    assert!(screen.contains("  Find why checkout duplicates an order."));
    assert!(screen.contains("Relay"));
    assert!(screen.contains("  I am tracing the checkout and retry paths."));
}

#[test]
fn typing_renders_the_draft_and_enter_submits() {
    let mut ui = ui_with(view(Vec::new(), Vec::new(), 0, None));
    type_text(&mut ui, "fix the tests");
    assert!(screen(&ui, 80, 24).contains("> fix the tests"));
    assert_eq!(
        ui.handle(ChatKey::Enter),
        ChatUiOutcome::Request(ChatUiRequest::SubmitPrompt {
            text: "fix the tests".to_string()
        })
    );
    assert_eq!(ui.editor_text(), "", "the draft clears on submit");
}

#[test]
fn ctrl_j_inserts_newlines_and_multiline_submits() {
    let mut ui = ui_with(view(Vec::new(), Vec::new(), 0, None));
    type_text(&mut ui, "first line");
    ui.handle(ChatKey::Ctrl('j'));
    type_text(&mut ui, "second line");
    assert_eq!(ui.editor_text(), "first line\nsecond line");
    let screen = screen(&ui, 80, 24);
    assert!(screen.contains("> first line"));
    assert!(screen.contains("  second line"), "continuation indent");
    assert_eq!(
        ui.handle(ChatKey::Enter),
        ChatUiOutcome::Request(ChatUiRequest::SubmitPrompt {
            text: "first line\nsecond line".to_string()
        })
    );
}

#[test]
fn busy_submit_is_refused_and_preserves_the_draft() {
    let mut ui = ui_with(view(Vec::new(), Vec::new(), 0, None));
    ui.set_busy(true);
    type_text(&mut ui, "please wait");
    assert_eq!(ui.handle(ChatKey::Enter), ChatUiOutcome::Continue);
    assert_eq!(
        ui.editor_text(),
        "please wait",
        "the draft is never dropped"
    );
    let screen = screen(&ui, 80, 24);
    assert!(screen.contains("wait: a turn is running (Ctrl-C cancels)"));
    assert!(screen.contains("working"), "busy state in the header");
    // the prompt-form path refuses too, without clearing anything
    assert_eq!(
        ui.handle_submit("another".to_string()),
        ChatUiOutcome::Continue
    );
    assert_eq!(ui.editor_text(), "please wait");
}

// ---------------------------------------------------------------------------
// streaming, cancellation, terminal entries
// ---------------------------------------------------------------------------

#[test]
fn streaming_tail_renders_while_busy_and_reconciles_on_set_view() {
    let mut ui = ui_with(view(Vec::new(), Vec::new(), 0, None));
    ui.set_busy(true);
    ui.push_event(&ConversationEvent::TurnStarted {
        conversation: ConversationId::parse("cnv_chat").unwrap(),
        turn: turn("turn_9"),
        user_text: "explain the checkout".to_string(),
    });
    ui.push_event(&ConversationEvent::AssistantDelta {
        text: "I am ".to_string(),
    });
    ui.push_event(&ConversationEvent::AssistantDelta {
        text: "tracing.".to_string(),
    });
    let lines = render(&ui, 80, 24);
    assert!(
        lines.join("\n").contains("· working"),
        "header shows the working state"
    );
    assert!(screen(&ui, 80, 24).contains("I am tracing."), "stream tail");

    // the durable view lands the assistant entry for the active turn
    ui.set_busy(false);
    ui.set_view(view(
        vec![
            entry(ChatEntryKind::User, "explain the checkout", None),
            entry(ChatEntryKind::Assistant, "I am tracing.", Some("turn_9")),
        ],
        Vec::new(),
        0,
        None,
    ));
    let lines = render(&ui, 80, 24);

    assert_eq!(
        lines
            .iter()
            .filter(|line| line.trim_matches(['│', ' ']) == "Relay")
            .count(),
        1,
        "the durable assistant entry replaces the stream without duplicating it"
    );

    assert!(lines.join("\n").contains("I am tracing."));
}

#[test]
fn ctrl_c_cancels_first_then_force_exits() {
    let mut ui = ui_with(view(Vec::new(), Vec::new(), 0, None));
    ui.set_busy(true);
    assert_eq!(
        ui.handle(ChatKey::Ctrl('c')),
        ChatUiOutcome::Request(ChatUiRequest::CancelTurn)
    );
    let screen = screen(&ui, 80, 24);
    assert!(screen.contains("cancelling the running turn"), "status");
    assert!(screen.contains("cancelling"), "header state word");
    assert_eq!(ui.handle(ChatKey::Ctrl('c')), ChatUiOutcome::Quit);
    // clearing busy resets the cancelling flag: idle Ctrl-C is a plain exit
    ui.set_busy(false);
    assert_eq!(ui.handle(ChatKey::Ctrl('c')), ChatUiOutcome::Quit);
}

#[test]
fn idle_ctrl_c_exits() {
    let mut ui = ui_with(view(Vec::new(), Vec::new(), 0, None));
    assert_eq!(ui.handle(ChatKey::Ctrl('c')), ChatUiOutcome::Quit);
}

#[test]
fn failed_cancelled_and_interrupted_entries_render() {
    let ui = ui_with(view(
        vec![
            entry(
                ChatEntryKind::Failed,
                "turn failed: provider unavailable",
                Some("turn_2"),
            ),
            entry(
                ChatEntryKind::Cancelled,
                "turn cancelled: user asked",
                Some("turn_3"),
            ),
            entry(
                ChatEntryKind::Interrupted,
                "turn turn_1 started but never finished (interrupted by a restart); no reply was recorded",
                Some("turn_1"),
            ),
        ],
        Vec::new(),
        1,
        None,
    ));
    let screen = screen(&ui, 80, 24);
    assert!(screen.contains("interrupted"), "header state word");
    assert!(screen.contains("failed turn failed: provider unavailable"));
    assert!(screen.contains("cancelled turn cancelled: user asked"));
    assert!(screen.contains("interrupted turn turn_1 started"));
    assert!(screen.contains("no reply was recorded"));
}

#[test]
fn open_decisions_set_the_awaiting_state() {
    let mut ui = ui_with(view(
        vec![entry(
            ChatEntryKind::Decision,
            "dec_pick: choose the idempotency boundary (options: order, payment)",
            None,
        )],
        vec![decision_row(
            "dec_pick",
            "choose the idempotency boundary",
            &["order", "payment"],
        )],
        0,
        None,
    ));
    let shown = screen(&ui, 80, 24);
    assert!(shown.contains("awaiting you"), "header state word");
    assert!(shown.contains("? decision dec_pick: choose the idempotency boundary"));
    ui.handle_submit("/decisions".to_string());
    let overlay = screen(&ui, 80, 24);
    assert!(overlay.contains("dec_pick: choose the idempotency boundary"));
    assert!(overlay.contains("- order"));
    assert!(overlay.contains("- payment"));
}

// ---------------------------------------------------------------------------
// bounds, long content, unicode, narrow/short terminals
// ---------------------------------------------------------------------------

#[test]
fn long_transcript_bounds_with_an_ellipsis_header() {
    // 250 two-line entries render far beyond the 200-line cap
    let entries: Vec<ChatEntry> = (0..250)
        .map(|i| entry(ChatEntryKind::User, &format!("msg {i}"), None))
        .collect();
    let mut ui = ui_with(view(entries, Vec::new(), 0, None));
    // open an overlay so Home means scroll-to-top, jump, then close the
    // overlay: the offset persists and the pane no longer covers the top
    ui.handle(ChatKey::Char('?'));
    ui.handle(ChatKey::Home);
    assert_eq!(ui.transcript_offset(), SCROLL_MAX);
    ui.handle(ChatKey::Esc);
    let screen = screen(&ui, 80, 24);
    assert!(
        screen.contains("+ 550 earlier messages"),
        "bounded transcript acknowledges hidden lines"
    );
    assert!(!screen.contains("msg 0"), "the oldest entry is dropped");
    // End returns to the bottom under overlay focus (End = ScrollBottom)
    ui.handle(ChatKey::Char('?'));
    ui.handle(ChatKey::End);
    assert_eq!(ui.transcript_offset(), 0);
    ui.handle(ChatKey::Esc);
}

#[test]
fn long_lines_wrap_and_unicode_renders() {
    let long_word = "a".repeat(200);
    let ui = ui_with(view(
        vec![
            entry(ChatEntryKind::User, &long_word, None),
            entry(
                ChatEntryKind::Assistant,
                "修复结账重复下单的问题，然后运行测试。",
                None,
            ),
        ],
        Vec::new(),
        0,
        None,
    ));
    let lines = render(&ui, 80, 24);
    assert!(
        lines.iter().all(|line| line.chars().count() <= 80),
        "no line exceeds the width"
    );
    // the 200-char word wraps into three transcript lines (the block
    // border prefixes each rendered line, so count the wrap fragments)
    assert_eq!(
        lines
            .iter()
            .filter(|l| l.matches('a').count() >= 40)
            .count(),
        3,
        "long words hard-wrap"
    );
    // wide glyphs occupy two cells; the helper concatenates the padding
    // cells as spaces, so compare space-stripped for the CJK assertion
    assert!(
        screen(&ui, 80, 24)
            .replace(' ', "")
            .contains("修复结账重复下单的问题"),
        "CJK text renders"
    );
}

#[test]
fn narrow_terminal_hides_footer_extras_and_activity() {
    let mut ui = ui_with(view(Vec::new(), Vec::new(), 0, None));
    ui.push_event(&ConversationEvent::Activity {
        text: "compiling crates".to_string(),
    });
    let narrow = screen(&ui, 40, 24);
    assert!(narrow.contains("Relay ·"), "header survives");
    assert!(!narrow.contains("Enter send"), "footer extras hidden");
    assert!(
        !narrow.contains("compiling crates"),
        "activity hidden when narrow"
    );
    assert!(narrow.contains("> "), "editor survives");
    let wide = screen(&ui, 80, 24);
    assert!(
        wide.contains("· compiling crates"),
        "activity shows at normal width"
    );
    assert!(wide.contains("Enter send"));
}

#[test]
fn short_terminal_keeps_header_transcript_and_editor() {
    let ui = ui_with(view(
        vec![entry(ChatEntryKind::User, "hello", None)],
        Vec::new(),
        0,
        None,
    ));
    let short = screen(&ui, 80, 8);
    assert!(short.contains("Relay ·"));
    assert!(short.contains("hello"));
    assert!(short.contains("> "));
    assert!(
        !short.contains("Enter send"),
        "the footer is dropped when short"
    );
    // degenerate heights never panic
    let _ = screen(&ui, 80, 4);
    let _ = screen(&ui, 80, 3);
}

// ---------------------------------------------------------------------------
// overlays, draft/scroll preservation, keys
// ---------------------------------------------------------------------------

#[test]
fn overlay_open_preserves_editor_text_and_scroll() {
    let mut ui = ui_with(view(Vec::new(), Vec::new(), 0, None));
    type_text(&mut ui, "draft under overlay");
    ui.handle(ChatKey::PageUp);
    ui.handle(ChatKey::PageUp);
    assert_eq!(ui.transcript_offset(), 2 * SCROLL_STEP_LINES);
    assert_eq!(
        ui.handle_submit("/work".to_string()),
        ChatUiOutcome::Request(ChatUiRequest::SlashWork)
    );
    assert_eq!(ui.overlay(), Some(ChatOverlay::Work));
    assert_eq!(ui.editor_text(), "draft under overlay", "draft preserved");
    assert_eq!(ui.transcript_offset(), 20, "scroll preserved");
    let shown = screen(&ui, 80, 24);
    assert!(shown.contains("(no ready work)"), "overlay content renders");
    // Esc closes the overlay; the draft and scroll survive
    assert_eq!(ui.handle(ChatKey::Esc), ChatUiOutcome::Continue);
    assert_eq!(ui.overlay(), None);
    assert_eq!(ui.editor_text(), "draft under overlay");
    assert_eq!(ui.transcript_offset(), 20);
    assert!(screen(&ui, 80, 24).contains("> draft under overlay"));
}

#[test]
fn esc_without_an_overlay_is_a_no_op() {
    let mut ui = ui_with(view(Vec::new(), Vec::new(), 0, None));
    type_text(&mut ui, "kept");
    assert_eq!(ui.handle(ChatKey::Esc), ChatUiOutcome::Continue);
    assert_eq!(ui.overlay(), None);
    assert_eq!(ui.editor_text(), "kept");
}

#[test]
fn question_mark_toggles_help_only_when_the_editor_is_empty() {
    let mut ui = ui_with(view(Vec::new(), Vec::new(), 0, None));
    ui.handle(ChatKey::Char('?'));
    assert_eq!(ui.overlay(), Some(ChatOverlay::Help));
    assert!(
        screen(&ui, 80, 24).contains("relay chat"),
        "help overlay renders"
    );
    ui.handle(ChatKey::Esc);
    assert_eq!(ui.overlay(), None);
    // with a non-empty editor, '?' is typed
    type_text(&mut ui, "x?");
    assert_eq!(ui.overlay(), None);
    assert_eq!(ui.editor_text(), "x?");
}

#[test]
fn paste_via_chat_key_lands_in_the_editor() {
    let mut ui = ui_with(view(Vec::new(), Vec::new(), 0, None));
    assert_eq!(
        ui.handle(ChatKey::Paste("pasted line\nsecond".to_string())),
        ChatUiOutcome::Continue
    );
    assert_eq!(ui.editor_text(), "pasted line\nsecond");
    let screen = screen(&ui, 80, 24);
    assert!(screen.contains("> pasted line"));
    assert!(screen.contains("  second"));
}

#[test]
fn up_down_recall_history_in_a_single_line_editor() {
    let mut ui = ui_with(view(Vec::new(), Vec::new(), 0, None));
    ui.handle_submit("first prompt".to_string());
    type_text(&mut ui, "draf");
    ui.handle(ChatKey::Up);
    assert_eq!(ui.editor_text(), "first prompt");
    ui.handle(ChatKey::Down);
    assert_eq!(
        ui.editor_text(),
        "draf",
        "the in-progress draft is restored"
    );
}

#[test]
fn slash_commands_map_to_typed_requests() {
    let mut ui = ui_with(view(Vec::new(), Vec::new(), 0, None));
    assert_eq!(
        ui.handle_submit("/status".to_string()),
        ChatUiOutcome::Request(ChatUiRequest::SlashStatus)
    );
    assert_eq!(ui.overlay(), Some(ChatOverlay::Status));
    assert_eq!(
        ui.handle_submit("/work".to_string()),
        ChatUiOutcome::Request(ChatUiRequest::SlashWork)
    );
    assert_eq!(
        ui.handle_submit("/decisions".to_string()),
        ChatUiOutcome::Request(ChatUiRequest::SlashDecisions)
    );
    assert_eq!(
        ui.handle_submit("/inspect work_trace".to_string()),
        ChatUiOutcome::Request(ChatUiRequest::SlashInspect {
            id: "work_trace".to_string()
        })
    );
    assert_eq!(
        ui.handle_submit("/help".to_string()),
        ChatUiOutcome::Continue
    );
    assert_eq!(ui.overlay(), Some(ChatOverlay::Help));
    assert_eq!(
        ui.handle_submit("/quit".to_string()),
        ChatUiOutcome::Request(ChatUiRequest::Quit)
    );
}

#[test]
fn unknown_slash_and_missing_inspect_argument_hint_usage() {
    let mut ui = ui_with(view(Vec::new(), Vec::new(), 0, None));
    assert_eq!(
        ui.handle_submit("/bogus".to_string()),
        ChatUiOutcome::Continue
    );
    assert!(screen(&ui, 80, 24).contains("unknown command /bogus"));
    assert_eq!(
        ui.handle_submit("/inspect".to_string()),
        ChatUiOutcome::Continue
    );
    let shown = screen(&ui, 80, 24);
    assert!(shown.contains("usage: /inspect <work-id>"));
}

#[test]
fn empty_submissions_do_nothing() {
    let mut ui = ui_with(view(Vec::new(), Vec::new(), 0, None));
    assert_eq!(ui.handle_submit("   ".to_string()), ChatUiOutcome::Continue);
    type_text(&mut ui, "  ");
    assert_eq!(ui.handle(ChatKey::Enter), ChatUiOutcome::Continue);
}

#[test]
fn status_overlay_shows_workspace_goal_and_conversation() {
    let mut chat_view = view(
        vec![entry(ChatEntryKind::User, "hello", None)],
        Vec::new(),
        0,
        Some("one-click CSV export"),
    );
    chat_view.conversation = Some(ConversationId::parse("cnv_chat").unwrap());
    let mut ui = ui_with(chat_view);
    ui.handle_submit("/status".to_string());
    let screen = screen(&ui, 80, 24);
    assert!(screen.contains("workspace: /tmp/demo-workspace"));
    assert!(screen.contains("conversation: cnv_chat"));
    assert!(screen.contains("goal: one-click CSV export"));
    assert!(screen.contains("transcript entries: 1"));
}

#[test]
fn work_overlay_lists_ready_rows() {
    let mut ui = ChatUi::new(
        view(Vec::new(), Vec::new(), 0, None),
        snapshot_with_ready_work(),
        WORKSPACE.to_string(),
    );
    ui.handle_submit("/work".to_string());
    let screen = screen(&ui, 80, 24);
    assert!(screen.contains("work_trace [model] ready"));
    assert!(screen.contains("Trace the checkout path"));
}

#[test]
fn detail_overlay_opens_from_the_work_snapshot() {
    let mut ui = ui_with(view(Vec::new(), Vec::new(), 0, None));
    ui.set_work(snapshot_with_detail());
    assert_eq!(
        ui.overlay(),
        Some(ChatOverlay::Detail("work_trace".to_string()))
    );
    let screen = screen(&ui, 80, 24);
    assert!(screen.contains("objective: Trace the checkout path"));
    assert!(screen.contains("inputs: art_brief@1"));
    // Esc closes; a detail-free snapshot does not reopen it
    ui.handle(ChatKey::Esc);
    assert_eq!(ui.overlay(), None);
    ui.set_work(empty_snapshot());
    assert_eq!(ui.overlay(), None);
}

#[test]
fn artifact_overlay_opens_from_the_work_snapshot() {
    let mut ui = ui_with(view(Vec::new(), Vec::new(), 0, None));
    ui.set_work(snapshot_with_viewer());
    assert_eq!(
        ui.overlay(),
        Some(ChatOverlay::Artifact("art_brief@1".to_string()))
    );
    let screen = screen(&ui, 80, 24);
    assert!(screen.contains("# Opportunity brief"));
    assert!(screen.contains("Team reports are stuck."));
}

#[test]
fn overlay_arrows_cycle_the_static_overlays() {
    let mut ui = ui_with(view(Vec::new(), Vec::new(), 0, None));
    ui.handle(ChatKey::Char('?'));
    assert_eq!(ui.overlay(), Some(ChatOverlay::Help));
    ui.handle(ChatKey::Right);
    assert_eq!(ui.overlay(), Some(ChatOverlay::Status));
    ui.handle(ChatKey::Right);
    assert_eq!(ui.overlay(), Some(ChatOverlay::Work));
    ui.handle(ChatKey::Left);
    assert_eq!(ui.overlay(), Some(ChatOverlay::Status));
    // under overlay focus, arrows scroll the transcript instead of the cursor
    ui.handle(ChatKey::Up);
    assert_eq!(ui.transcript_offset(), SCROLL_STEP_LINES);
}

#[test]
fn set_status_appears_in_the_footer() {
    let mut ui = ui_with(view(Vec::new(), Vec::new(), 0, None));
    ui.set_status("refreshed");
    assert!(screen(&ui, 80, 24).contains("refreshed"));
}

#[test]
fn activity_lines_render_and_are_capped() {
    let mut ui = ui_with(view(Vec::new(), Vec::new(), 0, None));
    for i in 0..(relay_ui::chat::ACTIVITY_MAX_LINES + 30) {
        ui.push_event(&ConversationEvent::Activity {
            text: format!("activity line {i}"),
        });
    }
    let wide = screen(&ui, 80, 24);
    assert!(!wide.contains("activity line 0"), "oldest dropped");
    assert!(!wide.contains("activity line 29"));
    assert!(wide.contains("activity line 229"), "newest kept");
}
