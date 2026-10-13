//! Deterministic workflow transition policy. Given a project state and a
//! trigger, `evaluate_transition` produces the single controller outcome.
//! Models may propose transitions; only this policy emits outcomes.
//! `can_enter_mode` answers whether a mode may be entered and why not,
//! `earliest_affected_stage` routes revision loops back to the earliest stage
//! invalidated by an upstream revision, and `should_auto_advance` bounds
//! auto-advancement by autonomy. Ported from `src/core/transitions.ts`.

use crate::artifacts::{ArtifactKind, downstream_artifact_kinds, is_contractual_artifact_kind};
use crate::modes::{
    AutonomyBound, MODE_CONTRACTS, WORKFLOW_MODES, WorkflowMode, autonomy_target, mode_index,
};
use crate::outcomes::ControllerOutcome;
use crate::project_state::ProjectState;

/// What the controller is reacting to.
#[derive(Debug, Clone, PartialEq)]
pub enum TransitionTrigger {
    /// The current mode finished its work.
    ModeComplete {
        /// Evidence the mode produced.
        evidence: Vec<String>,
    },
    /// The user approved an artifact.
    ArtifactApproved {
        /// The approved artifact's id.
        artifact_id: String,
    },
    /// Verification failed.
    VerificationFailed {
        /// The earliest stage affected by the failing work.
        earliest_affected_stage: WorkflowMode,
    },
    /// The user explicitly asked to enter a mode.
    UserRequest {
        /// The requested mode.
        target: WorkflowMode,
    },
    /// The workflow was resumed.
    Resume,
}

/// The mode that produces each artifact kind.
pub fn kind_to_mode(kind: ArtifactKind) -> WorkflowMode {
    WORKFLOW_MODES
        .iter()
        .copied()
        .find(|mode| mode.contract().produced_artifact == kind)
        .unwrap_or(WorkflowMode::Discovery)
}

/// Why a mode cannot be entered right now.
#[derive(Debug, Clone, PartialEq, Eq)]
enum EntryFailure {
    Paused { reason: String },
    MissingInput { kind: ArtifactKind, reason: String },
    StaleInput { kind: ArtifactKind, reason: String },
    ForwardSkip { reason: String },
    Verification { reason: String },
}

impl EntryFailure {
    fn reason(&self) -> &str {
        match self {
            EntryFailure::Paused { reason }
            | EntryFailure::MissingInput { reason, .. }
            | EntryFailure::StaleInput { reason, .. }
            | EntryFailure::ForwardSkip { reason }
            | EntryFailure::Verification { reason } => reason,
        }
    }

    fn options(&self) -> Vec<String> {
        match self {
            EntryFailure::Paused { .. } => vec!["/resume".to_string()],
            EntryFailure::StaleInput { .. } => vec!["/revise".to_string(), "/status".to_string()],
            EntryFailure::Verification { .. } => {
                vec![
                    "/revise".to_string(),
                    "/inspect verification-report".to_string(),
                ]
            }
            EntryFailure::ForwardSkip { .. } => vec!["/revise".to_string(), "/status".to_string()],
            EntryFailure::MissingInput { .. } => vec!["/status".to_string()],
        }
    }
}

fn verification_summary(state: &ProjectState) -> String {
    let verification = state.verification();
    if verification.last_run_at.is_none() {
        return "no verification run has been recorded yet".to_string();
    }
    format!(
        "last run: {} passed, {} failed, {} unevaluated",
        verification.passed_count, verification.failed_count, verification.unevaluated_count
    )
}

