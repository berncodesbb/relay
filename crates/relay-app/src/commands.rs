//! The command handlers: small, focused units that validate a command,
//! persist accepted facts through the checked store, and return a typed
//! view. No handler owns scheduling policy or readiness computation — those
//! come from `relay-engine` and `relay-workflows`.

use std::path::Path;

use relay_domain::{
    ArtifactId, ArtifactVersion, ArtifactVersionRef, AttemptConclusion, DecisionId,
    DecisionResolution, DomainFact, Evidence, EvidenceKind, EvidenceOutcome, EvidenceTarget, Goal,
    GoalId, ProjectGraph, ProjectId, Provenance, ProvenanceSource, WorkItemId,
};
use relay_store::fact::store::FactStoreOptions;
use relay_workflows::{WorkflowCatalog, import_prd, prd_revision, propose_next, revision_proposal};

use crate::error::AppError;
use crate::ids;
use crate::markdown::render_artifact_view;
use crate::session::ProjectSession;
use crate::session::SessionOptions;
use crate::session::workspace_name;
use crate::views::{
    self, ArtifactDetailView, CancelView, DecisionListView, DeclareGoalView, DoctorFindingView,
    DoctorView, EvidenceListView, ExpandView, ExplainView, ImportPrdView, InitView, MigrateView,
    PublishArtifactViewView, RebuildView, ResolveDecisionView, SummaryView, WorkDetailView,
    WorkListView,
};

/// The built-in workflow catalog (one per call; it is cheap, pure data).
fn catalog() -> WorkflowCatalog {
    WorkflowCatalog::builtin()
}

/// Initialize a project: `ProjectCreated` + `GoalDeclared`, then the first
/// workflow expansion round applied through `validate_graph_change`.
///
/// A goal is the root of the work graph, so `init` requires goal text; the
/// interface surfaces that as a usage error before reaching here when it
/// can.
pub fn init(
    workspace_root: &Path,
    goal_text: &str,
    options: SessionOptions,
) -> Result<InitView, AppError> {
    let goal_text = goal_text.trim();
    if goal_text.is_empty() {
        return Err(AppError::Rejected(
            "init needs a goal: `relay init --goal <what outcome do you want?>`".to_string(),
        ));
    }
    if let Ok(existing) = ProjectSession::open(workspace_root, options.clone())
        && existing.is_initialized()
    {
        return Err(AppError::AlreadyInitialized {
            project: existing
                .graph()
                .project_id()
                .map(|id| id.to_string())
                .unwrap_or_default(),
        });
    }
    let (session, _) = ProjectSession::open_or_create(workspace_root, options)?;
    let name = workspace_name(workspace_root);
    let project: ProjectId = ids::project_id_for(&name);
    let goal_id: GoalId = ids::goal_id_for(&name);
    let now = session.now_ms();
    let goal = Goal::new(
        goal_id,
        goal_text,
        Provenance {
            source: ProvenanceSource::User,
            statement: Some(goal_text.to_string()),
            at_ms: now,
        },
        now,
    );
    let mut facts = vec![
        DomainFact::ProjectCreated { project },
        DomainFact::GoalDeclared { goal },
    ];
    apply_expansion_round(&mut facts)?;
    let outcome = session.append_validated(facts.clone())?;
    let graph = session.graph();
    let evaluation = session.evaluation();
    let goal_view = graph
        .goal()
        .map(|goal| views::GoalView {
            id: goal.id.to_string(),
            outcome: goal.outcome.clone(),
            success_definition: goal.success_definition.clone(),
        })
        .ok_or(AppError::Rejected(
            "the goal fact did not fold; the store refused the batch".to_string(),
        ))?;
    Ok(InitView {
        project: session
            .graph()
            .project_id()
            .map(|id| id.to_string())
            .unwrap_or_default(),
        goal: goal_view,
        revision: outcome.revision.get(),
        applied: facts.iter().map(views::fact_summary).collect(),
        recommendation: views::recommendation_view(
            &graph,
            &evaluation,
            &relay_engine::SchedulingPolicy::default(),
            &catalog(),
        ),
    })
}

