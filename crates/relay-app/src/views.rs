//! Typed view models for the work-centric interface.
//!
//! These are serde-serializable snapshots of derived state, built **only**
//! from engine evaluation, workflow-catalog metadata, and graph reads. The
//! application owns no scheduling policy of its own: the "next action"
//! guidance re-uses engine/workflows output with documented deterministic
//! tie-breaks, and the CLI/TUI render these views without deciding
//! anything.
//!
//! JSON and human presentations are separate concerns owned by the
//! interface crates: these types are the single shape both render.

use relay_domain::{
    AttemptConclusion, AttemptOutcome, CompletionRule, DecisionId, DomainFact, Evidence,
    EvidenceKind, EvidenceOutcome, EvidenceTarget, GraphChangeProposal, ProjectGraph,
    ProjectRevision, WorkItemId, WorkNode,
};
use relay_engine::evaluate::Evaluation;
use relay_engine::explain::explain_status;
use relay_engine::schedule::{Autonomy, SchedulingPolicy};
use relay_engine::status::WorkStatus;
use relay_engine::work_status_by_id;
use relay_workflows::{Phase, WorkflowCatalog};

/// The seven delivery phases, in catalog (grouped-view) order. Used only to
/// order grouped views and the deterministic next-action tie-break — never
/// as engine state.
pub const PHASE_ORDER: [Phase; 7] = [
    Phase::Discovery,
    Phase::Specification,
    Phase::Design,
    Phase::Planning,
    Phase::Implementation,
    Phase::Verification,
    Phase::Delivery,
];

/// The goal as views carry it.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GoalView {
    /// The goal identity.
    pub id: String,
    /// The outcome statement.
    pub outcome: String,
    /// How success is recognized (may be empty).
    pub success_definition: String,
}

/// Progress of one delivery phase (a grouped view, not a state).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PhaseProgressView {
    /// The phase label (`Discovery` … `Delivery`).
    pub phase: String,
    /// Live work items of the phase's kinds.
    pub total: usize,
    /// Completed work items of the phase's kinds.
    pub completed: usize,
    /// Not-yet-completed work items of the phase's kinds.
    pub open: usize,
}

/// Counts by derived status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StatusCountsView {
    /// Ready work.
    pub ready: usize,
    /// Blocked work.
    pub blocked: usize,
    /// Work waiting on a human decision.
    pub awaiting_human: usize,
    /// Started, unconcluded attempts.
    pub active_attempts: usize,
    /// Invalid work.
    pub invalidated: usize,
    /// Completed work.
    pub completed: usize,
    /// Canceled work.
    pub canceled: usize,
    /// Superseded work.
    pub superseded: usize,
}

/// The project summary: what the interface opens with.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SummaryView {
    /// The project identity.
    pub project_id: Option<String>,
    /// The project goal.
    pub goal: Option<GoalView>,
    /// The revision (fact count) this view was derived at.
    pub revision: u64,
    /// `complete` or `in-progress`.
    pub completion: String,
    /// Required work still open.
    pub open_required: Vec<String>,
    /// Open (unresolved) decisions.
    pub open_decisions: Vec<String>,
    /// Status counts.
    pub counts: StatusCountsView,
    /// Phase-grouped progress.
    pub phases: Vec<PhaseProgressView>,
    /// The scheduling recommendation at the default policy.
    pub recommendation: RecommendationView,
}

/// One recommended attempt.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RecommendedEntryView {
    /// The work item.
    pub work: String,
    /// Its executor class.
    pub executor: String,
    /// Effective read scopes.
    pub read_scopes: Vec<String>,
    /// Effective write scopes (empty in the parallel batch).
    pub write_scopes: Vec<String>,
    /// Why it is recommended now.
    pub explanation: String,
}

/// One deferred ready item.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DeferredEntryView {
    /// The held-back work item.
    pub work: String,
    /// Why it was not recommended.
    pub reason: String,
}

/// Deterministic "what should happen next" guidance.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct NextActionView {
    /// `awaiting-human`, `ready-work`, `expand`, `active-attempts`, or
    /// `none`.
    pub kind: String,
    /// The id the action concerns (decision or work), when one applies.
    pub target: Option<String>,
    /// One-line guidance.
    pub detail: String,
}

/// The scheduling recommendation plus the derived next action.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RecommendationView {
    /// The autonomy the recommendation was computed under.
    pub autonomy: String,
    /// Work that may run concurrently (read-only/model-only).
    pub parallel: Vec<RecommendedEntryView>,
    /// Workspace-writing work to run one at a time.
    pub serialized: Vec<RecommendedEntryView>,
    /// Ready work held back, with reasons.
    pub deferred: Vec<DeferredEntryView>,
    /// The engine's rationale.
    pub rationale: String,
    /// The next action derived from engine/workflows output.
    pub next: NextActionView,
}

/// One work item as lists carry it.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WorkEntryView {
    /// The work identity.
    pub id: String,
    /// The work kind label.
    pub kind: String,
    /// The delivery phase the kind belongs to (grouped view).
    pub phase: String,
    /// The executor class.
    pub executor: String,
    /// The bounded objective.
    pub objective: String,
    /// Whether the project goal needs this work.
    pub required: bool,
    /// The derived status name.
    pub status: String,
    /// The deterministic explanation of that status.
    pub explanation: String,
}

