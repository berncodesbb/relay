//! Human renderers for the status and why views. Ported from
//! `src/cli/output.ts` (`renderStatus`/`renderWhy` and their internals): the
//! TypeScript renderers probe view fields defensively with type guards, so
//! every field contributes a line only when it carries a non-blank value —
//! the renderers below apply the same `stringOf` semantics (a string counts
//! when its trimmed form is non-empty, and it is printed untrimmed) to the
//! typed views. Rendering returns strings; nothing here prints.

use crate::views::{StatusArtifact, StatusView, WhyView};

/// Number of routing rows rendered by `render_why` (the TypeScript
/// `renderRouting` tail limit).
const ROUTING_ROWS: usize = 12;
/// Number of rejected transitions rendered by `render_why`.
const REJECTED_ROWS: usize = 5;

/// `stringOf`: the value when its trimmed form is non-empty, else `None`.
fn string_of(value: &str) -> Option<&str> {
    if value.trim().is_empty() {
        None
    } else {
        Some(value)
    }
}

/// Pad `text` on the right to `width` (JS `String.prototype.padEnd`; Relay's
/// rendered fields are ASCII so char counts agree).
fn pad_end(text: &str, width: usize) -> String {
    let length = text.chars().count();
    if length >= width {
        text.to_string()
    } else {
        format!("{text}{}", " ".repeat(width - length))
    }
}

fn bullet(text: &str) -> String {
    format!("  - {text}")
}

/// Render a status view as aligned key: value blocks plus artifact/blocker
/// lists. Empty fields are skipped exactly like the TypeScript renderer.
pub fn render_status(view: &StatusView) -> String {
    let mut lines: Vec<String> = Vec::new();

    if let Some(goal) = string_of(&view.goal) {
        lines.push(format!("Goal: {goal}"));
    }
    lines.push(format!("Mode: {}", view.mode.as_str()));
    if let Some(autonomy) = string_of(&view.autonomy) {
        lines.push(format!("Autonomy: {autonomy}"));
    }

    if let Some(paused) = &view.paused {
        let reason = string_of(&paused.reason).unwrap_or("paused");
        lines.push(format!("Paused: {reason}"));
        if let Some(question) = string_of(paused.question.as_deref().unwrap_or("")) {
            lines.push(format!("  question: {question}"));
        }
        for option in &paused.options {
            if !option.is_empty() {
                lines.push(format!("  option: {option}"));
            }
        }
    }

    // describeProgress: `{stage, total}` numbers render as `stage s/t`.
    lines.push(format!(
        "Progress: stage {}/{}",
        view.progress.stage, view.progress.total
    ));

    if !view.stale_artifacts.is_empty() {
        lines.push("Stale artifacts:".to_string());
        for artifact_id in &view.stale_artifacts {
            lines.push(format!("! {artifact_id} is stale"));
        }
    }

    if !view.artifacts.is_empty() {
        lines.push("Artifacts:".to_string());
        lines.extend(render_artifact_rows(&view.artifacts));
    }

    if !view.blockers.is_empty() {
        lines.push("Blockers:".to_string());
        for blocker in &view.blockers {
            lines.push(format!("! {blocker}"));
        }
    }

    if !view.changed_files.is_empty() {
        lines.push("Changed files:".to_string());
        for file in &view.changed_files {
            lines.push(format!("  {file}"));
        }
    }

    let mut stats_parts: Vec<String> = Vec::new();
    stats_parts.push(format!("events: {}", view.stats.events));
    stats_parts.push(format!("model calls: {}", view.stats.model_calls));
    stats_parts.push(format!("tool calls: {}", view.stats.tool_calls));
    lines.push(format!("Stats: {}", stats_parts.join("  ")));

    if let Some(next) = string_of(&view.next_action) {
        lines.push(format!("Next: {next}"));
    }

    lines.join("\n")
}

/// Render an explanation view: mode, last transition, evidence, routing,
/// rejected transitions, invalidated artifacts, and pause reason.
pub fn render_why(view: &WhyView) -> String {
    let mut lines: Vec<String> = Vec::new();

    lines.push(format!("Mode: {}", view.mode.as_str()));

    if let Some(outcome) = &view.last_outcome {
        let outcome_text = string_of(&outcome.outcome);
        let outcome_reason = string_of(&outcome.reason);
        if outcome_text.is_some() || outcome_reason.is_some() {
            let mut line = format!("Last outcome: {}", outcome_text.unwrap_or_default());
            if let Some(reason) = outcome_reason {
                line.push_str(" — ");
                line.push_str(reason);
            }
            lines.push(line.trim_end().to_string());
        }
    }

    if let Some(transition) = view.recent_transitions.last() {
        lines.push(format!(
            "Last transition: {}",
            render_transition(transition)
        ));
    }

    if let Some(outcome) = view
        .last_outcome
        .as_ref()
        .filter(|o| !o.evidence.is_empty())
    {
        lines.push("Evidence:".to_string());
        for item in &outcome.evidence {
            lines.push(bullet(item));
        }
    }

    let routing_lines = render_routing(&view.routing);
    if !routing_lines.is_empty() {
        lines.push("Routing:".to_string());
        lines.extend(routing_lines);
    }

    if !view.rejected_transitions.is_empty() {
        lines.push("Rejected transitions:".to_string());
        let tail_start = view
            .rejected_transitions
            .len()
            .saturating_sub(REJECTED_ROWS);
        for rejected in &view.rejected_transitions[tail_start..] {
            let mut line = format!(
                "{} -> {} rejected",
                rejected.from.as_str(),
                rejected.to.as_str()
            );
            if string_of(&rejected.reason).is_some() {
                line.push_str(" — ");
                line.push_str(&rejected.reason);
            }
            lines.push(bullet(&line));
        }
    }

    if !view.stale_artifacts.is_empty() {
        lines.push("Invalidated artifacts:".to_string());
        for item in &view.stale_artifacts {
            lines.push(bullet(item));
        }
    }

    if let Some(reason) = string_of(view.paused_reason.as_deref().unwrap_or("")) {
        lines.push(format!("Paused: {reason}"));
    }

    lines.join("\n")
}

