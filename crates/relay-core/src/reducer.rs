//! Pure event fold for Relay project state. `apply_event` folds one validated
//! event onto a [`ProjectState`] and `replay_project` folds a whole
//! append-only log. Both are pure: no clocks, randomness, or IO; every
//! timestamp comes from the event itself. Integrity violations (duplicate
//! initialization, broken revision chains, out-of-order sequences, mixed
//! projects) fail with [`CoreError::EventFold`] carrying the same diagnostics
//! as the TypeScript `EventFoldError`. Ported from `src/core/reducer.ts`.

use crate::artifacts::downstream_artifact_kinds;
use crate::events::{CoreError, RelayEvent, RelayEventType};
use crate::modes::{AutonomyBound, parse_autonomy_bound};
use crate::payloads::EventPayload;
use crate::payloads::PauseActor;
use crate::project_state::{
    ArtifactState, ArtifactStatus, OutcomeRecord, PauseState, ProjectState, RejectedTransition,
    RevisionRecord, TransitionRecord, TransitionSource, VerificationSummary,
    create_empty_project_state,
};

/// Cap on remembered rejected transitions; oldest are dropped beyond this.
const MAX_REJECTED_TRANSITIONS: usize = 50;

fn require_artifact<'a>(
    state: &'a ProjectState,
    event_type: &str,
    sequence: u64,
    artifact_id: &str,
) -> Result<&'a ArtifactState, CoreError> {
    state
        .artifacts
        .get(artifact_id)
        .ok_or_else(|| CoreError::EventFold {
            detail: format!(
                "event \"{}\" (sequence {}) references unknown artifact \"{}\"; \
             the event log may be corrupted or was written by a different project",
                event_type, sequence, artifact_id
            ),
        })
}

fn fold_error(detail: String) -> CoreError {
    CoreError::EventFold { detail }
}