/// A work list plus the revision it was derived at.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WorkListView {
    /// The entries, deterministically ordered.
    pub works: Vec<WorkEntryView>,
    /// The revision.
    pub revision: u64,
}

/// One dependency of a work item.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DependencyEntryView {
    /// The prerequisite.
    pub to: String,
    /// The edge kind.
    pub kind: String,
    /// The prerequisite's derived status.
    pub status: String,
}

/// One attempt of a work item.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AttemptEntryView {
    /// The attempt identity.
    pub id: String,
    /// Wall-clock milliseconds when it started.
    pub started_at_ms: i64,
    /// `active`, `accepted`, `failed`, `canceled`, or `abandoned`.
    pub conclusion: String,
    /// Terminal detail (failure reason, cancellation reason, …).
    pub detail: Option<String>,
}

/// One evidence record.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct EvidenceEntryView {
    /// The evidence identity.
    pub id: String,
    /// The kind of observation.
    pub kind: String,
    /// What it is attached to (`work:<id>`, `artifact:<ref>`, `decision:<id>`).
    pub target: String,
    /// `pass`, `fail`, or `info`.
    pub outcome: String,
    /// The one-line summary.
    pub summary: String,
    /// Wall-clock milliseconds when it was recorded.
    pub recorded_at_ms: i64,
}

/// Everything about one work item.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WorkDetailView {
    /// The list entry fields.
    pub entry: WorkEntryView,
    /// The dependencies gating it.
    pub dependencies: Vec<DependencyEntryView>,
    /// Work items depending on this one.
    pub dependents: Vec<String>,
    /// Declared inputs (exact refs or selection rules).
    pub inputs: Vec<String>,
    /// Expected outputs.
    pub expected_outputs: Vec<String>,
    /// Read scopes.
    pub read_scopes: Vec<String>,
    /// Effective write scopes.
    pub write_scopes: Vec<String>,
    /// The completion rule (`receipt`, `decision:<id>`, or
    /// `criterion:<ref>`).
    pub completion: String,
    /// The attempts that ran it.
    pub attempts: Vec<AttemptEntryView>,
    /// Evidence attached to it.
    pub evidence: Vec<EvidenceEntryView>,
}

/// One human-gate decision.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DecisionEntryView {
    /// The decision identity.
    pub id: String,
    /// The question.
    pub question: String,
    /// The options.
    pub options: Vec<DecisionOptionView>,
    /// The recommended option label, when present.
    pub recommendation: Option<String>,
    /// `open`, `resolved`, or `superseded`.
    pub status: String,
    /// The chosen label or free-form answer, when resolved.
    pub choice: Option<String>,
    /// The resolution rationale, when resolved.
    pub rationale: Option<String>,
    /// The superseding decision, when superseded.
    pub superseded_by: Option<String>,
    /// The work item completed by resolving this decision, when present.
    pub work: Option<String>,
}

/// One option of a decision.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DecisionOptionView {
    /// The option label.
    pub label: String,
    /// What choosing it means.
    pub description: String,
}

/// A decision list plus revision.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DecisionListView {
    /// The decisions, in id order.
    pub decisions: Vec<DecisionEntryView>,
    /// The revision.
    pub revision: u64,
}

/// A full artifact version with content and rendered view.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ArtifactDetailView {
    /// The artifact identity.
    pub artifact: String,
    /// The version number.
    pub version: u32,
    /// The version title, when present.
    pub title: Option<String>,
    /// The structured schema tag.
    pub schema: Option<String>,
    /// The authoritative structured content.
    pub content: serde_json::Value,
    /// The recorded digest commitment (64 hex chars).
    pub digest: String,
    /// The exact input versions this content was derived from.
    pub derived_from: Vec<String>,
    /// Wall-clock milliseconds when the version was published.
    pub published_at_ms: i64,
    /// The deterministic Markdown view, when renderable.
    pub markdown: Option<String>,
    /// Whether newer versions exist.
    pub current_version: u32,
}

/// An evidence list plus revision.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct EvidenceListView {
    /// What the evidence was collected for.
    pub target: String,
    /// The evidence records, in id order.
    pub evidence: Vec<EvidenceEntryView>,
    /// The revision.
    pub revision: u64,
}

/// The explanation of one work item.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ExplainView {
    /// The work identity.
    pub work: String,
    /// The derived status name.
    pub status: String,
    /// The deterministic prose (`<id> is <status>: <detail>`).
    pub detail: String,
}

/// What `init` did.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct InitView {
    /// The project identity.
    pub project: String,
    /// The goal.
    pub goal: GoalView,
    /// The revision after initialization.
    pub revision: u64,
    /// The facts appended, as one-line summaries.
    pub applied: Vec<String>,
    /// The recommendation after initialization.
    pub recommendation: RecommendationView,
}

