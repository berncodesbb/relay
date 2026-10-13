#![allow(clippy::panic)] // test-support module; `replay_events` mirrors the TS helper that throws

//! Deterministic event factories for tests, mirroring the TypeScript
//! `test/unit/core/event-factories.ts`. Sequences and timestamps derive from a
//! module-local counter over a fixed base instant, so replayed logs are
//! reproducible; tests that build raw or corrupt logs can override
//! sequence/project id/timestamp per event.
//!
//! These helpers exist for tests in this crate and in downstream test crates;
//! they never construct events that the decoder would reject.

use crate::artifacts::ArtifactKind;
use crate::events::{RelayEvent, RelayEventType};
use crate::modes::WorkflowMode;
use crate::payloads::EventPayload;
use crate::payloads::*;
use std::cell::Cell;

/// Fixed base instant for factory timestamps (2026-10-12T00:00:00.000Z),
/// matching the TypeScript factories.
const BASE_MS: u64 = 1_791_763_200_000;

thread_local! {
    static COUNTER: Cell<u64> = const { Cell::new(0) };
}

/// Reset the sequence/timestamp counter for fully deterministic assertions.
pub fn reset_event_factories() {
    COUNTER.with(|counter| counter.set(0));
}

/// The current factory counter value (for tests that need to reason about it).
pub fn factory_counter() -> u64 {
    COUNTER.with(|counter| counter.get())
}

fn next_counter() -> u64 {
    COUNTER.with(|counter| {
        let value = counter.get() + 1;
        counter.set(value);
        value
    })
}

fn uuid_tail(n: u64) -> String {
    format!("{n:012x}")
}