/// Declare a goal on an existing, goal-less project — the conversational
/// `declare-goal` path. Appends `GoalDeclared` plus the same first workflow
/// expansion round `init` applies, seeded from the live graph. Rejected when
/// the project is not initialized or a goal already exists (a project
/// carries one goal). Creates neither the project nor the workspace.
pub fn declare_goal(
    session: &ProjectSession,
    outcome: &str,
    success_definition: Option<&str>,
) -> Result<DeclareGoalView, AppError> {
    session.require_initialized()?;
    let outcome = outcome.trim();
    if outcome.is_empty() {
        return Err(AppError::Rejected(
            "declaring a goal needs a non-empty outcome".to_string(),
        ));
    }
    if let Some(existing) = session.graph().goal() {
        return Err(AppError::Rejected(format!(
            "the project already carries a goal ({}); a goal is declared once",
            existing.id
        )));
    }
    let name = workspace_name(session.workspace_root());
    let now = session.now_ms();
    let mut goal = Goal::new(
        ids::goal_id_for(&name),
        outcome,
        Provenance {
            source: ProvenanceSource::User,
            statement: Some(outcome.to_string()),
            at_ms: now,
        },
        now,
    );
    if let Some(definition) = success_definition
        .map(str::trim)
        .filter(|definition| !definition.is_empty())
    {
        goal.success_definition = definition.to_string();
    }
    let declared = DomainFact::GoalDeclared { goal };
    // Seed the expansion candidate from the live graph plus the new goal:
    // unlike `init`, the project facts already exist on disk.
    let mut candidate = session.graph().clone();
    candidate
        .apply(&declared)
        .map_err(|error| AppError::Rejected(error.to_string()))?;
    let mut facts = vec![declared];
    for proposal in propose_next(&candidate, &catalog()) {
        if let Ok(applied) = relay_engine::validate_graph_change(&candidate, &proposal) {
            for fact in applied.facts {
                candidate
                    .apply(&fact)
                    .map_err(|error| AppError::Rejected(error.to_string()))?;
                facts.push(fact);
            }
        }
    }
    let outcome = session.append_validated(facts.clone())?;
    let graph = session.graph();
    let goal_view = graph
        .goal()
        .map(|goal| views::GoalView {
            id: goal.id.to_string(),
            outcome: goal.outcome.clone(),
            success_definition: goal.success_definition.clone(),
        })
        .ok_or_else(|| {
            AppError::Rejected(
                "the goal fact did not fold; the store refused the batch".to_string(),
            )
        })?;
    let (work, decisions) = declared_targets(&facts);
    Ok(DeclareGoalView {
        goal: goal_view,
        work,
        decisions,
        revision: outcome.revision.get(),
    })
}

/// The work and decision ids a batch of facts declared (for view related-id
/// listings).
fn declared_targets(facts: &[DomainFact]) -> (Vec<String>, Vec<String>) {
    let mut work = Vec::new();
    let mut decisions = Vec::new();
    for fact in facts {
        match fact {
            DomainFact::WorkDeclared { work: item } => work.push(item.id.to_string()),
            DomainFact::DecisionOpened { decision } => decisions.push(decision.id.to_string()),
            _ => {}
        }
    }
    (work, decisions)
}

/// Append the next workflow expansion round: every proposal validated by
/// engine policy, accepted ones applied, rejected ones reported with
/// reasons.
pub fn expand(session: &ProjectSession) -> Result<ExpandView, AppError> {
    session.require_initialized()?;
    let graph = session.graph();
    let workflow = catalog();
    let mut facts: Vec<DomainFact> = Vec::new();
    let mut applied_views = Vec::new();
    let mut rejected_views = Vec::new();
    let mut candidate = graph.clone();
    for proposal in propose_next(&candidate, &workflow) {
        let summary = views::describe_proposal(&proposal);
        match relay_engine::validate_graph_change(&candidate, &proposal) {
            Ok(applied) => {
                for fact in &applied.facts {
                    candidate
                        .apply(fact)
                        .map_err(|error| AppError::Rejected(error.to_string()))?;
                    facts.push(fact.clone());
                }
                applied_views.push(views::AppliedProposalView {
                    summary,
                    facts: applied.facts.iter().map(views::fact_summary).collect(),
                    notes: applied.notes.clone(),
                    requires_human_decision: applied.requires_human_decision,
                });
            }
            Err(rejection) => rejected_views.push(views::RejectedProposalView {
                summary,
                reason: rejection.to_string(),
            }),
        }
    }
    let outcome = session.append_validated(facts)?;
    let graph = session.graph();
    let evaluation = session.evaluation();
    Ok(ExpandView {
        applied: applied_views,
        rejected: rejected_views,
        revision: outcome.revision.get(),
        next: views::next_action(&graph, &evaluation, &workflow),
    })
}

