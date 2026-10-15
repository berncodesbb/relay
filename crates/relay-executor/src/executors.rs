//! The executor trait and the five executor classes.
//!
//! Executors perform work and return [`AttemptReceipt`]s — data. They never
//! append facts, install graph changes, or accept their own completion:
//! that is the orchestrator's job after `relay_engine::accept_receipt`
//! validates. Every executor receives only [`AttemptSpec`] (immutable
//! domain data plus the grant) and [`RunContext`] (runtime handles), so no
//! executor can reach the store, the provider wire, or the graph mutably.

use std::sync::Arc;

use relay_domain::{
    AttemptOutcome, AttemptReceipt, Evidence, EvidenceId, EvidenceKind, EvidenceOutcome,
    EvidenceTarget, ExecutorClass, GraphChangeProposal, OutputProposal, Usage,
};
use serde_json::{Value, json};

use crate::model::ModelTurnRequest;
use crate::planner::{CallRecord, ToolCallRequest, ToolRunner, ToolSession};
use crate::spec::{AttemptSpec, RunContext};

/// Performs one attempt's work and accounts for it in a receipt.
pub trait Executor: Send + Sync {
    /// Which class this executor serves.
    fn class(&self) -> ExecutorClass;

    /// Run the attempt. Never panics: failures are `Failed` receipts.
    fn run(&self, spec: &AttemptSpec, context: &RunContext) -> AttemptReceipt;
}

/// Executors by class. The orchestrator looks up exactly one executor per
/// scheduled attempt.
#[derive(Clone, Default)]
pub struct ExecutorSet {
    executors: Vec<(ExecutorClass, Arc<dyn Executor>)>,
}

impl ExecutorSet {
    /// An empty set.
    pub fn new() -> Self {
        ExecutorSet::default()
    }

    /// Add an executor, keyed by its own class.
    pub fn with(mut self, executor: Arc<dyn Executor>) -> Self {
        self.executors
            .retain(|(class, _)| *class != executor.class());
        self.executors.push((executor.class(), executor));
        self
    }

    /// The executor for a class, when registered.
    pub fn get(&self, class: ExecutorClass) -> Option<Arc<dyn Executor>> {
        self.executors
            .iter()
            .find(|(registered, _)| *registered == class)
            .map(|(_, executor)| Arc::clone(executor))
    }

    /// The registered classes, in registration order.
    pub fn classes(&self) -> Vec<ExecutorClass> {
        self.executors.iter().map(|(class, _)| *class).collect()
    }
}

/// Plans the tool calls a workspace attempt performs. Scripted planners
/// implement [`plan`](Self::plan) and inherit the default `drive`
/// behavior; the model-guided planner overrides `drive` to run the whole
/// model/tool loop through the same kernel.
pub trait WorkspacePlanner: Send + Sync {
    /// The calls to perform for this attempt, in order.
    fn plan(&self, spec: &AttemptSpec, context: &RunContext) -> Vec<ToolCallRequest>;

    /// Drive a full session. The default runs the scripted calls through
    /// the shared kernel; model-guided planners override this to run
    /// model turns, executing every requested call through the same
    /// grant/budget/cancellation kernel.
    fn drive(
        &self,
        spec: &AttemptSpec,
        context: &RunContext,
        runner: &mut ToolRunner,
    ) -> ToolSession {
        let calls = self.plan(spec, context);
        let records = runner.run_all(calls);
        ToolSession::from_records(records)
    }
}

/// The workspace executor: performs grant-gated tool calls that may write
/// the repository, and accounts for every change in its receipt.
pub struct WorkspaceExecutor {
    planner: Arc<dyn WorkspacePlanner>,
}

impl WorkspaceExecutor {
    /// Use a custom planner.
    pub fn new(planner: Arc<dyn WorkspacePlanner>) -> Self {
        WorkspaceExecutor { planner }
    }

    /// Run a fixed script of calls.
    pub fn scripted(calls: Vec<ToolCallRequest>) -> Self {
        WorkspaceExecutor::new(Arc::new(FixedPlanner { calls }))
    }
}

impl Executor for WorkspaceExecutor {
    fn class(&self) -> ExecutorClass {
        ExecutorClass::Workspace
    }

    fn run(&self, spec: &AttemptSpec, context: &RunContext) -> AttemptReceipt {
        let mut runner = ToolRunner::new(spec, context);
        let session = self.planner.drive(spec, context, &mut runner);
        finish_session_receipt(
            spec,
            context,
            session,
            "workspace attempt",
            Accounting::Workspace,
        )
    }
}

/// Plans the tool calls a verifier attempt performs (read/execute only by
/// grant construction).
pub trait VerifierPlanner: Send + Sync {
    /// The calls to perform for this attempt, in order.
    fn plan(&self, spec: &AttemptSpec, context: &RunContext) -> Vec<ToolCallRequest>;