fn iso_at(ms: u64) -> String {
    // Fixed-epoch formatting without chrono: base + ms over a known date.
    let total = BASE_MS + ms;
    let seconds = total / 1000;
    let millis = total % 1000;
    let (year, month, day, hour, minute, second) = civil_from_unix(seconds as i64);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

/// Days/civil-time from a Unix timestamp (Howard Hinnant's algorithm).
fn civil_from_unix(seconds: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = seconds.div_euclid(86_400);
    let secs_of_day = seconds.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = if m <= 2 { y + 1 } else { y };
    (
        year,
        m,
        d,
        (secs_of_day / 3600) as u32,
        ((secs_of_day % 3600) / 60) as u32,
        (secs_of_day % 60) as u32,
    )
}

/// The project id used by every factory event.
pub const FACTORY_PROJECT_ID: &str = "prj_00000000-0000-4000-8000-0000000000f1";

/// Build one typed event, assigning the next sequence, id, timestamp, and
/// correlation id from the shared counter.
pub fn make_event(event_type: RelayEventType, payload: EventPayload) -> RelayEvent {
    let n = next_counter();
    RelayEvent {
        id: format!("evt_00000000-0000-4000-8000-{}", uuid_tail(n)),
        project_id: FACTORY_PROJECT_ID.to_string(),
        sequence: n,
        session_id: None,
        event_type,
        schema_version: event_type.payload_version(),
        timestamp: iso_at(n),
        correlation_id: format!("cor_00000000-0000-4000-8000-{}", uuid_tail(n)),
        causation_id: None,
        payload,
    }
}

/// A `project.initialized` event with the given goal and autonomy.
pub fn initialized(goal: &str, autonomy: &str) -> RelayEvent {
    make_event(
        RelayEventType::ProjectInitialized,
        EventPayload::ProjectInitialized(ProjectInitializedPayload {
            goal: goal.to_string(),
            workspace_root: "/tmp/relay-workspace".to_string(),
            initial_autonomy: autonomy.to_string(),
        }),
    )
}

/// The default `project.initialized` event used across the TS-ported tests.
pub fn initialized_default() -> RelayEvent {
    initialized(
        "Ship a workflow CLI that preserves product intent",
        "workflow",
    )
}

/// An `artifact.created` event.
pub fn artifact_created(kind: ArtifactKind, artifact_id: &str, title: &str) -> RelayEvent {
    make_event(
        RelayEventType::ArtifactCreated,
        EventPayload::ArtifactCreated(ArtifactCreatedPayload {
            artifact_id: artifact_id.to_string(),
            kind,
            title: title.to_string(),
            version: 1,
            path: format!(
                ".relay/artifacts/{kind}-{artifact_id}.md",
                kind = kind.as_str()
            ),
            summary: "generated in test".to_string(),
        }),
    )
}

/// An `artifact.approved` event.
pub fn artifact_approved(artifact_id: &str, kind: ArtifactKind, version: i64) -> RelayEvent {
    make_event(
        RelayEventType::ArtifactApproved,
        EventPayload::ArtifactApproved(ArtifactApprovedPayload {
            artifact_id: artifact_id.to_string(),
            kind,
            version,
            approved_by: "user",
        }),
    )
}

/// An `artifact.revised` event bumping the version by exactly one.
pub fn artifact_revised(
    artifact_id: &str,
    kind: ArtifactKind,
    from_version: i64,
    reason: &str,
) -> RelayEvent {
    make_event(
        RelayEventType::ArtifactRevised,
        EventPayload::ArtifactRevised(ArtifactRevisedPayload {
            artifact_id: artifact_id.to_string(),
            kind,
            from_version,
            to_version: from_version + 1,
            path: format!(
                ".relay/artifacts/{kind}-{artifact_id}.md",
                kind = kind.as_str()
            ),
            reason: reason.to_string(),
        }),
    )
}

/// A `mode.entered` event.
pub fn mode_entered(
    to: WorkflowMode,
    from: Option<WorkflowMode>,
    source: AutoUserSource,
    reason: &str,
) -> RelayEvent {
    make_event(
        RelayEventType::ModeEntered,
        EventPayload::ModeEntered(ModeEnteredPayload {
            from,
            to,
            reason: reason.to_string(),
            source,
        }),
    )
}

/// A `verification.completed` event with the TS factory's three criteria.
pub fn verification_completed(passed: bool, report_artifact_id: Option<&str>) -> RelayEvent {
    make_event(
        RelayEventType::VerificationCompleted,
        EventPayload::VerificationCompleted(VerificationCompletedPayload {
            passed,
            results: vec![
                VerificationResult {
                    criterion: "criterion A".to_string(),
                    status: if passed {
                        VerificationStatus::Pass
                    } else {
                        VerificationStatus::Fail
                    },
                    evidence: "evidence A".to_string(),
                },
                VerificationResult {
                    criterion: "criterion B".to_string(),
                    status: VerificationStatus::Pass,
                    evidence: "evidence B".to_string(),
                },
                VerificationResult {
                    criterion: "criterion C".to_string(),
                    status: VerificationStatus::Unevaluated,
                    evidence: String::new(),
                },
            ],
            report_artifact_id: report_artifact_id.map(str::to_string),
        }),
    )
}

/// Renumber events to contiguous 1..n sequences and replay them (factory
/// helpers draw sequence numbers from a shared counter, so any log assembled
/// from them must be renumbered first — exactly like the TS `replay` helper).
pub fn replay_events(events: &[RelayEvent]) -> crate::project_state::ProjectState {
    let renumbered: Vec<RelayEvent> = events
        .iter()
        .enumerate()
        .map(|(index, event)| {
            let mut renumbered = event.clone();
            renumbered.sequence = (index + 1) as u64;
            renumbered
        })
        .collect();
    match crate::reducer::replay_project(&renumbered) {
        Ok(state) => state,
        Err(error) => panic!("factory replay failed: {error}"),
    }
}

/// A full approved pipeline from initialization through the delivery report,
/// mirroring `fullPipelineApproved()` in the TypeScript factories.
pub fn full_pipeline_approved() -> Vec<RelayEvent> {
    vec![
        initialized_default(),
        artifact_created(
            ArtifactKind::OpportunityBrief,
            "art-brief",
            "Test opportunity-brief",
        ),
        artifact_approved("art-brief", ArtifactKind::OpportunityBrief, 1),
        mode_entered(
            WorkflowMode::Specification,
            Some(WorkflowMode::Discovery),
            AutoUserSource::Auto,
            "brief approved",
        ),
        artifact_created(ArtifactKind::Prd, "art-prd", "Test prd"),
        artifact_approved("art-prd", ArtifactKind::Prd, 1),
        mode_entered(
            WorkflowMode::TechnicalDesign,
            Some(WorkflowMode::Specification),
            AutoUserSource::Auto,
            "prd approved",
        ),
        artifact_created(ArtifactKind::Trd, "art-trd", "Test trd"),
        artifact_approved("art-trd", ArtifactKind::Trd, 1),
        mode_entered(
            WorkflowMode::Planning,
            Some(WorkflowMode::TechnicalDesign),
            AutoUserSource::Auto,
            "trd approved",
        ),
        artifact_created(
            ArtifactKind::ExecutionPlan,
            "art-plan",
            "Test execution-plan",
        ),
        artifact_approved("art-plan", ArtifactKind::ExecutionPlan, 1),
        mode_entered(
            WorkflowMode::Implementation,
            Some(WorkflowMode::Planning),
            AutoUserSource::Auto,
            "plan approved",
        ),
        artifact_created(ArtifactKind::DecisionLog, "art-log", "Test decision-log"),
        mode_entered(
            WorkflowMode::Verification,
            Some(WorkflowMode::Implementation),
            AutoUserSource::Auto,
            "implementation complete",
        ),
        artifact_created(
            ArtifactKind::VerificationReport,
            "art-vr",
            "Test verification-report",
        ),
        verification_completed(true, Some("art-vr")),
        artifact_approved("art-vr", ArtifactKind::VerificationReport, 1),
        mode_entered(
            WorkflowMode::Delivery,
            Some(WorkflowMode::Verification),
            AutoUserSource::Auto,
            "verification approved",
        ),
        artifact_created(
            ArtifactKind::DeliveryReport,
            "art-dr",
            "Test delivery-report",
        ),
    ]
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn produces_reproducible_ids_and_timestamps() {
        reset_event_factories();
        let first = initialized_default();
        let second = initialized_default();
        assert_eq!(first.sequence, 1);
        assert_eq!(second.sequence, 2);
        assert_eq!(first.timestamp, "2026-10-12T00:00:00.001Z");
        assert_eq!(second.timestamp, "2026-10-12T00:00:00.002Z");
        assert_eq!(first.id, "evt_00000000-0000-4000-8000-000000000001");
        assert_eq!(second.id, "evt_00000000-0000-4000-8000-000000000002");
    }
}
