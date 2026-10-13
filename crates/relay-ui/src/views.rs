//! Read-only status and explanation views over project state and the event
//! log. Ported byte-for-byte from `src/runtime/controller-views.ts`: the
//! serde field order of every view struct is the TypeScript key order, so
//! JSON output matches `JSON.stringify(view, null, 2)` exactly, and the
//! canonical fixture goldens (`tests/fixtures/cases/*/expected-status.json`
//! and `expected-why.json`) compare through
//! [`relay_core::canonical::canonical_json`].
//!
//! Both builders are pure projections — no events are appended.

use relay_core::events::RelayEvent;
use relay_core::modes::{WORKFLOW_MODES, WorkflowMode, mode_index};
use relay_core::payloads::EventPayload;
use relay_core::project_state::ProjectState;
use serde::Serialize;

/// The pause block of a [`StatusView`].
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusPause {
    /// Who paused the workflow (`user` or `policy`).
    pub by: String,
    /// Why the workflow is paused.
    pub reason: String,
    /// A focused question to answer, when the pause asks one.
    pub question: Option<String>,
    /// Options offered to the user.
    pub options: Vec<String>,
}

/// One artifact row of a [`StatusView`]: the latest artifact of its kind.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusArtifact {
    /// The artifact's id (`art_…`).
    pub artifact_id: String,
    /// The artifact's kind.
    pub kind: String,
    /// Current version.
    pub version: i64,
    /// Lifecycle status.
    pub status: String,
    /// Human-readable title.
    pub title: String,
}

/// `stage`/`total` progress across the seven workflow modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusProgress {
    /// 1-based index of the current mode.
    pub stage: usize,
    /// Number of workflow modes.
    pub total: usize,
}

/// Event/model/tool counters folded from the log.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusStats {
    /// Events folded.
    pub events: i64,
    /// Model calls that completed.
    pub model_calls: i64,
    /// Tool executions.
    pub tool_calls: i64,
}

/// `/status`: goal, mode, progress, artifacts, changed files, blockers, and
/// the next action. Field order matches the TypeScript view.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusView {
    /// The product goal.
    pub goal: String,
    /// The current workflow mode.
    pub mode: WorkflowMode,
    /// The current autonomy bound (`off`, `next-checkpoint`,
    /// `until:<mode>`, `workflow`).
    pub autonomy: String,
    /// The current pause, or `null`.
    pub paused: Option<StatusPause>,
    /// Stage progress.
    pub progress: StatusProgress,
    /// Latest artifact per kind, in canonical kind order.
    pub artifacts: Vec<StatusArtifact>,
    /// Stale artifact ids.
    pub stale_artifacts: Vec<String>,
    /// Files changed by tool activity in `session_id`, in order, deduplicated.
    pub changed_files: Vec<String>,
    /// Blockers (staleness and pause reasons), capped at 8.
    pub blockers: Vec<String>,
    /// The suggested next action.
    pub next_action: String,
    /// Run statistics.
    pub stats: StatusStats,
}

/// The last recorded outcome of a [`WhyView`].
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WhyOutcome {
    /// The outcome kind (`CONTINUE`, `ADVANCE`, `NEEDS_USER`, `REVISE`).
    pub outcome: String,
    /// Why the outcome was chosen.
    pub reason: String,
    /// Supporting evidence entries.
    pub evidence: Vec<String>,
    /// When the outcome was recorded.
    pub at: String,
}

/// One accepted mode transition of a [`WhyView`].
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WhyTransition {
    /// The mode left (`null` for the first entry).
    pub from: Option<WorkflowMode>,
    /// The mode entered.
    pub to: WorkflowMode,
    /// Why the transition happened.
    pub reason: String,
    /// When the transition happened.
    pub at: String,
    /// Whether the transition was automatic (`auto`) or user-requested
    /// (`user`).
    pub source: String,
}

/// One policy-rejected mode transition of a [`WhyView`].
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WhyRejection {
    /// The mode the transition was attempted from.
    pub from: WorkflowMode,
    /// The mode the transition targeted.
    pub to: WorkflowMode,
    /// Why policy rejected the transition.
    pub reason: String,
    /// When the rejection happened.
    pub at: String,
}

