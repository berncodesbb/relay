//! The work-centric terminal interface: immutable snapshots of the
//! application's view models, a typed keymap, ratatui screens, and the
//! terminal-restoration guard.
//!
//! Nothing here decides domain policy. The screens render what
//! [`relay_app`] handlers returned; the only "actions" the interface takes
//! are typed requests ([`TuiRequest`]) the driver maps onto application
//! commands. Drawing targets a ratatui backend — `TestBackend` in tests, a
//! crossterm backend in the CLI — so no TTY is ever required to construct,
//! render, or snapshot any of this.

pub mod keymap;
pub mod screens;
pub mod terminal;

use relay_app::ProjectSession;
use relay_app::views::{DecisionListView, SummaryView, WorkDetailView, WorkListView};

pub use keymap::{Key, Keymap, TuiAction};
pub use terminal::{RestoreFn, TerminalGuard};

/// Build a snapshot of one session's current derivation: summary, ready,
/// blocked, and decisions, freshly derived from the folded graph. Pure
/// projection — no policy decisions live here.
pub fn snapshot_from_session(session: &ProjectSession) -> WorkSnapshot {
    let graph = session.graph();
    let evaluation = session.evaluation();
    let catalog = relay_workflows::WorkflowCatalog::builtin();
    WorkSnapshot::new(
        relay_app::views::summary_view(&graph, &evaluation, &catalog),
        relay_app::views::ready_view(&graph, &evaluation, &catalog),
        relay_app::views::blocked_view(&graph, &evaluation, &catalog),
        relay_app::views::decision_list_view(&graph),
    )
}

/// The rendered-Markdown viewer state.
#[derive(Debug, Clone, PartialEq)]
pub struct ViewerState {
    /// The viewer's title (usually the artifact reference).
    pub title: String,
    /// The rendered Markdown.
    pub markdown: String,
}

/// One immutable snapshot of everything the interface shows. Rebuilt after
/// every applied action; never mutated in place.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkSnapshot {
    /// The project summary screen's data.
    pub summary: SummaryView,
    /// Ready work.
    pub ready: WorkListView,
    /// Blocked work.
    pub blocked: WorkListView,
    /// Decisions (open and resolved).
    pub decisions: DecisionListView,
    /// The inspected work item, when one is open.
    pub detail: Option<WorkDetailView>,
    /// The artifact viewer, when one is open.
    pub viewer: Option<ViewerState>,
}

impl WorkSnapshot {
    /// Assemble a snapshot from application views.
    pub fn new(
        summary: SummaryView,
        ready: WorkListView,
        blocked: WorkListView,
        decisions: DecisionListView,
    ) -> Self {
        WorkSnapshot {
            summary,
            ready,
            blocked,
            decisions,
            detail: None,
            viewer: None,
        }
    }
}

/// The screens, in cycle order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    /// Goal, phases, counts, recommendation.
    Summary,
    /// Ready work with explanations.
    Ready,
    /// Blocked work with reasons.
    Blocked,
    /// Decisions with selectable options.
    Decisions,
    /// One work item in full.
    Detail,
    /// A rendered Markdown view.
    Viewer,
}

impl Screen {
    /// The screen's title.
    pub fn title(self) -> &'static str {
        match self {
            Screen::Summary => "summary",
            Screen::Ready => "ready",
            Screen::Blocked => "blocked",
            Screen::Decisions => "decisions",
            Screen::Detail => "work",
            Screen::Viewer => "view",
        }
    }

    /// The next screen in the cycle.
    pub fn next(self) -> Screen {
        match self {
            Screen::Summary => Screen::Ready,
            Screen::Ready => Screen::Blocked,
            Screen::Blocked => Screen::Decisions,
            Screen::Decisions => Screen::Summary,
            Screen::Detail => Screen::Detail,
            Screen::Viewer => Screen::Viewer,
        }
    }

    /// The previous screen in the cycle.
    pub fn previous(self) -> Screen {
        match self {
            Screen::Summary => Screen::Decisions,
            Screen::Ready => Screen::Summary,
            Screen::Blocked => Screen::Ready,
            Screen::Decisions => Screen::Blocked,
            Screen::Detail => Screen::Detail,
            Screen::Viewer => Screen::Viewer,
        }
    }
}