/// Resolve one human decision with a validated choice. The store's checked
/// append re-validates; this pre-check gives the friendly error.
pub fn resolve_decision(
    session: &ProjectSession,
    decision: &str,
    choice: &str,
    rationale: Option<&str>,
) -> Result<ResolveDecisionView, AppError> {
    session.require_initialized()?;
    let id = DecisionId::parse(decision).map_err(|_| AppError::UnknownTarget {
        kind: "decision",
        id: decision.to_string(),
    })?;
    let graph = session.graph();
    let node = graph.decision(&id).ok_or_else(|| AppError::UnknownTarget {
        kind: "decision",
        id: decision.to_string(),
    })?;
    if node.superseded_by.is_some() {
        return Err(AppError::Rejected(format!(
            "decision {id} was superseded by {} and cannot be resolved",
            node.superseded_by
                .as_ref()
                .map(|id| id.to_string())
                .unwrap_or_default()
        )));
    }
    if node.resolution.is_some() {
        return Err(AppError::Rejected(format!(
            "decision {id} is already resolved (\"{}\")",
            node.resolution
                .as_ref()
                .map(|resolution| resolution.choice.clone())
                .unwrap_or_default()
        )));
    }
    if !node.decision.accepts_choice(choice) {
        let options = node
            .decision
            .options
            .iter()
            .map(|option| option.label.clone())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(AppError::InvalidChoice {
            decision: decision.to_string(),
            choice: choice.to_string(),
            options,
        });
    }
    let fact = DomainFact::DecisionResolved {
        resolution: DecisionResolution {
            choice: choice.to_string(),
            rationale: rationale.unwrap_or_default().to_string(),
            resolved_at_ms: session.now_ms(),
        },
        decision: id.clone(),
    };
    let outcome = session.append_validated(vec![fact])?;
    let graph = session.graph();
    let evaluation = session.evaluation();
    let completed_work: Vec<String> = graph
        .works()
        .filter(|(_, work)| {
            matches!(
                &work.work.completion,
                relay_domain::CompletionRule::Decision { decision } if decision == &id
            ) && evaluation.completed.contains(&work.work.id)
        })
        .map(|(work, _)| work.to_string())
        .collect();
    Ok(ResolveDecisionView {
        decision: views::decision_view(&graph, &id).ok_or_else(|| AppError::UnknownTarget {
            kind: "decision",
            id: decision.to_string(),
        })?,
        completed_work,
        recommendation: views::recommendation_view(
            &graph,
            &evaluation,
            &relay_engine::SchedulingPolicy::default(),
            &catalog(),
        ),
        revision: outcome.revision.get(),
    })
}

/// Cancel one active attempt: the durable cancellation conclusion.
pub fn cancel_attempt(
    session: &ProjectSession,
    attempt: &str,
    reason: Option<&str>,
) -> Result<CancelView, AppError> {
    session.require_initialized()?;
    let id = relay_domain::AttemptId::parse(attempt).map_err(|_| AppError::UnknownTarget {
        kind: "attempt",
        id: attempt.to_string(),
    })?;
    let graph = session.graph();
    let node = graph.attempt(&id).ok_or_else(|| AppError::UnknownTarget {
        kind: "attempt",
        id: attempt.to_string(),
    })?;
    if let Some(conclusion) = &node.conclusion {
        return Err(AppError::Rejected(format!(
            "attempt {id} is already concluded ({})",
            views::fact_summary(&DomainFact::AttemptConcluded {
                attempt: id.clone(),
                conclusion: conclusion.clone(),
            })
            .trim_start_matches("attempt concluded: ")
        )));
    }
    let work = node.work.clone();
    let fact = DomainFact::AttemptConcluded {
        attempt: id.clone(),
        conclusion: AttemptConclusion::Canceled {
            reason: reason
                .map(str::to_string)
                .unwrap_or_else(|| "canceled by the user".to_string()),
        },
    };
    let outcome = session.append_validated(vec![fact])?;
    Ok(CancelView {
        attempt: id.to_string(),
        work: work.to_string(),
        conclusion: "canceled".to_string(),
        revision: outcome.revision.get(),
    })
}