/// One routing decision of a [`WhyView`], folded from the event tail.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WhyRouting {
    /// Provider name.
    pub provider: String,
    /// Model id.
    pub model: String,
    /// Routing purpose.
    pub purpose: String,
    /// Role of the attempted route; `null` for legacy inline (role-less)
    /// events.
    pub role: Option<String>,
    /// When the request was made.
    pub at: String,
    /// `false` when the request later failed.
    pub ok: bool,
}

/// `/why`: the latest outcome, recent and rejected transitions, routing
/// decisions folded from the event tail, staleness, and any pause reason.
/// Field order matches the TypeScript view.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WhyView {
    /// The current workflow mode.
    pub mode: WorkflowMode,
    /// The last recorded outcome, or `null`.
    pub last_outcome: Option<WhyOutcome>,
    /// The last (up to) 10 accepted transitions.
    pub recent_transitions: Vec<WhyTransition>,
    /// The last (up to) 10 rejected transitions.
    pub rejected_transitions: Vec<WhyRejection>,
    /// Routing rows folded from the last 40 events.
    pub routing: Vec<WhyRouting>,
    /// Stale artifact ids.
    pub stale_artifacts: Vec<String>,
    /// Why the workflow is paused, or `null`.
    pub paused_reason: Option<String>,
}

/// Number of events folded into the routing tail of the why view.
pub const ROUTING_TAIL_EVENTS: usize = 40;
/// Number of transitions kept in the why view's transition tails.
pub const TRANSITION_TAIL: usize = 10;
/// Maximum number of blockers in a status view.
pub const BLOCKER_LIMIT: usize = 8;

/// Files changed by tool activity in `session_id`, in order, deduplicated.
///
/// `session_id` of `None` collects across every session (the TypeScript
/// signature's `sessionId: string | null` with `null` skipping the filter).
pub fn changed_files_for_session(events: &[RelayEvent], session_id: Option<&str>) -> Vec<String> {
    let mut files: Vec<String> = Vec::new();
    for event in events {
        let affected = match &event.payload {
            EventPayload::ToolExecuted(payload) => &payload.affected_files,
            _ => continue,
        };
        if let Some(session) = session_id
            && event.session_id.as_deref() != Some(session)
        {
            continue;
        }
        for file in affected {
            if !files.contains(file) {
                files.push(file.clone());
            }
        }
    }
    files
}

fn next_action_for(state: &ProjectState) -> String {
    if let Some(paused) = state.paused() {
        return paused
            .question
            .clone()
            .unwrap_or_else(|| paused.reason.clone());
    }
    let contract = state.current_mode().contract();
    let produced_id = state.artifact_id_by_kind(contract.produced_artifact);
    let produced = produced_id.and_then(|id| state.artifact(id));
    if let Some(artifact) = produced
        && artifact.status() == relay_core::project_state::ArtifactStatus::Draft
        && relay_core::is_contractual_artifact_kind(contract.produced_artifact)
    {
        return format!(
            "approve the {} with /approve",
            contract.produced_artifact.as_str()
        );
    }
    if state.current_mode() == WorkflowMode::Delivery
        && state
            .artifact_id_by_kind(relay_core::ArtifactKind::DeliveryReport)
            .is_some()
    {
        return "workflow complete — delivery actions require your authorization".to_string();
    }
    format!(
        "run /auto or relay run to continue {}",
        state.current_mode().as_str()
    )
}

fn blockers_for(state: &ProjectState) -> Vec<String> {
    let mut blockers: Vec<String> = Vec::new();
    for artifact_id in state.stale_artifacts() {
        let kind = state
            .artifact(artifact_id)
            .map(|artifact| artifact.kind().as_str().to_string())
            .unwrap_or_else(|| artifact_id.clone());
        blockers.push(format!(
            "{kind} is stale — regenerate it from its updated upstream inputs"
        ));
        if blockers.len() >= BLOCKER_LIMIT {
            return blockers;
        }
    }
    if let Some(paused) = state.paused()
        && blockers.len() < BLOCKER_LIMIT
    {
        blockers.push(format!(
            "paused ({}): {}",
            paused.by.as_str(),
            paused.reason
        ));
    }
    blockers
}

