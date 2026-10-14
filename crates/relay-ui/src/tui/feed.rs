//! The controller feed seam: the read-only controller surface the terminal
//! interface consumes, expressed as a trait so a runtime controller can
//! drive the interface when one lands. The store-backed implementation
//! (`relay-cli`'s `DirectStoreOps`) was retired with the work-graph
//! restructuring and survives only in `relay-cli`'s inert `src/ops.rs`
//! reference; nothing in the compiled workspace implements this trait
//! today. Only this trait — not a concrete controller type — crosses into
//! `relay-ui`.

use relay_core::project_state::ProjectState;

/// Read-only snapshot provider for the terminal interface.
///
/// Mirrors the structural `TuiController` port of the retired
/// `src/tui/view-model.ts` (removed from the tree 2026-10-14):
/// reads may fail before a project is initialized, and callers fall back to
/// the empty projection (the reference's `readControllerState`).
pub trait ControllerFeed {
    /// The workflow state snapshot, or `None` when no project is
    /// initialized yet (the interface then renders the empty projection).
    fn state(&self) -> Option<ProjectState>;

    /// The project id, or `None` when no project is initialized yet.
    fn project_id(&self) -> Option<String>;
}

/// Adapter mirroring the reference's `readControllerState`: the feed's
/// snapshot or the empty projection when the controller is uninitialized.
pub fn read_feed_state(feed: &dyn ControllerFeed) -> ProjectState {
    feed.state()
        .unwrap_or_else(relay_core::create_empty_project_state)
}

/// Whether the feed has no initialized project yet.
pub fn feed_is_uninitialized(feed: &dyn ControllerFeed) -> bool {
    match feed.project_id() {
        Some(project_id) => project_id.is_empty(),
        // No project id accessor semantics: fall back to the state snapshot.
        None => read_feed_state(feed).project_id().is_empty(),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    struct Uninitialized;

    impl ControllerFeed for Uninitialized {
        fn state(&self) -> Option<ProjectState> {
            None
        }
        fn project_id(&self) -> Option<String> {
            None
        }
    }

    struct Initialized;

    impl ControllerFeed for Initialized {
        fn state(&self) -> Option<ProjectState> {
            Some(relay_core::create_empty_project_state())
        }
        fn project_id(&self) -> Option<String> {
            Some("prj_1".to_string())
        }
    }

    #[test]
    fn uninitialized_feeds_read_as_the_empty_projection() {
        let state = read_feed_state(&Uninitialized);
        assert_eq!(state.project_id(), "");
        assert_eq!(state.current_mode(), relay_core::WorkflowMode::Discovery,);
        assert!(feed_is_uninitialized(&Uninitialized));
    }

    #[test]
    fn initialized_feeds_report_their_project() {
        assert!(!feed_is_uninitialized(&Initialized));
    }
}