/// `from -> to (source) — reason`, collapsing absent ends exactly like the
/// TypeScript `latestTransition` helper.
fn render_transition(transition: &crate::views::WhyTransition) -> String {
    let from = string_of(transition.from.map(|m| m.as_str()).unwrap_or(""));
    let to = string_of(transition.to.as_str());
    let source = string_of(&transition.source);
    let reason = string_of(&transition.reason);
    let path = match (from, to) {
        (Some(from), Some(to)) => format!("{from} -> {to}"),
        (Some(only), None) | (None, Some(only)) => only.to_string(),
        (None, None) => String::new(),
    };
    let qualifier = source
        .map(|source| format!(" ({source})"))
        .unwrap_or_default();
    let why = reason
        .map(|reason| format!(" — {reason}"))
        .unwrap_or_default();
    format!("{path}{qualifier}{why}")
}

/// Route description: `role provider/model` when the routing row carries a
/// semantic model-role name, plain `provider/model` for legacy role-less
/// rows. The tail is limited to the last [`ROUTING_ROWS`] rows; failed rows
/// are prefixed `! `.
fn render_routing(routing: &[crate::views::WhyRouting]) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let tail_start = routing.len().saturating_sub(ROUTING_ROWS);
    for row in &routing[tail_start..] {
        let route = format!("{}/{}", row.provider, row.model);
        let route = match string_of(row.role.as_deref().unwrap_or("")) {
            Some(role) => format!("{role} {route}"),
            None => route,
        };
        let prefix = if row.ok { "  " } else { "! " };
        let mut line = format!("{prefix}{}: {route}", row.purpose);
        if line.trim_end().len() < line.len() {
            line = line.trim_end().to_string();
        }
        lines.push(line);
    }
    lines
}