/// What declaring a goal on an existing, goal-less project did (the
/// conversational `declare-goal` path).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DeclareGoalView {
    /// The goal.
    pub goal: GoalView,
    /// Work items the expansion round declared.
    pub work: Vec<String>,
    /// Decisions the expansion round opened.
    pub decisions: Vec<String>,
    /// The revision after the declaration.
    pub revision: u64,
}

/// One applied expansion proposal.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AppliedProposalView {
    /// The proposal summary.
    pub summary: String,
    /// The facts appended, as one-line summaries.
    pub facts: Vec<String>,
    /// Deterministic notes about effects.
    pub notes: Vec<String>,
    /// Whether acting on it requires a human decision.
    pub requires_human_decision: bool,
}

/// One rejected expansion proposal.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RejectedProposalView {
    /// The proposal summary.
    pub summary: String,
    /// The policy rejection reason.
    pub reason: String,
}

/// What `expand` did.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ExpandView {
    /// The applied proposals.
    pub applied: Vec<AppliedProposalView>,
    /// The rejected proposals, with reasons.
    pub rejected: Vec<RejectedProposalView>,
    /// The revision after expansion.
    pub revision: u64,
    /// The next action after expansion.
    pub next: NextActionView,
}

/// One refused scheduling, with its reason.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RefusalEntryView {
    /// The work item refused.
    pub work: String,
    /// Why it was not started.
    pub reason: String,
}

/// One failed attempt of a tick.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FailedEntryView {
    /// The attempt.
    pub attempt: String,
    /// Why it failed.
    pub reason: String,
}

/// What one orchestrator tick did.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TickView {
    /// The tick's 1-based index.
    pub index: usize,
    /// Attempts leased and run.
    pub started: Vec<String>,
    /// Work refused at scheduling, with reasons.
    pub refusals: Vec<RefusalEntryView>,
    /// Attempts whose receipts were accepted.
    pub accepted: Vec<String>,
    /// Attempts concluded failed.
    pub failed: Vec<FailedEntryView>,
    /// Attempts concluded canceled.
    pub canceled: Vec<String>,
}

/// What `run` did across its ticks.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RunView {
    /// The autonomy the run used.
    pub autonomy: String,
    /// Per-tick outcomes.
    pub ticks: Vec<TickView>,
    /// Decisions awaiting a human after the run.
    pub awaiting_human: Vec<DecisionEntryView>,
    /// The recommendation after the run.
    pub recommendation: RecommendationView,
    /// The revision after the run.
    pub revision: u64,
    /// Markdown views auto-published for artifact versions this run
    /// accepted (canonical blob + digest convention, idempotent).
    pub published_views: Vec<PublishArtifactViewView>,
    /// Why an accepted version's view was not auto-published (unrenderable
    /// schema, publication failure, or the per-run bound).
    pub view_notes: Vec<String>,
}

/// What resolving a decision did.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ResolveDecisionView {
    /// The resolved decision.
    pub decision: DecisionEntryView,
    /// Work items completed by the resolution.
    pub completed_work: Vec<String>,
    /// The recommendation after the resolution.
    pub recommendation: RecommendationView,
    /// The revision after the resolution.
    pub revision: u64,
}

/// What canceling an attempt did.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CancelView {
    /// The canceled attempt.
    pub attempt: String,
    /// The work it ran.
    pub work: String,
    /// The recorded conclusion (`canceled`).
    pub conclusion: String,
    /// The revision after the cancellation.
    pub revision: u64,
}

/// One revised document from a controlled PRD import.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RevisedDocView {
    /// The artifact being revised.
    pub artifact: String,
    /// The schema of the revised document.
    pub schema: String,
    /// The document slug.
    pub slug: String,
}

/// What importing a PRD revision proposed and applied.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ImportPrdView {
    /// The revised documents.
    pub documents: Vec<RevisedDocView>,
    /// The `revise-artifact` work item that will publish them.
    pub work: String,
    /// The revision after the import.
    pub revision: u64,
}

/// What publishing an artifact view did.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PublishArtifactViewView {
    /// The artifact.
    pub artifact: String,
    /// The version.
    pub version: u32,
    /// The blob path, relative to the workspace root.
    pub path: String,
    /// Lowercase hex SHA-256 of the rendered Markdown bytes (empty when the
    /// blob already existed).
    pub sha256: String,
    /// The evidence id recording the publication.
    pub evidence: String,
    /// The revision after the publication fact.
    pub revision: u64,
    /// `published` or `already-published`.
    pub status: String,
}

/// One doctor finding.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DoctorFindingView {
    /// `ok`, `warning`, or `error`.
    pub severity: String,
    /// The stable finding code.
    pub code: String,
    /// The human-readable detail.
    pub detail: String,
}

/// The doctor check result.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DoctorView {
    /// Whether any finding has `error` severity.
    pub healthy: bool,
    /// The findings.
    pub findings: Vec<DoctorFindingView>,
}

/// The projection-rebuild result.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RebuildView {
    /// Facts replayed into the fresh projection.
    pub facts_indexed: u64,
    /// Where the previous database was quarantined, when it existed.
    pub quarantine: Option<String>,
}