/// Import an edited PRD view as a controlled revision: parse, validate
/// against the graph, declare the `revise-artifact` work that publishes the
/// new versions.
pub fn import_prd_revision(
    session: &ProjectSession,
    markdown: &str,
) -> Result<ImportPrdView, AppError> {
    session.require_initialized()?;
    let import = import_prd(markdown)?;
    let graph = session.graph();
    let work_number = graph
        .works()
        .filter(|(_, node)| node.work.kind.as_str() == relay_workflows::KIND_REVISE)
        .count()
        + 1;
    let work_id = WorkItemId::parse(&format!(
        "work_{}-{work_number}",
        relay_workflows::KIND_REVISE
    ))
    .map_err(|_| AppError::Rejected("could not mint a revision work id".to_string()))?;
    let revision = prd_revision(&graph, &import, work_id.clone())?;
    let proposal = revision_proposal(&revision);
    let applied = relay_engine::validate_graph_change(&graph, &proposal)
        .map_err(|rejection| AppError::Rejected(rejection.to_string()))?;
    let documents = revision
        .documents
        .iter()
        .map(|(artifact, document)| views::RevisedDocView {
            artifact: artifact.to_string(),
            schema: match document {
                relay_workflows::PrdDocument::Requirement(_) => {
                    relay_workflows::REQUIREMENT_SCHEMA.to_string()
                }
                relay_workflows::PrdDocument::Criterion(_) => {
                    relay_workflows::CRITERION_SCHEMA.to_string()
                }
            },
            slug: match document {
                relay_workflows::PrdDocument::Requirement(doc) => doc.slug.clone(),
                relay_workflows::PrdDocument::Criterion(doc) => doc.slug.clone(),
            },
        })
        .collect();
    let outcome = session.append_validated(applied.facts.clone())?;
    Ok(ImportPrdView {
        documents,
        work: work_id.to_string(),
        revision: outcome.revision.get(),
    })
}

/// What a view publication found.
#[derive(Debug, Clone, PartialEq)]
pub enum PublishOutcome {
    /// The blob was written and its evidence recorded.
    Published(PublishArtifactViewView),
    /// The blob already exists at the convention path — nothing was done.
    AlreadyPublished {
        /// The artifact.
        artifact: String,
        /// The version.
        version: u32,
        /// The existing blob path, relative to the workspace root.
        path: String,
    },
}

/// The blob path for one published artifact version, per the accepted
/// convention `.relay-graph/artifacts/<artifact>/<version>-<digest16>.blob`
/// (the digest is the version's recorded content commitment).
pub fn view_blob_path(graph: &ProjectGraph, reference: &ArtifactVersionRef) -> Option<String> {
    let record = graph.version_record(reference)?;
    let digest16: String = record.digest.to_string().chars().take(16).collect();
    Some(format!(
        ".relay-graph/artifacts/{}/{}-{digest16}.blob",
        reference.artifact,
        reference.version.get()
    ))
}

/// Publish the Markdown view of one artifact version as a view-attachment
/// blob under the accepted convention, then record the publication as
/// durable evidence. Idempotent: an existing blob is reported, not
/// rewritten.
pub fn publish_artifact_view(
    session: &ProjectSession,
    artifact: &str,
    version: Option<u32>,
) -> Result<PublishArtifactViewView, AppError> {
    session.require_initialized()?;
    let reference = artifact_reference(&session.graph(), artifact, version)?;
    match publish_version_view(session, &reference)? {
        PublishOutcome::Published(view) => Ok(view),
        PublishOutcome::AlreadyPublished {
            ref artifact,
            version,
            ref path,
        } => Ok(PublishArtifactViewView {
            artifact: artifact.clone(),
            version,
            path: path.clone(),
            sha256: String::new(),
            evidence: ids::view_evidence_id(artifact, version).to_string(),
            revision: session.graph().revision().get(),
            status: "already-published".to_string(),
        }),
    }
}