/// Latest artifact per kind, in canonical kind order.
fn latest_artifacts(state: &ProjectState) -> Vec<StatusArtifact> {
    let mut listed = Vec::new();
    for kind in relay_core::ARTIFACT_KINDS {
        let Some(artifact_id) = state.artifact_id_by_kind(kind) else {
            continue;
        };
        let Some(artifact) = state.artifact(artifact_id) else {
            continue;
        };
        listed.push(StatusArtifact {
            artifact_id: artifact.artifact_id().to_string(),
            kind: artifact.kind().as_str().to_string(),
            version: artifact.current_version(),
            status: artifact.status().as_str().to_string(),
            title: artifact.title().to_string(),
        });
    }
    listed
}

/// Build the `/status` view over the replayed `state` and the full event
/// log. `session_id` scopes the changed-files list to one session; the CLI
/// passes its own (fresh) session id, exactly like the TypeScript controller.
pub fn build_status_view(
    state: &ProjectState,
    events: &[RelayEvent],
    session_id: Option<&str>,
) -> StatusView {
    StatusView {
        goal: state.goal().to_string(),
        mode: state.current_mode(),
        autonomy: state.autonomy().as_str(),
        paused: state.paused().map(|paused| StatusPause {
            by: paused.by.as_str().to_string(),
            reason: paused.reason.clone(),
            question: paused.question.clone(),
            options: paused.options.clone(),
        }),
        progress: StatusProgress {
            stage: mode_index(state.current_mode()) + 1,
            total: WORKFLOW_MODES.len(),
        },
        artifacts: latest_artifacts(state),
        stale_artifacts: state.stale_artifacts().to_vec(),
        changed_files: changed_files_for_session(events, session_id),
        blockers: blockers_for(state),
        next_action: next_action_for(state),
        stats: StatusStats {
            events: state.stats().events,
            model_calls: state.stats().model_calls,
            tool_calls: state.stats().tool_calls,
        },
    }
}

/// Fold `model.requested`/`model.failed` events from the tail into routing
/// rows. A failure with a role prefers the request row for the same role and
/// only then falls back to the legacy provider+model+purpose match, so
/// role-less events keep rendering exactly as before.
fn routing_tail(events: &[RelayEvent]) -> Vec<WhyRouting> {
    let mut routing: Vec<WhyRouting> = Vec::new();
    let start = events.len().saturating_sub(ROUTING_TAIL_EVENTS);
    for event in &events[start..] {
        match &event.payload {
            EventPayload::ModelRequested(payload) => {
                routing.push(WhyRouting {
                    provider: payload.provider.clone(),
                    model: payload.model.clone(),
                    purpose: payload.purpose.clone(),
                    role: payload.role.clone(),
                    at: event.timestamp.clone(),
                    ok: true,
                });
            }
            EventPayload::ModelFailed(payload) => {
                let failure_role = payload.role.clone();
                let match_row = |require_role_match: bool| -> Option<usize> {
                    for index in (0..routing.len()).rev() {
                        let entry = &routing[index];
                        if !entry.ok {
                            continue;
                        }
                        if entry.provider != payload.provider {
                            continue;
                        }
                        if entry.model != payload.model {
                            continue;
                        }
                        if entry.purpose != payload.purpose {
                            continue;
                        }
                        if require_role_match && entry.role != failure_role {
                            continue;
                        }
                        return Some(index);
                    }
                    None
                };
                let mut index = match_row(failure_role.is_some());
                if index.is_none() && failure_role.is_some() {
                    index = match_row(false);
                }
                if let Some(found) = index
                    && let Some(entry) = routing.get_mut(found)
                {
                    entry.ok = false;
                }
            }
            _ => {}
        }
    }
    routing
}

