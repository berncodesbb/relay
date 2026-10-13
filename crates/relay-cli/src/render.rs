//! Human-readable rendering of the application's view models.
//!
//! Pure functions returning strings: no printing, no styling decisions
//! beyond plain text. `--json` mode never reaches these; the dispatcher
//! serializes the view models instead. Only the CLI writes to
//! stdout/stderr, through the [`OutputWriter`](crate::output::OutputWriter)
//! seam.

use relay_app::views::{
    ArtifactDetailView, CancelView, DecisionListView, DoctorView, EvidenceListView, ExpandView,
    ExplainView, ImportPrdView, InitView, MigrateView, PublishArtifactViewView, RebuildView,
    ResolveDecisionView, RunView, SummaryView, WorkDetailView, WorkListView,
};

/// Render the project summary.
pub fn render_summary(view: &SummaryView) -> String {
    let mut out = String::new();
    match &view.goal {
        Some(goal) => {
            out.push_str(&format!("Goal: {}\n", goal.outcome));
            if !goal.success_definition.is_empty() {
                out.push_str(&format!("Success: {}\n", goal.success_definition));
            }
        }
        None => out.push_str("Goal: (none declared)\n"),
    }
    out.push_str(&format!(
        "Project: {} · revision {} · {}\n",
        view.project_id.as_deref().unwrap_or("(none)"),
        view.revision,
        view.completion
    ));
    out.push('\n');
    out.push_str("Phases:\n");
    for phase in &view.phases {
        out.push_str(&format!(
            "  {:<14} {}/{} done\n",
            phase.phase, phase.completed, phase.total
        ));
    }
    let counts = &view.counts;
    out.push('\n');
    out.push_str(&format!(
        "Work: {} ready · {} blocked · {} awaiting you · {} active · {} completed · {} invalid\n",
        counts.ready,
        counts.blocked,
        counts.awaiting_human,
        counts.active_attempts,
        counts.completed,
        counts.invalidated
    ));
    if !view.open_decisions.is_empty() {
        out.push_str(&format!(
            "Open decisions: {}\n",
            view.open_decisions.join(", ")
        ));
    }
    out.push('\n');
    out.push_str(&format!(
        "Recommendation: {}\n",
        view.recommendation.rationale
    ));
    out.push_str(&format!("Next: {}\n", view.recommendation.next.detail));
    out
}

/// Render a work list.
pub fn render_work_list(view: &WorkListView) -> String {
    let mut out = String::new();
    if view.works.is_empty() {
        out.push_str("(no work items)\n");
        return out;
    }
    for work in &view.works {
        out.push_str(&format!(
            "{} [{}] {} · {} · {}\n  {}\n",
            work.id, work.phase, work.kind, work.executor, work.status, work.objective
        ));
    }
    out.push_str(&format!(
        "({} item(s) at revision {})\n",
        view.works.len(),
        view.revision
    ));
    out
}

/// Render one work item in full.
pub fn render_work_detail(view: &WorkDetailView) -> String {
    let entry = &view.entry;
    let mut out = String::new();
    out.push_str(&format!(
        "{} [{}] {} · {} · {}\n",
        entry.id, entry.phase, entry.kind, entry.executor, entry.status
    ));
    out.push_str(&format!("  objective: {}\n", entry.objective));
    out.push_str(&format!("  required: {}\n", entry.required));
    out.push_str(&format!("  completion: {}\n", view.completion));
    if !view.dependencies.is_empty() {
        out.push_str("  depends on:\n");
        for dependency in &view.dependencies {
            out.push_str(&format!(
                "    {} ({}) — {}\n",
                dependency.to, dependency.kind, dependency.status
            ));
        }
    }
    if !view.dependents.is_empty() {
        out.push_str(&format!("  dependents: {}\n", view.dependents.join(", ")));
    }
    if !view.inputs.is_empty() {
        out.push_str(&format!("  inputs: {}\n", view.inputs.join(", ")));
    }
    if !view.expected_outputs.is_empty() {
        out.push_str(&format!(
            "  expected outputs: {}\n",
            view.expected_outputs.join(", ")
        ));
    }
    if !view.read_scopes.is_empty() {
        out.push_str(&format!("  reads: {}\n", view.read_scopes.join(", ")));
    }
    if !view.write_scopes.is_empty() {
        out.push_str(&format!("  writes: {}\n", view.write_scopes.join(", ")));
    }
    if !view.attempts.is_empty() {
        out.push_str("  attempts:\n");
        for attempt in &view.attempts {
            match &attempt.detail {
                Some(detail) => out.push_str(&format!(
                    "    {} {} — {}\n",
                    attempt.id, attempt.conclusion, detail
                )),
                None => out.push_str(&format!("    {} {}\n", attempt.id, attempt.conclusion)),
            }
        }
    }
    if !view.evidence.is_empty() {
        out.push_str("  evidence:\n");
        for evidence in &view.evidence {
            out.push_str(&format!(
                "    {} [{} · {}] {}\n",
                evidence.id, evidence.kind, evidence.outcome, evidence.summary
            ));
        }
    }
    out.push_str(&format!("\n  why: {}\n", entry.explanation));
    out
}