/// The migrate result: a dry-run plan or an applied migration over a
/// legacy project copy. The legacy source is only ever read.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MigrateView {
    /// Whether a legacy Relay project was found to migrate.
    pub available: bool,
    /// Whether this was a dry run (nothing staged or switched).
    pub dry_run: bool,
    /// One-line outcome.
    pub detail: String,
    /// Legacy events read.
    pub legacy_events: u64,
    /// Facts the extraction emitted.
    pub facts_emitted: u64,
    /// Legacy kinds with no durable meaning, as `kind × count`.
    pub unmapped: Vec<String>,
    /// How many relationships were inferred (coarse) rather than read.
    pub inferences: usize,
    /// Items a human should review, with their notes.
    pub needs_review: Vec<String>,
    /// The new store root, when the migration applied.
    pub store_root: Option<String>,
    /// Where the previous fact store was retained, when one existed.
    pub previous_store_backup: Option<String>,
}

// ---------------------------------------------------------------------------
// Conversions
// ---------------------------------------------------------------------------

/// Name of an evidence kind for views.
pub fn evidence_kind_name(kind: EvidenceKind) -> &'static str {
    match kind {
        EvidenceKind::TestRun => "test-run",
        EvidenceKind::CommandOutput => "command-output",
        EvidenceKind::StaticAnalysis => "static-analysis",
        EvidenceKind::Inspection => "inspection",
        EvidenceKind::Review => "review",
        EvidenceKind::HumanStatement => "human-statement",
    }
}

/// Name of an evidence outcome for views.
pub fn evidence_outcome_name(outcome: EvidenceOutcome) -> &'static str {
    match outcome {
        EvidenceOutcome::Pass => "pass",
        EvidenceOutcome::Fail => "fail",
        EvidenceOutcome::Info => "info",
    }
}

/// Name of an autonomy level for views.
pub fn autonomy_name(autonomy: Autonomy) -> String {
    match autonomy {
        Autonomy::Manual => "manual".to_string(),
        Autonomy::Safe => "safe".to_string(),
        Autonomy::UntilDecision => "until-decision".to_string(),
        Autonomy::Bounded(limit) => format!("bounded:{limit}"),
    }
}

/// The phase a work kind belongs to, per the built-in catalog (grouped-view
/// metadata only).
pub fn phase_of(catalog: &WorkflowCatalog, kind: &str) -> Phase {
    catalog
        .get(kind)
        .map(|definition| definition.phase)
        .unwrap_or(Phase::Discovery)
}

/// The delivery-phase label for a work kind; `custom` for kinds outside the
/// catalog.
pub fn phase_label(catalog: &WorkflowCatalog, kind: &str) -> String {
    if catalog.get(kind).is_some() {
        phase_of(catalog, kind).to_string()
    } else {
        "custom".to_string()
    }
}

/// The ordinal of a phase in [`PHASE_ORDER`] (unknown kinds sort last).
pub fn phase_ordinal(catalog: &WorkflowCatalog, kind: &str) -> usize {
    if catalog.get(kind).is_none() {
        return PHASE_ORDER.len();
    }
    PHASE_ORDER
        .iter()
        .position(|phase| *phase == phase_of(catalog, kind))
        .unwrap_or(PHASE_ORDER.len())
}

fn status_name(status: &WorkStatus) -> String {
    status.name().to_string()
}

/// Build the status counts from an evaluation.
pub fn status_counts(evaluation: &Evaluation) -> StatusCountsView {
    StatusCountsView {
        ready: evaluation.ready.len(),
        blocked: evaluation.blocked.len(),
        awaiting_human: evaluation.awaiting_human.len(),
        active_attempts: evaluation.active_attempts.len(),
        invalidated: evaluation.invalidated.len(),
        completed: evaluation.completed.len(),
        canceled: evaluation.canceled.len(),
        superseded: evaluation.superseded.len(),
    }
}

/// Build the phase-grouped progress (a grouped view over work kinds, in
/// catalog phase order).
pub fn phase_progress(graph: &ProjectGraph, catalog: &WorkflowCatalog) -> Vec<PhaseProgressView> {
    let mut phases = Vec::new();
    for phase in PHASE_ORDER {
        let kinds: Vec<&str> = catalog
            .iter()
            .filter(|definition| definition.phase == phase)
            .map(|definition| definition.label)
            .collect();
        let mut total = 0usize;
        let mut completed = 0usize;
        for (_, node) in graph.works() {
            if !kinds.contains(&node.work.kind.as_str()) {
                continue;
            }
            total += 1;
            if matches!(work_status_of(graph, node), WorkStatus::Completed) {
                completed += 1;
            }
        }
        phases.push(PhaseProgressView {
            phase: phase.to_string(),
            total,
            completed,
            open: total - completed,
        });
    }
    phases
}

fn work_status_of(graph: &ProjectGraph, node: &WorkNode) -> WorkStatus {
    relay_engine::status::work_status(graph, node)
}