fn inspect_mode_entry(state: &ProjectState, target: WorkflowMode) -> Result<(), EntryFailure> {
    if let Some(pause) = state.paused() {
        return Err(EntryFailure::Paused {
            reason: format!(
                "workflow is paused ({}); resume first with /resume",
                pause.reason
            ),
        });
    }
    let contract = target.contract();
    for kind in contract.required_inputs {
        let Some(artifact_id) = state.artifact_id_by_kind(*kind) else {
            return Err(EntryFailure::MissingInput {
                kind: *kind,
                reason: format!(
                    "cannot enter {}: the required \"{}\" artifact does not exist yet; \
                     complete {} first (currently in {})",
                    target.as_str(),
                    kind.as_str(),
                    kind_to_mode(*kind).as_str(),
                    state.current_mode().as_str()
                ),
            });
        };
        let Some(artifact) = state.artifact(artifact_id) else {
            return Err(EntryFailure::MissingInput {
                kind: *kind,
                reason: format!(
                    "cannot enter {}: the required \"{}\" input points at artifact \"{}\" \
                     which is missing from project state; the event log may be corrupted",
                    target.as_str(),
                    kind.as_str(),
                    artifact_id
                ),
            });
        };
        if artifact.status() == crate::project_state::ArtifactStatus::Stale {
            return Err(EntryFailure::StaleInput {
                kind: *kind,
                reason: format!(
                    "cannot enter {}: the required \"{}\" artifact ({}) is stale; \
                     revise or regenerate it from its updated upstream inputs first",
                    target.as_str(),
                    kind.as_str(),
                    artifact_id
                ),
            });
        }
    }
    if mode_index(target) as i64 - mode_index(state.current_mode()) as i64 > 1 {
        return Err(EntryFailure::ForwardSkip {
            reason: format!(
                "workflow modes run in order: cannot jump from {} to {}; \
                 advance one mode at a time or use /revise to go back",
                state.current_mode().as_str(),
                target.as_str()
            ),
        });
    }
    if target == WorkflowMode::Delivery && state.verification().passed != Some(true) {
        return Err(EntryFailure::Verification {
            reason: format!(
                "cannot enter delivery: verification has not passed ({}); \
                 complete the verification mode and address failing criteria first",
                verification_summary(state)
            ),
        });
    }
    Ok(())
}

/// Whether `target` may be entered right now, with an actionable reason either
/// way.
pub fn can_enter_mode(state: &ProjectState, target: WorkflowMode) -> ModeEntryCheck {
    match inspect_mode_entry(state, target) {
        Ok(()) => ModeEntryCheck {
            ok: true,
            reason: format!(
                "all requirements to enter {} are satisfied",
                target.as_str()
            ),
        },
        Err(failure) => ModeEntryCheck {
            ok: false,
            reason: failure.reason().to_string(),
        },
    }
}

/// Result of [`can_enter_mode`]: allowed or not, plus the user-facing reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModeEntryCheck {
    /// Whether the mode may be entered.
    pub ok: bool,
    /// Why (or why not), user-facing.
    pub reason: String,
}

/// Earliest workflow stage affected by a revision of `revised_kind`: the
/// lowest mode whose produced artifact already exists and is transitively
/// downstream of the revised kind. When no downstream artifact exists yet,
/// work resumes in the mode that produces the revised kind itself.
pub fn earliest_affected_stage(state: &ProjectState, revised_kind: ArtifactKind) -> WorkflowMode {
    let downstream = downstream_artifact_kinds(revised_kind);
    for mode in WORKFLOW_MODES {
        let kind = mode.contract().produced_artifact;
        if state.artifact_id_by_kind(kind).is_some() && downstream.contains(&kind) {
            return mode;
        }
    }
    kind_to_mode(revised_kind)
}

fn needs_user_from_failure(failure: &EntryFailure) -> ControllerOutcome {
    let reason = failure.reason().to_string();
    ControllerOutcome::NeedsUser {
        question: reason.clone(),
        options: failure.options(),
        reason,
    }
}

fn advance_after_gate(
    state: &ProjectState,
    next: WorkflowMode,
    on_advance: &str,
) -> ControllerOutcome {
    match inspect_mode_entry(state, next) {
        Ok(()) => ControllerOutcome::Advance {
            to: next,
            reason: on_advance.to_string(),
        },
        Err(EntryFailure::StaleInput { kind, reason }) => ControllerOutcome::Revise {
            stage: earliest_affected_stage(state, kind),
            reason: reason.to_string(),
        },
        Err(failure) => needs_user_from_failure(&failure),
    }
}