/// Render the decision list.
pub fn render_decisions(view: &DecisionListView) -> String {
    let mut out = String::new();
    if view.decisions.is_empty() {
        out.push_str("(no decisions)\n");
        return out;
    }
    for decision in &view.decisions {
        out.push_str(&format!("{} [{}]\n", decision.id, decision.status));
        out.push_str(&format!("  question: {}\n", decision.question));
        if let Some(work) = &decision.work {
            out.push_str(&format!("  completes: {work}\n"));
        }
        for option in &decision.options {
            out.push_str(&format!("  - {} — {}\n", option.label, option.description));
        }
        if let Some(recommendation) = &decision.recommendation {
            out.push_str(&format!("  recommended: {recommendation}\n"));
        }
        if let Some(choice) = &decision.choice {
            out.push_str(&format!("  resolved: {choice}\n"));
        }
        if let Some(rationale) = decision
            .rationale
            .as_deref()
            .filter(|rationale| !rationale.is_empty())
        {
            out.push_str(&format!("  rationale: {rationale}\n"));
        }
    }
    out
}

/// Render one artifact version with its Markdown view.
pub fn render_artifact(view: &ArtifactDetailView) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "{}@{} (current: {})\n",
        view.artifact, view.version, view.current_version
    ));
    if let Some(title) = &view.title {
        out.push_str(&format!("  title: {title}\n"));
    }
    if let Some(schema) = &view.schema {
        out.push_str(&format!("  schema: {schema}\n"));
    }
    out.push_str(&format!("  digest: {}\n", view.digest));
    if !view.derived_from.is_empty() {
        out.push_str(&format!(
            "  derived from: {}\n",
            view.derived_from.join(", ")
        ));
    }
    out.push_str("  content:\n");
    for line in serde_json::to_string_pretty(&view.content)
        .unwrap_or_else(|_| "{}".to_string())
        .lines()
    {
        out.push_str(&format!("    {line}\n"));
    }
    if let Some(markdown) = &view.markdown {
        out.push_str("\n  view:\n");
        for line in markdown.lines() {
            out.push_str(&format!("    {line}\n"));
        }
    }
    out
}

/// Render an evidence list.
pub fn render_evidence(view: &EvidenceListView) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "Evidence for {} (revision {}):\n",
        view.target, view.revision
    ));
    if view.evidence.is_empty() {
        out.push_str("  (none)\n");
        return out;
    }
    for evidence in &view.evidence {
        out.push_str(&format!(
            "  {} [{} · {}] {}\n",
            evidence.id, evidence.kind, evidence.outcome, evidence.summary
        ));
    }
    out
}

/// Render an explanation.
pub fn render_explain(view: &ExplainView) -> String {
    format!("{}\n", view.detail)
}

/// Render `init`'s result.
pub fn render_init(view: &InitView) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "Initialized project {} (goal {})\n",
        view.project, view.goal.id
    ));
    out.push_str(&format!("  goal: {}\n", view.goal.outcome));
    for fact in &view.applied {
        out.push_str(&format!("  + {fact}\n"));
    }
    out.push_str(&format!("\nNext: {}\n", view.recommendation.next.detail));
    out
}

/// Render `expand`'s result.
pub fn render_expand(view: &ExpandView) -> String {
    let mut out = String::new();
    if view.applied.is_empty() && view.rejected.is_empty() {
        out.push_str("nothing to propose at this revision\n");
        return out;
    }
    for applied in &view.applied {
        out.push_str(&format!("applied: {}\n", applied.summary));
        for fact in &applied.facts {
            out.push_str(&format!("  + {fact}\n"));
        }
        for note in &applied.notes {
            out.push_str(&format!("  note: {note}\n"));
        }
    }
    for rejected in &view.rejected {
        out.push_str(&format!("rejected: {}\n", rejected.summary));
        out.push_str(&format!("  reason: {}\n", rejected.reason));
    }
    out.push_str(&format!("\nNext: {}\n", view.next.detail));
    out
}