/// One-line summary of a durable fact, for applied/rejected listings.
pub fn fact_summary(fact: &DomainFact) -> String {
    match fact {
        DomainFact::ProjectCreated { project } => format!("project created: {project}"),
        DomainFact::GoalDeclared { goal } => format!("goal declared: {}", goal.id),
        DomainFact::WorkDeclared { work } => format!("work declared: {}", work.id),
        DomainFact::WorkSuperseded {
            work,
            superseded_by,
            ..
        } => format!("work superseded: {work} -> {superseded_by}"),
        DomainFact::WorkCanceled { work, .. } => format!("work canceled: {work}"),
        DomainFact::WorkScopeExpanded { work, .. } => format!("work scope expanded: {work}"),
        DomainFact::DependencyAsserted { from, to, kind } => {
            format!("dependency asserted: {from} {kind} {to}")
        }
        DomainFact::DependencyRetracted { from, to, kind, .. } => {
            format!("dependency retracted: {from} {kind} {to}")
        }
        DomainFact::DecisionOpened { decision } => format!("decision opened: {}", decision.id),
        DomainFact::DecisionResolved { decision, .. } => format!("decision resolved: {decision}"),
        DomainFact::DecisionSuperseded {
            decision,
            superseded_by,
        } => format!("decision superseded: {decision} -> {superseded_by}"),
        DomainFact::ArtifactVersionPublished {
            artifact, version, ..
        } => format!("published: {artifact}@{version}"),
        DomainFact::EvidenceRecorded { evidence } => format!("evidence recorded: {}", evidence.id),
        DomainFact::AttemptStarted { attempt, work, .. } => {
            format!("attempt started: {attempt} ({work})")
        }
        DomainFact::AttemptConcluded {
            attempt,
            conclusion,
        } => format!(
            "attempt concluded: {attempt} ({})",
            conclusion_name(conclusion)
        ),
        DomainFact::InvalidationDeclared { target, .. } => {
            format!("invalidation declared: {target:?}")
        }
        DomainFact::ConversationStarted { conversation, .. } => {
            format!("conversation started: {conversation}")
        }
        DomainFact::UserMessageRecorded {
            conversation,
            turn,
            message,
            ..
        } => format!("user message recorded: {message} ({conversation}/{turn})"),
        DomainFact::AssistantMessageRecorded {
            conversation,
            turn,
            message,
            ..
        } => format!("assistant message recorded: {message} ({conversation}/{turn})"),
        DomainFact::ConversationTurnFailed {
            conversation, turn, ..
        } => format!("conversation turn failed: {turn} ({conversation})"),
        DomainFact::ConversationTurnCancelled {
            conversation, turn, ..
        } => format!("conversation turn cancelled: {turn} ({conversation})"),
    }
}

fn conclusion_name(conclusion: &AttemptConclusion) -> &'static str {
    match conclusion {
        AttemptConclusion::Accepted { .. } => "accepted",
        AttemptConclusion::Failed { .. } => "failed",
        AttemptConclusion::Canceled { .. } => "canceled",
        AttemptConclusion::Abandoned { .. } => "abandoned",
    }
}

/// One-line summary of a graph-change proposal.
pub fn describe_proposal(proposal: &GraphChangeProposal) -> String {
    match proposal {
        GraphChangeProposal::DeclareWork { work } => {
            format!("declare work {} ({})", work.id, work.kind)
        }
        GraphChangeProposal::DeclareDependency { edge } => format!("assert dependency {edge}"),
        GraphChangeProposal::SupersedeWork {
            supersede,
            replacement,
            ..
        } => format!("supersede {supersede} with {}", replacement.id),
        GraphChangeProposal::CancelWork { work, .. } => format!("cancel {work}"),
        GraphChangeProposal::RetractDependency { edge, .. } => format!("retract dependency {edge}"),
        GraphChangeProposal::OpenDecision { decision } => {
            let options = decision
                .options
                .iter()
                .map(|option| option.label.clone())
                .collect::<Vec<_>>()
                .join(", ");
            if options.is_empty() {
                format!("open decision {} (free-form)", decision.id)
            } else {
                format!("open decision {} (options: {options})", decision.id)
            }
        }
        GraphChangeProposal::ExpandScope { work, .. } => format!("expand scope of {work}"),
        GraphChangeProposal::DeclareInvalidation { target, .. } => {
            format!("declare invalidation of {target:?}")
        }
    }
}

fn work_entry(
    graph: &ProjectGraph,
    catalog: &WorkflowCatalog,
    id: &WorkItemId,
    node: &WorkNode,
) -> WorkEntryView {
    let status = work_status_of(graph, node);
    WorkEntryView {
        id: id.to_string(),
        kind: node.work.kind.to_string(),
        phase: phase_label(catalog, node.work.kind.as_str()),
        executor: node.work.executor.to_string(),
        objective: node.work.objective.clone(),
        required: node.work.required,
        status: status_name(&status),
        explanation: explain_status(graph, node, &status),
    }
}

