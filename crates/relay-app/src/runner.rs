//! The tick runner: composition of the orchestrator for `relay run`.
//!
//! The application decides **composition only** — which executors are
//! registered, which model client and tool registry they share, how jobs
//! spawn — never scheduling policy (engine `recommend`) or acceptance
//! (engine `accept_receipt` inside the orchestrator).
//!
//! Non-interactive ticks never register a human executor: awaiting-human
//! entries are graph state the interface surfaces, and the engine never
//! recommends human work for execution anyway, so there is double
//! protection against an attempt "answering" a decision on a person's
//! behalf.

use std::sync::Arc;

use relay_domain::ModelRoute;
use relay_engine::SchedulingPolicy;
use relay_executor::spec::Spawn;
use relay_executor::{
    ExecutorSet, ModelClient, ModelExecutor, ModelGuidedVerifierPlanner,
    ModelGuidedWorkspacePlanner, Orchestrator, SystemExecutor, VerifierExecutor, WorkspaceExecutor,
    grant_from_work, inline_spawn, thread_spawn,
};
use relay_providers::contract::ModelProvider;
use relay_tools::ToolRegistry;
use relay_workflows::WorkflowCatalog;

use crate::error::AppError;
use crate::jobs::DeliveryPrepJob;
use crate::session::ProjectSession;
use crate::sink::StoreFactSink;
use crate::views::{self, RunView, TickView};

/// How the runtime is wired for a run.
#[derive(Clone)]
pub struct RuntimeWiring {
    /// Executors by class (one per class).
    pub executors: ExecutorSet,
    /// The model client shared by attempts.
    pub model: Arc<dyn ModelClient>,
    /// The tool registry shared by attempts (grant-gated per call).
    pub tools: Arc<ToolRegistry>,
    /// How attempt jobs spawn: inline for deterministic tests, threads in
    /// production (watchdogs require threads).
    pub spawn: Spawn,
    /// Whether duration watchdogs run (requires the threaded spawner).
    pub watchdogs: bool,
    /// The model route granted to model-attempt work.
    pub route: ModelRoute,
}

impl RuntimeWiring {
    /// Production wiring over a provider adapter. Registers the system
    /// executor (always), the model executor, and the runtime owner's
    /// model-guided workspace/verifier planners. The human executor is
    /// deliberately absent: awaiting-human work is never scheduled by the
    /// engine, and a non-interactive tick must never answer for a person.
    pub fn production(provider: Arc<dyn ModelProvider>, route: ModelRoute) -> Self {
        let executors = ExecutorSet::new()
            .with(Arc::new(SystemExecutor::new(Arc::new(DeliveryPrepJob))))
            .with(Arc::new(ModelExecutor))
            .with(Arc::new(WorkspaceExecutor::new(Arc::new(
                ModelGuidedWorkspacePlanner::new(),
            ))))
            .with(Arc::new(VerifierExecutor::new(Arc::new(
                ModelGuidedVerifierPlanner::new(),
            ))));
        RuntimeWiring {
            executors,
            model: Arc::new(relay_executor::ProviderModelClient::new(provider)),
            tools: default_tools(),
            spawn: thread_spawn(),
            watchdogs: true,
            route,
        }
    }

    /// Deterministic wiring for tests: caller-supplied executors and model
    /// client, inline spawning, no watchdogs.
    pub fn scripted(executors: ExecutorSet, model: Arc<dyn ModelClient>) -> Self {
        RuntimeWiring {
            executors,
            model,
            tools: default_tools(),
            spawn: inline_spawn(),
            watchdogs: false,
            route: ModelRoute {
                provider: "fake".to_string(),
                model: "fake-standard".to_string(),
                role: None,
            },
        }
    }
}

/// The shared tool registry. Registration is compile-time fixed; if it ever
/// failed, run with an empty registry and let the grant gate refuse every
/// call (fail closed) rather than panicking in the application service.
fn default_tools() -> Arc<ToolRegistry> {
    Arc::new(
        relay_tools::create_default_tool_registry(&relay_config::ToolSettings::default())
            .unwrap_or_else(|_| ToolRegistry::new()),
    )
}