/// Build the `/why` view over the replayed `state` and the full event log.
pub fn build_why_view(state: &ProjectState, events: &[RelayEvent]) -> WhyView {
    WhyView {
        mode: state.current_mode(),
        last_outcome: state.last_outcome().map(|outcome| WhyOutcome {
            outcome: outcome.outcome.as_str().to_string(),
            reason: outcome.reason.clone(),
            evidence: outcome.evidence.clone(),
            at: outcome.at.clone(),
        }),
        recent_transitions: state.transitions()
            [state.transitions().len().saturating_sub(TRANSITION_TAIL)..]
            .iter()
            .map(|transition| WhyTransition {
                from: transition.from,
                to: transition.to,
                reason: transition.reason.clone(),
                at: transition.at.clone(),
                source: transition.source.as_str().to_string(),
            })
            .collect(),
        rejected_transitions: state.rejected_transitions()[state
            .rejected_transitions()
            .len()
            .saturating_sub(TRANSITION_TAIL)..]
            .iter()
            .map(|rejected| WhyRejection {
                from: rejected.from,
                to: rejected.to,
                reason: rejected.reason.clone(),
                at: rejected.at.clone(),
            })
            .collect(),
        routing: routing_tail(events),
        stale_artifacts: state.stale_artifacts().to_vec(),
        paused_reason: state.paused().map(|paused| paused.reason.clone()),
    }
}