    /// Drive a full session (see
    /// [`WorkspacePlanner::drive`]).
    fn drive(
        &self,
        spec: &AttemptSpec,
        context: &RunContext,
        runner: &mut ToolRunner,
    ) -> ToolSession {
        let calls = self.plan(spec, context);
        let records = runner.run_all(calls);
        ToolSession::from_records(records)
    }
}

/// The verifier executor: collects evidence through read/execute tools and
/// reports pass/fail per observation. Whether a criterion is satisfied is
/// derived from the evidence by the engine, never asserted here.
pub struct VerifierExecutor {
    planner: Arc<dyn VerifierPlanner>,
}

impl VerifierExecutor {
    /// Use a custom planner.
    pub fn new(planner: Arc<dyn VerifierPlanner>) -> Self {
        VerifierExecutor { planner }
    }

    /// Run a fixed script of calls.
    pub fn scripted(calls: Vec<ToolCallRequest>) -> Self {
        VerifierExecutor::new(Arc::new(FixedVerifierPlanner { calls }))
    }
}

impl Executor for VerifierExecutor {
    fn class(&self) -> ExecutorClass {
        ExecutorClass::Verifier
    }

    fn run(&self, spec: &AttemptSpec, context: &RunContext) -> AttemptReceipt {
        let mut runner = ToolRunner::new(spec, context);
        let session = self.planner.drive(spec, context, &mut runner);
        // Verification completes by producing evidence; tool failures are
        // Fail evidence, not failed attempts.
        finish_session_receipt(
            spec,
            context,
            session,
            "verification attempt",
            Accounting::Verifier,
        )
    }
}

/// How a tool session maps onto receipt evidence.
enum Accounting {
    /// One inspection summarizing the calls plus extracted proposals.
    Workspace,
    /// Per-observation verdicts plus extracted proposals.
    Verifier,
}

/// Build the receipt for a tool-driven attempt (workspace or verifier)
/// from its session: records, extracted outputs/evidence/proposals, and
/// failures.
fn finish_session_receipt(
    spec: &AttemptSpec,
    context: &RunContext,
    session: ToolSession,
    label: &str,
    accounting: Accounting,
) -> AttemptReceipt {
    let records = &session.records;
    let canceled = session.stopped_canceled || records.iter().any(|record| record.canceled);
    let call_failures: Vec<&CallRecord> = records.iter().filter(|record| !record.ok).collect();
    let affected = session.affected_files();
    let summary = format!(
        "{label} {} performed {} tool call(s), {} failed, {} file(s) affected",
        spec.attempt,
        records.len(),
        call_failures.len(),
        affected.len()
    );

    let mut failure_reasons: Vec<String> = session.failures.clone();
    if canceled && failure_reasons.is_empty() {
        failure_reasons.push("canceled or budget-stopped before completion".to_owned());
    }
    if !canceled {
        failure_reasons.extend(
            call_failures
                .iter()
                .map(|failure| format!("{}: {}", failure.tool, failure.output_head)),
        );
    }
    if failure_reasons.is_empty() && session.is_empty_accounting() {
        failure_reasons.push(format!("{label} produced nothing account-worthy"));
    }

    let outcome = if failure_reasons.is_empty() {
        AttemptOutcome::Completed
    } else {
        AttemptOutcome::Failed {
            reason: failure_reasons.join("; "),
        }
    };

    // Evidence: the verifier reports per-observation verdicts; the
    // workspace reports one inspection. Extracted (model-proposed)
    // evidence joins both.
    let mut evidence = match accounting {
        Accounting::Verifier => records
            .iter()
            .enumerate()
            .map(|(index, record)| Evidence {
                id: evidence_id(spec, &format!("v{index}")),
                kind: if record.tool == "run-command" {
                    EvidenceKind::CommandOutput
                } else {
                    EvidenceKind::Inspection
                },
                target: EvidenceTarget::WorkItem(spec.work.clone()),
                outcome: if record.ok {
                    EvidenceOutcome::Pass
                } else {
                    EvidenceOutcome::Fail
                },
                summary: format!("{}: {}", record.tool, record.output_head),
                payload: json!({ "tool": record.tool, "ok": record.ok }),
                recorded_at_ms: context.now_ms,
            })
            .collect::<Vec<_>>(),
        Accounting::Workspace => vec![Evidence {
            id: evidence_id(spec, "tools"),
            kind: EvidenceKind::Inspection,
            target: EvidenceTarget::WorkItem(spec.work.clone()),
            outcome: EvidenceOutcome::Info,
            summary: summary.clone(),
            payload: json!({
                "calls": records
                    .iter()
                    .map(|record| json!({
                        "tool": record.tool,
                        "ok": record.ok,
                        "output": record.output_head,
                    }))
                    .collect::<Vec<_>>(),
                "affected_files": affected,
            }),
            recorded_at_ms: context.now_ms,
        }],
    };
    evidence.extend(session.evidence);

    // Outputs and graph changes ride only on completed receipts
    // (acceptance rejects outputs on failures); on failure they are
    // dropped with an explicit note, never silently coerced.
    let mut notes = session.notes;
    if !session.outputs.is_empty() && !matches!(outcome, AttemptOutcome::Completed) {
        notes.push(format!(
            "{} proposed output(s) discarded with the failed attempt",
            session.outputs.len()
        ));
    }
    if !session.graph_changes.is_empty() && !matches!(outcome, AttemptOutcome::Completed) {
        notes.push(format!(
            "{} graph change proposal(s) discarded with the failed attempt",
            session.graph_changes.len()
        ));
    }

    let mut accounted = receipt(
        spec,
        context,
        outcome,
        evidence,
        Usage {
            input_tokens: 0,
            output_tokens: 0,
            tool_calls: records.len() as u64,
            duration_ms: duration_of(spec, context),
        },
    );
    if matches!(accounted.outcome, AttemptOutcome::Completed) {
        accounted.outputs = session.outputs;
        accounted.graph_changes = session.graph_changes;
    }
    if !notes.is_empty()
        && let Some(evidence) = accounted.evidence.first_mut()
        && let Value::Object(payload) = &mut evidence.payload
    {
        payload.insert("notes".to_owned(), json!(notes));
    }
    accounted
}