/// Publish one exact version's view (the shared, idempotent core).
pub(crate) fn publish_version_view(
    session: &ProjectSession,
    reference: &ArtifactVersionRef,
) -> Result<PublishOutcome, AppError> {
    let graph = session.graph();
    let path = view_blob_path(&graph, reference).ok_or_else(|| AppError::UnknownTarget {
        kind: "artifact version",
        id: reference.to_string(),
    })?;
    if session.workspace_root().join(&path).is_file() {
        return Ok(PublishOutcome::AlreadyPublished {
            artifact: reference.artifact.to_string(),
            version: reference.version.get(),
            path,
        });
    }
    let markdown = render_artifact_view(&graph, reference).ok_or_else(|| {
        AppError::Rejected(format!(
            "artifact {reference} carries no schema the workflow can render"
        ))
    })?;
    let digest16: String = path
        .rsplit('/')
        .next()
        .map(|name| name.to_string())
        .unwrap_or_default();
    let relative_dir = format!(".relay-graph/artifacts/{}", reference.artifact);
    let directory = session.workspace_root().join(&relative_dir);
    std::fs::create_dir_all(&directory)?;
    let published = relay_artifacts::publish_file(&directory, &digest16, markdown.as_bytes())
        .map_err(|error| AppError::Rejected(error.to_string()))?;
    let evidence_id = ids::view_evidence_id(reference.artifact.as_str(), reference.version.get());
    let evidence = Evidence::new(
        evidence_id.clone(),
        EvidenceKind::Inspection,
        EvidenceTarget::ArtifactVersion(reference.clone()),
        EvidenceOutcome::Info,
        format!(
            "markdown view published at {}/{} (sha256 {})",
            relative_dir, digest16, published.sha256
        ),
        session.now_ms(),
    );
    let outcome = session.append_validated(vec![DomainFact::EvidenceRecorded { evidence }])?;
    Ok(PublishOutcome::Published(PublishArtifactViewView {
        artifact: reference.artifact.to_string(),
        version: reference.version.get(),
        path,
        sha256: published.sha256,
        evidence: evidence_id.to_string(),
        revision: outcome.revision.get(),
        status: "published".to_string(),
    }))
}

/// Read-only doctor check over the workspace's fact store.
pub fn doctor_check(workspace_root: &Path, options: &FactStoreOptions) -> DoctorView {
    let findings = relay_store::fact::doctor::check(workspace_root, options);
    let mapped: Vec<DoctorFindingView> = findings
        .iter()
        .map(|finding| DoctorFindingView {
            severity: match finding.severity {
                relay_store::fact::doctor::FactSeverity::Ok => "ok".to_string(),
                relay_store::fact::doctor::FactSeverity::Warning => "warning".to_string(),
                relay_store::fact::doctor::FactSeverity::Error => "error".to_string(),
            },
            code: finding.code.clone(),
            detail: finding.detail.clone(),
        })
        .collect();
    let healthy = mapped.iter().all(|finding| finding.severity != "error");
    DoctorView {
        healthy,
        findings: mapped,
    }
}

/// Quarantine and rebuild the SQLite projection from the JSONL log.
pub fn rebuild_projection(session: &ProjectSession) -> Result<RebuildView, AppError> {
    session.require_initialized()?;
    let report = session.store().rebuild_projection()?;
    Ok(RebuildView {
        facts_indexed: report.facts_indexed,
        quarantine: report
            .quarantine
            .map(|path| path.to_string_lossy().into_owned()),
    })
}

/// Migrate a legacy Relay project copy into a new `.relay-graph` fact store.
///
/// `dry_run` extracts the plan (facts plus report) without staging or
/// switching anything. The legacy source is opened read-only and never
/// modified; coarse legacy relationships stay coarse and are reported, and
/// legacy kinds with no durable meaning are reported as unmapped.
pub fn migrate(
    legacy_copy: &Path,
    dry_run: bool,
    options: &FactStoreOptions,
) -> Result<MigrateView, AppError> {
    let Some(source) = relay_store::migrate::default_source(legacy_copy) else {
        return Ok(MigrateView {
            available: false,
            dry_run,
            detail: format!(
                "no legacy Relay project found in {} (looked for .relay/state.db or a .relay \
                 JSONL event store); nothing was read or written",
                legacy_copy.display()
            ),
            legacy_events: 0,
            facts_emitted: 0,
            unmapped: Vec::new(),
            inferences: 0,
            needs_review: Vec::new(),
            store_root: None,
            previous_store_backup: None,
        });
    };
    if dry_run {
        let (_facts, report) = relay_store::migrate::plan(&source)?;
        return Ok(migration_view(
            true,
            &report,
            "dry run: nothing was staged or switched".to_string(),
            None,
            None,
        ));
    }
    let outcome = relay_store::migrate::migrate(
        legacy_copy,
        &source,
        &relay_store::migrate::SemanticMigrationOptions {
            store: options.clone(),
            integrity: relay_store::fact::record::Integrity::Checksums,
        },
    )?;
    Ok(migration_view(
        false,
        &outcome.report,
        format!(
            "migrated {} legacy event(s) into {} fact(s)",
            outcome.report.legacy_events, outcome.facts
        ),
        Some(outcome.store_root.to_string_lossy().into_owned()),
        outcome
            .previous_store_backup
            .map(|path| path.to_string_lossy().into_owned()),
    ))
}

