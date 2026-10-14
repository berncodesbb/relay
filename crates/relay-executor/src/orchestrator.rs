//! The orchestrator: schedules attempts, leases them durably, runs
//! executors, and concludes attempts through engine validation.
//!
//! Deliberately small and composable — not a controller:
//!
//! 1. `recommend` (engine) decides what is ready and how it may batch;
//! 2. `AttemptStarted` is appended (the lease, under the sink's per-append
//!    writer lock) *before* any executor runs;
//! 3. executors run on the caller-supplied spawner; the writer lock is
//!    never held across executor work;
//! 4. receipts are validated by `accept_receipt` (engine) and only then
//!    recorded — a stale or invalid receipt becomes a `Failed` conclusion,
//!    never a completion;
//! 5. cancellation wins over completion: work performed while the attempt
//!    token fired is concluded `Canceled`, never accepted;
//! 6. at open, `reconcile_abandoned` marks every unterminated attempt
//!    `Abandoned` — never success — and reports write-authority abandons
//!    so the app can surface a human decision instead of auto-reverting.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;

use relay_domain::{
    ArtifactVersionRef, AttemptConclusion, AttemptId, AttemptReceipt, CapabilityGrant, DomainFact,
    ExecutorClass, GrantId, InputRef, ModelRoute, ProjectGraph, ScopePath, WorkItemId, WorkNode,
};
use relay_engine::schedule::SchedulingPolicy;
use relay_engine::{accept_receipt, recommend};
use relay_providers::contract::CancelToken;
use relay_tools::ToolRegistry;

use crate::budget::{BudgetGuard, spawn_watchdog};
use crate::executors::ExecutorSet;
use crate::model::ModelClient;
use crate::sink::{FactSink, FactSinkError};
use crate::spec::{AttemptSpec, Clock, GraphView, IdSource, RunContext, Spawn};

/// Decides the grant for a scheduled work item. Compose policy here
/// (routes, budgets, ceilings) without touching scheduling.
pub type GrantSource = Arc<dyn Fn(&WorkNode) -> CapabilityGrant + Send + Sync>;

/// Why an orchestration step failed outright (sink failures, missing
/// executors). Attempt-level failures are conclusions, not errors.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OrchestratorError {
    /// The fact sink rejected or failed an append/read.
    #[error(transparent)]
    Sink(#[from] FactSinkError),
    /// No executor is registered for a scheduled class.
    #[error("no executor registered for {0} work")]
    MissingExecutor(ExecutorClass),
}

/// One scheduled-but-refused work item, with the deterministic reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    /// The work item refused.
    pub work: WorkItemId,
    /// Why it was not started.
    pub reason: String,
}

/// One reconciled abandoned attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbandonedReport {
    /// The abandoned attempt.
    pub attempt: AttemptId,
    /// The work it ran.
    pub work: WorkItemId,
    /// Whether its grant carried write authority; when true the workspace
    /// may hold partial writes and the app should surface a human decision
    /// (never auto-revert).
    pub had_write_authority: bool,
}

/// What one `tick` did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TickReport {
    /// Attempts leased and run this tick.
    pub started: Vec<AttemptId>,
    /// Work the orchestrator refused to start, with reasons.
    pub refusals: Vec<Refusal>,
    /// Attempts whose receipts were accepted (facts appended).
    pub accepted: Vec<AttemptId>,
    /// Attempts concluded failed, with the rejection or failure reason.
    pub failed: Vec<(AttemptId, String)>,
    /// Attempts concluded canceled (token fired during the run).
    pub canceled: Vec<AttemptId>,
}

/// Build a grant from a work node's effective scopes and operations.
///
/// The default composition: read scopes become `read_paths`, effective
/// write scopes become `write_paths` (with the write operation added when
/// scopes exist), operations come from the work's effective set, and the
/// caller supplies the model route and budget. Command allowlists and
/// extra environment names stay empty unless policy adds them.
pub fn grant_from_work(
    id: GrantId,
    work: &WorkNode,
    model_route: Option<ModelRoute>,
    budget: relay_domain::Budget,
) -> CapabilityGrant {
    let write_paths = work.effective_write_scopes();
    let mut operations = work.effective_operations();
    if !write_paths.is_empty() && !operations.contains(&relay_domain::Operation::Write) {
        operations.push(relay_domain::Operation::Write);
    }
    CapabilityGrant {
        id,
        read_paths: work.work.read_scopes.clone(),
        write_paths,
        operations,
        command_allowlist: Vec::new(),
        environment_names: Vec::new(),
        model_route,
        budget,
        delivery: relay_domain::DeliveryAuthority::None,
    }
}