/// Build the ready list, ordered by (phase, id) — the documented
/// deterministic tie-break.
pub fn ready_view(
    graph: &ProjectGraph,
    evaluation: &Evaluation,
    catalog: &WorkflowCatalog,
) -> WorkListView {
    let mut entries: Vec<WorkEntryView> = evaluation
        .ready
        .iter()
        .filter_map(|entry| {
            graph
                .work(&entry.work)
                .map(|node| work_entry(graph, catalog, &entry.work, node))
        })
        .collect();
    entries.sort_by_key(sort_key);
    WorkListView {
        works: entries,
        revision: revision_number(evaluation.revision),
    }
}

/// Build the blocked list, ordered by (phase, id).
pub fn blocked_view(
    graph: &ProjectGraph,
    evaluation: &Evaluation,
    catalog: &WorkflowCatalog,
) -> WorkListView {
    let mut entries: Vec<WorkEntryView> = evaluation
        .blocked
        .iter()
        .filter_map(|entry| {
            graph
                .work(&entry.work)
                .map(|node| work_entry(graph, catalog, &entry.work, node))
        })
        .collect();
    entries.sort_by_key(sort_key);
    WorkListView {
        works: entries,
        revision: revision_number(evaluation.revision),
    }
}

/// Build the full work list, ordered by (phase, id).
pub fn work_list_view(graph: &ProjectGraph, catalog: &WorkflowCatalog) -> WorkListView {
    let mut entries: Vec<WorkEntryView> = graph
        .works()
        .map(|(id, node)| work_entry(graph, catalog, id, node))
        .collect();
    entries.sort_by_key(sort_key);
    WorkListView {
        works: entries,
        revision: revision_number(graph.revision()),
    }
}

fn sort_key(entry: &WorkEntryView) -> (usize, String) {
    (
        PHASE_ORDER
            .iter()
            .position(|phase| phase.to_string() == entry.phase)
            .unwrap_or(PHASE_ORDER.len()),
        entry.id.clone(),
    )
}

fn revision_number(revision: ProjectRevision) -> u64 {
    revision.get()
}

/// Build the full detail for one work item.
pub fn work_detail_view(
    graph: &ProjectGraph,
    catalog: &WorkflowCatalog,
    id: &WorkItemId,
) -> Option<WorkDetailView> {
    let node = graph.work(id)?;
    let entry = work_entry(graph, catalog, id, node);
    let dependencies: Vec<DependencyEntryView> = graph
        .dependencies_of(id)
        .map(|edge| DependencyEntryView {
            to: edge.to.to_string(),
            kind: edge.kind.to_string(),
            status: work_status_by_id(graph, &edge.to)
                .map(|status| status.name().to_string())
                .unwrap_or_else(|| "unknown".to_string()),
        })
        .collect();
    let dependents: Vec<String> = graph
        .dependents_of(id)
        .map(|edge| edge.from.to_string())
        .collect();
    let inputs: Vec<String> = node
        .work
        .inputs
        .iter()
        .map(|input| input.to_string())
        .collect();
    let expected_outputs: Vec<String> = node
        .work
        .expected_outputs
        .iter()
        .map(|output| output.label.clone())
        .collect();
    let completion = match &node.work.completion {
        CompletionRule::Receipt => "receipt".to_string(),
        CompletionRule::Decision { decision } => format!("decision:{decision}"),
        CompletionRule::CriterionSatisfied { criterion } => format!("criterion:{criterion}"),
    };
    let attempts: Vec<AttemptEntryView> = graph
        .attempts_of(id)
        .map(|attempt| {
            let (conclusion, detail) = match &attempt.conclusion {
                None => ("active".to_string(), None),
                Some(AttemptConclusion::Accepted { receipt }) => (
                    "accepted".to_string(),
                    match &receipt.outcome {
                        AttemptOutcome::Completed => Some("completed".to_string()),
                        AttemptOutcome::Failed { reason } => Some(reason.clone()),
                    },
                ),
                Some(AttemptConclusion::Failed { reason }) => {
                    ("failed".to_string(), Some(reason.clone()))
                }
                Some(AttemptConclusion::Canceled { reason }) => {
                    ("canceled".to_string(), Some(reason.clone()))
                }
                Some(AttemptConclusion::Abandoned { reason }) => {
                    ("abandoned".to_string(), Some(reason.clone()))
                }
            };
            AttemptEntryView {
                id: attempt.id.to_string(),
                started_at_ms: attempt.started_at_ms,
                conclusion,
                detail,
            }
        })
        .collect();
    let evidence = evidence_views_for(graph, &EvidenceTarget::WorkItem(id.clone()));
    Some(WorkDetailView {
        entry,
        dependencies,
        dependents,
        inputs,
        expected_outputs,
        read_scopes: node
            .work
            .read_scopes
            .iter()
            .map(|scope| scope.to_string())
            .collect(),
        write_scopes: node
            .effective_write_scopes()
            .iter()
            .map(|scope| scope.to_string())
            .collect(),
        completion,
        attempts,
        evidence,
    })
}

