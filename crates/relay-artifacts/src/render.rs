//! Artifact Markdown rendering.
//!
//! Turns a validated draft payload into a deterministic, human-readable
//! Markdown body (same input always produces the identical string). Sections,
//! headings, and tables are fixed per artifact kind so that diffs between
//! versions are meaningful; user/model prose only ever appears as content, and
//! table cells are escaped so values cannot break out of the table. No HTML is
//! emitted.
//!
//! Byte-for-byte port of `src/artifacts/render.ts`.

use relay_core::ArtifactKind;
use serde_json::Value;

use crate::drafts::{
    ArtifactDraft, DecisionLogDraft, DeliveryReportDraft, ExecutionPlanDraft,
    OpportunityBriefDraft, PrdDraft, TrdDraft, VerificationReportDraft, require_artifact_draft,
};
use crate::error::ArtifactsError;

/// Deterministic placeholder for empty lists (`_(none recorded)_`).
const NONE: &str = "_(none recorded)_";

/// Heading plus its content, separated by a blank line.
fn section(heading: &str, content: String) -> String {
    format!("{heading}\n\n{content}")
}

/// Markdown bullet list, or a deterministic placeholder when empty.
fn bullets(items: &[String]) -> String {
    if items.is_empty() {
        return NONE.to_string();
    }
    items
        .iter()
        .map(|item| format!("- {item}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Sub-bullets indented under a labeled parent line.
fn labeled_list<F>(label: &str, items: &[String], item_formatter: F) -> String
where
    F: Fn(&str) -> String,
{
    if items.is_empty() {
        return format!("- **{label}:** none");
    }
    let rendered = items
        .iter()
        .map(|item| format!("  - {}", item_formatter(item)))
        .collect::<Vec<_>>()
        .join("\n");
    format!("- **{label}:**\n{rendered}")
}

/// Inline comma-joined values, or "none".
fn inline_list(items: &[String]) -> String {
    if items.is_empty() {
        "none".to_string()
    } else {
        items.join(", ")
    }
}

/// Make a value safe for a single table row: escape pipes, flatten newlines
/// (runs of CR/LF collapse to one space).
fn cell(value: &str) -> String {
    let escaped = value.replace('|', "\\|");
    let mut out = String::with_capacity(escaped.len());
    let mut in_break = false;
    for ch in escaped.chars() {
        if ch == '\r' || ch == '\n' {
            if !in_break {
                out.push(' ');
                in_break = true;
            }
        } else {
            out.push(ch);
            in_break = false;
        }
    }
    out
}

fn table(headers: &[&str], rows: &[Vec<String>]) -> String {
    let head = format!("| {} |", headers.join(" | "));
    let separator = format!(
        "| {} |",
        headers
            .iter()
            .map(|_| "---")
            .collect::<Vec<_>>()
            .join(" | ")
    );
    let body = rows
        .iter()
        .map(|row| {
            format!(
                "| {} |",
                row.iter().map(|c| cell(c)).collect::<Vec<_>>().join(" | ")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    [head, separator, body]
        .into_iter()
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn join_sections(sections: &[String]) -> String {
    let mut out = sections.join("\n\n");
    out.push('\n');
    out
}

fn render_opportunity_brief(draft: &OpportunityBriefDraft) -> String {
    let mut sections = vec![format!("# {}", draft.title)];
    sections.push(section("## Problem", draft.problem.clone()));
    sections.push(section(
        "## Target users",
        table(
            &["Name", "Description"],
            &draft
                .target_users
                .iter()
                .map(|user| vec![user.name.clone(), user.description.clone()])
                .collect::<Vec<_>>(),
        ),
    ));
    sections.push(section(
        "## Evidence",
        bullets(
            &draft
                .evidence
                .iter()
                .map(|item| {
                    if item.source.is_empty() {
                        item.description.clone()
                    } else {
                        format!("{} (source: {})", item.description, item.source)
                    }
                })
                .collect::<Vec<_>>(),
        ),
    ));
    sections.push(section(
        "## Desired outcomes",
        bullets(&draft.desired_outcomes),
    ));
    sections.push(section("## Assumptions", bullets(&draft.assumptions)));
    sections.push(section("## Constraints", bullets(&draft.constraints)));
    sections.push(section("## Open questions", bullets(&draft.open_questions)));
    join_sections(&sections)
}

fn render_prd(draft: &PrdDraft) -> String {
    let mut sections = vec![format!("# {}", draft.title)];
    sections.push(section("## Summary", draft.summary.clone()));
    sections.push(section(
        "## Requirements",
        draft
            .requirements
            .iter()
            .map(|requirement| {
                [
                    format!("### {}", requirement.id),
                    requirement.statement.clone(),
                    format!("_Rationale:_ {}", requirement.rationale),
                ]
                .join("\n\n")
            })
            .collect::<Vec<_>>()
            .join("\n\n"),
    ));
    sections.push(section("## Exclusions", bullets(&draft.exclusions)));
    sections.push(section(
        "## Acceptance criteria",
        draft
            .acceptance_criteria
            .iter()
            .map(|criterion| {
                [
                    format!("### {}", criterion.id),
                    format!("- **Given:** {}", criterion.given),
                    format!("- **When:** {}", criterion.when),
                    format!("- **Then:** {}", criterion.then),
                ]
                .join("\n")
            })
            .collect::<Vec<_>>()
            .join("\n\n"),
    ));
    join_sections(&sections)
}

fn render_trd(draft: &TrdDraft) -> String {
    let mut sections = vec![format!("# {}", draft.title)];
    sections.push(section("## Summary", draft.summary.clone()));
    sections.push(section(
        "## Repository findings",
        if draft.repository_findings.is_empty() {
            NONE.to_string()
        } else {
            table(
                &["Area", "Finding"],
                &draft
                    .repository_findings
                    .iter()
                    .map(|finding| vec![finding.area.clone(), finding.finding.clone()])
                    .collect::<Vec<_>>(),
            )
        },
    ));
    sections.push(section("## Architecture", draft.architecture.clone()));
    sections.push(section(
        "## Interfaces",
        bullets(
            &draft
                .interfaces
                .iter()
                .map(|iface| format!("**{}** — {}", iface.name, iface.description))
                .collect::<Vec<_>>(),
        ),
    ));
    sections.push(section(
        "## Migrations",
        bullets(
            &draft
                .migrations
                .iter()
                .map(|migration| format!("{} (risk: {})", migration.description, migration.risk))
                .collect::<Vec<_>>(),
        ),
    ));
    sections.push(section(
        "## Risks",
        bullets(
            &draft
                .risks
                .iter()
                .map(|risk| format!("{} — mitigation: {}", risk.risk, risk.mitigation))
                .collect::<Vec<_>>(),
        ),
    ));
    sections.push(section("## Rollout", draft.rollout_plan.clone()));
    sections.push(section("## Rollback", draft.rollback_plan.clone()));
    join_sections(&sections)
}

fn render_execution_plan(draft: &ExecutionPlanDraft) -> String {
    let mut sections = vec![format!("# {}", draft.title)];
    sections.push(section("## Summary", draft.summary.clone()));
    let tasks = draft
        .tasks
        .iter()
        .map(|task| {
            [
                format!("### Task {} — {}", task.id, task.title),
                task.description.clone(),
                format!("- **Dependencies:** {}", inline_list(&task.dependencies)),
                labeled_list("Affected files", &task.affected_files, |file| {
                    format!("`{file}`")
                }),
                labeled_list("Checks", &task.checks, |check| format!("`{check}`")),
                labeled_list("Integration points", &task.integration_points, |point| {
                    point.to_string()
                }),
            ]
            .join("\n\n")
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    sections.push(section("## Tasks", tasks));
    join_sections(&sections)
}

fn render_verification_report(draft: &VerificationReportDraft) -> String {
    let mut sections = vec![format!("# {}", draft.title)];
    sections.push(section("## Summary", draft.summary.clone()));
    sections.push(section(
        "## Results",
        table(
            &["Criterion", "Status", "Evidence"],
            &draft
                .results
                .iter()
                .map(|result| {
                    vec![
                        if result.criterion_id.is_empty() {
                            result.criterion.clone()
                        } else {
                            format!("{} — {}", result.criterion_id, result.criterion)
                        },
                        result.status.as_str().to_string(),
                        result.evidence.clone(),
                    ]
                })
                .collect::<Vec<_>>(),
        ),
    ));
    sections.push(section(
        "## Overall",
        format!("**{}**", if draft.overall_pass { "pass" } else { "fail" }),
    ));
    if !draft.notes.is_empty() {
        sections.push(section("## Notes", draft.notes.clone()));
    }
    join_sections(&sections)
}

fn render_delivery_report(draft: &DeliveryReportDraft) -> String {
    let mut sections = vec![format!("# {}", draft.title)];
    sections.push(section("## Summary", draft.summary.clone()));
    let trace = draft
        .trace
        .iter()
        .map(|entry| {
            [
                format!(
                    "### {}",
                    if entry.requirement_id.is_empty() {
                        "Requirement"
                    } else {
                        &entry.requirement_id
                    }
                ),
                entry.requirement.clone(),
                labeled_list("Design decisions", &entry.design_decisions, |decision| {
                    decision.to_string()
                }),
                labeled_list("Changes", &entry.changes, |change| change.to_string()),
                labeled_list("Evidence", &entry.evidence, |item| item.to_string()),
            ]
            .join("\n\n")
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    sections.push(section("## Trace", trace));
    sections.push(section("## Next steps", bullets(&draft.next_steps)));
    sections.push(section("## Held actions", bullets(&draft.held_actions)));
    join_sections(&sections)
}

fn render_decision_log(draft: &DecisionLogDraft) -> String {
    let mut sections = vec![format!("# {}", draft.title)];
    let entries = draft
        .entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let mut lines = vec![
                format!("- **Mode:** {}", entry.mode),
                format!("- **Rationale:** {}", entry.rationale),
            ];
            if !entry.alternatives.is_empty() {
                lines.push(format!("- **Alternatives:** {}", entry.alternatives));
            }
            [
                format!("### {}. {}", index + 1, entry.decision),
                lines.join("\n"),
            ]
            .join("\n\n")
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    sections.push(section("## Entries", entries));
    join_sections(&sections)
}

/// Render an already-validated typed draft. Rendering is total and
/// deterministic: the same draft always yields the identical string.
pub fn render_draft(draft: &ArtifactDraft) -> String {
    match draft {
        ArtifactDraft::OpportunityBrief(inner) => render_opportunity_brief(inner),
        ArtifactDraft::Prd(inner) => render_prd(inner),
        ArtifactDraft::Trd(inner) => render_trd(inner),
        ArtifactDraft::ExecutionPlan(inner) => render_execution_plan(inner),
        ArtifactDraft::VerificationReport(inner) => render_verification_report(inner),
        ArtifactDraft::DeliveryReport(inner) => render_delivery_report(inner),
        ArtifactDraft::DecisionLog(inner) => render_decision_log(inner),
    }
}

/// Render an untrusted draft payload as a deterministic Markdown body. The
/// draft is validated first; invalid drafts fail with the full issue list.
pub fn render_artifact_markdown(
    kind: ArtifactKind,
    draft: &Value,
) -> Result<String, ArtifactsError> {
    let parsed = require_artifact_draft(kind, draft)?;
    Ok(render_draft(&parsed))
}