/// Fold one validated event onto `state`, returning the next state.
pub fn apply_event(state: &ProjectState, event: &RelayEvent) -> Result<ProjectState, CoreError> {
    // Version guard mirrors the TypeScript reducer: a typed event with a
    // schema version this build does not understand must fail loudly.
    let known_version = event.event_type.payload_version();
    if event.schema_version != known_version {
        return Err(CoreError::IncompatibleEventVersion {
            event_type: event.event_type.as_str().to_string(),
            found: event.schema_version,
            expected: known_version,
        });
    }

    // Common tail for every event: bump the counter and record activity.
    let mut next = state.clone();
    next.stats.events += 1;
    next.last_activity_at = event.timestamp.clone();

    match (&event.event_type, &event.payload) {
        (RelayEventType::ProjectInitialized, EventPayload::ProjectInitialized(payload)) => {
            if !state.goal.is_empty() {
                return Err(fold_error(format!(
                    "duplicate project.initialized at sequence {}: project \"{}\" \
                     was already initialized at {}; start a new project instead of reinitializing",
                    event.sequence, event.project_id, state.initialized_at
                )));
            }
            next.project_id = event.project_id.clone();
            next.goal = payload.goal.clone();
            next.workspace_root = Some(payload.workspace_root.clone());
            next.initialized_at = event.timestamp.clone();
            next.autonomy = parse_autonomy_bound(&payload.initial_autonomy)
                .unwrap_or(AutonomyBound::NextCheckpoint);
            next.current_mode = crate::modes::WorkflowMode::Discovery;
            Ok(next)
        }
        (RelayEventType::ModeEntered, EventPayload::ModeEntered(payload)) => {
            next.current_mode = payload.to;
            // A mode entry clears a policy pause but keeps a user pause.
            if state
                .paused
                .as_ref()
                .is_some_and(|pause| pause.by == PauseActor::Policy)
            {
                next.paused = None;
            }
            next.transitions.push(TransitionRecord {
                at: event.timestamp.clone(),
                from: payload.from,
                to: payload.to,
                reason: payload.reason.clone(),
                source: match payload.source {
                    crate::payloads::AutoUserSource::Auto => TransitionSource::Auto,
                    crate::payloads::AutoUserSource::User => TransitionSource::User,
                },
            });
            Ok(next)
        }
        (RelayEventType::ModeTransitionRejected, EventPayload::ModeTransitionRejected(payload)) => {
            next.rejected_transitions.push(RejectedTransition {
                at: event.timestamp.clone(),
                from: payload.from,
                to: payload.to,
                reason: payload.reason.clone(),
            });
            if next.rejected_transitions.len() > MAX_REJECTED_TRANSITIONS {
                let drop = next.rejected_transitions.len() - MAX_REJECTED_TRANSITIONS;
                next.rejected_transitions.drain(0..drop);
            }
            Ok(next)
        }
        (RelayEventType::AutonomyChanged, EventPayload::AutonomyChanged(payload)) => {
            next.autonomy =
                parse_autonomy_bound(&payload.to).unwrap_or(AutonomyBound::NextCheckpoint);
            Ok(next)
        }
        (RelayEventType::ArtifactCreated, EventPayload::ArtifactCreated(payload)) => {
            if state.artifacts.contains(&payload.artifact_id) {
                return Err(fold_error(format!(
                    "artifact.created at sequence {} reuses existing artifact id \"{}\"; \
                     artifact ids are immutable — create a new artifact or revise the existing one",
                    event.sequence, payload.artifact_id
                )));
            }
            next.artifacts.insert(
                payload.artifact_id.clone(),
                ArtifactState {
                    artifact_id: payload.artifact_id.clone(),
                    kind: payload.kind,
                    title: payload.title.clone(),
                    current_version: 1,
                    status: ArtifactStatus::Draft,
                    path: Some(payload.path.clone()),
                    created_at: event.timestamp.clone(),
                    last_changed_at: event.timestamp.clone(),
                    approved_at: None,
                    revisions: Vec::new(),
                },
            );
            next.artifacts_by_kind
                .insert(payload.kind, payload.artifact_id.clone());
            Ok(next)
        }
        (RelayEventType::ArtifactRevised, EventPayload::ArtifactRevised(payload)) => {
            let existing = require_artifact(
                state,
                event.event_type.as_str(),
                event.sequence,
                &payload.artifact_id,
            )?;
            if existing.kind != payload.kind {
                return Err(fold_error(format!(
                    "artifact.revised at sequence {} labels artifact \"{}\" as \"{}\" \
                     but it is a \"{}\"; the event log may be corrupted",
                    event.sequence,
                    payload.artifact_id,
                    payload.kind.as_str(),
                    existing.kind.as_str()
                )));
            }
            if payload.to_version != existing.current_version + 1 {
                return Err(fold_error(format!(
                    "artifact.revised at sequence {} moves \"{}\" ({}) to version {}, \
                     but the current version is {}; revisions must increment by exactly 1 \
                     (expected version {})",
                    event.sequence,
                    payload.artifact_id,
                    payload.kind.as_str(),
                    payload.to_version,
                    existing.current_version,
                    existing.current_version + 1
                )));
            }
            let mut revised = existing.clone();
            revised.current_version = payload.to_version;
            revised.status = ArtifactStatus::Draft;
            revised.approved_at = None;
            revised.path = Some(payload.path.clone());
            revised.last_changed_at = event.timestamp.clone();
            revised.revisions.push(RevisionRecord {
                version: payload.to_version,
                at: event.timestamp.clone(),
                reason: payload.reason.clone(),
            });

            // Deterministic downstream invalidation: every other artifact whose
            // kind is transitively downstream of the revised kind goes stale.
            let downstream = downstream_artifact_kinds(payload.kind);
            next.artifacts.insert(payload.artifact_id.clone(), revised);
            let mut newly_stale: Vec<String> = Vec::new();
            for (id, artifact) in state.artifacts.iter() {
                if *id == payload.artifact_id || artifact.status == ArtifactStatus::Stale {
                    continue;
                }
                if !downstream.contains(&artifact.kind) {
                    continue;
                }
                let mut stale_artifact = artifact.clone();
                stale_artifact.status = ArtifactStatus::Stale;
                next.artifacts.insert(id.clone(), stale_artifact);
                newly_stale.push(id.clone());
            }
            next.stale_artifacts.retain(|id| id != &payload.artifact_id);
            for id in newly_stale {
                if !next.stale_artifacts.contains(&id) {
                    next.stale_artifacts.push(id);
                }
            }
            Ok(next)
        }
        (RelayEventType::ArtifactInvalidated, EventPayload::ArtifactInvalidated(payload)) => {
            for id in &payload.artifact_ids {
                let Some(artifact) = next.artifacts.get(id) else {
                    continue;
                };
                if artifact.status == ArtifactStatus::Stale {
                    continue;
                }
                let mut stale_artifact = artifact.clone();
                stale_artifact.status = ArtifactStatus::Stale;
                next.artifacts.insert(id.clone(), stale_artifact);
                if !next.stale_artifacts.contains(id) {
                    next.stale_artifacts.push(id.to_string());
                }
            }
            Ok(next)
        }
        (RelayEventType::ArtifactApproved, EventPayload::ArtifactApproved(payload)) => {
            let existing = require_artifact(
                state,
                event.event_type.as_str(),
                event.sequence,
                &payload.artifact_id,
            )?;
            if existing.kind != payload.kind {
                return Err(fold_error(format!(
                    "artifact.approved at sequence {} labels artifact \"{}\" as \"{}\" \
                     but it is a \"{}\"; the event log may be corrupted",
                    event.sequence,
                    payload.artifact_id,
                    payload.kind.as_str(),
                    existing.kind.as_str()
                )));
            }
            if payload.version != existing.current_version {
                return Err(fold_error(format!(
                    "artifact.approved at sequence {} targets {} \"{}\" at version {}, \
                     but the current version is {}; approve the current version after any revision",
                    event.sequence,
                    payload.kind.as_str(),
                    payload.artifact_id,
                    payload.version,
                    existing.current_version
                )));
            }
            let mut approved = existing.clone();
            approved.status = ArtifactStatus::Approved;
            approved.approved_at = Some(event.timestamp.clone());
            next.artifacts.insert(payload.artifact_id.clone(), approved);
            next.stale_artifacts.retain(|id| id != &payload.artifact_id);
            Ok(next)
        }
        (RelayEventType::TransitionEvaluated, EventPayload::TransitionEvaluated(payload)) => {
            next.last_outcome = Some(OutcomeRecord {
                at: event.timestamp.clone(),
                outcome: payload.outcome,
                to: payload.to,
                revise_stage: payload.revise_stage,
                reason: payload.reason.clone(),
                evidence: payload.evidence.clone(),
            });
            Ok(next)
        }
        (RelayEventType::ModelResponded, EventPayload::ModelResponded(_payload)) => {
            next.stats.model_calls += 1;
            Ok(next)
        }
        (RelayEventType::ModelFailed, EventPayload::ModelFailed(payload)) => {
            next.stats.last_model_error = Some(format!(
                "{}: {}",
                payload.error_kind.as_str(),
                payload.message
            ));
            Ok(next)
        }
        (RelayEventType::ToolExecuted, EventPayload::ToolExecuted(_payload)) => {
            next.stats.tool_calls += 1;
            Ok(next)
        }
        (RelayEventType::VerificationCompleted, EventPayload::VerificationCompleted(payload)) => {
            next.verification = VerificationSummary {
                passed: Some(payload.passed),
                last_run_at: Some(event.timestamp.clone()),
                passed_count: payload
                    .results
                    .iter()
                    .filter(|result| result.status == crate::payloads::VerificationStatus::Pass)
                    .count() as i64,
                failed_count: payload
                    .results
                    .iter()
                    .filter(|result| result.status == crate::payloads::VerificationStatus::Fail)
                    .count() as i64,
                unevaluated_count: payload
                    .results
                    .iter()
                    .filter(|result| {
                        result.status == crate::payloads::VerificationStatus::Unevaluated
                    })
                    .count() as i64,
            };
            Ok(next)
        }
        (RelayEventType::WorkflowPaused, EventPayload::WorkflowPaused(payload)) => {
            next.paused = Some(PauseState {
                by: payload.by,
                reason: payload.reason.clone(),
                question: payload.question.clone(),
                options: payload.options.clone(),
                at: event.timestamp.clone(),
            });
            Ok(next)
        }
        (RelayEventType::WorkflowResumed, EventPayload::WorkflowResumed(_payload)) => {
            next.paused = None;
            Ok(next)
        }
        (RelayEventType::BudgetExhausted, EventPayload::BudgetExhausted(payload)) => {
            if state.paused.is_some() {
                return Ok(next);
            }
            next.paused = Some(PauseState {
                by: PauseActor::Policy,
                reason: format!(
                    "budget exhausted: {} (limit {})",
                    payload.budget, payload.limit
                ),
                question: None,
                options: Vec::new(),
                at: event.timestamp.clone(),
            });
            Ok(next)
        }
        (RelayEventType::RunInterrupted, EventPayload::RunInterrupted(payload)) => {
            if state.paused.is_some() {
                return Ok(next);
            }
            next.paused = Some(PauseState {
                by: PauseActor::User,
                reason: format!("interrupted: {}", payload.hint),
                question: None,
                options: Vec::new(),
                at: event.timestamp.clone(),
            });
            Ok(next)
        }
        // Activity-only events: the common tail already recorded them.
        (RelayEventType::SessionStarted, EventPayload::SessionStarted(_))
        | (RelayEventType::IntentProposed, EventPayload::IntentProposed(_))
        | (RelayEventType::IntentApplied, EventPayload::IntentApplied(_))
        | (RelayEventType::ArtifactApprovalRequested, EventPayload::ArtifactApprovalRequested(_))
        | (RelayEventType::ArtifactInspected, EventPayload::ArtifactInspected(_))
        | (RelayEventType::ModelRequested, EventPayload::ModelRequested(_))
        | (RelayEventType::ToolDenied, EventPayload::ToolDenied(_)) => Ok(next),
        // A validated event always matches its payload variant; the arm below
        // keeps the fold total (and future-proof) rather than panicking.
        _ => Err(CoreError::UnknownEventType {
            event_type: event.event_type.as_str().to_string(),
            detail: format!(
                "at sequence {}: the reducer has no fold for this event; \
                 the log may come from a different tool or a newer Relay build",
                event.sequence
            ),
        }),
    }
}