/// A typed request the driver maps onto an application command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TuiRequest {
    /// Resolve a decision with the selected option.
    ResolveDecision {
        /// The decision id.
        decision: String,
        /// The chosen option label.
        choice: String,
    },
    /// Open one work item's detail.
    InspectWork {
        /// The work id.
        work: String,
    },
    /// Open an artifact version's rendered view.
    ViewArtifact {
        /// The artifact reference (`art_<slug>` or `art_<slug>@<n>`).
        reference: String,
    },
    /// Re-read the project (reread the graph and rebuild the snapshot).
    Refresh,
}

/// What handling an action produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TuiOutcome {
    /// Keep running.
    Continue,
    /// The user asked to quit.
    Quit,
    /// The driver should run this request, then refresh the snapshot.
    Request(TuiRequest),
}

/// One selectable row of the decisions screen: an option of an open
/// decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionRow {
    /// The decision id.
    pub decision: String,
    /// The option label.
    pub choice: String,
}

/// The terminal interface state: a snapshot, the active screen, a cursor,
/// help visibility, and a status line. All presentation; no policy.
pub struct WorkTui {
    snapshot: WorkSnapshot,
    screen: Screen,
    viewer_from: Option<Screen>,
    cursor: usize,
    help: bool,
    status: String,
}

impl WorkTui {
    /// A new interface over a snapshot, starting on the summary screen.
    pub fn new(snapshot: WorkSnapshot) -> Self {
        let status = snapshot.summary.recommendation.next.detail.clone();
        WorkTui {
            snapshot,
            screen: Screen::Summary,
            viewer_from: None,
            cursor: 0,
            help: false,
            status,
        }
    }

    /// Replace the snapshot (keeping the screen when it is still valid).
    pub fn set_snapshot(&mut self, snapshot: WorkSnapshot) {
        if self.screen == Screen::Detail && snapshot.detail.is_none() {
            self.screen = Screen::Summary;
        }
        if self.screen == Screen::Viewer && snapshot.viewer.is_none() {
            self.screen = Screen::Summary;
        }
        self.snapshot = snapshot;
        self.clamp_cursor();
    }

    /// Open a work detail (switches to the detail screen).
    pub fn set_snapshot_detail(&mut self, detail: WorkDetailView) {
        self.snapshot.detail = Some(detail);
        self.screen = Screen::Detail;
    }

    /// Open the artifact viewer (switches to the viewer screen, remembering
    /// where to return).
    pub fn set_snapshot_viewer(&mut self, viewer: ViewerState) {
        self.viewer_from = Some(self.screen);
        self.snapshot.viewer = Some(viewer);
        self.screen = Screen::Viewer;
    }

    /// The current snapshot.
    pub fn snapshot(&self) -> &WorkSnapshot {
        &self.snapshot
    }

    /// The active screen.
    pub fn screen(&self) -> Screen {
        self.screen
    }

    /// The cursor position within the active screen's rows.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Whether help is shown.
    pub fn help_visible(&self) -> bool {
        self.help
    }

    /// The status line.
    pub fn status(&self) -> &str {
        &self.status
    }

    /// Set the status line (the driver reports request outcomes here).
    pub fn set_status(&mut self, status: impl Into<String>) {
        self.status = status.into();
    }

    /// The selectable rows of the active screen.
    pub fn rows(&self) -> Vec<String> {
        match self.screen {
            Screen::Summary | Screen::Viewer => Vec::new(),
            Screen::Ready => self
                .snapshot
                .ready
                .works
                .iter()
                .map(|work| work.id.clone())
                .collect(),
            Screen::Blocked => self
                .snapshot
                .blocked
                .works
                .iter()
                .map(|work| work.id.clone())
                .collect(),
            Screen::Decisions => self
                .decision_rows()
                .iter()
                .map(|row| format!("{} — {}", row.decision, row.choice))
                .collect(),
            Screen::Detail => Vec::new(),
        }
    }

    /// The selectable decision rows: every option of every open decision.
    /// Free-form decisions (no options) need text input and are resolved
    /// through the CLI, not the interface.
    pub fn decision_rows(&self) -> Vec<DecisionRow> {
        self.snapshot
            .decisions
            .decisions
            .iter()
            .filter(|decision| decision.status == "open")
            .flat_map(|decision| {
                decision.options.iter().map(move |option| DecisionRow {
                    decision: decision.id.clone(),
                    choice: option.label.clone(),
                })
            })
            .collect()
    }

