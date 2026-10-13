//! Policy facade over the core transition rules. `evaluate_policy_transition`
//! composes the deterministic core transition policy with the intervention
//! policy: completing the delivery mode must always surface a NEEDS_USER
//! outcome citing the delivery intervention rule. `assert_transition_allowed`
//! turns `can_enter_mode` rejections into [`PolicyError`]s the controller can
//! record as `mode.transition.rejected` events. Ported from
//! `src/policies/transitions.ts`.

use crate::modes::WorkflowMode;
use crate::outcomes::ControllerOutcome;
use crate::policies::intervention::{InterventionAction, requires_user_intervention};
use crate::project_state::ProjectState;
use crate::transitions::{TransitionTrigger, can_enter_mode, evaluate_transition};

/// A mode transition rejected by policy; `message` is the user-facing reason.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct PolicyError {
    /// The user-facing rejection reason.
    pub message: String,
}

impl PolicyError {
    /// The TypeScript error class name.
    pub fn name(&self) -> &'static str {
        "PolicyRejectionError"
    }
}

/// The core transition policy composed with the intervention policy.
pub fn evaluate_policy_transition(
    state: &ProjectState,
    trigger: &TransitionTrigger,
) -> ControllerOutcome {
    if matches!(trigger, TransitionTrigger::ModeComplete { .. })
        && state.current_mode() == WorkflowMode::Delivery
    {
        let base = evaluate_transition(state, trigger);
        let intervention = requires_user_intervention(state, InterventionAction::Delivery, None);
        if let ControllerOutcome::NeedsUser {
            question,
            options,
            reason,
        } = base
        {
            return ControllerOutcome::NeedsUser {
                question,
                options,
                reason: format!("{reason}; policy: {}", intervention.reason),
            };
        }
        return ControllerOutcome::NeedsUser {
            question: "Workflow complete; delivery actions require explicit authorization"
                .to_string(),
            options: vec![
                "review delivery report".to_string(),
                "authorize delivery".to_string(),
            ],
            reason: intervention.reason,
        };
    }
    evaluate_transition(state, trigger)
}

/// Fail with [`PolicyError`] unless `target` may be entered right now.
pub fn assert_transition_allowed(
    state: &ProjectState,
    target: WorkflowMode,
) -> Result<(), PolicyError> {
    let check = can_enter_mode(state, target);
    if check.ok {
        Ok(())
    } else {
        Err(PolicyError {
            message: check.reason,
        })
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::fakes::{full_pipeline_approved, initialized_default, replay_events};

    #[test]
    fn delegates_to_the_core_transition_policy_for_ordinary_triggers() {
        let events = full_pipeline_approved();
        let state = replay_events(&events[..3]);
        let trigger = TransitionTrigger::ModeComplete {
            evidence: vec!["research done".to_string()],
        };
        assert_eq!(
            evaluate_policy_transition(&state, &trigger),
            evaluate_transition(&state, &trigger)
        );
    }

    #[test]
    fn forces_completing_delivery_into_needs_user_citing_the_delivery_intervention() {
        let state = replay_events(&full_pipeline_approved());
        let outcome = evaluate_policy_transition(
            &state,
            &TransitionTrigger::ModeComplete { evidence: vec![] },
        );
        match outcome {
            ControllerOutcome::NeedsUser { reason, .. } => assert!(
                reason
                    .contains("delivery actions (commit, push, PR, deploy, publish) require explicit user authorization")
            ),
            other => panic!("unexpected outcome {other:?}"),
        }
    }

    #[test]
    fn passes_silently_when_the_target_mode_may_be_entered() {
        let events = full_pipeline_approved();
        let state = replay_events(&events[..3]);
        assert!(assert_transition_allowed(&state, WorkflowMode::Specification).is_ok());
    }

    #[test]
    fn throws_the_actionable_reason_otherwise() {
        let state = replay_events(&[initialized_default()]);
        let error = assert_transition_allowed(&state, WorkflowMode::Specification)
            .expect_err("specification entry must be rejected without a brief");
        assert_eq!(error.name(), "PolicyRejectionError");
        assert!(error.message.contains("opportunity-brief"));
        assert_eq!(error.to_string(), error.message);
    }
}