/// The human executor: accounts for attempts whose work completes through
/// a human decision. It never fabricates the decision — it names the one
/// it waits for, and the work completes when the decision resolves.
pub struct HumanExecutor;

impl Executor for HumanExecutor {
    fn class(&self) -> ExecutorClass {
        ExecutorClass::Human
    }

    fn run(&self, spec: &AttemptSpec, context: &RunContext) -> AttemptReceipt {
        let Some(decision) = spec.decision.clone() else {
            return receipt(
                spec,
                context,
                AttemptOutcome::Failed {
                    reason: "human work carries no decision to await".to_owned(),
                },
                Vec::new(),
                Usage {
                    duration_ms: duration_of(spec, context),
                    ..Usage::default()
                },
            );
        };
        let evidence = vec![Evidence {
            id: evidence_id(spec, "decision"),
            kind: EvidenceKind::HumanStatement,
            target: EvidenceTarget::Decision(decision.clone()),
            outcome: EvidenceOutcome::Info,
            summary: format!("awaiting human decision {decision}"),
            payload: json!({ "attempt": spec.attempt.as_str(), "decision": decision.as_str() }),
            recorded_at_ms: context.now_ms,
        }];
        receipt(
            spec,
            context,
            AttemptOutcome::Failed {
                reason: format!("awaiting human decision {decision}"),
            },
            evidence,
            Usage {
                duration_ms: duration_of(spec, context),
                ..Usage::default()
            },
        )
    }
}

/// The model executor: one model turn per attempt through the
/// [`ModelClient`](crate::model::ModelClient) port, budgeted by the grant,
/// recorded as inspection evidence. The model's proposals (if any) arrive
/// with later structured-output work; this executor accounts for the turn.
pub struct ModelExecutor;

/// How many remediation turns the model executor may spend re-asking
/// after malformed output before failing (bounded independently of the
/// budget).
pub const MODEL_REMEDIATION_TURNS: u32 = 2;

impl ModelExecutor {
    /// The system prompt for model-class sessions (plain data): the
    /// objective plus the strict final-object protocol. Model-class work is
    /// tool-free: no tool definitions are sent, and a turn carrying native
    /// tool calls is rejected with a remediation turn, never executed.
    pub fn system_prompt() -> String {
        format!(
            "You are a careful model reasoner producing one structured \
             deliverable. You have NO tools and cannot call any; reply with \
             the final object only.\n\n{}",
            crate::planner::protocol_contract()
        )
    }

    /// The opening turn prompt (plain data): objective and inputs.
    pub fn opening_prompt(objective: &str, inputs: &[String]) -> String {
        let inputs = if inputs.is_empty() {
            "none".to_owned()
        } else {
            inputs.join(", ")
        };
        format!("Objective: {objective}\nInputs: {inputs}\n\nProduce the final object now.")
    }

    /// The remediation turn prompt (plain data): the actionable reasons
    /// the previous reply was rejected.
    pub fn remediation_prompt(reasons: &[String]) -> String {
        format!(
            "Your previous reply was rejected for these reasons:\n{}\n\nReply with \
             the corrected final object only.",
            reasons
                .iter()
                .map(|reason| format!("- {reason}"))
                .collect::<Vec<_>>()
                .join("\n")
        )
    }
}

impl Executor for ModelExecutor {
    fn class(&self) -> ExecutorClass {
        ExecutorClass::Model
    }