/// Assemble the migration view from an extraction report.
fn migration_view(
    dry_run: bool,
    report: &relay_store::migrate::ExtractionReport,
    detail: String,
    store_root: Option<String>,
    previous_store_backup: Option<String>,
) -> MigrateView {
    MigrateView {
        available: true,
        dry_run,
        detail,
        legacy_events: report.legacy_events,
        facts_emitted: report.facts_emitted,
        unmapped: report
            .unmapped
            .iter()
            .map(|kind| format!("{} × {}", kind.kind, kind.count))
            .collect(),
        inferences: report.inferences.len(),
        needs_review: report
            .needs_review
            .iter()
            .map(|item| item.note.clone())
            .collect(),
        store_root,
        previous_store_backup,
    }
}

/// The project summary.
pub fn summary(session: &ProjectSession) -> Result<SummaryView, AppError> {
    session.require_initialized()?;
    let graph = session.graph();
    let evaluation = session.evaluation();
    Ok(views::summary_view(&graph, &evaluation, &catalog()))
}

/// The ready list.
pub fn ready(session: &ProjectSession) -> Result<WorkListView, AppError> {
    session.require_initialized()?;
    let graph = session.graph();
    let evaluation = session.evaluation();
    Ok(views::ready_view(&graph, &evaluation, &catalog()))
}

/// The blocked list.
pub fn blocked(session: &ProjectSession) -> Result<WorkListView, AppError> {
    session.require_initialized()?;
    let graph = session.graph();
    let evaluation = session.evaluation();
    Ok(views::blocked_view(&graph, &evaluation, &catalog()))
}

/// Every work item.
pub fn work_list(session: &ProjectSession) -> Result<WorkListView, AppError> {
    session.require_initialized()?;
    Ok(views::work_list_view(&session.graph(), &catalog()))
}

/// One work item in full.
pub fn work_detail(session: &ProjectSession, work: &str) -> Result<WorkDetailView, AppError> {
    session.require_initialized()?;
    let id = parse_work_id(work)?;
    views::work_detail_view(&session.graph(), &catalog(), &id).ok_or_else(|| {
        AppError::UnknownTarget {
            kind: "work",
            id: work.to_string(),
        }
    })
}

/// The decision list.
pub fn decisions(session: &ProjectSession) -> Result<DecisionListView, AppError> {
    session.require_initialized()?;
    Ok(views::decision_list_view(&session.graph()))
}

/// One artifact version with content and optional rendered view.
pub fn artifact_detail(
    session: &ProjectSession,
    artifact: &str,
    version: Option<u32>,
    include_markdown: bool,
) -> Result<ArtifactDetailView, AppError> {
    session.require_initialized()?;
    let reference = artifact_reference(&session.graph(), artifact, version)?;
    let graph = session.graph();
    let node = graph
        .artifact(&reference.artifact)
        .ok_or_else(|| AppError::UnknownTarget {
            kind: "artifact",
            id: artifact.to_string(),
        })?;
    let record = node
        .versions
        .get(&reference.version)
        .ok_or_else(|| AppError::UnknownTarget {
            kind: "artifact version",
            id: reference.to_string(),
        })?;
    Ok(ArtifactDetailView {
        artifact: reference.artifact.to_string(),
        version: reference.version.get(),
        title: record.title.clone(),
        schema: relay_workflows::schemas::content_schema(&record.content).map(str::to_string),
        content: record.content.0.clone(),
        digest: record.digest.to_string(),
        derived_from: record
            .derived_from
            .iter()
            .map(|reference| reference.to_string())
            .collect(),
        published_at_ms: record.published_at_ms,
        markdown: if include_markdown {
            render_artifact_view(&graph, &reference)
        } else {
            None
        },
        current_version: node.current_version().get(),
    })
}