    /// Handle one action. Returns what the driver should do next.
    pub fn handle(&mut self, action: TuiAction) -> TuiOutcome {
        match action {
            TuiAction::Quit => {
                if self.help {
                    self.help = false;
                    TuiOutcome::Continue
                } else if self.screen == Screen::Viewer {
                    // esc leaves the viewer, returning to where it opened.
                    self.screen = self.viewer_from.take().unwrap_or(Screen::Summary);
                    self.snapshot.viewer = None;
                    TuiOutcome::Continue
                } else {
                    TuiOutcome::Quit
                }
            }
            TuiAction::NextScreen => {
                self.screen = self.screen.next();
                self.cursor = 0;
                self.status = self.status_for_screen();
                TuiOutcome::Continue
            }
            TuiAction::PrevScreen => {
                self.screen = self.screen.previous();
                self.cursor = 0;
                self.status = self.status_for_screen();
                TuiOutcome::Continue
            }
            TuiAction::SelectNext => {
                let len = self.rows().len();
                if len > 0 {
                    self.cursor = (self.cursor + 1) % len;
                }
                TuiOutcome::Continue
            }
            TuiAction::SelectPrev => {
                let len = self.rows().len();
                if len > 0 {
                    self.cursor = if self.cursor == 0 {
                        len - 1
                    } else {
                        self.cursor - 1
                    };
                }
                TuiOutcome::Continue
            }
            TuiAction::Help => {
                self.help = !self.help;
                TuiOutcome::Continue
            }
            TuiAction::Refresh => TuiOutcome::Request(TuiRequest::Refresh),
            TuiAction::Activate => self.activate(),
            TuiAction::View => self.view(),
        }
    }

    fn activate(&mut self) -> TuiOutcome {
        match self.screen {
            Screen::Ready | Screen::Blocked => match self.rows().get(self.cursor) {
                Some(work) => TuiOutcome::Request(TuiRequest::InspectWork { work: work.clone() }),
                None => TuiOutcome::Continue,
            },
            Screen::Decisions => match self.decision_rows().get(self.cursor) {
                Some(row) => TuiOutcome::Request(TuiRequest::ResolveDecision {
                    decision: row.decision.clone(),
                    choice: row.choice.clone(),
                }),
                None => TuiOutcome::Continue,
            },
            _ => TuiOutcome::Continue,
        }
    }

    fn view(&mut self) -> TuiOutcome {
        if self.screen == Screen::Detail
            && let Some(detail) = &self.snapshot.detail
            && let Some(reference) = detail.inputs.first()
        {
            return TuiOutcome::Request(TuiRequest::ViewArtifact {
                reference: reference.clone(),
            });
        }
        TuiOutcome::Continue
    }

    fn status_for_screen(&self) -> String {
        match self.screen {
            Screen::Decisions => {
                let open = self
                    .snapshot
                    .decisions
                    .decisions
                    .iter()
                    .filter(|decision| decision.status == "open")
                    .count();
                format!("{open} open decision(s); enter resolves the selected option")
            }
            Screen::Ready => format!("{} ready; enter inspects", self.snapshot.ready.works.len()),
            Screen::Blocked => format!(
                "{} blocked; enter inspects",
                self.snapshot.blocked.works.len()
            ),
            Screen::Detail => "v opens the first input's view; esc returns".to_string(),
            Screen::Viewer => "esc returns to the work detail".to_string(),
            Screen::Summary => self.snapshot.summary.recommendation.next.detail.clone(),
        }
    }

    fn clamp_cursor(&mut self) {
        let len = self.rows().len();
        if len == 0 {
            self.cursor = 0;
        } else if self.cursor >= len {
            self.cursor = len - 1;
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn screens_cycle_in_order() {
        assert_eq!(Screen::Summary.next(), Screen::Ready);
        assert_eq!(Screen::Ready.next(), Screen::Blocked);
        assert_eq!(Screen::Blocked.next(), Screen::Decisions);
        assert_eq!(Screen::Decisions.next(), Screen::Summary);
        assert_eq!(Screen::Decisions.previous(), Screen::Blocked);
    }
}