/// Serialize a view the way the TypeScript CLI prints `--json` output:
/// `JSON.stringify(view, null, 2)` (two-space indent, view field order).
pub fn view_json<T: Serialize>(view: &T) -> String {
    serde_json::to_string_pretty(view).unwrap_or_default()
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use relay_core::fakes::{
        full_pipeline_approved, initialized_default, make_event, replay_events,
    };
    use relay_core::modes::ToolCapability;
    use relay_core::payloads::ToolOutcome;
    use relay_core::payloads::{
        EventPayload, ModelErrorKind, ModelFailedPayload, ModelRequestedPayload,
        ToolExecutedPayload,
    };

    fn tool_executed(files: Vec<&str>, session: Option<&str>) -> RelayEvent {
        let mut event = make_event(
            relay_core::events::RelayEventType::ToolExecuted,
            EventPayload::ToolExecuted(ToolExecutedPayload {
                tool: "write-file".to_string(),
                capability: ToolCapability::Write,
                mode: None,
                outcome: ToolOutcome::Ok,
                duration_ms: 10,
                summary: "wrote files".to_string(),
                affected_files: files.into_iter().map(str::to_string).collect(),
            }),
        );
        event.session_id = session.map(str::to_string);
        event
    }

    #[test]
    fn changed_files_collect_tool_activity_for_one_session_in_order() {
        let ses_a = "ses_aaa";
        let ses_b = "ses_bbb";
        let events = vec![
            initialized_default(),
            tool_executed(
                vec!["src/greet.js", "README.md", "src/greet.js"],
                Some(ses_a),
            ),
            tool_executed(vec!["src/other.js"], Some(ses_b)),
            tool_executed(vec!["src/no-session.js"], None),
        ];
        assert_eq!(
            changed_files_for_session(&events, Some(ses_a)),
            vec!["src/greet.js".to_string(), "README.md".to_string()]
        );
        assert_eq!(changed_files_for_session(&events, None).len(), 4);
        assert!(changed_files_for_session(&events, Some("ses_unknown")).is_empty());
    }

    #[test]
    fn why_view_folds_routing_rows_with_role_aware_failure_matching() {
        let mut requested_reasoning = make_event(
            relay_core::events::RelayEventType::ModelRequested,
            EventPayload::ModelRequested(ModelRequestedPayload {
                provider: "anthropic".to_string(),
                model: "claude-sonnet-5-5".to_string(),
                purpose: "mode:discovery".to_string(),
                mode: None,
                attempt: 1,
                role: Some("reasoning".to_string()),
            }),
        );
        requested_reasoning.session_id = Some("ses_aaa".to_string());
        let mut requested_review = make_event(
            relay_core::events::RelayEventType::ModelRequested,
            EventPayload::ModelRequested(ModelRequestedPayload {
                provider: "openai".to_string(),
                model: "gpt-5.2".to_string(),
                purpose: "mode:verification".to_string(),
                mode: None,
                attempt: 1,
                role: Some("review".to_string()),
            }),
        );
        requested_review.session_id = Some("ses_aaa".to_string());
        let mut failed_review = make_event(
            relay_core::events::RelayEventType::ModelFailed,
            EventPayload::ModelFailed(ModelFailedPayload {
                provider: "openai".to_string(),
                model: "gpt-5.2".to_string(),
                purpose: "mode:verification".to_string(),
                role: Some("review".to_string()),
                error_kind: ModelErrorKind::RateLimit,
                message: "rate limited".to_string(),
                attempts: 1,
                duration_ms: 5,
            }),
        );
        failed_review.session_id = Some("ses_aaa".to_string());
        let events = vec![
            initialized_default(),
            requested_reasoning,
            requested_review,
            failed_review,
        ];
        let state = replay_events(&events);
        let view = build_why_view(&state, &events);
        assert_eq!(view.routing.len(), 2);
        assert_eq!(view.routing[0].role.as_deref(), Some("reasoning"));
        assert!(view.routing[0].ok);
        assert_eq!(view.routing[1].role.as_deref(), Some("review"));
        assert!(
            !view.routing[1].ok,
            "the failure marks its matching request row"
        );
    }

    #[test]
    fn status_view_shape_matches_the_reference() {
        let events = full_pipeline_approved();
        let state = replay_events(&events);
        let view = build_status_view(&state, &events, None);
        assert_eq!(
            view.goal,
            "Ship a workflow CLI that preserves product intent"
        );
        assert_eq!(view.mode, WorkflowMode::Delivery);
        assert_eq!(view.autonomy, "workflow");
        assert_eq!(view.progress.stage, 7);
        assert_eq!(view.progress.total, 7);
        assert_eq!(
            view.artifacts.len(),
            7,
            "one artifact per kind, in canonical kind order"
        );
        assert_eq!(view.artifacts[0].kind, "opportunity-brief");
        assert_eq!(
            view.next_action,
            "workflow complete — delivery actions require your authorization"
        );
        assert_eq!(view.stats.events, state.stats().events);
    }

    #[test]
    fn draft_contract_artifact_suggests_approval() {
        let events = vec![
            initialized_default(),
            relay_core::fakes::artifact_created(
                relay_core::ArtifactKind::OpportunityBrief,
                "art-brief",
                "Test brief",
            ),
        ];
        let state = replay_events(&events);
        let view = build_status_view(&state, &events, None);
        assert_eq!(
            view.next_action,
            "approve the opportunity-brief with /approve"
        );
    }

    #[test]
    fn fresh_project_suggests_running() {
        let events = vec![initialized_default()];
        let state = replay_events(&events);
        let view = build_status_view(&state, &events, None);
        assert_eq!(
            view.next_action,
            "run /auto or relay run to continue discovery"
        );
        assert!(view.artifacts.is_empty());
        assert!(view.blockers.is_empty());
        assert!(view.paused.is_none());
    }

    #[test]
    fn json_output_uses_view_field_order_and_camel_case() {
        let events = vec![initialized_default()];
        let state = replay_events(&events);
        let json = view_json(&build_status_view(&state, &events, None));
        // Read the top-level key order straight from the pretty-printed text
        // (re-parsing into a Value would impose a map ordering).
        let keys: Vec<&str> = json
            .split('\n')
            .filter_map(|line| {
                line.strip_prefix("  \"")
                    .and_then(|rest| rest.split("\":").next())
            })
            .collect();
        assert_eq!(
            keys,
            vec![
                "goal",
                "mode",
                "autonomy",
                "paused",
                "progress",
                "artifacts",
                "staleArtifacts",
                "changedFiles",
                "blockers",
                "nextAction",
                "stats"
            ]
        );
        assert!(json.contains("\"paused\": null"));
    }
}