/// Whether a new grant's write authority overlaps any active attempt's.
/// The engine already defers overlapping writers in scheduling; this is
/// the orchestrator's independent refusal (defense in depth).
pub fn overlapping_write_conflict(graph: &ProjectGraph, grant: &CapabilityGrant) -> Option<String> {
    if grant.write_paths.is_empty() {
        return None;
    }
    for (_, attempt) in graph.attempts() {
        if !attempt.is_active() || attempt.grant.write_paths.is_empty() {
            continue;
        }
        let overlaps = attempt.grant.write_paths.iter().any(|held| {
            grant
                .write_paths
                .iter()
                .any(|candidate| held.overlaps(candidate))
        });
        if overlaps {
            return Some(format!(
                "attempt {} already holds overlapping write authority [{}]",
                attempt.id,
                scope_names(&attempt.grant.write_paths),
            ));
        }
    }
    None
}

fn scope_names(scopes: &[ScopePath]) -> String {
    scopes
        .iter()
        .map(|scope| scope.as_str().to_owned())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Schedules and supervises attempts against a fact sink. All state lives
/// in the sink; the orchestrator itself is stateless between ticks.
pub struct Orchestrator {
    sink: Arc<dyn FactSink>,
    clock: Clock,
    ids: Arc<IdSource>,
    grants: GrantSource,
    executors: ExecutorSet,
    spawn: Spawn,
    model: Arc<dyn ModelClient>,
    tools: Arc<ToolRegistry>,
    workspace_root: PathBuf,
    policy: SchedulingPolicy,
    watchdogs: bool,
    graph_view: Option<GraphView>,
    using_default_grants: bool,
}

impl Orchestrator {
    /// An orchestrator over `sink` and `clock` with safe defaults: inline
    /// spawner, default grants (scopes from the work node), and the
    /// default scheduling policy. Configure with the `with_*` builders,
    /// then wrap in `Arc` to run ticks.
    pub fn new(sink: Arc<dyn FactSink>, clock: Clock) -> Self {
        let ids = Arc::new(IdSource::new());
        Orchestrator {
            sink,
            clock,
            // One shared source mints both att_ and grant_ ids so seeding
            // covers everything the default grant factory mints.
            grants: default_grant_source(Arc::clone(&ids), None),
            ids,
            executors: ExecutorSet::new(),
            spawn: crate::spec::inline_spawn(),
            model: Arc::new(NoModel),
            tools: Arc::new(
                relay_tools::create_default_tool_registry(&relay_config::ToolSettings::default())
                    .unwrap_or_else(|error| unreachable_registry(&error)),
            ),
            workspace_root: std::env::temp_dir(),
            policy: SchedulingPolicy::default(),
            watchdogs: false,
            graph_view: None,
            using_default_grants: true,
        }
    }

    /// Register the executor set (one executor per class).
    pub fn with_executors(mut self, executors: ExecutorSet) -> Self {
        self.executors = executors;
        self
    }

    /// Use a custom spawner (threads in production, inline in tests).
    pub fn with_spawn(mut self, spawn: Spawn) -> Self {
        self.spawn = spawn;
        self
    }

    /// Use a custom grant source.
    pub fn with_grants(mut self, grants: GrantSource) -> Self {
        self.grants = grants;
        self.using_default_grants = false;
        self
    }

    /// Use a specific id source. Seed it first when opening an existing
    /// store ([`IdSource::seed_from`]) so a second process never mints
    /// `att_1` against a history that already holds it; the default grant
    /// factory is rebound to the same source.
    pub fn with_ids(mut self, ids: Arc<IdSource>) -> Self {
        if self.using_default_grants {
            self.grants = default_grant_source(Arc::clone(&ids), None);
        }
        self.ids = ids;
        self
    }

    /// Supply the read-only graph view executors receive. The default
    /// snapshots the fact sink once at attempt start; an explicit view
    /// (for example a cached projection) replaces it. No storage lock is
    /// ever held into executor work either way.
    pub fn with_graph_view(mut self, graph_view: GraphView) -> Self {
        self.graph_view = Some(graph_view);
        self
    }

    /// Use a specific model client.
    pub fn with_model(mut self, model: Arc<dyn ModelClient>) -> Self {
        self.model = model;
        self
    }

    /// Use a specific tool registry.
    pub fn with_tools(mut self, tools: Arc<ToolRegistry>) -> Self {
        self.tools = tools;
        self
    }

    /// Set the workspace root tools resolve against.
    pub fn with_workspace_root(mut self, root: PathBuf) -> Self {
        self.workspace_root = root;
        self
    }

    /// Set the scheduling policy.
    pub fn with_policy(mut self, policy: SchedulingPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Enable background duration watchdogs (requires a threaded spawner;
    /// inline spawners would block).
    pub fn with_duration_watchdogs(mut self, enabled: bool) -> Self {
        self.watchdogs = enabled;
        self
    }

    /// Reconcile at open: every `AttemptStarted` without a conclusion is
    /// concluded `Abandoned` (never success), and write-authority abandons
    /// are reported for the app to surface as human decisions.
    pub fn reconcile_abandoned(&self) -> Result<Vec<AbandonedReport>, OrchestratorError> {
        let graph = self.sink.graph()?;
        let mut reports = Vec::new();
        for (_, attempt) in graph.attempts() {
            if !attempt.is_active() {
                continue;
            }
            let report = AbandonedReport {
                attempt: attempt.id.clone(),
                work: attempt.work.clone(),
                had_write_authority: !attempt.grant.write_paths.is_empty(),
            };
            self.sink.append(DomainFact::AttemptConcluded {
                attempt: attempt.id.clone(),
                conclusion: AttemptConclusion::Abandoned {
                    reason:
                        "process reopened with an unterminated attempt; never treated as success"
                            .to_owned(),
                },
            })?;
            reports.push(report);
        }
        Ok(reports)
    }

    /// Run one scheduling wave: lease and run every recommended parallel
    /// attempt concurrently (on the spawner) plus at most one serialized
    /// writer, then conclude each through engine validation.
    pub fn tick(self: &Arc<Self>) -> Result<TickReport, OrchestratorError> {
        let report = Arc::new(Mutex::new(TickReport::default()));
        let recommendation = recommend(&self.sink.graph()?, &self.policy);

        let parallel_count = recommendation.parallel.len();
        let mut specs: Vec<AttemptSpec> = Vec::new();
        for entry in recommendation
            .parallel
            .iter()
            .chain(recommendation.serialized.first())
        {
            match self.start_attempt(&entry.work)? {
                Ok(spec) => specs.push(spec),
                Err(reason) => {
                    report
                        .lock()
                        .unwrap_or_else(|poison| poison.into_inner())
                        .refusals
                        .push(Refusal {
                            work: entry.work.clone(),
                            reason,
                        });
                }
            }
        }

        // Defense in depth: parallel entries must never write.
        for spec in specs.iter().take(parallel_count) {
            if !spec.grant.write_paths.is_empty() {
                return Err(OrchestratorError::Sink(FactSinkError::Failed(format!(
                    "engine recommended {} for the parallel batch with write authority",
                    spec.work
                ))));
            }
        }

        let mut handles = Vec::new();
        for spec in specs.iter().take(parallel_count) {
            let orchestrator = Arc::clone(self);
            let report = Arc::clone(&report);
            let spec = spec.clone();
            handles.push((self.spawn)(Box::new(move || {
                orchestrator.run_attempt(&spec, &report);
            })));
        }
        for handle in handles {
            handle.join();
        }
        // The single serialized writer runs only after the parallel batch
        // joined, keeping workspace writes exclusive.
        if let Some(spec) = specs.get(parallel_count) {
            self.run_attempt(spec, &report);
        }

        Ok(Arc::try_unwrap(report)
            .map(|mutex| mutex.into_inner().unwrap_or_default())
            .unwrap_or_default())
    }

    /// Lease an attempt: append `AttemptStarted` (the durable lease, under
    /// the sink's per-append writer lock) before anything runs.
    fn start_attempt(
        &self,
        work: &WorkItemId,
    ) -> Result<Result<AttemptSpec, String>, OrchestratorError> {
        let graph = self.sink.graph()?;
        let Some(node) = graph.work(work) else {
            return Ok(Err(format!("work {work} no longer exists")));
        };
        if self.executors.get(node.work.executor).is_none() {
            return Err(OrchestratorError::MissingExecutor(node.work.executor));
        }
        let grant = (self.grants)(node);
        if let Some(reason) = overlapping_write_conflict(&graph, &grant) {
            return Ok(Err(reason));
        }
        let expected_revision = graph.revision();
        let spec = AttemptSpec {
            attempt: self.ids.next_attempt_id(),
            work: work.clone(),
            objective: node.work.objective.clone(),
            inputs: resolve_inputs(&graph, node),
            decision: match &node.work.completion {
                relay_domain::CompletionRule::Decision { decision } => Some(decision.clone()),
                _ => None,
            },
            grant: grant.clone(),
            expected_revision,
            started_at_ms: (self.clock)(),
        };
        self.sink.append(DomainFact::AttemptStarted {
            attempt: spec.attempt.clone(),
            work: work.clone(),
            expected_revision,
            grant,
            started_at_ms: spec.started_at_ms,
        })?;
        Ok(Ok(spec))
    }

    /// Run one leased attempt and conclude it. Never panics and never
    /// propagates errors: attempt-level failures become conclusions.
    fn run_attempt(self: &Arc<Self>, spec: &AttemptSpec, report: &Arc<Mutex<TickReport>>) {
        report
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .started
            .push(spec.attempt.clone());

        let graph = match self.sink.graph() {
            Ok(graph) => graph,
            Err(error) => {
                self.conclude_without_receipt(spec, report, format!("sink failed: {error}"));
                return;
            }
        };
        let Some(node) = graph.work(&spec.work) else {
            self.conclude_without_receipt(spec, report, format!("work {} vanished", spec.work));
            return;
        };
        let Some(executor) = self.executors.get(node.work.executor) else {
            self.conclude_without_receipt(
                spec,
                report,
                format!("no executor for {} work", node.work.executor),
            );
            return;
        };

        let (context, watchdog_stop) = self.context_for(spec);
        let receipt = executor.run(spec, &context);
        if self.watchdogs {
            watchdog_stop.cancel("attempt concluded");
        }
        self.conclude(spec, receipt, &context.signal, report);
    }

    /// Conclude an attempt that produced no receipt (infrastructure
    /// failure before execution).
    fn conclude_without_receipt(
        &self,
        spec: &AttemptSpec,
        report: &Arc<Mutex<TickReport>>,
        reason: String,
    ) {
        let conclusion = AttemptConclusion::Failed {
            reason: reason.clone(),
        };
        let _ = self.sink.append(DomainFact::AttemptConcluded {
            attempt: spec.attempt.clone(),
            conclusion,
        });
        report
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .failed
            .push((spec.attempt.clone(), reason));
    }

    /// Validate and record a receipt, or record the matching terminal
    /// conclusion. Cancellation wins over completion.
    fn conclude(
        &self,
        spec: &AttemptSpec,
        receipt: AttemptReceipt,
        signal: &CancelToken,
        report: &Arc<Mutex<TickReport>>,
    ) {
        if signal.is_cancelled() {
            let reason = signal.reason().unwrap_or_else(|| "canceled".to_owned());
            let _ = self.sink.append(DomainFact::AttemptConcluded {
                attempt: spec.attempt.clone(),
                conclusion: AttemptConclusion::Canceled { reason },
            });
            report
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .canceled
                .push(spec.attempt.clone());
            return;
        }
        let graph = match self.sink.graph() {
            Ok(graph) => graph,
            Err(error) => {
                self.conclude_without_receipt(
                    spec,
                    report,
                    format!("sink failed during validation: {error}"),
                );
                return;
            }
        };
        match accept_receipt(&graph, &receipt) {
            Ok(acceptance) => {
                for fact in acceptance.facts {
                    if let Err(error) = self.sink.append(fact) {
                        self.conclude_without_receipt(
                            spec,
                            report,
                            format!("acceptance append failed: {error}"),
                        );
                        return;
                    }
                }
                report
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner())
                    .accepted
                    .push(spec.attempt.clone());
            }
            Err(rejection) => {
                let reason = rejection.to_string();
                let _ = self.sink.append(DomainFact::AttemptConcluded {
                    attempt: spec.attempt.clone(),
                    conclusion: AttemptConclusion::Failed {
                        reason: reason.clone(),
                    },
                });
                report
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner())
                    .failed
                    .push((spec.attempt.clone(), reason));
            }
        }
    }

    /// Build the per-attempt run context: fresh cancellation token, armed
    /// budget guard, optional watchdog, shared model/tools.
    fn context_for(self: &Arc<Self>, spec: &AttemptSpec) -> (RunContext, CancelToken) {
        let signal = CancelToken::new();
        let budget = Arc::new(BudgetGuard::new(
            spec.grant.budget,
            spec.started_at_ms,
            Arc::clone(&self.clock),
            signal.clone(),
        ));
        let watchdog_stop = CancelToken::new();
        if self.watchdogs && spec.grant.budget.max_duration_ms.is_some() {
            spawn_watchdog(Arc::clone(&budget), watchdog_stop.clone(), &self.spawn);
        }
        // The graph view: an explicit app-supplied projection, or a
        // one-time snapshot of the sink taken here, before executor work
        // begins — never a lock held across it.
        let graph = self.graph_view.clone().unwrap_or_else(|| {
            let snapshot = self.sink.graph().unwrap_or_default();
            Arc::new(move || snapshot.clone())
        });
        (
            RunContext {
                workspace_root: self.workspace_root.clone(),
                signal,
                model: Arc::clone(&self.model),
                tools: Arc::clone(&self.tools),
                budget,
                graph,
                now_ms: (self.clock)(),
            },
            watchdog_stop,
        )
    }
}

/// Resolve a work item's inputs to exact versions at scheduling time.
fn resolve_inputs(graph: &ProjectGraph, node: &WorkNode) -> Vec<ArtifactVersionRef> {
    node.work
        .inputs
        .iter()
        .filter_map(|input| match input {
            InputRef::Exact(reference) => Some(reference.clone()),
            InputRef::LatestPublished(artifact) => graph
                .current_version(artifact)
                .map(|version| ArtifactVersionRef::new(artifact.clone(), version)),
        })
        .collect()
}

/// The default grant source: scopes and operations from the work node, no
/// route, no budget, no delivery authority.
fn default_grant_source(ids: Arc<IdSource>, route: Option<ModelRoute>) -> GrantSource {
    Arc::new(move |work| {
        grant_from_work(
            ids.next_grant_id(),
            work,
            route.clone(),
            relay_domain::Budget::default(),
        )
    })
}

/// A model client that fails every call; replaced by `with_model`.
struct NoModel;

impl ModelClient for NoModel {
    fn complete(
        &self,
        _route: &ModelRoute,
        _request: &crate::model::ModelTurnRequest,
        _signal: &CancelToken,
    ) -> Result<crate::model::ModelTurnResponse, crate::model::ModelClientError> {
        Err(crate::model::ModelClientError::Provider(
            crate::model::ModelCallFailure::from_message("no model client configured"),
        ))
    }
}

/// Registry construction cannot fail for the built-in tool set; fail loud
/// rather than silently running without tools.
#[allow(clippy::panic)] // scoped: built-in registration is compile-time fixed
fn unreachable_registry(error: &relay_tools::ToolRegistrationError) -> ! {
    panic!("built-in tool registry failed to build: {error}");
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::executors::{ExecutorSet, SystemExecutor, SystemJob, SystemOutcome};
    use crate::sink::InMemoryFactSink;
    use relay_domain::{CompletionRule, DomainFact, ExecutorClass, WorkItem, WorkItemId, WorkKind};

    /// A system job that records one evidence entry (no graph needed).
    struct NoteJob;

    impl SystemJob for NoteJob {
        fn run(&self, spec: &crate::AttemptSpec, context: &crate::RunContext) -> SystemOutcome {
            SystemOutcome {
                outputs: Vec::new(),
                proposals: Vec::new(),
                evidence: vec![relay_domain::Evidence {
                    id: relay_domain::EvidenceId::parse(&format!(
                        "ev_{}-note",
                        spec.attempt.as_str().strip_prefix("att_").unwrap_or("x")
                    ))
                    .unwrap(),
                    kind: relay_domain::EvidenceKind::Inspection,
                    target: relay_domain::EvidenceTarget::WorkItem(spec.work.clone()),
                    outcome: relay_domain::EvidenceOutcome::Info,
                    summary: "noted".to_owned(),
                    payload: serde_json::json!({ "at_ms": context.now_ms }),
                    recorded_at_ms: context.now_ms,
                }],
                note: None,
            }
        }
    }

    /// A store holding two concluded attempts (`att_1`, `att_2`) for one
    /// unfinished system work item — the state a second process opens.
    fn seeded_store() -> Arc<InMemoryFactSink> {
        let sink = InMemoryFactSink::new();
        sink.append(DomainFact::ProjectCreated {
            project: relay_domain::ProjectId::parse("prj_seed2").unwrap(),
        })
        .expect("project");
        sink.append(DomainFact::GoalDeclared {
            goal: relay_domain::Goal {
                id: relay_domain::GoalId::parse("goal_seed2").unwrap(),
                outcome: "seed ids across processes".into(),
                constraints: Vec::new(),
                success_definition: "no duplicate ids".into(),
                provenance: relay_domain::Provenance {
                    source: relay_domain::ProvenanceSource::User,
                    statement: None,
                    at_ms: 0,
                },
                declared_at_ms: 0,
            },
        })
        .expect("goal");
        sink.append(DomainFact::WorkDeclared {
            work: WorkItem::new(
                WorkItemId::parse("work_seed2").unwrap(),
                WorkKind::parse("note-work").unwrap(),
                ExecutorClass::System,
                "leave a note",
                CompletionRule::Receipt,
            ),
        })
        .expect("work");
        for n in 1..=2u64 {
            let revision = sink.graph().expect("graph").revision();
            sink.append(DomainFact::AttemptStarted {
                attempt: relay_domain::AttemptId::parse(&format!("att_{n}")).unwrap(),
                work: WorkItemId::parse("work_seed2").unwrap(),
                expected_revision: revision,
                grant: relay_domain::CapabilityGrant::read_only(
                    relay_domain::GrantId::parse(&format!("grant_{n}")).unwrap(),
                    vec![],
                ),
                started_at_ms: n as i64,
            })
            .expect("start");
            sink.append(DomainFact::AttemptConcluded {
                attempt: relay_domain::AttemptId::parse(&format!("att_{n}")).unwrap(),
                conclusion: AttemptConclusion::Failed {
                    reason: "earlier process".to_owned(),
                },
            })
            .expect("conclude");
        }
        Arc::new(sink)
    }

    fn orchestrator(sink: Arc<InMemoryFactSink>, ids: Arc<IdSource>) -> Arc<Orchestrator> {
        Arc::new(
            Orchestrator::new(sink as Arc<dyn FactSink>, crate::system_clock())
                .with_executors(
                    ExecutorSet::new().with(Arc::new(SystemExecutor::new(Arc::new(NoteJob)))),
                )
                .with_ids(ids),
        )
    }

    #[test]
    fn an_unseeded_id_source_collides_with_an_existing_store() {
        let sink = seeded_store();
        // A fresh process-local source mints att_1, which the fold rejects
        // as a duplicate: exactly the cross-process bug seeding fixes.
        let error = orchestrator(Arc::clone(&sink), Arc::new(IdSource::new()))
            .tick()
            .expect_err("duplicate id must be rejected");
        assert!(error.to_string().contains("fact rejected"), "{error}");
    }

    #[test]
    fn a_seeded_id_source_continues_past_existing_attempts() {
        let sink = seeded_store();
        let ids = Arc::new(IdSource::new());
        ids.seed_from(&sink.graph().expect("graph"));
        let report = orchestrator(Arc::clone(&sink), ids).tick().expect("tick");
        assert_eq!(report.started.len(), 1, "{report:?}");
        assert_eq!(report.accepted.len(), 1, "{report:?}");
        let graph = sink.graph().expect("graph");
        let attempt_id = report.started[0].as_str();
        assert!(
            attempt_id != "att_1" && attempt_id != "att_2",
            "minted {attempt_id}; must continue past the existing ids"
        );
        assert!(graph.attempt(&report.started[0]).is_some());
    }
}