/// Build one decision's view.
pub fn decision_view(graph: &ProjectGraph, id: &DecisionId) -> Option<DecisionEntryView> {
    let node = graph.decision(id)?;
    let status = if node.resolution.is_some() {
        "resolved"
    } else if node.superseded_by.is_some() {
        "superseded"
    } else {
        "open"
    };
    // The work item this decision completes, when there is one.
    let work = graph
        .works()
        .find(|(_, work)| {
            matches!(
                &work.work.completion,
                CompletionRule::Decision { decision } if decision == id
            )
        })
        .map(|(work_id, _)| work_id.to_string());
    Some(DecisionEntryView {
        id: id.to_string(),
        question: node.decision.question.clone(),
        options: node
            .decision
            .options
            .iter()
            .map(|option| DecisionOptionView {
                label: option.label.clone(),
                description: option.description.clone(),
            })
            .collect(),
        recommendation: node.decision.recommendation.clone(),
        status: status.to_string(),
        choice: node.resolution.as_ref().map(|r| r.choice.clone()),
        rationale: node.resolution.as_ref().map(|r| r.rationale.clone()),
        superseded_by: node.superseded_by.as_ref().map(|id| id.to_string()),
        work,
    })
}

/// Build the decision list, in id order.
pub fn decision_list_view(graph: &ProjectGraph) -> DecisionListView {
    let decisions = graph
        .decisions()
        .filter_map(|(id, _)| decision_view(graph, id))
        .collect();
    DecisionListView {
        decisions,
        revision: revision_number(graph.revision()),
    }
}

fn evidence_views_for(graph: &ProjectGraph, target: &EvidenceTarget) -> Vec<EvidenceEntryView> {
    graph
        .evidence_for(target)
        .into_iter()
        .map(evidence_entry_view)
        .collect()
}

/// Build one evidence record's view.
pub fn evidence_entry_view(evidence: &Evidence) -> EvidenceEntryView {
    EvidenceEntryView {
        id: evidence.id.to_string(),
        kind: evidence_kind_name(evidence.kind).to_string(),
        target: evidence_target_label(&evidence.target),
        outcome: evidence_outcome_name(evidence.outcome).to_string(),
        summary: evidence.summary.clone(),
        recorded_at_ms: evidence.recorded_at_ms,
    }
}

fn evidence_target_label(target: &EvidenceTarget) -> String {
    match target {
        EvidenceTarget::WorkItem(work) => format!("work:{work}"),
        EvidenceTarget::ArtifactVersion(reference) => format!("artifact:{reference}"),
        EvidenceTarget::Decision(decision) => format!("decision:{decision}"),
    }
}

/// Build the evidence view for a work item or an artifact-version
/// reference, keyed by whatever the user named.
pub fn evidence_view(graph: &ProjectGraph, target: &EvidenceTarget) -> EvidenceListView {
    EvidenceListView {
        target: evidence_target_label(target),
        evidence: evidence_views_for(graph, target),
        revision: revision_number(graph.revision()),
    }
}

/// Build the explanation view for one work item.
pub fn explain_view(graph: &ProjectGraph, id: &WorkItemId) -> Option<ExplainView> {
    let node = graph.work(id)?;
    let status = work_status_of(graph, node);
    let detail = explain_status(graph, node, &status);
    Some(ExplainView {
        work: id.to_string(),
        status: status.name().to_string(),
        detail: format!("{} is {}: {}", id, status.name(), detail),
    })
}

/// Build the recommendation view, including the derived next action.
///
/// The next-action rule, documented and deterministic: (1) awaiting-human
/// decisions first, in decision-id order; (2) then ready work, ordered by
/// catalog phase order with work-id tie-break; (3) then workflow expansion
/// when [`relay_workflows::propose_next`] has a next round; (4) then active
/// attempts; (5) otherwise nothing. Everything comes from engine or
/// workflows output — no interface-side policy.
pub fn recommendation_view(
    graph: &ProjectGraph,
    evaluation: &Evaluation,
    policy: &SchedulingPolicy,
    catalog: &WorkflowCatalog,
) -> RecommendationView {
    let recommendation = relay_engine::recommend(graph, policy);
    let next = next_action(graph, evaluation, catalog);
    RecommendationView {
        autonomy: autonomy_name(policy.autonomy),
        parallel: recommendation
            .parallel
            .iter()
            .map(|entry| RecommendedEntryView {
                work: entry.work.to_string(),
                executor: entry.executor.to_string(),
                read_scopes: entry
                    .read_scopes
                    .iter()
                    .map(|scope| scope.to_string())
                    .collect(),
                write_scopes: entry
                    .write_scopes
                    .iter()
                    .map(|scope| scope.to_string())
                    .collect(),
                explanation: entry.explanation.clone(),
            })
            .collect(),
        serialized: recommendation
            .serialized
            .iter()
            .map(|entry| RecommendedEntryView {
                work: entry.work.to_string(),
                executor: entry.executor.to_string(),
                read_scopes: entry
                    .read_scopes
                    .iter()
                    .map(|scope| scope.to_string())
                    .collect(),
                write_scopes: entry
                    .write_scopes
                    .iter()
                    .map(|scope| scope.to_string())
                    .collect(),
                explanation: entry.explanation.clone(),
            })
            .collect(),
        deferred: recommendation
            .deferred
            .iter()
            .map(|entry| DeferredEntryView {
                work: entry.work.to_string(),
                reason: entry.reason.clone(),
            })
            .collect(),
        rationale: recommendation.rationale.clone(),
        next,
    }
}