/// Fold a whole append-only log. The first event must be
/// `project.initialized`; every event must belong to one project and carry
/// contiguous 1-based sequences.
pub fn replay_project(events: &[RelayEvent]) -> Result<ProjectState, CoreError> {
    let Some(first) = events.first() else {
        return Err(fold_error(
            "cannot replay an empty event log: expected the first event to be project.initialized; \
             the .relay project directory may be missing its event store"
                .to_string(),
        ));
    };
    if first.event_type != RelayEventType::ProjectInitialized {
        return Err(fold_error(format!(
            "event log must start with project.initialized, but the first event (sequence {}) is \"{}\"; \
             the log is truncated or corrupted — restore the .relay directory from a backup",
            first.sequence,
            first.event_type.as_str()
        )));
    }

    let mut state = create_empty_project_state();
    let project_id = first.project_id.clone();
    let mut expected_sequence: u64 = 1;
    for event in events {
        if event.project_id != project_id {
            return Err(fold_error(format!(
                "event \"{}\" (sequence {}) belongs to project \"{}\", \
                 but this log belongs to project \"{}\"; each project keeps its own event log",
                event.event_type.as_str(),
                event.sequence,
                event.project_id,
                project_id
            )));
        }
        if event.sequence != expected_sequence {
            return Err(fold_error(format!(
                "event log sequence mismatch at \"{}\": found sequence {} but expected {} \
                 (sequences must increase by exactly 1 starting at 1); \
                 the log is corrupted, truncated, or contains duplicates",
                event.event_type.as_str(),
                event.sequence,
                expected_sequence
            )));
        }
        expected_sequence += 1;
        state = apply_event(&state, event)?;
    }
    Ok(state)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::artifacts::ArtifactKind;
    use crate::intents::{IntentKind, IntentSource};
    use crate::outcomes::OutcomeKind;
    use crate::events::RelayEventType;
    use crate::payloads::EventPayload;
    use crate::fakes::{
        artifact_approved, artifact_created, artifact_revised, full_pipeline_approved, initialized,
        initialized_default, make_event, mode_entered, replay_events, verification_completed,
    };
    use crate::modes::{AutonomyBound, WorkflowMode};
    use crate::payloads::*;
    use crate::project_state::create_empty_project_state;

    fn session_started() -> crate::events::RelayEvent {
        make_event(
            RelayEventType::SessionStarted,
            EventPayload::SessionStarted(SessionStartedPayload {
                session_id: "s1".to_string(),
                interactive: true,
            }),
        )
    }

    fn paused(
        by: PauseActor,
        reason: &str,
        question: Option<&str>,
        options: Vec<&str>,
    ) -> crate::events::RelayEvent {
        make_event(
            RelayEventType::WorkflowPaused,
            EventPayload::WorkflowPaused(WorkflowPausedPayload {
                by,
                reason: reason.to_string(),
                question: question.map(str::to_string),
                options: options.into_iter().map(str::to_string).collect(),
            }),
        )
    }

    #[test]
    fn folds_project_initialized_into_fresh_state() {
        let event = initialized("Build a CLI", "until:planning");
        let state = apply_event(&create_empty_project_state(), &event).expect("init folds");
        assert_eq!(
            state.project_id(),
            "prj_00000000-0000-4000-8000-0000000000f1"
        );
        assert_eq!(state.goal(), "Build a CLI");
        assert_eq!(state.workspace_root(), Some("/tmp/relay-workspace"));
        assert_eq!(state.initialized_at(), event.timestamp);
        assert_eq!(state.current_mode(), WorkflowMode::Discovery);
        assert_eq!(
            state.autonomy(),
            AutonomyBound::UntilMode(WorkflowMode::Planning)
        );
        assert_eq!(state.stats().events, 1);
        assert_eq!(state.last_activity_at(), event.timestamp);
    }

    #[test]
    fn falls_back_to_next_checkpoint_for_unparseable_initial_autonomy() {
        assert_eq!(
            replay_events(&[initialized("goal", "yolo")]).autonomy(),
            AutonomyBound::NextCheckpoint
        );
        assert_eq!(
            replay_events(&[initialized("goal", "")]).autonomy(),
            AutonomyBound::NextCheckpoint
        );
    }

    #[test]
    fn rejects_a_duplicate_project_initialized() {
        let events = vec![
            initialized_default(),
            session_started(),
            initialized("another goal", "workflow"),
        ];
        let error = replay_project(&renumber(&events)).expect_err("duplicate init must fail");
        assert_eq!(error.error_name(), "EventFoldError");
        assert!(
            error.to_string().contains("duplicate project.initialized"),
            "{}",
            error
        );
    }

    fn renumber(events: &[crate::events::RelayEvent]) -> Vec<crate::events::RelayEvent> {
        events
            .iter()
            .enumerate()
            .map(|(index, event)| {
                let mut renumbered = event.clone();
                renumbered.sequence = (index + 1) as u64;
                renumbered
            })
            .collect()
    }

    #[test]
    fn activity_only_events_bump_counters_and_last_activity_only() {
        let events = vec![
            initialized_default(),
            session_started(),
            make_event(
                RelayEventType::IntentProposed,
                EventPayload::IntentProposed(IntentProposedPayload {
                    source: IntentSource::NaturalLanguage,
                    raw: "go faster".to_string(),
                    proposal: IntentProposalSer {
                        kind: IntentKind::Advance,
                        confidence: 0.9,
                        rationale: "clear request".to_string(),
                        mode: None,
                        autonomy: None,
                        artifact_kind: None,
                        question: None,
                        free_text: None,
                    },
                }),
            ),
            make_event(
                RelayEventType::IntentApplied,
                EventPayload::IntentApplied(IntentAppliedPayload {
                    kind: "advance".to_string(),
                    source: IntentSource::NaturalLanguage,
                    confidence: 0.9,
                    mode: None,
                    autonomy: None,
                    artifact_kind: None,
                    overridden: false,
                }),
            ),
            make_event(
                RelayEventType::ArtifactApprovalRequested,
                EventPayload::ArtifactApprovalRequested(ArtifactApprovalRequestedPayload {
                    artifact_id: "a1".to_string(),
                    kind: ArtifactKind::Prd,
                    version: 1,
                    reason: "ready for review".to_string(),
                }),
            ),
            make_event(
                RelayEventType::ArtifactInspected,
                EventPayload::ArtifactInspected(ArtifactInspectedPayload {
                    artifact_id: "a1".to_string(),
                    via: InspectVia::User,
                }),
            ),
            make_event(
                RelayEventType::ModelRequested,
                EventPayload::ModelRequested(ModelRequestedPayload {
                    provider: "openai".to_string(),
                    model: "gpt-x".to_string(),
                    purpose: "draft".to_string(),
                    mode: Some(WorkflowMode::Discovery),
                    attempt: 1,
                    role: None,
                }),
            ),
            make_event(
                RelayEventType::ToolDenied,
                EventPayload::ToolDenied(ToolDeniedPayload {
                    tool: "shell".to_string(),
                    capability: crate::modes::ToolCapability::Execute,
                    mode: Some(WorkflowMode::Discovery),
                    reason: "not permitted".to_string(),
                }),
            ),
        ];
        let state = replay_events(&events);
        assert_eq!(state.stats().events, events.len() as i64);
        assert_eq!(state.last_activity_at(), events.last().unwrap().timestamp);
        assert_eq!(state.stats().model_calls, 0);
        assert_eq!(state.stats().tool_calls, 0);
        assert_eq!(
            serde_json::to_value(&state).unwrap()["artifacts"],
            serde_json::json!({})
        );
        assert!(state.paused().is_none());
    }

    #[test]
    fn folds_mode_entered_with_its_transition_record() {
        let events = vec![
            initialized_default(),
            mode_entered(
                WorkflowMode::Specification,
                Some(WorkflowMode::Discovery),
                AutoUserSource::User,
                "brief approved",
            ),
        ];
        let state = replay_events(&events);
        assert_eq!(state.current_mode(), WorkflowMode::Specification);
        assert_eq!(state.transitions().len(), 1);
        let record = &state.transitions()[0];
        assert_eq!(record.from, Some(WorkflowMode::Discovery));
        assert_eq!(record.to, WorkflowMode::Specification);
        assert_eq!(record.reason, "brief approved");
    }

    #[test]
    fn caps_rejected_transitions_at_fifty_dropping_the_oldest() {
        let mut events = vec![initialized_default()];
        for index in 0..60 {
            events.push(make_event(
                RelayEventType::ModeTransitionRejected,
                EventPayload::ModeTransitionRejected(ModeTransitionRejectedPayload {
                    from: WorkflowMode::Discovery,
                    to: WorkflowMode::Delivery,
                    reason: format!("rejection {index}"),
                }),
            ));
        }
        let state = replay_events(&events);
        assert_eq!(state.rejected_transitions().len(), 50);
        assert_eq!(state.rejected_transitions()[0].reason, "rejection 10");
        assert_eq!(state.rejected_transitions()[49].reason, "rejection 59");
    }

    #[test]
    fn folds_autonomy_changed_falling_back_on_invalid_bounds() {
        let base = vec![initialized("goal", "off")];
        let changed = replay_events(&{
            let mut events = base.clone();
            events.push(make_event(
                RelayEventType::AutonomyChanged,
                EventPayload::AutonomyChanged(AutonomyChangedPayload {
                    from: "off".to_string(),
                    to: "until:verification".to_string(),
                    source: AutonomyChangeSource::User,
                }),
            ));
            events
        });
        assert_eq!(
            changed.autonomy(),
            AutonomyBound::UntilMode(WorkflowMode::Verification)
        );

        let invalid = replay_events(&{
            let mut events = base;
            events.push(make_event(
                RelayEventType::AutonomyChanged,
                EventPayload::AutonomyChanged(AutonomyChangedPayload {
                    from: "off".to_string(),
                    to: "whatever".to_string(),
                    source: AutonomyChangeSource::User,
                }),
            ));
            events
        });
        assert_eq!(invalid.autonomy(), AutonomyBound::NextCheckpoint);
    }

    #[test]
    fn folds_artifact_created_as_a_draft_and_indexes_it_by_kind() {
        let events = vec![
            initialized_default(),
            artifact_created(ArtifactKind::OpportunityBrief, "art-brief", "My brief"),
        ];
        let state = replay_events(&events);
        let artifact = state.artifact("art-brief").expect("artifact exists");
        assert_eq!(artifact.artifact_id(), "art-brief");
        assert_eq!(artifact.kind(), ArtifactKind::OpportunityBrief);
        assert_eq!(artifact.title(), "My brief");
        assert_eq!(artifact.current_version(), 1);
        assert_eq!(artifact.status(), ArtifactStatus::Draft);
        assert_eq!(artifact.approved_at(), None);
        assert!(artifact.revisions().is_empty());
        assert_eq!(
            state.artifact_id_by_kind(ArtifactKind::OpportunityBrief),
            Some("art-brief")
        );
    }

    #[test]
    fn rejects_artifact_created_reusing_an_existing_artifact_id() {
        let events = vec![
            initialized_default(),
            artifact_created(ArtifactKind::Prd, "art-prd", "Test prd"),
            artifact_created(ArtifactKind::Prd, "art-prd", "duplicate"),
        ];
        let error = replay_err(&events);
        assert!(
            error
                .to_string()
                .contains("reuses existing artifact id \"art-prd\""),
            "{}",
            error
        );
    }

    fn replay_err(events: &[crate::events::RelayEvent]) -> CoreError {
        let renumbered: Vec<_> = events
            .iter()
            .enumerate()
            .map(|(index, event)| {
                let mut renumbered = event.clone();
                renumbered.sequence = (index + 1) as u64;
                renumbered
            })
            .collect();
        replay_project(&renumbered).expect_err("replay must fail")
    }

    #[test]
    fn folds_artifact_approved_and_clears_it_from_stale_artifacts() {
        let events = vec![
            initialized_default(),
            artifact_created(ArtifactKind::Prd, "art-prd", "Test prd"),
            make_event(
                RelayEventType::ArtifactInvalidated,
                EventPayload::ArtifactInvalidated(ArtifactInvalidatedPayload {
                    artifact_ids: vec!["art-prd".to_string()],
                    cause: InvalidationCause {
                        kind: ArtifactKind::OpportunityBrief,
                        artifact_id: "art-brief".to_string(),
                        version: 2,
                    },
                    reason: "upstream revised".to_string(),
                }),
            ),
            artifact_approved("art-prd", ArtifactKind::Prd, 1),
        ];
        let state = replay_events(&events);
        assert_eq!(
            state.artifact("art-prd").unwrap().status(),
            ArtifactStatus::Approved
        );
        assert!(state.stale_artifacts().is_empty());
    }

    #[test]
    fn rejects_approving_a_version_other_than_the_current_one() {
        let events = vec![
            initialized_default(),
            artifact_created(ArtifactKind::Prd, "art-prd", "Test prd"),
            artifact_approved("art-prd", ArtifactKind::Prd, 1),
            artifact_revised("art-prd", ArtifactKind::Prd, 1, "tighten scope"),
            artifact_approved("art-prd", ArtifactKind::Prd, 1),
        ];
        let error = replay_err(&events);
        assert!(
            error
                .to_string()
                .contains("targets prd \"art-prd\" at version 1, but the current version is 2"),
            "{}",
            error
        );
    }

    #[test]
    fn rejects_revisions_that_do_not_increment_by_exactly_one() {
        let events = vec![
            initialized_default(),
            artifact_created(ArtifactKind::Prd, "art-prd", "Test prd"),
            make_event(
                RelayEventType::ArtifactRevised,
                EventPayload::ArtifactRevised(ArtifactRevisedPayload {
                    artifact_id: "art-prd".to_string(),
                    kind: ArtifactKind::Prd,
                    from_version: 1,
                    to_version: 3,
                    path: ".relay/artifacts/prd.md".to_string(),
                    reason: "skip a version".to_string(),
                }),
            ),
        ];
        let error = replay_err(&events);
        assert!(
            error
                .to_string()
                .contains("to version 3, but the current version is 1"),
            "{}",
            error
        );
    }

    #[test]
    fn rejects_revisions_and_approvals_referencing_unknown_artifacts() {
        let revised = vec![
            initialized_default(),
            make_event(
                RelayEventType::ArtifactRevised,
                EventPayload::ArtifactRevised(ArtifactRevisedPayload {
                    artifact_id: "ghost".to_string(),
                    kind: ArtifactKind::Prd,
                    from_version: 1,
                    to_version: 2,
                    path: "x.md".to_string(),
                    reason: "r".to_string(),
                }),
            ),
        ];
        assert!(
            replay_err(&revised)
                .to_string()
                .contains("unknown artifact \"ghost\""),
            "{}",
            replay_err(&revised)
        );

        let approved = vec![
            initialized_default(),
            artifact_approved("ghost", ArtifactKind::Prd, 1),
        ];
        assert!(
            replay_err(&approved)
                .to_string()
                .contains("unknown artifact \"ghost\"")
        );
    }

    #[test]
    fn folds_model_and_tool_telemetry_into_stats() {
        let model_responded = || {
            make_event(
                RelayEventType::ModelResponded,
                EventPayload::ModelResponded(ModelRespondedPayload {
                    provider: "openai".to_string(),
                    model: "gpt-x".to_string(),
                    purpose: "draft".to_string(),
                    role: None,
                    usage: TokenUsage {
                        input_tokens: 10,
                        output_tokens: 5,
                    },
                    tool_calls: 0,
                    duration_ms: 100,
                }),
            )
        };
        let events = vec![
            initialized_default(),
            model_responded(),
            model_responded(),
            make_event(
                RelayEventType::ModelFailed,
                EventPayload::ModelFailed(ModelFailedPayload {
                    provider: "openai".to_string(),
                    model: "gpt-x".to_string(),
                    purpose: "draft".to_string(),
                    role: None,
                    error_kind: ModelErrorKind::RateLimit,
                    message: "throttled after retries".to_string(),
                    attempts: 3,
                    duration_ms: 900,
                }),
            ),
            make_event(
                RelayEventType::ToolExecuted,
                EventPayload::ToolExecuted(ToolExecutedPayload {
                    tool: "read-file".to_string(),
                    capability: crate::modes::ToolCapability::Read,
                    mode: Some(WorkflowMode::Discovery),
                    outcome: ToolOutcome::Ok,
                    duration_ms: 5,
                    summary: "read a file".to_string(),
                    affected_files: vec![],
                }),
            ),
        ];
        let state = replay_events(&events);
        assert_eq!(state.stats().model_calls, 2);
        assert_eq!(state.stats().tool_calls, 1);
        assert_eq!(
            state.stats().last_model_error.as_deref(),
            Some("rate-limit: throttled after retries")
        );
    }

    #[test]
    fn folds_transition_evaluated_into_last_outcome() {
        let events = vec![
            initialized_default(),
            make_event(
                RelayEventType::TransitionEvaluated,
                EventPayload::TransitionEvaluated(TransitionEvaluatedPayload {
                    outcome: OutcomeKind::Advance,
                    to: Some(WorkflowMode::Specification),
                    revise_stage: None,
                    reason: "brief approved".to_string(),
                    evidence: vec!["approved v1".to_string()],
                }),
            ),
        ];
        let state = replay_events(&events);
        let outcome = state.last_outcome().expect("outcome recorded");
        assert_eq!(outcome.outcome, OutcomeKind::Advance);
        assert_eq!(outcome.to, Some(WorkflowMode::Specification));
        assert_eq!(outcome.evidence, vec!["approved v1".to_string()]);
    }

    #[test]
    fn tallies_verification_completed_results() {
        let events = vec![
            initialized_default(),
            verification_completed(false, Some("art-vr")),
        ];
        let state = replay_events(&events);
        let verification = state.verification();
        assert_eq!(verification.passed, Some(false));
        assert_eq!(verification.passed_count, 1);
        assert_eq!(verification.failed_count, 1);
        assert_eq!(verification.unevaluated_count, 1);
    }

    #[test]
    fn folds_workflow_paused_and_resumed() {
        let paused_events = vec![
            initialized_default(),
            paused(
                PauseActor::User,
                "reviewing",
                Some("Keep going?"),
                vec!["yes", "no"],
            ),
        ];
        let state = replay_events(&paused_events);
        let pause = state.paused().expect("paused");
        assert_eq!(pause.by, PauseActor::User);
        assert_eq!(pause.reason, "reviewing");
        assert_eq!(pause.question.as_deref(), Some("Keep going?"));
        assert_eq!(pause.options, vec!["yes".to_string(), "no".to_string()]);

        let resumed_events = vec![
            initialized_default(),
            paused(PauseActor::User, "reviewing", None, vec![]),
            make_event(
                RelayEventType::WorkflowResumed,
                EventPayload::WorkflowResumed(WorkflowResumedPayload {
                    by: "user",
                    note: "back".to_string(),
                }),
            ),
        ];
        assert!(replay_events(&resumed_events).paused().is_none());
    }

    #[test]
    fn mode_entered_clears_a_policy_pause_but_keeps_a_user_pause() {
        let cleared = replay_events(&vec![
            initialized_default(),
            paused(PauseActor::Policy, "checkpoint", None, vec![]),
            mode_entered(
                WorkflowMode::Specification,
                Some(WorkflowMode::Discovery),
                AutoUserSource::Auto,
                "resumed work",
            ),
        ]);
        assert!(cleared.paused().is_none());

        let kept = replay_events(&vec![
            initialized_default(),
            paused(PauseActor::User, "lunch", None, vec![]),
            mode_entered(
                WorkflowMode::Specification,
                Some(WorkflowMode::Discovery),
                AutoUserSource::User,
                "forced while paused",
            ),
        ]);
        assert_eq!(kept.paused().unwrap().by, PauseActor::User);
    }

    #[test]
    fn budget_exhausted_and_run_interrupted_pause_without_overwriting() {
        let events = vec![
            initialized_default(),
            paused(PauseActor::User, "reviewing first", None, vec![]),
            make_event(
                RelayEventType::BudgetExhausted,
                EventPayload::BudgetExhausted(BudgetExhaustedPayload {
                    budget: "toolCalls".to_string(),
                    limit: 5,
                    detail: String::new(),
                }),
            ),
            make_event(
                RelayEventType::RunInterrupted,
                EventPayload::RunInterrupted(RunInterruptedPayload {
                    phase: "verification".to_string(),
                    hint: "signal".to_string(),
                }),
            ),
        ];
        let state = replay_events(&events);
        assert_eq!(state.paused().unwrap().reason, "reviewing first");
        assert_eq!(state.paused().unwrap().by, PauseActor::User);

        let budget_only = replay_events(&vec![
            initialized_default(),
            make_event(
                RelayEventType::BudgetExhausted,
                EventPayload::BudgetExhausted(BudgetExhaustedPayload {
                    budget: "modelCalls".to_string(),
                    limit: 25,
                    detail: "too many calls".to_string(),
                }),
            ),
        ]);
        assert_eq!(
            budget_only.paused().unwrap().reason,
            "budget exhausted: modelCalls (limit 25)"
        );

        let interrupted = replay_events(&vec![
            initialized_default(),
            make_event(
                RelayEventType::RunInterrupted,
                EventPayload::RunInterrupted(RunInterruptedPayload {
                    phase: "implementation".to_string(),
                    hint: "Ctrl-C during patch".to_string(),
                }),
            ),
        ]);
        assert_eq!(
            interrupted.paused().unwrap().reason,
            "interrupted: Ctrl-C during patch"
        );
        assert_eq!(interrupted.paused().unwrap().by, PauseActor::User);
    }

    #[test]
    fn revising_the_approved_prd_stales_every_transitive_dependent() {
        let mut events = full_pipeline_approved();
        events.push(artifact_revised(
            "art-prd",
            ArtifactKind::Prd,
            1,
            "requirements changed",
        ));
        let state = replay_events(&events);

        let prd = state.artifact("art-prd").unwrap();
        assert_eq!(prd.status(), ArtifactStatus::Draft);
        assert_eq!(prd.current_version(), 2);
        assert_eq!(prd.approved_at(), None);
        assert_eq!(
            state.artifact("art-trd").unwrap().status(),
            ArtifactStatus::Stale
        );
        assert_eq!(
            state.artifact("art-plan").unwrap().status(),
            ArtifactStatus::Stale
        );
        assert_eq!(
            state.artifact("art-vr").unwrap().status(),
            ArtifactStatus::Stale
        );
        assert_eq!(
            state.artifact("art-dr").unwrap().status(),
            ArtifactStatus::Stale
        );
        assert_eq!(
            state.artifact("art-log").unwrap().status(),
            ArtifactStatus::Draft
        );
        assert_eq!(
            state.artifact("art-brief").unwrap().status(),
            ArtifactStatus::Approved
        );
        assert_eq!(
            state.stale_artifacts(),
            &vec![
                "art-trd".to_string(),
                "art-plan".to_string(),
                "art-vr".to_string(),
                "art-dr".to_string(),
            ]
        );
    }

    #[test]
    fn revising_the_opportunity_brief_also_stales_the_decision_log() {
        let mut events = full_pipeline_approved();
        events.push(artifact_revised(
            "art-brief",
            ArtifactKind::OpportunityBrief,
            1,
            "problem reframed",
        ));
        let state = replay_events(&events);
        assert_eq!(
            state.artifact("art-log").unwrap().status(),
            ArtifactStatus::Stale
        );
        assert!(state.stale_artifacts().contains(&"art-log".to_string()));
    }

    #[test]
    fn does_not_remark_stale_artifacts_and_drops_the_regenerated_one() {
        let mut events = full_pipeline_approved();
        events.push(artifact_revised(
            "art-prd",
            ArtifactKind::Prd,
            1,
            "first change",
        ));
        events.push(artifact_revised(
            "art-trd",
            ArtifactKind::Trd,
            1,
            "regenerate trd",
        ));
        let state = replay_events(&events);
        assert_eq!(
            state.stale_artifacts(),
            &vec![
                "art-plan".to_string(),
                "art-vr".to_string(),
                "art-dr".to_string(),
            ]
        );
        assert_eq!(
            state.artifact("art-trd").unwrap().status(),
            ArtifactStatus::Draft
        );
    }

    #[test]
    fn artifact_invalidated_marks_listed_artifacts_stale_and_is_idempotent() {
        let invalidate = |ids: Vec<&str>| {
            make_event(
                RelayEventType::ArtifactInvalidated,
                EventPayload::ArtifactInvalidated(ArtifactInvalidatedPayload {
                    artifact_ids: ids.into_iter().map(str::to_string).collect(),
                    cause: InvalidationCause {
                        kind: ArtifactKind::Prd,
                        artifact_id: "art-prd".to_string(),
                        version: 2,
                    },
                    reason: "upstream revision".to_string(),
                }),
            )
        };
        let mut events = full_pipeline_approved();
        events.push(invalidate(vec!["art-trd", "art-plan"]));
        events.push(invalidate(vec!["art-plan", "art-vr"]));
        let state = replay_events(&events);
        assert_eq!(
            state.stale_artifacts(),
            &vec![
                "art-trd".to_string(),
                "art-plan".to_string(),
                "art-vr".to_string(),
            ]
        );
        assert_eq!(
            state.artifact("art-dr").unwrap().status(),
            ArtifactStatus::Draft
        );

        let mut with_ghost = full_pipeline_approved();
        with_ghost.push(invalidate(vec!["ghost"]));
        assert!(replay_events(&with_ghost).stale_artifacts().is_empty());
    }

    #[test]
    fn replays_deterministically_folding_twice_yields_identical_state() {
        let mut events = full_pipeline_approved();
        events.push(artifact_revised(
            "art-trd",
            ArtifactKind::Trd,
            1,
            "design fix",
        ));
        assert_eq!(replay_events(&events), replay_events(&events));
    }

    #[test]
    fn incremental_apply_event_folds_equal_a_full_replay() {
        let events = full_pipeline_approved();
        let mut incremental = create_empty_project_state();
        for event in &events {
            incremental = apply_event(&incremental, event).expect("incremental fold");
        }
        assert_eq!(incremental, replay_events(&events));
    }

    #[test]
    fn rejects_an_empty_event_log() {
        let error = replay_project(&[]).expect_err("empty log must fail");
        assert!(
            error
                .to_string()
                .contains("cannot replay an empty event log")
        );
    }

    #[test]
    fn rejects_a_log_whose_first_event_is_not_project_initialized() {
        let error = replay_project(&[session_started()]).expect_err("bad first event");
        assert_eq!(error.error_name(), "EventFoldError");
        assert!(error.to_string().contains("session.started"));
        assert!(error.to_string().contains("sequence 1"));
    }

    #[test]
    fn rejects_sequence_gaps_and_duplicates_naming_expected_and_found() {
        let mut gap = vec![initialized_default(), session_started()];
        gap[1].sequence = 3;
        let error = replay_project(&gap).expect_err("gap must fail");
        assert!(
            error
                .to_string()
                .contains("found sequence 3 but expected 2"),
            "{}",
            error
        );

        let mut duplicate = vec![initialized_default(), session_started()];
        duplicate[1].sequence = 1;
        let error = replay_project(&duplicate).expect_err("duplicate must fail");
        assert!(
            error
                .to_string()
                .contains("found sequence 1 but expected 2")
        );
    }

    #[test]
    fn rejects_events_from_a_different_project() {
        let mut events = vec![initialized_default(), session_started()];
        events[1].project_id = "prj_00000000-0000-4000-8000-00000000dead".to_string();
        events[1].sequence = 2;
        let error = replay_project(&events).expect_err("foreign project must fail");
        let message = error.to_string();
        assert!(
            message.contains("belongs to project \"prj_00000000-0000-4000-8000-00000000dead\"")
        );
        assert!(message.contains("but this log belongs to project"));
    }

    #[test]
    fn throws_incompatible_event_version_for_a_mismatched_schema_version() {
        let mut events = vec![initialized_default(), session_started()];
        events[1].sequence = 2;
        events[1].schema_version = 99;
        let error = apply_event(&replay_events(&events[..1]), &events[1])
            .expect_err("version mismatch must fail");
        assert!(error.to_string().contains("schema version 99"), "{}", error);
    }

    #[test]
    fn serialization_matches_the_typescript_field_names() {
        let state = replay_events(&full_pipeline_approved());
        let value = serde_json::to_value(&state).unwrap();
        for key in [
            "projectId",
            "goal",
            "workspaceRoot",
            "initializedAt",
            "currentMode",
            "autonomy",
            "paused",
            "artifacts",
            "artifactsByKind",
            "transitions",
            "rejectedTransitions",
            "lastOutcome",
            "stats",
            "staleArtifacts",
            "verification",
            "lastActivityAt",
        ] {
            assert!(
                value.as_object().unwrap().contains_key(key),
                "missing {key}"
            );
        }
        let artifact = &value["artifacts"]["art-brief"];
        for key in [
            "artifactId",
            "kind",
            "title",
            "currentVersion",
            "status",
            "path",
            "createdAt",
            "lastChangedAt",
            "approvedAt",
            "revisions",
        ] {
            assert!(
                artifact.as_object().unwrap().contains_key(key),
                "missing {key}"
            );
        }
        assert_eq!(value["currentMode"], serde_json::json!("delivery"));
        assert_eq!(value["autonomy"], serde_json::json!("workflow"));
    }
}