/// Produce the single controller outcome for a trigger in the current state.
pub fn evaluate_transition(state: &ProjectState, trigger: &TransitionTrigger) -> ControllerOutcome {
    match trigger {
        TransitionTrigger::ModeComplete { evidence } => {
            let contract = state.current_mode().contract();
            let produced_kind = contract.produced_artifact;
            let Some(produced_id) = state.artifact_id_by_kind(produced_kind) else {
                return ControllerOutcome::NeedsUser {
                    question: format!(
                        "The {} mode has not produced its \"{}\" artifact yet; \
                         finish that work before completing {}.",
                        state.current_mode().as_str(),
                        produced_kind.as_str(),
                        state.current_mode().as_str()
                    ),
                    options: vec![
                        format!("produce the {}", produced_kind.as_str()),
                        "change mode".to_string(),
                    ],
                    reason: format!(
                        "{} is incomplete: no {} artifact exists",
                        state.current_mode().as_str(),
                        produced_kind.as_str()
                    ),
                };
            };
            let produced = state.artifact(produced_id);
            let produced_status = produced.map(|artifact| artifact.status());
            if is_contractual_artifact_kind(produced_kind)
                && produced_status != Some(crate::project_state::ArtifactStatus::Approved)
            {
                let version = produced
                    .map(|artifact| artifact.current_version())
                    .unwrap_or(1);
                let status_text = produced
                    .map(|artifact| artifact.status().as_str().to_string())
                    .unwrap_or_else(|| "unknown".to_string());
                return ControllerOutcome::NeedsUser {
                    question: format!(
                        "Approve the {} (v{}) to continue",
                        produced_kind.as_str(),
                        version
                    ),
                    options: vec!["approve".to_string(), "request changes".to_string()],
                    reason: format!(
                        "{} is a contractual checkpoint; its current status is {} \
                         and only the user can approve it",
                        produced_kind.as_str(),
                        status_text
                    ),
                };
            }
            let Some(next) = contract.next_mode else {
                return ControllerOutcome::NeedsUser {
                    question: "Workflow complete; delivery actions require explicit authorization"
                        .to_string(),
                    options: vec![
                        "review delivery report".to_string(),
                        "authorize delivery".to_string(),
                    ],
                    reason:
                        "delivery mode is the final stage; external delivery actions need explicit user authorization"
                            .to_string(),
                };
            };
            let evidence_suffix = if evidence.is_empty() {
                String::new()
            } else {
                format!(" ({})", evidence.join("; "))
            };
            advance_after_gate(
                state,
                next,
                &format!(
                    "{} is complete{}; advancing to {}",
                    state.current_mode().as_str(),
                    evidence_suffix,
                    next.as_str()
                ),
            )
        }
        TransitionTrigger::ArtifactApproved { artifact_id } => {
            let Some(artifact) = state.artifact(artifact_id) else {
                return ControllerOutcome::NeedsUser {
                    question: format!(
                        "The approved artifact \"{}\" is not part of this project; \
                         check the artifact id with /status before approving.",
                        artifact_id
                    ),
                    options: vec!["/status".to_string()],
                    reason: format!(
                        "artifact-approved trigger references unknown artifact \"{}\"",
                        artifact_id
                    ),
                };
            };
            let contract = state.current_mode().contract();
            let produced_kind = contract.produced_artifact;
            let completes_current_mode = is_contractual_artifact_kind(produced_kind)
                && state.artifact_id_by_kind(produced_kind) == Some(artifact_id.as_str())
                && artifact.kind() == produced_kind;
            if !completes_current_mode {
                return ControllerOutcome::Continue {
                    reason: format!(
                        "approved {} does not gate {} (waiting on {}); continuing current work",
                        artifact.kind().as_str(),
                        state.current_mode().as_str(),
                        produced_kind.as_str()
                    ),
                };
            }
            let Some(next) = contract.next_mode else {
                return ControllerOutcome::NeedsUser {
                    question: "Workflow complete; delivery actions require explicit authorization"
                        .to_string(),
                    options: vec![
                        "review delivery report".to_string(),
                        "authorize delivery".to_string(),
                    ],
                    reason:
                        "delivery mode is the final stage; external delivery actions need explicit user authorization"
                            .to_string(),
                };
            };
            advance_after_gate(
                state,
                next,
                &format!(
                    "{} approved; advancing to {}",
                    artifact.kind().as_str(),
                    next.as_str()
                ),
            )
        }
        TransitionTrigger::VerificationFailed {
            earliest_affected_stage,
        } => ControllerOutcome::Revise {
            stage: *earliest_affected_stage,
            reason: format!(
                "verification failed; routing back to {} (the earliest stage affected by the failing work) \
                 to address the failing criteria",
                earliest_affected_stage.as_str()
            ),
        },
        TransitionTrigger::UserRequest { target } => match inspect_mode_entry(state, *target) {
            Ok(()) => ControllerOutcome::Advance {
                to: *target,
                reason: format!(
                    "user override (from {} to {})",
                    state.current_mode().as_str(),
                    target.as_str()
                ),
            },
            Err(failure) => needs_user_from_failure(&failure),
        },
        TransitionTrigger::Resume => {
            if let Some(pause) = state.paused() {
                ControllerOutcome::NeedsUser {
                    question: pause
                        .question
                        .clone()
                        .unwrap_or_else(|| "resumed; continue".to_string()),
                    options: if pause.options.is_empty() {
                        vec!["continue".to_string()]
                    } else {
                        pause.options.clone()
                    },
                    reason: format!(
                        "workflow is paused ({}); answer the pause to continue",
                        pause.reason
                    ),
                }
            } else {
                ControllerOutcome::Continue {
                    reason: "resumed; continue".to_string(),
                }
            }
        }
    }
}