/// Aligned `kind  version  status  title` rows for the artifacts table.
fn render_artifact_rows(rows: &[StatusArtifact]) -> Vec<String> {
    let cells: Vec<[String; 4]> = rows
        .iter()
        .map(|row| {
            [
                row.kind.clone(),
                format!("v{}", row.version),
                row.status.clone(),
                row.title.clone(),
            ]
        })
        .collect();
    let width = |index: usize| {
        cells
            .iter()
            .map(|cell| cell[index].chars().count())
            .max()
            .unwrap_or(0)
    };
    let (w0, w1, w2) = (width(0), width(1), width(2));
    cells
        .into_iter()
        .map(|cell| {
            format!(
                "  {}  {}  {}  {}",
                pad_end(&cell[0], w0),
                pad_end(&cell[1], w1),
                pad_end(&cell[2], w2),
                cell[3]
            )
            .trim_end()
            .to_string()
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::views::{
        StatusPause, StatusProgress, StatusStats, WhyOutcome, WhyRejection, WhyRouting,
        WhyTransition,
    };
    use relay_core::modes::WorkflowMode;

    fn status_fixture() -> StatusView {
        StatusView {
            goal: "Ship it".to_string(),
            mode: WorkflowMode::Specification,
            autonomy: "next-checkpoint".to_string(),
            paused: Some(StatusPause {
                by: "policy".to_string(),
                reason: "PRD is a contractual artifact".to_string(),
                question: Some("Approve the PRD?".to_string()),
                options: vec!["approve it".to_string(), "request changes".to_string()],
            }),
            progress: StatusProgress { stage: 2, total: 7 },
            artifacts: vec![
                StatusArtifact {
                    artifact_id: "art-1".to_string(),
                    kind: "opportunity-brief".to_string(),
                    version: 1,
                    status: "approved".to_string(),
                    title: "Brief".to_string(),
                },
                StatusArtifact {
                    artifact_id: "art-2".to_string(),
                    kind: "prd".to_string(),
                    version: 3,
                    status: "draft".to_string(),
                    title: "Product requirements".to_string(),
                },
            ],
            stale_artifacts: vec!["art-0".to_string()],
            changed_files: vec!["src/a.js".to_string()],
            blockers: vec!["prd is stale — regenerate it".to_string()],
            next_action: "approve the prd with /approve".to_string(),
            stats: StatusStats {
                events: 12,
                model_calls: 2,
                tool_calls: 4,
            },
        }
    }

    #[test]
    fn renders_status_blocks_in_reference_order() {
        let text = render_status(&status_fixture());
        assert_eq!(
            text,
            [
                "Goal: Ship it",
                "Mode: specification",
                "Autonomy: next-checkpoint",
                "Paused: PRD is a contractual artifact",
                "  question: Approve the PRD?",
                "  option: approve it",
                "  option: request changes",
                "Progress: stage 2/7",
                "Stale artifacts:",
                "! art-0 is stale",
                "Artifacts:",
                "  opportunity-brief  v1  approved  Brief",
                "  prd                v3  draft     Product requirements",
                "Blockers:",
                "! prd is stale — regenerate it",
                "Changed files:",
                "  src/a.js",
                "Stats: events: 12  model calls: 2  tool calls: 4",
                "Next: approve the prd with /approve",
            ]
            .join("\n")
        );
    }

    #[test]
    fn minimal_status_omits_optional_blocks() {
        let text = render_status(&StatusView {
            goal: String::new(),
            mode: WorkflowMode::Discovery,
            autonomy: String::new(),
            paused: None,
            progress: StatusProgress { stage: 1, total: 7 },
            artifacts: vec![],
            stale_artifacts: vec![],
            changed_files: vec![],
            blockers: vec![],
            next_action: String::new(),
            stats: StatusStats {
                events: 1,
                model_calls: 0,
                tool_calls: 0,
            },
        });
        assert_eq!(
            text,
            [
                "Mode: discovery",
                "Progress: stage 1/7",
                "Stats: events: 1  model calls: 0  tool calls: 0"
            ]
            .join("\n")
        );
    }

    #[test]
    fn renders_why_blocks_with_routing_tail_and_rejections() {
        let mut routing = vec![WhyRouting {
            provider: "fake".to_string(),
            model: "fake-standard".to_string(),
            purpose: "intent".to_string(),
            role: None,
            at: "t".to_string(),
            ok: true,
        }];
        for index in 0..14 {
            routing.push(WhyRouting {
                provider: "anthropic".to_string(),
                model: "claude-sonnet-5-5".to_string(),
                purpose: format!("mode:{index}"),
                role: Some("reasoning".to_string()),
                at: "t".to_string(),
                ok: index != 13,
            });
        }
        let view = WhyView {
            mode: WorkflowMode::Verification,
            last_outcome: Some(WhyOutcome {
                outcome: "ADVANCE".to_string(),
                reason: "brief approved".to_string(),
                evidence: vec!["opportunity-brief v1 approved".to_string()],
                at: "t".to_string(),
            }),
            recent_transitions: vec![WhyTransition {
                from: Some(WorkflowMode::Discovery),
                to: WorkflowMode::Specification,
                reason: "brief approved".to_string(),
                at: "t".to_string(),
                source: "auto".to_string(),
            }],
            rejected_transitions: vec![WhyRejection {
                from: WorkflowMode::Discovery,
                to: WorkflowMode::Delivery,
                reason: "missing artifacts".to_string(),
                at: "t".to_string(),
            }],
            routing,
            stale_artifacts: vec!["art-x".to_string()],
            paused_reason: None,
        };
        let text = render_why(&view);
        let lines: Vec<&str> = text.split('\n').collect();
        assert_eq!(lines[0], "Mode: verification");
        assert_eq!(lines[1], "Last outcome: ADVANCE — brief approved");
        assert_eq!(
            lines[2],
            "Last transition: discovery -> specification (auto) — brief approved"
        );
        assert_eq!(lines[3], "Evidence:");
        assert_eq!(lines[4], "  - opportunity-brief v1 approved");
        assert_eq!(lines[5], "Routing:");
        // 15 rows total, tail of 12: the legacy (role-less) first row drops off.
        let routing_lines: Vec<&&str> = lines
            .iter()
            .filter(|l| l.contains("claude-sonnet"))
            .collect();
        assert_eq!(routing_lines.len(), 12);
        assert!(lines.contains(&"! mode:13: reasoning anthropic/claude-sonnet-5-5"));
        assert!(lines.contains(&"  - discovery -> delivery rejected — missing artifacts"));
        assert!(lines.contains(&"Invalidated artifacts:"));
        assert!(lines.contains(&"  - art-x"));
    }

    #[test]
    fn empty_why_view_renders_only_the_mode_line() {
        let view = WhyView {
            mode: WorkflowMode::Discovery,
            last_outcome: None,
            recent_transitions: vec![],
            rejected_transitions: vec![],
            routing: vec![],
            stale_artifacts: vec![],
            paused_reason: None,
        };
        assert_eq!(render_why(&view), "Mode: discovery");
    }
}