/// Run up to `max_ticks` orchestrator ticks under `policy`, stopping early
/// when a tick schedules nothing. Every tick's facts go through the store's
/// checked append; the session refreshes after the run, then Markdown views
/// are auto-published for newly accepted artifact versions (idempotent,
/// bounded by [`AUTO_PUBLISH_LIMIT`]).
///
/// Attempt ids are seeded past everything the graph already holds
/// ([`relay_executor::IdSource::seed_from`] through `with_ids`), so a fresh
/// process — or a later run in the same process — never mints an id the
/// fold would reject. Concurrently driving one store from two orchestrators
/// remains unsupported: the writer lock serializes appends, but both would
/// seed from the same prefix.
pub fn run_ticks(
    session: &ProjectSession,
    wiring: &RuntimeWiring,
    policy: SchedulingPolicy,
    max_ticks: usize,
) -> Result<RunView, AppError> {
    session.require_initialized()?;
    let versions_before = published_version_set(&session.graph());
    let sink = Arc::new(StoreFactSink::new(session.store()));
    let route = wiring.route.clone();
    let grant_ids = Arc::new(relay_executor::IdSource::new());
    let grants: relay_executor::GrantSource = Arc::new(move |work| {
        grant_from_work(
            grant_ids.next_grant_id(),
            work,
            Some(route.clone()),
            relay_domain::Budget::default(),
        )
    });
    let ids = Arc::new(relay_executor::IdSource::new());
    ids.seed_from(&session.graph());
    let orchestrator = Arc::new(
        Orchestrator::new(sink, session.clock())
            .with_executors(wiring.executors.clone())
            .with_spawn(Arc::clone(&wiring.spawn))
            .with_grants(grants)
            .with_ids(ids)
            .with_model(Arc::clone(&wiring.model))
            .with_tools(Arc::clone(&wiring.tools))
            .with_workspace_root(session.workspace_root().to_path_buf())
            .with_policy(policy)
            .with_duration_watchdogs(wiring.watchdogs),
    );
    let mut ticks = Vec::new();
    for index in 0..max_ticks {
        let report = orchestrator.tick().map_err(AppError::Executor)?;
        let scheduled = report.started.len() + report.refusals.len();
        ticks.push(TickView {
            index: index + 1,
            started: report
                .started
                .iter()
                .map(|attempt| attempt.to_string())
                .collect(),
            refusals: report
                .refusals
                .iter()
                .map(|refusal| views::RefusalEntryView {
                    work: refusal.work.to_string(),
                    reason: refusal.reason.clone(),
                })
                .collect(),
            accepted: report
                .accepted
                .iter()
                .map(|attempt| attempt.to_string())
                .collect(),
            failed: report
                .failed
                .iter()
                .map(|(attempt, reason)| views::FailedEntryView {
                    attempt: attempt.to_string(),
                    reason: reason.clone(),
                })
                .collect(),
            canceled: report
                .canceled
                .iter()
                .map(|attempt| attempt.to_string())
                .collect(),
        });
        if scheduled == 0 {
            break;
        }
    }
    session.refresh()?;
    let (published_views, view_notes) = auto_publish_views(session, &versions_before);
    let graph = session.graph();
    let evaluation = session.evaluation();
    let awaiting_human = evaluation
        .awaiting_human
        .iter()
        .filter_map(|entry| views::decision_view(&graph, &entry.decision))
        .collect();
    Ok(RunView {
        autonomy: views::autonomy_name(policy.autonomy),
        ticks,
        awaiting_human,
        recommendation: views::recommendation_view(
            &graph,
            &evaluation,
            &relay_engine::SchedulingPolicy::default(),
            &WorkflowCatalog::builtin(),
        ),
        revision: session.graph().revision().get(),
        published_views,
        view_notes,
    })
}

/// The per-run bound on automatic view publication: a run that accepted an
/// unreasonable number of versions publishes the first
/// [`AUTO_PUBLISH_LIMIT`] and reports the rest instead of looping.
pub const AUTO_PUBLISH_LIMIT: usize = 64;

/// Every published artifact version of a graph, keyed by
/// `"<artifact>@<version>"`.
fn published_version_set(graph: &relay_domain::ProjectGraph) -> std::collections::BTreeSet<String> {
    graph
        .artifacts()
        .flat_map(|(artifact, node)| {
            node.versions
                .keys()
                .map(move |version| format!("{artifact}@{version}"))
        })
        .collect()
}

/// Publish Markdown views for artifact versions the run newly accepted
/// (idempotent, bounded). A version whose schema has no renderer — or whose
/// publication fails — is reported in the notes; a successful run is never
/// failed by its views.
fn auto_publish_views(
    session: &ProjectSession,
    before: &std::collections::BTreeSet<String>,
) -> (Vec<views::PublishArtifactViewView>, Vec<String>) {
    let mut published = Vec::new();
    let mut notes = Vec::new();
    let after = published_version_set(&session.graph());
    let mut fresh: Vec<&String> = after.difference(before).collect();
    fresh.sort();
    if fresh.len() > AUTO_PUBLISH_LIMIT {
        notes.push(format!(
            "{} new artifact version(s) this run; publishing the first {} and leaving the rest \
             for `relay artifact inspect <id> --publish`",
            fresh.len(),
            AUTO_PUBLISH_LIMIT
        ));
    }
    for key in fresh.into_iter().take(AUTO_PUBLISH_LIMIT) {
        let Some((artifact, version)) = key.as_str().split_once('@') else {
            continue;
        };
        let Some(version) = version
            .parse::<u32>()
            .ok()
            .and_then(|n| relay_domain::ArtifactVersion::new(n).ok())
        else {
            continue;
        };
        let reference = match relay_domain::ArtifactId::parse(artifact) {
            Ok(id) => relay_domain::ArtifactVersionRef::new(id, version),
            Err(_) => continue,
        };
        match crate::commands::publish_version_view(session, &reference) {
            Ok(crate::commands::PublishOutcome::Published(view)) => published.push(view),
            Ok(crate::commands::PublishOutcome::AlreadyPublished { .. }) => {}
            Err(error) => notes.push(format!(
                "view for {reference} not published: {}",
                error.message()
            )),
        }
    }
    (published, notes)
}