/// Evidence for a work item or an artifact-version reference
/// (`art_<slug>` or `art_<slug>@<version>`).
pub fn evidence(session: &ProjectSession, target: &str) -> Result<EvidenceListView, AppError> {
    session.require_initialized()?;
    let graph = session.graph();
    let parsed = parse_evidence_target(&graph, target)?;
    Ok(views::evidence_view(&graph, &parsed))
}

/// The engine explanation for one work item.
pub fn explain(session: &ProjectSession, work: &str) -> Result<ExplainView, AppError> {
    session.require_initialized()?;
    let id = parse_work_id(work)?;
    views::explain_view(&session.graph(), &id).ok_or_else(|| AppError::UnknownTarget {
        kind: "work",
        id: work.to_string(),
    })
}

// -- helpers ---------------------------------------------------------------

/// Parse a work id with a friendly error.
pub fn parse_work_id(work: &str) -> Result<WorkItemId, AppError> {
    WorkItemId::parse(work).map_err(|_| AppError::UnknownTarget {
        kind: "work",
        id: work.to_string(),
    })
}

/// Resolve `artifact[@version]` to an exact published version; a bare id
/// means the current version. An explicit numeric version may also arrive
/// as a separate parameter, with the `@` suffix winning when both are
/// present.
pub fn artifact_reference(
    graph: &ProjectGraph,
    artifact: &str,
    version: Option<u32>,
) -> Result<ArtifactVersionRef, AppError> {
    let (id_text, version_text) = match artifact.split_once('@') {
        Some((id, suffix)) => (id, Some(suffix)),
        None => (artifact, None),
    };
    let id = ArtifactId::parse(id_text).map_err(|_| AppError::UnknownTarget {
        kind: "artifact",
        id: artifact.to_string(),
    })?;
    let node = graph.artifact(&id).ok_or_else(|| AppError::UnknownTarget {
        kind: "artifact",
        id: artifact.to_string(),
    })?;
    let version = match version_text
        .map(str::to_string)
        .or_else(|| version.map(|n| n.to_string()))
    {
        Some(text) => text
            .parse::<u32>()
            .ok()
            .and_then(|n| ArtifactVersion::new(n).ok())
            .ok_or_else(|| AppError::UnknownTarget {
                kind: "artifact version",
                id: artifact.to_string(),
            })?,
        None => node.current_version(),
    };
    if !node.versions.contains_key(&version) {
        return Err(AppError::UnknownTarget {
            kind: "artifact version",
            id: format!("{id}@{version}"),
        });
    }
    Ok(ArtifactVersionRef::new(id, version))
}

fn parse_evidence_target(graph: &ProjectGraph, target: &str) -> Result<EvidenceTarget, AppError> {
    if let Ok(work) = WorkItemId::parse(target)
        && graph.work(&work).is_some()
    {
        return Ok(EvidenceTarget::WorkItem(work));
    }
    if let Ok(reference) = artifact_reference(graph, target, None) {
        return Ok(EvidenceTarget::ArtifactVersion(reference));
    }
    if let Ok(decision) = DecisionId::parse(target)
        && graph.decision(&decision).is_some()
    {
        return Ok(EvidenceTarget::Decision(decision));
    }
    Err(AppError::UnknownTarget {
        kind: "work, artifact, or decision",
        id: target.to_string(),
    })
}

/// Fold an expansion round into a candidate fact list (used by `init`).
fn apply_expansion_round(facts: &mut Vec<DomainFact>) -> Result<(), AppError> {
    let workflow = catalog();
    let mut candidate = ProjectGraph::from_facts(facts.iter())
        .map_err(|error| AppError::Rejected(error.to_string()))?;
    for proposal in propose_next(&candidate, &workflow) {
        if let Ok(applied) = relay_engine::validate_graph_change(&candidate, &proposal) {
            for fact in applied.facts {
                candidate
                    .apply(&fact)
                    .map_err(|error| AppError::Rejected(error.to_string()))?;
                facts.push(fact);
            }
        }
    }
    Ok(())
}
