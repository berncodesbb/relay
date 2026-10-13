//! User-intervention policy. Answers one question: does this action require
//! an explicit human decision before Relay may proceed? Approvals of
//! contractual artifacts, changes to workflow direction or autonomy,
//! destructive operations, deployment, delivery, scope expansion beyond the
//! approved plan, and consequential product choices all require the user.
//! Routine reversible work does not. Ported from `src/policies/intervention.ts`.

use crate::project_state::ProjectState;

/// The action classes the policy distinguishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InterventionAction {
    /// Approve a contractual artifact.
    ApproveArtifact,
    /// Revise an existing artifact.
    ReviseArtifact,
    /// Change the workflow mode.
    ChangeMode,
    /// Change the autonomy bound.
    SetAutonomy,
    /// Run a destructive operation.
    DestructiveOperation,
    /// Work beyond the approved execution plan.
    ScopeExpansion,
    /// Deploy.
    Deployment,
    /// Deliver (commit, push, PR, publish).
    Delivery,
    /// Make a consequential product choice.
    ProductChoice,
}

/// The policy's answer for one action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterventionDecision {
    /// Whether an explicit human decision is required.
    pub required: bool,
    /// User-facing reason explaining the decision.
    pub reason: String,
}

/// Whether `action` requires explicit user intervention in `state`.
pub fn requires_user_intervention(
    state: &ProjectState,
    action: InterventionAction,
    detail: Option<&str>,
) -> InterventionDecision {
    match action {
        InterventionAction::ApproveArtifact => InterventionDecision {
            required: true,
            reason: format!(
                "approving a contractual artifact while in {} makes it the binding \
                 contract for every later stage; the user must approve it explicitly",
                state.current_mode().as_str()
            ),
        },
        InterventionAction::ReviseArtifact => InterventionDecision {
            required: true,
            reason: format!(
                "revising an artifact invalidates downstream work derived from it (currently in {}); \
                 the user must request the revision explicitly",
                state.current_mode().as_str()
            ),
        },
        InterventionAction::ChangeMode => InterventionDecision {
            required: true,
            reason: format!(
                "changing the workflow mode (currently {}) redirects all subsequent work; \
                 the user must choose the target mode",
                state.current_mode().as_str()
            ),
        },
        InterventionAction::SetAutonomy => InterventionDecision {
            required: true,
            reason: format!(
                "changing the autonomy bound (currently {}) changes how far Relay runs without asking; \
                 the user must set it",
                state.autonomy()
            ),
        },
        InterventionAction::DestructiveOperation => InterventionDecision {
            required: true,
            reason: "destructive operations can irreversibly destroy user work or system state; \
                     they require explicit user authorization"
                .to_string(),
        },
        InterventionAction::ScopeExpansion => {
            let trimmed = detail.unwrap_or("").trim();
            if trimmed.is_empty() {
                return InterventionDecision {
                    required: false,
                    reason:
                        "no out-of-scope work detected; staying within the approved execution plan requires no intervention"
                            .to_string(),
                };
            }
            InterventionDecision {
                required: true,
                reason: format!(
                    "work expanded beyond the approved execution plan: {trimmed}; \
                     the user must approve the scope change"
                ),
            }
        }
        InterventionAction::Deployment => InterventionDecision {
            required: true,
            reason: "deployment affects systems outside the local workspace and cannot be undone from here; \
                     it requires explicit user authorization"
                .to_string(),
        },
        InterventionAction::Delivery => InterventionDecision {
            required: true,
            reason: "delivery actions (commit, push, PR, deploy, publish) require explicit user authorization"
                .to_string(),
        },
        InterventionAction::ProductChoice => InterventionDecision {
            required: true,
            reason: "consequential product decisions shape the approved artifacts and the delivered result; \
                     the user must make them"
                .to_string(),
        },
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::fakes::{full_pipeline_approved, initialized_default, replay_events};

    #[test]
    fn requires_the_user_for_every_consequential_action() {
        let state = replay_events(&full_pipeline_approved());
        for action in [
            InterventionAction::ApproveArtifact,
            InterventionAction::ReviseArtifact,
            InterventionAction::ChangeMode,
            InterventionAction::SetAutonomy,
            InterventionAction::DestructiveOperation,
            InterventionAction::Deployment,
            InterventionAction::Delivery,
            InterventionAction::ProductChoice,
        ] {
            let result = requires_user_intervention(&state, action, None);
            assert!(result.required, "{action:?}");
            assert!(result.reason.len() > 10);
        }
    }

    #[test]
    fn explains_delivery_authorization_in_terms_of_the_concrete_actions() {
        let state = replay_events(&full_pipeline_approved());
        let result = requires_user_intervention(&state, InterventionAction::Delivery, None);
        assert_eq!(
            result.reason,
            "delivery actions (commit, push, PR, deploy, publish) require explicit user authorization"
        );
    }

    #[test]
    fn anchors_reasons_in_the_current_mode_and_autonomy() {
        let in_discovery = replay_events(&[initialized_default()]);
        assert!(
            requires_user_intervention(&in_discovery, InterventionAction::ApproveArtifact, None)
                .reason
                .contains("discovery")
        );
        assert!(
            requires_user_intervention(&in_discovery, InterventionAction::ChangeMode, None)
                .reason
                .contains("discovery")
        );
        assert!(
            requires_user_intervention(&in_discovery, InterventionAction::SetAutonomy, None)
                .reason
                .contains(&in_discovery.autonomy().as_str())
        );
    }

    #[test]
    fn requires_scope_expansion_only_when_out_of_scope_detail_is_provided() {
        let state = replay_events(&full_pipeline_approved());
        assert!(
            !requires_user_intervention(&state, InterventionAction::ScopeExpansion, None).required
        );
        assert!(
            !requires_user_intervention(&state, InterventionAction::ScopeExpansion, Some(""))
                .required
        );
        assert!(
            !requires_user_intervention(&state, InterventionAction::ScopeExpansion, Some("   "))
                .required
        );
        let flagged = requires_user_intervention(
            &state,
            InterventionAction::ScopeExpansion,
            Some("touches src/extra-module.ts not in the plan"),
        );
        assert!(flagged.required);
        assert!(flagged.reason.contains("src/extra-module.ts"));
    }
}