    fn run(&self, spec: &AttemptSpec, context: &RunContext) -> AttemptReceipt {
        let Some(route) = spec.grant.model_route.clone() else {
            return failed(spec, context, "the grant carries no model route");
        };
        let inputs = spec
            .inputs
            .iter()
            .map(|input| input.to_string())
            .collect::<Vec<_>>();
        let mut total_usage = Usage {
            duration_ms: duration_of(spec, context),
            ..Usage::default()
        };
        let mut turn_texts: Vec<String> = Vec::new();
        let mut last_rejection: Option<String> = None;

        for _turn in 0..=MODEL_REMEDIATION_TURNS {
            if context.signal.is_cancelled() {
                return failed(spec, context, "canceled before a final object was produced");
            }
            if let Err(exhausted) = context.budget.record_attempt() {
                return failed(spec, context, &exhausted.to_string());
            }
            let prompt = match &last_rejection {
                None => Self::opening_prompt(&spec.objective, &inputs),
                Some(reason) => Self::remediation_prompt(std::slice::from_ref(reason)),
            };
            let request = ModelTurnRequest {
                system: Some(Self::system_prompt()),
                messages: vec![relay_providers::contract::ModelMessage::user_text(prompt)],
                tools: None,
                max_output_tokens: spec.grant.budget.max_output_tokens,
                temperature: None,
            };
            let response = match context.model.complete(&route, &request, &context.signal) {
                Ok(response) => response,
                Err(error) => return failed(spec, context, &error.to_string()),
            };
            total_usage.input_tokens = total_usage
                .input_tokens
                .saturating_add(response.input_tokens);
            total_usage.output_tokens = total_usage
                .output_tokens
                .saturating_add(response.output_tokens);
            turn_texts.push(head_of(&response.text(), 20_000));
            if let Err(exhausted) = context
                .budget
                .record_model_usage(response.input_tokens, response.output_tokens)
            {
                return receipt(
                    spec,
                    context,
                    AttemptOutcome::Failed {
                        reason: exhausted.to_string(),
                    },
                    Vec::new(),
                    total_usage,
                );
            }
            // Strict final-object decode, shared with the guided planner.
            // A turn carrying native tool calls is rejected outright —
            // model-class work is tool-free, so nothing is ever executed
            // and the model is asked for the final object again.
            let turn_value: Result<serde_json::Map<String, Value>, String> =
                match serde_json::from_str::<Value>(response.text().trim()) {
                    Ok(Value::Object(map)) => Ok(map),
                    Ok(_) => Err("the reply was JSON but not an object".to_owned()),
                    Err(error) => Err(error.to_string()),
                };
            let mut session = crate::planner::ToolSession::default();
            if !response.tool_calls().is_empty() {
                session.failures.push(
                    "model-class work has no tools; a tool call was requested but none are \
                     available; reply with the final object only"
                        .to_owned(),
                );
            } else {
                match turn_value {
                    Ok(map) => {
                        crate::planner::extract_final(
                            spec,
                            context,
                            &Value::Object(map),
                            &crate::planner::Guidance::model_work(),
                            &mut session,
                        );
                    }
                    Err(error) => {
                        session
                            .failures
                            .push(crate::planner::malformed_final_note(&error).to_owned());
                    }
                }
            }
            if session.failures.is_empty() {
                return model_receipt(
                    spec,
                    context,
                    &route,
                    session,
                    turn_texts,
                    total_usage,
                    true,
                );
            }
            last_rejection = Some(session.failures.join("; "));
        }
        let reason = last_rejection.unwrap_or_else(|| "no final object was produced".to_owned());
        // Every remediation turn failed: account for the turns and fail
        // with the actionable reasons.
        let mut session = crate::planner::ToolSession::default();
        session.failures.push(reason);
        model_receipt(
            spec,
            context,
            &route,
            session,
            turn_texts,
            total_usage,
            false,
        )
    }
}

/// Assemble the model executor's receipt from a decoded session: the
/// turns are inspection evidence (usage accounting), the decoded sections
/// become outputs/evidence/graph_changes on completion.
fn model_receipt(
    spec: &AttemptSpec,
    context: &RunContext,
    route: &relay_domain::ModelRoute,
    session: crate::planner::ToolSession,
    turn_texts: Vec<String>,
    usage: Usage,
    succeeded: bool,
) -> AttemptReceipt {
    let mut evidence = vec![Evidence {
        id: evidence_id(spec, "model"),
        kind: EvidenceKind::Inspection,
        target: EvidenceTarget::WorkItem(spec.work.clone()),
        outcome: EvidenceOutcome::Info,
        summary: format!(
            "model session via {}/{} took {} turn(s) producing {} chars",
            route.provider,
            route.model,
            turn_texts.len(),
            turn_texts
                .iter()
                .map(|text| text.chars().count())
                .sum::<usize>(),
        ),
        payload: json!({
            "route": { "provider": route.provider, "model": route.model },
            "turns": turn_texts,
            "usage": {
                "input_tokens": usage.input_tokens,
                "output_tokens": usage.output_tokens,
            },
        }),
        recorded_at_ms: context.now_ms,
    }];
    let outcome = if succeeded {
        AttemptOutcome::Completed
    } else {
        AttemptOutcome::Failed {
            reason: session.failures.join("; "),
        }
    };
    evidence.extend(session.evidence);
    let mut accounted = receipt(spec, context, outcome, evidence, usage);
    if succeeded {
        accounted.outputs = session.outputs;
        accounted.graph_changes = session.graph_changes;
    }
    accounted
}