/// Render `run`'s result.
pub fn render_run(view: &RunView) -> String {
    let mut out = String::new();
    out.push_str(&format!("run (autonomy {})\n", view.autonomy));
    if view.ticks.is_empty() {
        out.push_str("  no ticks ran\n");
    }
    for tick in &view.ticks {
        out.push_str(&format!("tick {}:\n", tick.index));
        for started in &tick.started {
            out.push_str(&format!("  started {started}\n"));
        }
        for accepted in &tick.accepted {
            out.push_str(&format!("  accepted {accepted}\n"));
        }
        for failed in &tick.failed {
            out.push_str(&format!(
                "  failed {} — {}\n",
                failed.attempt, failed.reason
            ));
        }
        for canceled in &tick.canceled {
            out.push_str(&format!("  canceled {canceled}\n"));
        }
        for refusal in &tick.refusals {
            out.push_str(&format!(
                "  refused {} — {}\n",
                refusal.work, refusal.reason
            ));
        }
    }
    for published in &view.published_views {
        out.push_str(&format!(
            "  published view {}@{} -> {}\n",
            published.artifact, published.version, published.path
        ));
    }
    for note in &view.view_notes {
        out.push_str(&format!("  view note: {note}\n"));
    }
    if !view.awaiting_human.is_empty() {
        out.push_str("\ndecisions awaiting you (never auto-resolved):\n");
        for decision in &view.awaiting_human {
            out.push_str(&format!("  {} — {}\n", decision.id, decision.question));
        }
    }
    out.push_str(&format!("\nNext: {}\n", view.recommendation.next.detail));
    out
}

/// Render a decision resolution.
pub fn render_resolve(view: &ResolveDecisionView) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "resolved {} with \"{}\"\n",
        view.decision.id,
        view.decision.choice.as_deref().unwrap_or_default()
    ));
    for work in &view.completed_work {
        out.push_str(&format!("  completed {work}\n"));
    }
    out.push_str(&format!("\nNext: {}\n", view.recommendation.next.detail));
    out
}

/// Render a cancellation.
pub fn render_cancel(view: &CancelView) -> String {
    format!(
        "canceled attempt {} (work {}) at revision {}\n",
        view.attempt, view.work, view.revision
    )
}

/// Render a controlled PRD import.
pub fn render_import(view: &ImportPrdView) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "imported a controlled PRD revision; {} will publish it\n",
        view.work
    ));
    for document in &view.documents {
        out.push_str(&format!(
            "  revised {} [{}] {}\n",
            document.artifact, document.schema, document.slug
        ));
    }
    out
}

/// Render an artifact-view publication.
pub fn render_publish(view: &PublishArtifactViewView) -> String {
    format!(
        "published the Markdown view of {}@{} to {} (sha256 {}, evidence {})\n",
        view.artifact, view.version, view.path, view.sha256, view.evidence
    )
}

/// Render the doctor report.
pub fn render_doctor(view: &DoctorView) -> String {
    let mut out = String::new();
    out.push_str(if view.healthy {
        "fact store healthy\n"
    } else {
        "fact store has errors\n"
    });
    for finding in &view.findings {
        out.push_str(&format!(
            "  [{}] {}: {}\n",
            finding.severity, finding.code, finding.detail
        ));
    }
    out
}

/// Render a projection rebuild.
pub fn render_rebuild(view: &RebuildView) -> String {
    match &view.quarantine {
        Some(path) => format!(
            "rebuilt the projection from {} fact(s); previous database quarantined at {path}\n",
            view.facts_indexed
        ),
        None => format!(
            "rebuilt the projection from {} fact(s); no previous database existed\n",
            view.facts_indexed
        ),
    }
}

/// Render a migration result.
pub fn render_migrate(view: &MigrateView) -> String {
    let mut out = String::new();
    out.push_str(&format!("{}\n", view.detail));
    if view.available {
        out.push_str(&format!(
            "  legacy events read: {}\n  facts emitted: {}\n",
            view.legacy_events, view.facts_emitted
        ));
        for unmapped in &view.unmapped {
            out.push_str(&format!("  unmapped legacy kind: {unmapped}\n"));
        }
        if view.inferences > 0 {
            out.push_str(&format!(
                "  coarse inferences reported: {}\n",
                view.inferences
            ));
        }
        for note in &view.needs_review {
            out.push_str(&format!("  needs your review: {note}\n"));
        }
        if let Some(root) = &view.store_root {
            out.push_str(&format!("  new store: {root}\n"));
        }
        if let Some(backup) = &view.previous_store_backup {
            out.push_str(&format!("  previous store retained at: {backup}\n"));
        }
    }
    out
}