/// Derive the next action (see [`recommendation_view`] for the rule).
pub fn next_action(
    graph: &ProjectGraph,
    evaluation: &Evaluation,
    catalog: &WorkflowCatalog,
) -> NextActionView {
    if let Some(entry) = evaluation.awaiting_human.first() {
        return NextActionView {
            kind: "awaiting-human".to_string(),
            target: Some(entry.decision.to_string()),
            detail: format!(
                "resolve decision {} (\"{}\") with `relay decisions resolve {} --choice <label>`",
                entry.decision, entry.question, entry.decision
            ),
        };
    }
    if !evaluation.ready.is_empty() {
        // Ready work in catalog phase order, work-id tie-break.
        let mut ready: Vec<&WorkItemId> =
            evaluation.ready.iter().map(|entry| &entry.work).collect();
        ready.sort_by_key(|id| {
            graph
                .work(id)
                .map(|node| {
                    (
                        phase_ordinal(catalog, node.work.kind.as_str()),
                        id.to_string(),
                    )
                })
                .unwrap_or((PHASE_ORDER.len(), id.to_string()))
        });
        if let Some(first) = ready.first() {
            return NextActionView {
                kind: "ready-work".to_string(),
                target: Some(first.to_string()),
                detail: format!(
                    "run ready work {} with `relay run` (see `relay explain {}`)",
                    first, first
                ),
            };
        }
    }
    let proposals = relay_workflows::propose_next(graph, catalog);
    if !proposals.is_empty() {
        return NextActionView {
            kind: "expand".to_string(),
            target: None,
            detail: format!(
                "the workflow proposes {} next change(s); apply them with `relay expand`",
                proposals.len()
            ),
        };
    }
    if !evaluation.active_attempts.is_empty() {
        return NextActionView {
            kind: "active-attempts".to_string(),
            target: evaluation
                .active_attempts
                .first()
                .map(|attempt| attempt.attempt.to_string()),
            detail: format!(
                "{} attempt(s) are running; re-check with `relay project summary` later",
                evaluation.active_attempts.len()
            ),
        };
    }
    NextActionView {
        kind: "none".to_string(),
        target: None,
        detail: "nothing is ready, blocked on a human, or proposed next".to_string(),
    }
}

/// Build the project summary.
pub fn summary_view(
    graph: &ProjectGraph,
    evaluation: &Evaluation,
    catalog: &WorkflowCatalog,
) -> SummaryView {
    let policy = SchedulingPolicy::default();
    let (completion, open_required, open_decisions) = match &evaluation.completion {
        relay_engine::ProjectCompletion::Complete => {
            ("complete".to_string(), Vec::new(), Vec::new())
        }
        relay_engine::ProjectCompletion::InProgress {
            open_required,
            open_decisions,
            ..
        } => (
            "in-progress".to_string(),
            open_required.iter().map(|id| id.to_string()).collect(),
            open_decisions.iter().map(|id| id.to_string()).collect(),
        ),
    };
    SummaryView {
        project_id: graph.project_id().map(|id| id.to_string()),
        goal: graph.goal().map(|goal| GoalView {
            id: goal.id.to_string(),
            outcome: goal.outcome.clone(),
            success_definition: goal.success_definition.clone(),
        }),
        revision: revision_number(evaluation.revision),
        completion,
        open_required,
        open_decisions,
        counts: status_counts(evaluation),
        phases: phase_progress(graph, catalog),
        recommendation: recommendation_view(graph, evaluation, &policy, catalog),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)] // test-only, house style
mod tests {
    use super::*;
    use relay_domain::{DependencyEdge, DependencyKind};

    #[test]
    fn autonomy_names_round_trip_the_documented_grammar() {
        assert_eq!(autonomy_name(Autonomy::Manual), "manual");
        assert_eq!(autonomy_name(Autonomy::Safe), "safe");
        assert_eq!(autonomy_name(Autonomy::UntilDecision), "until-decision");
        assert_eq!(autonomy_name(Autonomy::Bounded(3)), "bounded:3");
    }

    #[test]
    fn evidence_names_are_stable() {
        assert_eq!(evidence_kind_name(EvidenceKind::TestRun), "test-run");
        assert_eq!(evidence_outcome_name(EvidenceOutcome::Fail), "fail");
    }

    #[test]
    fn dependency_edges_display() {
        let a = WorkItemId::parse("work_a").ok();
        let b = WorkItemId::parse("work_b").ok();
        let (a, b) = match (a, b) {
            (Some(a), Some(b)) => (a, b),
            _ => panic!("literal ids parse"),
        };
        let edge = DependencyEdge::new(a, b, DependencyKind::Requires);
        let summary = describe_proposal(&GraphChangeProposal::RetractDependency {
            edge: edge.clone(),
            reason: "no longer needed".to_string(),
        });
        assert!(summary.contains("work_a"), "{summary}");
        assert!(summary.contains("requires"), "{summary}");
    }
}