/// The deterministic outcome a system job produces: outputs, proposals,
/// and evidence — never direct mutations.
#[derive(Debug, Clone, Default)]
pub struct SystemOutcome {
    /// Artifact versions to propose (for example a rendered delivery
    /// report assembled from the read-only graph snapshot). Digests are
    /// computed through the shared canonical definition.
    pub outputs: Vec<OutputProposal>,
    /// Graph changes to propose (validated separately from acceptance).
    pub proposals: Vec<GraphChangeProposal>,
    /// Evidence to record.
    pub evidence: Vec<Evidence>,
    /// An optional note used in the failure reason when the job produced
    /// nothing account-worthy.
    pub note: Option<String>,
}

/// Deterministic system work: rendering, projection, invalidation
/// proposals. System executors propose; policy validates.
pub trait SystemJob: Send + Sync {
    /// Compute the job's outcome for this attempt.
    fn run(&self, spec: &AttemptSpec, context: &RunContext) -> SystemOutcome;
}

/// The system executor: runs a deterministic job and returns its proposals
/// and evidence as receipt data.
pub struct SystemExecutor {
    job: Arc<dyn SystemJob>,
}

impl SystemExecutor {
    /// Use a custom job.
    pub fn new(job: Arc<dyn SystemJob>) -> Self {
        SystemExecutor { job }
    }
}

impl Executor for SystemExecutor {
    fn class(&self) -> ExecutorClass {
        ExecutorClass::System
    }

    fn run(&self, spec: &AttemptSpec, context: &RunContext) -> AttemptReceipt {
        let outcome = self.job.run(spec, context);
        if outcome.outputs.is_empty() && outcome.proposals.is_empty() && outcome.evidence.is_empty()
        {
            return failed(
                spec,
                context,
                &outcome
                    .note
                    .unwrap_or_else(|| "system job produced nothing".to_owned()),
            );
        }
        let mut accounted = receipt(
            spec,
            context,
            AttemptOutcome::Completed,
            outcome.evidence,
            Usage {
                duration_ms: duration_of(spec, context),
                ..Usage::default()
            },
        );
        // Outputs and proposals ride inside the receipt; acceptance
        // assigns versions and passes proposals through for separate
        // validation — never installing them here.
        accounted.outputs = outcome.outputs;
        accounted.graph_changes = outcome.proposals;
        accounted
    }
}

struct FixedPlanner {
    calls: Vec<ToolCallRequest>,
}

impl WorkspacePlanner for FixedPlanner {
    fn plan(&self, _spec: &AttemptSpec, _context: &RunContext) -> Vec<ToolCallRequest> {
        self.calls.clone()
    }
}

struct FixedVerifierPlanner {
    calls: Vec<ToolCallRequest>,
}

impl VerifierPlanner for FixedVerifierPlanner {
    fn plan(&self, _spec: &AttemptSpec, _context: &RunContext) -> Vec<ToolCallRequest> {
        self.calls.clone()
    }
}

/// Assemble a receipt with the common identity and timing fields.
fn receipt(
    spec: &AttemptSpec,
    context: &RunContext,
    outcome: AttemptOutcome,
    evidence: Vec<Evidence>,
    usage: Usage,
) -> AttemptReceipt {
    AttemptReceipt {
        attempt: spec.attempt.clone(),
        work: spec.work.clone(),
        outcome,
        completed_at_ms: context.now_ms,
        outputs: Vec::new(),
        evidence,
        graph_changes: Vec::new(),
        usage,
    }
}

/// A failed receipt with no outputs (as acceptance requires).
fn failed(spec: &AttemptSpec, context: &RunContext, reason: &str) -> AttemptReceipt {
    receipt(
        spec,
        context,
        AttemptOutcome::Failed {
            reason: reason.to_owned(),
        },
        Vec::new(),
        Usage {
            duration_ms: duration_of(spec, context),
            ..Usage::default()
        },
    )
}

fn duration_of(spec: &AttemptSpec, context: &RunContext) -> i64 {
    (context.now_ms - spec.started_at_ms).max(0)
}

/// The first `limit` characters of a text, for bounded payloads.
fn head_of(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
}