/// Whether the controller may keep advancing without asking the user, bounded
/// by the current autonomy setting. Never true while paused.
pub fn should_auto_advance(state: &ProjectState, outcome: &ControllerOutcome) -> bool {
    if state.paused().is_some() {
        return false;
    }
    match state.autonomy() {
        AutonomyBound::Off => false,
        AutonomyBound::NextCheckpoint => outcome.kind() == crate::outcomes::OutcomeKind::Advance,
        AutonomyBound::Workflow => matches!(
            outcome.kind(),
            crate::outcomes::OutcomeKind::Advance | crate::outcomes::OutcomeKind::Continue
        ),
        AutonomyBound::UntilMode(_) => {
            autonomy_target(state.autonomy()).is_some_and(|target| state.current_mode() != target)
        }
    }
}

/// All mode contracts, exported for consumers that render mode metadata.
pub fn mode_contracts() -> &'static [crate::modes::ModeContract] {
    &MODE_CONTRACTS
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::artifacts::ArtifactKind;
    use crate::fakes::{full_pipeline_approved, initialized_default, make_event, replay_events};
    use crate::payloads::*;

    fn mode_entered(to: WorkflowMode, from: Option<WorkflowMode>) -> crate::events::RelayEvent {
        make_event(
            crate::events::RelayEventType::ModeEntered,
            EventPayload::ModeEntered(ModeEnteredPayload {
                from,
                to,
                reason: format!("entered {}", to.as_str()),
                source: AutoUserSource::Auto,
            }),
        )
    }

    #[test]
    fn requires_the_mode_contract_inputs_to_exist() {
        let state = replay_events(&[initialized_default()]);
        let check = can_enter_mode(&state, WorkflowMode::Specification);
        assert!(!check.ok);
        assert!(
            check.reason.contains("opportunity-brief"),
            "{}",
            check.reason
        );
    }

    #[test]
    fn rejects_forward_jumps_beyond_the_next_mode() {
        let state = replay_events(&full_pipeline_approved()[..8].to_vec());
        let check = can_enter_mode(&state, WorkflowMode::Planning);
        assert!(!check.ok);
        assert!(check.reason.contains("workflow modes run in order"));
        assert!(check.reason.contains("/revise"));
    }

    #[test]
    fn maps_each_kind_to_the_mode_that_produces_it() {
        assert_eq!(
            kind_to_mode(ArtifactKind::OpportunityBrief),
            WorkflowMode::Discovery
        );
        assert_eq!(kind_to_mode(ArtifactKind::Prd), WorkflowMode::Specification);
        assert_eq!(
            kind_to_mode(ArtifactKind::Trd),
            WorkflowMode::TechnicalDesign
        );
        assert_eq!(
            kind_to_mode(ArtifactKind::ExecutionPlan),
            WorkflowMode::Planning
        );
        assert_eq!(
            kind_to_mode(ArtifactKind::DecisionLog),
            WorkflowMode::Implementation
        );
        assert_eq!(
            kind_to_mode(ArtifactKind::VerificationReport),
            WorkflowMode::Verification
        );
        assert_eq!(
            kind_to_mode(ArtifactKind::DeliveryReport),
            WorkflowMode::Delivery
        );
    }

    #[test]
    fn ends_in_needs_user_when_delivery_completes() {
        let state = replay_events(&full_pipeline_approved());
        let outcome = evaluate_transition(
            &state,
            &TransitionTrigger::ModeComplete { evidence: vec![] },
        );
        match outcome {
            ControllerOutcome::NeedsUser { question, .. } => assert_eq!(
                question,
                "Workflow complete; delivery actions require explicit authorization"
            ),
            other => panic!("unexpected outcome {other:?}"),
        }
    }

    #[test]
    fn surfaces_pause_rejections_as_needs_user() {
        let mut events = vec![initialized_default()];
        events.push(make_event(
            crate::events::RelayEventType::WorkflowPaused,
            EventPayload::WorkflowPaused(WorkflowPausedPayload {
                by: PauseActor::User,
                reason: "reviewing".to_string(),
                question: None,
                options: vec![],
            }),
        ));
        let state = replay_events(&events);
        let outcome = evaluate_transition(
            &state,
            &TransitionTrigger::ModeComplete { evidence: vec![] },
        );
        assert!(matches!(outcome, ControllerOutcome::NeedsUser { .. }));
        if let ControllerOutcome::NeedsUser { question, .. } = outcome {
            assert!(question.contains("/resume"), "{question}");
        }
    }

    #[test]
    fn honors_valid_user_requests_with_a_user_override_advance() {
        let state = replay_events(&full_pipeline_approved()[..12].to_vec());
        let outcome = evaluate_transition(
            &state,
            &TransitionTrigger::UserRequest {
                target: WorkflowMode::Specification,
            },
        );
        match outcome {
            ControllerOutcome::Advance { to, reason } => {
                assert_eq!(to, WorkflowMode::Specification);
                assert!(reason.contains("user override"));
            }
            other => panic!("unexpected outcome {other:?}"),
        }
    }

    #[test]
    fn re_presents_the_pause_question_on_resume() {
        let mut events = vec![initialized_default()];
        events.push(make_event(
            crate::events::RelayEventType::WorkflowPaused,
            EventPayload::WorkflowPaused(WorkflowPausedPayload {
                by: PauseActor::Policy,
                reason: "approval needed".to_string(),
                question: Some("Approve the prd (v1)?".to_string()),
                options: vec!["approve".to_string(), "request changes".to_string()],
            }),
        ));
        let state = replay_events(&events);
        let outcome = evaluate_transition(&state, &TransitionTrigger::Resume);
        match outcome {
            ControllerOutcome::NeedsUser {
                question, options, ..
            } => {
                assert_eq!(question, "Approve the prd (v1)?");
                assert_eq!(
                    options,
                    vec!["approve".to_string(), "request changes".to_string()]
                );
            }
            other => panic!("unexpected outcome {other:?}"),
        }
    }

    #[test]
    fn returns_the_earliest_mode_with_an_existing_downstream_artifact() {
        let complete = replay_events(&full_pipeline_approved());
        assert_eq!(
            earliest_affected_stage(&complete, ArtifactKind::OpportunityBrief),
            WorkflowMode::Specification
        );
        assert_eq!(
            earliest_affected_stage(&complete, ArtifactKind::Prd),
            WorkflowMode::TechnicalDesign
        );
        assert_eq!(
            earliest_affected_stage(&complete, ArtifactKind::Trd),
            WorkflowMode::Planning
        );
        assert_eq!(
            earliest_affected_stage(&complete, ArtifactKind::ExecutionPlan),
            WorkflowMode::Verification
        );
    }

    #[test]
    fn should_auto_advance_respects_autonomy_and_pause() {
        let advance = ControllerOutcome::Advance {
            to: WorkflowMode::Specification,
            reason: "r".to_string(),
        };
        let continue_outcome = ControllerOutcome::Continue {
            reason: "r".to_string(),
        };
        let off = replay_events(&[initialized_with_autonomy("off")]);
        assert!(!should_auto_advance(&off, &advance));
        let next_checkpoint = replay_events(&[initialized_with_autonomy("next-checkpoint")]);
        assert!(should_auto_advance(&next_checkpoint, &advance));
        assert!(!should_auto_advance(&next_checkpoint, &continue_outcome));
        let workflow = replay_events(&[initialized_with_autonomy("workflow")]);
        assert!(should_auto_advance(&workflow, &advance));
        assert!(should_auto_advance(&workflow, &continue_outcome));
        let until = replay_events(&[initialized_with_autonomy("until:planning")]);
        assert!(should_auto_advance(&until, &advance));
        let at_target_events = vec![
            initialized_with_autonomy("until:planning"),
            mode_entered(WorkflowMode::Planning, Some(WorkflowMode::TechnicalDesign)),
        ];
        let at_target = replay_events(&at_target_events);
        assert!(!should_auto_advance(&at_target, &advance));
    }

    fn initialized_with_autonomy(autonomy: &str) -> crate::events::RelayEvent {
        make_event(
            crate::events::RelayEventType::ProjectInitialized,
            EventPayload::ProjectInitialized(ProjectInitializedPayload {
                goal: "goal".to_string(),
                workspace_root: "/tmp/relay-workspace".to_string(),
                initial_autonomy: autonomy.to_string(),
            }),
        )
    }

    #[test]
    fn mode_entry_check_reports_satisfied_requirements() {
        let state = replay_events(&[initialized_default()]);
        let check = can_enter_mode(&state, WorkflowMode::Discovery);
        assert!(check.ok);
        assert!(check.reason.contains("all requirements"));
        assert_eq!(mode_contracts().len(), 7);
    }
}