/// A fresh evidence id derived from the attempt id, so receipts never
/// collide evidence ids within or across attempts.
#[allow(clippy::panic)] // scoped: the fallback id is a constant that always parses
fn evidence_id(spec: &AttemptSpec, tag: &str) -> EvidenceId {
    let slug = spec
        .attempt
        .as_str()
        .strip_prefix("att_")
        .unwrap_or("attempt");
    match EvidenceId::parse(&format!("ev_{slug}-{tag}")) {
        Ok(id) => id,
        // Slug characters are id characters and tags are fixed literals,
        // so this arm is unreachable in practice.
        Err(_) => EvidenceId::parse("ev_0").unwrap_or_else(|_| {
            panic!("static fallback evidence id failed to parse");
        }),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use relay_domain::ArtifactContent;

    use crate::BudgetGuard;
    use crate::model::{ModelClient, ModelClientError, ModelTurnRequest, ModelTurnResponse};
    use crate::spec::ManualClock;
    use relay_domain::{
        ArtifactVersionRef, Budget, CapabilityGrant, GrantId, ModelRoute, ScopePath,
    };
    use relay_providers::contract::CancelToken;
    use relay_tools::create_default_tool_registry;
    use std::path::PathBuf;
    use std::sync::Arc;

    /// Always returns the same final-object text.
    struct StructuredModel {
        text: String,
    }

    impl ModelClient for StructuredModel {
        fn complete(
            &self,
            _route: &ModelRoute,
            _request: &ModelTurnRequest,
            _signal: &CancelToken,
        ) -> Result<ModelTurnResponse, ModelClientError> {
            Ok(ModelTurnResponse {
                input_tokens: 3,
                output_tokens: 5,
                ..ModelTurnResponse::from_text(self.text.clone())
            })
        }
    }

    /// Returns a malformed first reply, then the corrected final object
    /// (remediation turns are prompted with the rejection reasons).
    struct RemediationModel {
        first: String,
        second: String,
    }

    impl ModelClient for RemediationModel {
        fn complete(
            &self,
            _route: &ModelRoute,
            request: &ModelTurnRequest,
            _signal: &CancelToken,
        ) -> Result<ModelTurnResponse, ModelClientError> {
            Ok(ModelTurnResponse {
                input_tokens: 3,
                output_tokens: 5,
                ..ModelTurnResponse::from_text(if request.last_user_text().contains("rejected") {
                    self.second.clone()
                } else {
                    self.first.clone()
                })
            })
        }
    }

    fn spec(grant: CapabilityGrant) -> AttemptSpec {
        AttemptSpec {
            attempt: relay_domain::AttemptId::parse("att_1").unwrap(),
            work: relay_domain::WorkItemId::parse("work_model").unwrap(),
            objective: "summarize the plan".into(),
            inputs: vec![ArtifactVersionRef::new(
                relay_domain::ArtifactId::parse("art_brief").unwrap(),
                relay_domain::ArtifactVersion::FIRST,
            )],
            decision: None,
            grant,
            expected_revision: relay_domain::ProjectRevision::default(),
            started_at_ms: 1_000,
        }
    }

    fn context_with(model: Arc<dyn ModelClient>) -> RunContext {
        let clock = Arc::new(ManualClock::new(1_050));
        RunContext {
            workspace_root: PathBuf::from("/tmp/relay-executor-test"),
            signal: CancelToken::new(),
            model,
            tools: Arc::new(create_default_tool_registry(&Default::default()).unwrap()),
            budget: Arc::new(BudgetGuard::new(
                Budget::default(),
                1_000,
                Arc::clone(&clock).into_clock(),
                CancelToken::new(),
            )),
            graph: Arc::new(relay_domain::ProjectGraph::default),
            now_ms: 1_050,
        }
    }

    fn grant(adjust: impl FnOnce(&mut CapabilityGrant)) -> CapabilityGrant {
        let mut value = CapabilityGrant {
            id: GrantId::parse("grant_1").unwrap(),
            read_paths: vec![ScopePath::root()],
            write_paths: Vec::new(),
            operations: vec![relay_domain::Operation::Read],
            command_allowlist: Vec::new(),
            environment_names: Vec::new(),
            model_route: Some(ModelRoute {
                provider: "fake".into(),
                model: "fake-standard".into(),
                role: None,
            }),
            budget: Budget::default(),
            delivery: relay_domain::DeliveryAuthority::None,
        };
        adjust(&mut value);
        value
    }

    #[test]
    fn model_executor_extracts_structured_outputs_strictly() {
        let model = StructuredModel {
            text: r#"{"outputs":[{"artifact":"art_brief","title":"Brief","content":{"summary":"the plan"}}],"evidence":[],"graph_changes":[]}"#
                .to_owned(),
        };
        let spec = spec(grant(|_| {}));
        let receipt = ModelExecutor.run(&spec, &context_with(Arc::new(model)));
        assert!(
            matches!(receipt.outcome, AttemptOutcome::Completed),
            "{:?}",
            receipt.outcome
        );
        assert_eq!(receipt.outputs.len(), 1);
        assert_eq!(receipt.outputs[0].artifact.as_str(), "art_brief");
        assert_eq!(receipt.outputs[0].digest.as_str().len(), 64);
        assert_eq!(receipt.evidence[0].kind, EvidenceKind::Inspection);
        assert_eq!(receipt.usage.input_tokens, 3);
        assert_eq!(receipt.usage.output_tokens, 5);
    }

    #[test]
    fn model_executor_fails_plain_text_with_actionable_reasons() {
        let model = StructuredModel {
            text: "just some prose, no JSON".to_owned(),
        };
        let spec = spec(grant(|_| {}));
        let receipt = ModelExecutor.run(&spec, &context_with(Arc::new(model)));
        match receipt.outcome {
            AttemptOutcome::Failed { reason } => {
                assert!(
                    reason.contains("was not the final JSON object")
                        || reason.contains("final object"),
                    "{reason}"
                );
            }
            AttemptOutcome::Completed => panic!("plain prose must not complete"),
        }
        // Initial turn plus every remediation turn is accounted.
        assert_eq!(
            receipt.usage.input_tokens,
            3 * u64::from(MODEL_REMEDIATION_TURNS + 1)
        );
    }

    #[test]
    fn model_executor_recovers_after_one_malformed_turn() {
        let model = RemediationModel {
            first: "not json".to_owned(),
            second: r#"{"evidence":[{"kind":"inspection","target":{"work_item":"work_model"},"outcome":"info","summary":"recovered"}]}"#
                .to_owned(),
        };
        let spec = spec(grant(|_| {}));
        let receipt = ModelExecutor.run(&spec, &context_with(Arc::new(model)));
        assert!(
            matches!(receipt.outcome, AttemptOutcome::Completed),
            "{:?}",
            receipt.outcome
        );
        assert!(
            receipt
                .evidence
                .iter()
                .any(|evidence| evidence.summary.contains("recovered"))
        );
    }

    /// Always requests a native tool call (never a valid final object).
    struct ToolCallingModel;

    impl ModelClient for ToolCallingModel {
        fn complete(
            &self,
            _route: &ModelRoute,
            _request: &ModelTurnRequest,
            _signal: &CancelToken,
        ) -> Result<ModelTurnResponse, ModelClientError> {
            Ok(ModelTurnResponse {
                input_tokens: 3,
                output_tokens: 5,
                blocks: vec![relay_providers::contract::AssistantContent::ToolCall {
                    call: relay_providers::contract::ToolCall {
                        id: "c1".into(),
                        name: "read-file".into(),
                        arguments: serde_json::json!({ "path": "src/lib.rs" }),
                    },
                }],
                routes_attempted: Vec::new(),
            })
        }
    }

    #[test]
    fn model_executor_rejects_native_tool_calls_without_executing_them() {
        let spec = spec(grant(|_| {}));
        let receipt = ModelExecutor.run(&spec, &context_with(Arc::new(ToolCallingModel)));
        match receipt.outcome {
            AttemptOutcome::Failed { reason } => {
                assert!(reason.contains("has no tools"), "{reason}");
                assert!(reason.contains("final object"), "{reason}");
            }
            AttemptOutcome::Completed => panic!("a tool-calling model must not complete"),
        }
        // Nothing executed (there is nothing to execute), and every
        // remediation turn was accounted.
        assert_eq!(receipt.usage.tool_calls, 0);
        assert_eq!(
            receipt.usage.input_tokens,
            3 * u64::from(MODEL_REMEDIATION_TURNS + 1)
        );
        assert_eq!(receipt.evidence.len(), 1, "no verifier records were minted");
    }

    #[test]
    fn model_executor_recovers_after_a_rejected_tool_call() {
        struct ToolThenFinalModel;
        impl ModelClient for ToolThenFinalModel {
            fn complete(
                &self,
                _route: &ModelRoute,
                request: &ModelTurnRequest,
                _signal: &CancelToken,
            ) -> Result<ModelTurnResponse, ModelClientError> {
                if request.last_user_text().contains("rejected") {
                    Ok(ModelTurnResponse {
                        input_tokens: 3,
                        output_tokens: 5,
                        ..ModelTurnResponse::from_text(
                            r#"{"evidence":[{"kind":"inspection","target":{"work_item":"work_model"},"outcome":"info","summary":"recovered"}]}"#,
                        )
                    })
                } else {
                    Ok(ModelTurnResponse {
                        input_tokens: 3,
                        output_tokens: 5,
                        blocks: vec![relay_providers::contract::AssistantContent::ToolCall {
                            call: relay_providers::contract::ToolCall {
                                id: "c1".into(),
                                name: "write-file".into(),
                                arguments: serde_json::json!({
                                    "path": "src/evil.rs",
                                    "content": "nope"
                                }),
                            },
                        }],
                        routes_attempted: Vec::new(),
                    })
                }
            }
        }
        let spec = spec(grant(|_| {}));
        let context = context_with(Arc::new(ToolThenFinalModel));
        let workspace = context.workspace_root.clone();
        let receipt = ModelExecutor.run(&spec, &context);
        assert!(
            matches!(receipt.outcome, AttemptOutcome::Completed),
            "{:?}",
            receipt.outcome
        );
        // The rejected call never executed.
        assert!(!workspace.join("src/evil.rs").exists());
        assert!(
            receipt
                .evidence
                .iter()
                .any(|evidence| evidence.summary.contains("recovered"))
        );
    }

    #[test]
    fn model_executor_fails_without_a_route() {
        let spec = spec(grant(|value| value.model_route = None));
        let receipt = ModelExecutor.run(
            &spec,
            &context_with(Arc::new(StructuredModel {
                text: String::new(),
            })),
        );
        assert!(matches!(receipt.outcome, AttemptOutcome::Failed { .. }));
    }

    #[test]
    fn human_executor_names_the_awaited_decision() {
        let mut spec = spec(grant(|_| {}));
        spec.decision = Some(relay_domain::DecisionId::parse("dec_approve").unwrap());
        let receipt = HumanExecutor.run(
            &spec,
            &context_with(Arc::new(StructuredModel {
                text: String::new(),
            })),
        );
        match &receipt.outcome {
            AttemptOutcome::Failed { reason } => {
                assert!(reason.contains("dec_approve"), "{reason}");
            }
            AttemptOutcome::Completed => panic!("human attempts do not self-complete"),
        }
        assert_eq!(receipt.evidence.len(), 1);
        assert_eq!(receipt.evidence[0].kind, EvidenceKind::HumanStatement);
    }

    /// A deterministic assembly job: renders a delivery-style document
    /// from the read-only graph snapshot and proposes it as an output.
    struct DeliveryDocJob;

    impl SystemJob for DeliveryDocJob {
        fn run(&self, _spec: &AttemptSpec, context: &RunContext) -> SystemOutcome {
            let graph = (context.graph)();
            let works: Vec<String> = graph
                .works()
                .map(|(id, _)| id.as_str().to_owned())
                .collect();
            let evidence = graph.evidence().count();
            let content = ArtifactContent::new(json!({
                "document": "delivery-report",
                "work_items": works,
                "recorded_evidence": evidence,
            }));
            let digest = crate::planner::digest_of(&content).ok();
            SystemOutcome {
                outputs: vec![relay_domain::OutputProposal {
                    artifact: relay_domain::ArtifactId::parse("art_delivery-doc").unwrap(),
                    title: Some("Delivery report".to_owned()),
                    digest: digest.unwrap(),
                    content,
                }],
                proposals: Vec::new(),
                evidence: Vec::new(),
                note: None,
            }
        }
    }

    #[test]
    fn system_jobs_read_the_graph_snapshot_and_propose_outputs() {
        // The graph view the context carries: a snapshot with one declared
        // work item and no evidence, taken before the job runs.
        let mut facts = vec![
            relay_domain::DomainFact::ProjectCreated {
                project: relay_domain::ProjectId::parse("prj_sys").unwrap(),
            },
            relay_domain::DomainFact::GoalDeclared {
                goal: relay_domain::Goal {
                    id: relay_domain::GoalId::parse("goal_sys").unwrap(),
                    outcome: "assemble the delivery report".into(),
                    constraints: Vec::new(),
                    success_definition: "the doc exists".into(),
                    provenance: relay_domain::Provenance {
                        source: relay_domain::ProvenanceSource::User,
                        statement: None,
                        at_ms: 0,
                    },
                    declared_at_ms: 0,
                },
            },
            relay_domain::DomainFact::WorkDeclared {
                work: relay_domain::WorkItem::new(
                    relay_domain::WorkItemId::parse("work_delivery").unwrap(),
                    relay_domain::WorkKind::parse("delivery-prep").unwrap(),
                    ExecutorClass::System,
                    "assemble the delivery report from evidence",
                    relay_domain::CompletionRule::Receipt,
                ),
            },
        ];
        let graph = relay_domain::ProjectGraph::from_facts(facts.iter()).expect("folds");
        facts.clear();
        let snapshot = Arc::new(move || graph.clone());

        let mut spec = spec(grant(|_| {}));
        spec.work = relay_domain::WorkItemId::parse("work_delivery").unwrap();
        let context = RunContext {
            graph: snapshot,
            ..context_with(Arc::new(StructuredModel {
                text: String::new(),
            }))
        };
        let receipt = SystemExecutor::new(Arc::new(DeliveryDocJob)).run(&spec, &context);
        assert!(
            matches!(receipt.outcome, AttemptOutcome::Completed),
            "{:?}",
            receipt.outcome
        );
        assert_eq!(receipt.outputs.len(), 1);
        let output = &receipt.outputs[0];
        assert_eq!(output.artifact.as_str(), "art_delivery-doc");
        assert_eq!(output.digest.as_str().len(), 64);
        assert_eq!(
            output.content.0["work_items"],
            json!(["work_delivery"]),
            "the output reflects the graph snapshot, not live state"
        );
    }

    #[test]
    fn executor_sets_register_one_executor_per_class() {
        let set = ExecutorSet::new()
            .with(Arc::new(ModelExecutor))
            .with(Arc::new(HumanExecutor));
        // Registration order is preserved; replacing a class keeps its slot.
        assert_eq!(
            set.classes(),
            vec![ExecutorClass::Model, ExecutorClass::Human]
        );
        assert!(set.get(ExecutorClass::Workspace).is_none());
        let set = set.with(Arc::new(HumanExecutor));
        assert_eq!(set.classes().len(), 2);
    }
}
