//! Tool-session planning: the shared execution kernel and the
//! model-guided planners.
//!
//! [`ToolRunner`] is the single execution kernel every planner shares:
//! each call is checked against the attempt's grant ([`GrantGate`]), the
//! budget, and the cancellation signal before the tool body runs. Scripted
//! planners produce calls up front; the model-guided planners
//! ([`ModelGuidedWorkspacePlanner`], [`ModelGuidedVerifierPlanner`]) drive
//! a real model/tool loop — model turns see only the grant-visible tool
//! specs, requested calls execute through the same kernel, iterations stay
//! bounded by the budget and a turn cap, and the final turn's structured
//! proposals ([`OutputProposal`], [`Evidence`],
//! [`GraphChangeProposal`]) are schema-validated. Malformed model output
//! becomes a `Failed` receipt with actionable reasons — never silently
//! coerced.
//!
//! Prompts are plain-data builder methods on the planners (strings in,
//! strings out); no provider type appears anywhere in this module.

use std::sync::Arc;

use relay_artifacts::content_digest as canonical_content_digest;
use relay_domain::{
    ArtifactContent, ArtifactDigest, ArtifactId, ArtifactVersionRef, Evidence, EvidenceId,
    EvidenceKind, EvidenceOutcome, EvidenceTarget, GraphChangeProposal, OutputProposal,
};
use relay_tools::GrantGate;
use serde::Deserialize;
use serde_json::Value;

use crate::budget::BudgetGuard;
use crate::model::ModelTurnRequest;
use crate::spec::{AttemptSpec, RunContext};

/// One executed tool call, as recorded for the receipt.
#[derive(Debug, Clone, PartialEq)]
pub struct CallRecord {
    /// The tool that was called.
    pub tool: String,
    /// The arguments that were passed.
    pub args: Value,
    /// Whether the call succeeded.
    pub ok: bool,
    /// The head of the (already redacted and bounded) tool output.
    pub output_head: String,
    /// Workspace-relative files the call changed.
    pub affected_files: Vec<String>,
    /// Whether the call was skipped because the attempt was canceled or
    /// the budget fired.
    pub canceled: bool,
    /// Whether the grant denied the call before the tool body ran.
    pub denied_by_grant: bool,
}

/// The shared execution kernel: grant gate + budget + cancellation around
/// every call, with records for the receipt.
pub struct ToolRunner {
    gate: GrantGate,
    tool_context: relay_tools::ToolContext,
    tools: Arc<relay_tools::ToolRegistry>,
    budget: Arc<BudgetGuard>,
    signal: relay_providers::contract::CancelToken,
    records: Vec<CallRecord>,
}

impl ToolRunner {
    /// Arm the kernel for one attempt.
    pub fn new(spec: &AttemptSpec, context: &RunContext) -> Self {
        ToolRunner {
            gate: GrantGate::new(spec.grant.clone()),
            tool_context: relay_tools::ToolContext {
                workspace_root: context.workspace_root.clone(),
                signal: context.signal.clone(),
                log: Arc::new(|_message| {}),
            },
            tools: Arc::clone(&context.tools),
            budget: Arc::clone(&context.budget),
            signal: context.signal.clone(),
            records: Vec::new(),
        }
    }

    /// Execute one call through every enforcement layer, recording it.
    pub fn run(&mut self, call: ToolCallRequest) -> CallRecord {
        if self.signal.is_cancelled() {
            let record = CallRecord {
                tool: call.tool,
                args: call.args,
                ok: false,
                output_head: "canceled before the call ran".to_owned(),
                affected_files: Vec::new(),
                canceled: true,
                denied_by_grant: false,
            };
            self.records.push(record.clone());
            return record;
        }
        if let Err(exhausted) = self.budget.record_attempt() {
            let record = CallRecord {
                tool: call.tool,
                args: call.args,
                ok: false,
                output_head: format!("budget stopped the batch: {exhausted}"),
                affected_files: Vec::new(),
                canceled: true,
                denied_by_grant: false,
            };
            self.records.push(record.clone());
            return record;
        }
        let result = self
            .gate
            .call(&self.tools, &call.tool, &call.args, &self.tool_context);
        let record = CallRecord {
            denied_by_grant: result.output.starts_with("denied by capability grant"),
            tool: call.tool,
            args: call.args,
            ok: result.ok,
            output_head: head_of(&result.output, 400),
            affected_files: result.affected_files,
            canceled: false,
        };
        self.records.push(record.clone());
        record
    }

    /// Execute a scripted batch.
    pub fn run_all(&mut self, calls: Vec<ToolCallRequest>) -> Vec<CallRecord> {
        for call in calls {
            self.run(call);
        }
        self.records.clone()
    }

    /// The grant-visible tool surface as JSON for model prompts:
    /// name, description, and parameters schema — nothing else.
    pub fn visible_tool_specs(&self) -> String {
        let specs: Vec<Value> = self
            .gate
            .visible_tools(&self.tools)
            .iter()
            .map(|tool| {
                serde_json::json!({
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": serde_json::Value::Object(
                        relay_tools::tool_json_schema(tool)
                    ),
                })
            })
            .collect();
        serde_json::to_string(&specs).unwrap_or_else(|_| "[]".to_owned())
    }

    /// The records collected so far.
    pub fn records(&self) -> &[CallRecord] {
        &self.records
    }
}

/// What a planner produced: every executed call plus the structured
/// proposals extracted from the final model turn (model-guided planners).
#[derive(Debug, Clone, Default)]
pub struct ToolSession {
    /// Every executed call, in order.
    pub records: Vec<CallRecord>,
    /// Artifact versions the final turn proposed to publish.
    pub outputs: Vec<OutputProposal>,
    /// Evidence the final turn proposed to record.
    pub evidence: Vec<Evidence>,
    /// Graph changes the final turn proposed (validated separately from
    /// acceptance, never installed by execution).
    pub graph_changes: Vec<GraphChangeProposal>,
    /// Actionable failure reasons; any entry fails the attempt.
    pub failures: Vec<String>,
    /// Whether the session stopped because the attempt was canceled or the
    /// budget fired.
    pub stopped_canceled: bool,
    /// Human-readable notes that do not fail the attempt.
    pub notes: Vec<String>,
}

impl ToolSession {
    /// A session that only ran calls.
    pub fn from_records(records: Vec<CallRecord>) -> Self {
        ToolSession {
            records,
            ..ToolSession::default()
        }
    }

    /// Every file any call changed.
    pub fn affected_files(&self) -> Vec<String> {
        self.records
            .iter()
            .flat_map(|record| record.affected_files.iter().cloned())
            .collect()
    }

    /// Whether anything account-worthy happened.
    pub fn is_empty_accounting(&self) -> bool {
        self.records.is_empty()
            && self.outputs.is_empty()
            && self.evidence.is_empty()
            && self.graph_changes.is_empty()
    }
}

/// One requested tool invocation (see [`crate::executors`]).
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCallRequest {
    /// The tool name.
    pub tool: String,
    /// The arguments, validated by the tool schema and permitted by the
    /// grant gate before execution.
    pub args: Value,
}

/// Knobs for the model-guided loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelGuidedConfig {
    /// Maximum model turns per attempt (bounding the loop independent of
    /// the budget).
    pub max_turns: u32,
}

impl Default for ModelGuidedConfig {
    fn default() -> Self {
        ModelGuidedConfig { max_turns: 8 }
    }
}

/// The model-guided workspace planner: a real model-driven tool loop
/// behind the [`WorkspacePlanner`](crate::executors::WorkspacePlanner)
/// port. Select it through `ExecutorSet` construction
/// (`WorkspaceExecutor::new(Arc::new(ModelGuidedWorkspacePlanner::new()))`).
pub struct ModelGuidedWorkspacePlanner {
    config: ModelGuidedConfig,
}

impl ModelGuidedWorkspacePlanner {
    /// Default-bounded planner.
    pub fn new() -> Self {
        ModelGuidedWorkspacePlanner {
            config: ModelGuidedConfig::default(),
        }
    }

    /// A planner with explicit bounds.
    pub fn with_config(config: ModelGuidedConfig) -> Self {
        ModelGuidedWorkspacePlanner { config }
    }

    /// The loop bounds.
    pub fn config(&self) -> &ModelGuidedConfig {
        &self.config
    }

    /// The system prompt for workspace sessions (plain data).
    pub fn system_prompt(&self) -> String {
        workspace_system_prompt()
    }

    /// The opening turn prompt: objective, inputs, and the visible tool
    /// surface (plain data).
    pub fn opening_prompt(&self, objective: &str, inputs: &[String], tools: &str) -> String {
        opening_prompt(objective, inputs, tools)
    }

    /// A follow-up turn prompt carrying the observations so far (plain
    /// data).
    pub fn observation_prompt(&self, observations: &[String], tools: &str) -> String {
        observation_prompt(observations, tools)
    }
}

impl Default for ModelGuidedWorkspacePlanner {
    fn default() -> Self {
        Self::new()
    }
}

impl crate::executors::WorkspacePlanner for ModelGuidedWorkspacePlanner {
    fn plan(&self, _spec: &AttemptSpec, _context: &RunContext) -> Vec<ToolCallRequest> {
        // The scripted surface is unused; the loop lives in `drive`.
        Vec::new()
    }

    fn drive(
        &self,
        spec: &AttemptSpec,
        context: &RunContext,
        runner: &mut ToolRunner,
    ) -> ToolSession {
        guided_session(spec, context, runner, self.config, Guidance::Workspace)
    }
}

/// The model-guided verifier planner: read/execute-only evidence
/// collection per criterion (test-run style) behind the
/// [`VerifierPlanner`](crate::executors::VerifierPlanner) port.
pub struct ModelGuidedVerifierPlanner {
    criterion: Option<ArtifactVersionRef>,
    config: ModelGuidedConfig,
}

impl ModelGuidedVerifierPlanner {
    /// A planner that attributes evidence to the attempted work item.
    pub fn new() -> Self {
        ModelGuidedVerifierPlanner {
            criterion: None,
            config: ModelGuidedConfig::default(),
        }
    }

    /// A planner that collects evidence for one exact criterion version.
    pub fn for_criterion(criterion: ArtifactVersionRef) -> Self {
        ModelGuidedVerifierPlanner {
            criterion: Some(criterion),
            config: ModelGuidedConfig::default(),
        }
    }

    /// A planner with explicit bounds.
    pub fn with_config(config: ModelGuidedConfig) -> Self {
        ModelGuidedVerifierPlanner {
            criterion: None,
            config,
        }
    }

    /// The criterion being verified, when pinned.
    pub fn criterion(&self) -> Option<&ArtifactVersionRef> {
        self.criterion.as_ref()
    }

    /// The system prompt for verification sessions (plain data).
    pub fn system_prompt(&self) -> String {
        match &self.criterion {
            Some(criterion) => format!(
                "You are a deterministic verifier. Collect evidence for criterion \
                 {criterion} using only the provided tools, then stop with the final \
                 object. Attribute every evidence entry to criterion {criterion} \
                 (artifact_version target) or to the work item; outcome must be \
                 \"pass\" or \"fail\".\n\n{}",
                protocol_contract()
            ),
            None => format!(
                "You are a deterministic verifier. Collect evidence for the work item \
                 using only the provided tools, then stop with the final object.\n\n{}",
                protocol_contract()
            ),
        }
    }

    /// The opening turn prompt (plain data).
    pub fn opening_prompt(&self, objective: &str, inputs: &[String], tools: &str) -> String {
        opening_prompt(objective, inputs, tools)
    }

    /// A follow-up turn prompt carrying observations (plain data).
    pub fn observation_prompt(&self, observations: &[String], tools: &str) -> String {
        observation_prompt(observations, tools)
    }
}

impl Default for ModelGuidedVerifierPlanner {
    fn default() -> Self {
        Self::new()
    }
}

impl crate::executors::VerifierPlanner for ModelGuidedVerifierPlanner {
    fn plan(&self, _spec: &AttemptSpec, _context: &RunContext) -> Vec<ToolCallRequest> {
        Vec::new()
    }

    fn drive(
        &self,
        spec: &AttemptSpec,
        context: &RunContext,
        runner: &mut ToolRunner,
    ) -> ToolSession {
        guided_session(
            spec,
            context,
            runner,
            self.config,
            Guidance::Verifier(self.criterion.clone()),
        )
    }
}

/// Which prompts and final-extraction rules apply.
pub(crate) enum Guidance {
    /// Workspace sessions may propose outputs.
    Workspace,
    /// Model-class work: the same extraction rules as workspace (outputs
    /// allowed, no criterion); the model executor supplies its own
    /// prompts rather than using the guidance system prompt.
    Model,
    /// Verifier sessions collect evidence, optionally for one criterion.
    Verifier(Option<ArtifactVersionRef>),
}

impl Guidance {
    /// Model-class work: outputs allowed (briefs, designs, research
    /// reports), no criterion targeting.
    pub(crate) fn model_work() -> Self {
        Guidance::Model
    }

    fn system_prompt(&self) -> String {
        match self {
            Guidance::Workspace | Guidance::Model => workspace_system_prompt(),
            Guidance::Verifier(criterion) => ModelGuidedVerifierPlanner {
                criterion: criterion.clone(),
                config: ModelGuidedConfig::default(),
            }
            .system_prompt(),
        }
    }

    fn allows_outputs(&self) -> bool {
        matches!(self, Guidance::Workspace | Guidance::Model)
    }

    fn criterion(&self) -> Option<&ArtifactVersionRef> {
        match self {
            Guidance::Workspace | Guidance::Model | Guidance::Verifier(None) => None,
            Guidance::Verifier(Some(criterion)) => Some(criterion),
        }
    }
}

/// The workspace session system prompt: the bounded-grant preamble plus the
/// shared turn protocol.
fn workspace_system_prompt() -> String {
    format!(
        "You are a careful workspace agent working inside a bounded capability \
         grant. Use only the provided tools; every call is checked against the \
         grant before it runs. Work step by step, then stop with the final \
         object.\n\n{}",
        protocol_contract()
    )
}

/// The textual turn protocol shared by both planners.
pub(crate) fn protocol_contract() -> String {
    "Reply with EXACTLY ONE JSON object per turn, nothing else:\n\
     - to run tools: {\"tool_calls\":[{\"tool\":\"<name>\",\"args\":{...}}]}\n\
     - to finish: {\"outputs\":[{\"artifact\":\"art_…\",\"title\":\"…\",\"content\":{…}}],\n\
     \x20              \"evidence\":[{\"kind\":\"inspection|test_run|command_output|static_analysis|review|human_statement\",\n\
     \x20                          \"target\":{\"work_item\":\"work_…\"}|{\"artifact_version\":{\"artifact\":\"art_…\",\"version\":1}},\n\
     \x20                          \"outcome\":\"pass|fail|info\",\"summary\":\"…\",\"payload\":{…}}],\n\
     \x20              \"graph_changes\":[…]}\n\
     All three final arrays are optional but at least one entry must exist. \
     Ids must use their exact prefixed forms; malformed entries are rejected."
        .to_owned()
}

fn opening_prompt(objective: &str, inputs: &[String], tools: &str) -> String {
    let inputs = if inputs.is_empty() {
        "none".to_owned()
    } else {
        inputs.join(", ")
    };
    format!(
        "Objective: {objective}\nInputs: {inputs}\n\nTools you may use (JSON):\n{tools}\n\n\
         Begin. Reply with one JSON object per turn."
    )
}

fn observation_prompt(observations: &[String], tools: &str) -> String {
    let observations = if observations.is_empty() {
        "- (none yet)".to_owned()
    } else {
        observations
            .iter()
            .map(|observation| format!("- {observation}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!(
        "Observations so far:\n{observations}\n\nTools you may use (JSON):\n{tools}\n\n\
         Continue. Reply with one JSON object per turn."
    )
}

/// The model-guided loop shared by both planners.
fn guided_session(
    spec: &AttemptSpec,
    context: &RunContext,
    runner: &mut ToolRunner,
    config: ModelGuidedConfig,
    guidance: Guidance,
) -> ToolSession {
    let mut session = ToolSession::default();
    let Some(route) = spec.grant.model_route.clone() else {
        session
            .failures
            .push("the grant carries no model route".to_owned());
        return finalize_session(session, runner);
    };
    let tools = runner.visible_tool_specs();
    let mut observations: Vec<String> = Vec::new();
    for turn in 0..config.max_turns {
        if context.signal.is_cancelled() {
            session.stopped_canceled = true;
            return finalize_session(session, runner);
        }
        if let Err(exhausted) = context.budget.record_attempt() {
            session
                .failures
                .push(format!("budget stopped the session: {exhausted}"));
            session.stopped_canceled = true;
            return finalize_session(session, runner);
        }
        let prompt = if turn == 0 {
            opening_prompt(
                &spec.objective,
                &spec
                    .inputs
                    .iter()
                    .map(|input| input.to_string())
                    .collect::<Vec<_>>(),
                &tools,
            )
        } else {
            observation_prompt(&observations, &tools)
        };
        let request = ModelTurnRequest {
            system: Some(guidance.system_prompt()),
            prompt,
            max_output_tokens: spec.grant.budget.max_output_tokens,
            temperature: None,
        };
        let response = match context.model.complete(&route, &request, &context.signal) {
            Ok(response) => response,
            Err(error) => {
                session.failures.push(error.to_string());
                return finalize_session(session, runner);
            }
        };
        if let Err(exhausted) = context
            .budget
            .record_model_usage(response.input_tokens, response.output_tokens)
        {
            session
                .failures
                .push(format!("budget stopped the session: {exhausted}"));
            session.stopped_canceled = true;
            return finalize_session(session, runner);
        }
        let turn_value: Value = match serde_json::from_str(response.text.trim()) {
            Ok(Value::Object(turn_value)) => Value::Object(turn_value),
            Ok(_) => {
                observations = vec![malformed_turn_note("the reply was JSON but not an object")];
                continue;
            }
            Err(error) => {
                observations = vec![malformed_turn_note(&error.to_string())];
                continue;
            }
        };
        if let Some(calls) = turn_value.get("tool_calls") {
            match decode_tool_calls(calls) {
                Ok(calls) if !calls.is_empty() => {
                    observations = calls
                        .into_iter()
                        .map(|call| {
                            let record = runner.run(call);
                            observation_line(&record)
                        })
                        .collect();
                }
                _ => {
                    observations = vec![malformed_tool_calls_note()];
                }
            }
            continue;
        }
        extract_final(spec, context, &turn_value, &guidance, &mut session);
        return finalize_session(session, runner);
    }
    session
        .failures
        .push(format!("no final answer within {} turns", config.max_turns));
    finalize_session(session, runner)
}

/// Attach the runner's records to the session (single exit path).
fn finalize_session(mut session: ToolSession, runner: &ToolRunner) -> ToolSession {
    session.records = runner.records().to_vec();
    session
}

pub(crate) fn malformed_turn_note(reason: &str) -> String {
    format!(
        "your previous reply was not a valid JSON object ({reason}); reply with \
         either {{\"tool_calls\":[…]}} or the final {{\"outputs\":…,\"evidence\":…,\"graph_changes\":…}} object"
    )
}

fn malformed_tool_calls_note() -> String {
    "tool_calls must be a non-empty array of {{\"tool\":\"<name>\",\"args\":{{…}}}} objects; \
     unknown names and non-object args are rejected"
        .to_owned()
}

fn observation_line(record: &CallRecord) -> String {
    if record.canceled {
        format!("{}: {}", record.tool, record.output_head)
    } else if record.denied_by_grant {
        format!("{}: DENIED BY GRANT: {}", record.tool, record.output_head)
    } else if record.ok {
        format!("{}: ok: {}", record.tool, record.output_head)
    } else {
        format!("{}: failed: {}", record.tool, record.output_head)
    }
}

/// Decode the tool_calls array strictly: known shape only.
fn decode_tool_calls(value: &Value) -> Result<Vec<ToolCallRequest>, String> {
    #[derive(Deserialize)]
    struct Draft {
        tool: String,
        args: Value,
    }
    let drafts: Vec<Draft> =
        serde_json::from_value(value.clone()).map_err(|error| error.to_string())?;
    Ok(drafts
        .into_iter()
        .map(|draft| ToolCallRequest {
            tool: draft.tool,
            args: draft.args,
        })
        .collect())
}

/// An output proposal draft; the digest commitment is computed here
/// because models cannot be trusted to hash their own content.
#[derive(Debug, Clone, Deserialize)]
struct OutputDraft {
    artifact: ArtifactId,
    #[serde(default)]
    title: Option<String>,
    content: ArtifactContent,
}

/// An evidence draft; id and timestamp are minted by the planner.
#[derive(Debug, Clone, Deserialize)]
struct EvidenceDraft {
    kind: EvidenceKind,
    target: EvidenceTarget,
    outcome: EvidenceOutcome,
    summary: String,
    #[serde(default)]
    payload: Value,
}

/// Extract and validate the final turn's structured proposals. Malformed
/// entries become actionable failure reasons; nothing is silently coerced.
pub(crate) fn extract_final(
    spec: &AttemptSpec,
    context: &RunContext,
    turn_value: &Value,
    guidance: &Guidance,
    session: &mut ToolSession,
) {
    let mut malformed: Vec<String> = Vec::new();
    let slug = spec
        .attempt
        .as_str()
        .strip_prefix("att_")
        .unwrap_or("attempt");

    if let Some(outputs) = turn_value.get("outputs") {
        if !guidance.allows_outputs() {
            if !outputs_is_empty(outputs) {
                malformed
                    .push("outputs: verifier sessions cannot publish artifact versions".to_owned());
            }
        } else {
            match serde_json::from_value::<Vec<OutputDraft>>(outputs.clone()) {
                Ok(drafts) => {
                    for draft in drafts {
                        match digest_of(&draft.content) {
                            Ok(digest) => session.outputs.push(OutputProposal {
                                artifact: draft.artifact,
                                title: draft.title,
                                digest,
                                content: draft.content,
                            }),
                            Err(reason) => malformed.push(format!("outputs: {reason}")),
                        }
                    }
                }
                Err(error) => malformed.push(format!("outputs: {error}")),
            }
        }
    }

    if let Some(evidence) = turn_value.get("evidence") {
        match serde_json::from_value::<Vec<EvidenceDraft>>(evidence.clone()) {
            Ok(drafts) => {
                for (index, draft) in drafts.into_iter().enumerate() {
                    if let Some(criterion) = guidance.criterion()
                        && !matches_target(draft.target.clone(), criterion, &spec.work)
                    {
                        malformed.push(format!(
                            "evidence[{index}]: verifier evidence must target criterion \
                             {criterion} or the work item, got {}",
                            target_name(&draft.target)
                        ));
                        continue;
                    }
                    session.evidence.push(Evidence {
                        id: EvidenceId::parse(&format!("ev_{slug}-p{index}"))
                            .unwrap_or_else(|_| fallback_evidence_id()),
                        kind: draft.kind,
                        target: draft.target,
                        outcome: draft.outcome,
                        summary: draft.summary,
                        payload: draft.payload,
                        recorded_at_ms: context.now_ms,
                    });
                }
            }
            Err(error) => malformed.push(format!("evidence: {error}")),
        }
    }

    if let Some(changes) = turn_value.get("graph_changes") {
        match serde_json::from_value::<Vec<GraphChangeProposal>>(changes.clone()) {
            Ok(changes) => session.graph_changes.extend(changes),
            Err(error) => malformed.push(format!("graph_changes: {error}")),
        }
    }

    if !malformed.is_empty() {
        session
            .failures
            .push(format!("malformed final output: {}", malformed.join("; ")));
    }
}

fn outputs_is_empty(outputs: &Value) -> bool {
    outputs.as_array().is_some_and(Vec::is_empty)
}

fn matches_target(
    target: EvidenceTarget,
    criterion: &ArtifactVersionRef,
    work: &relay_domain::WorkItemId,
) -> bool {
    match target {
        EvidenceTarget::ArtifactVersion(reference) => &reference == criterion,
        EvidenceTarget::WorkItem(target_work) => &target_work == work,
        EvidenceTarget::Decision(_) => false,
    }
}

fn target_name(target: &EvidenceTarget) -> String {
    match target {
        EvidenceTarget::WorkItem(work) => format!("work_item {work}"),
        EvidenceTarget::ArtifactVersion(reference) => format!("artifact_version {reference}"),
        EvidenceTarget::Decision(decision) => format!("decision {decision}"),
    }
}

/// The content commitment, computed through the ONE shared definition:
/// SHA-256 over the canonical JSON serialization of the structured content
/// (`relay_artifacts::content_digest`). The planner never fabricates a
/// digest the model supplied, and canonicalization failures become
/// actionable malformed-output reasons rather than panics.
pub(crate) fn digest_of(content: &ArtifactContent) -> Result<ArtifactDigest, String> {
    let digest = canonical_content_digest(&content.0)
        .map_err(|error| format!("content could not be canonicalized: {error}"))?;
    ArtifactDigest::parse(&digest).map_err(|error| error.to_string())
}

#[allow(clippy::panic)] // scoped: the fallback id is a constant that always parses
fn fallback_evidence_id() -> EvidenceId {
    EvidenceId::parse("ev_0").unwrap_or_else(|_| {
        panic!("static fallback evidence id failed to parse");
    })
}

fn head_of(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    use crate::BudgetGuard;
    use crate::executors::{Executor, VerifierExecutor, WorkspaceExecutor};
    use crate::model::{ModelClient, ModelClientError, ModelTurnResponse};
    use crate::spec::ManualClock;
    use relay_domain::{
        Budget, CapabilityGrant, GrantId, ModelRoute, Operation, ScopePath, WorkItemId,
    };
    use relay_providers::contract::CancelToken;
    use relay_tools::create_default_tool_registry;

    /// A model that replays scripted turn texts in order; calls beyond
    /// the script get the last text again.
    struct ScriptedModel {
        turns: Mutex<std::collections::VecDeque<String>>,
    }

    impl ScriptedModel {
        fn new(turns: Vec<String>) -> Self {
            ScriptedModel {
                turns: Mutex::new(turns.into()),
            }
        }
    }

    impl ModelClient for ScriptedModel {
        fn complete(
            &self,
            _route: &ModelRoute,
            _request: &ModelTurnRequest,
            _signal: &CancelToken,
        ) -> Result<ModelTurnResponse, ModelClientError> {
            let mut turns = self
                .turns
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            let text = if turns.len() > 1 {
                turns.pop_front().unwrap_or_default()
            } else {
                turns.front().cloned().unwrap_or_default()
            };
            Ok(ModelTurnResponse {
                text,
                input_tokens: 1,
                output_tokens: 1,
                routes_attempted: Vec::new(),
            })
        }
    }

    fn spec(grant: CapabilityGrant) -> AttemptSpec {
        AttemptSpec {
            attempt: relay_domain::AttemptId::parse("att_p1").unwrap(),
            work: WorkItemId::parse("work_planner").unwrap(),
            objective: "add the config file".into(),
            inputs: Vec::new(),
            decision: None,
            grant,
            expected_revision: relay_domain::ProjectRevision::default(),
            started_at_ms: 1_000,
        }
    }

    fn context(model: ScriptedModel) -> (RunContext, Arc<ManualClock>) {
        let clock = Arc::new(ManualClock::new(1_050));
        let signal = CancelToken::new();
        let budget = Arc::new(BudgetGuard::new(
            Budget::default(),
            1_000,
            Arc::clone(&clock).into_clock(),
            signal.clone(),
        ));
        let context = RunContext {
            workspace_root: std::env::temp_dir(),
            signal,
            model: Arc::new(model),
            tools: Arc::new(
                create_default_tool_registry(&relay_config::ToolSettings::default()).unwrap(),
            ),
            budget,
            graph: Arc::new(relay_domain::ProjectGraph::default),
            now_ms: 1_050,
        };
        (context, clock)
    }

    fn grant(mutate: impl FnOnce(&mut CapabilityGrant)) -> CapabilityGrant {
        let mut grant = CapabilityGrant {
            id: GrantId::parse("grant_p").unwrap(),
            read_paths: vec![ScopePath::root()],
            write_paths: vec![ScopePath::parse("src").unwrap()],
            operations: vec![Operation::Read, Operation::Write, Operation::Execute],
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
        mutate(&mut grant);
        grant
    }

    #[test]
    fn the_model_guided_loop_executes_gated_calls_and_extracts_proposals() {
        let model = ScriptedModel::new(vec![
                r#"{"tool_calls":[{"tool":"write-file","args":{"path":"src/added.rs","content":"pub fn f() {}\n"}}]}"#
                    .to_owned(),
                r#"{"outputs":[{"artifact":"art_added","title":"Added","content":{"file":"src/added.rs"}}],
                    "evidence":[{"kind":"inspection","target":{"work_item":"work_planner"},"outcome":"info","summary":"added the file"}],
                    "graph_changes":[]}"#
                    .to_owned(),
            ]);
        let workspace = tempfile::Builder::new()
            .prefix("relay-planner-")
            .tempdir()
            .expect("temp dir")
            .keep();
        let (mut context, _clock) = context(model);
        context.workspace_root = workspace.clone();
        let executor = WorkspaceExecutor::new(Arc::new(ModelGuidedWorkspacePlanner::new()));
        let receipt = executor.run(&spec(grant(|_| {})), &context);
        assert!(
            matches!(receipt.outcome, relay_domain::AttemptOutcome::Completed),
            "{:?}",
            receipt.outcome
        );
        // The tool call really ran through the gate.
        assert!(workspace.join("src/added.rs").exists());
        // The structured proposals were extracted and validated.
        assert_eq!(receipt.outputs.len(), 1);
        assert_eq!(receipt.outputs[0].artifact.as_str(), "art_added");
        assert_eq!(receipt.outputs[0].digest.as_str().len(), 64);
        assert_eq!(receipt.evidence.len(), 2, "inspection + extracted evidence");
        assert!(
            receipt
                .evidence
                .iter()
                .any(|evidence| evidence.id.as_str() == "ev_p1-p0")
        );
    }

    #[test]
    fn grant_denials_fail_the_session_with_the_denial_reason() {
        let model = ScriptedModel::new(vec![
                r#"{"tool_calls":[{"tool":"run-command","args":{"command":"/bin/rm","args":["-rf","."]}}]}"#
                    .to_owned(),
                r#"{"evidence":[{"kind":"inspection","target":{"work_item":"work_planner"},"outcome":"info","summary":"done"}]}"#
                    .to_owned(),
            ]);
        let (context, _clock) = context(model);
        let executor = WorkspaceExecutor::new(Arc::new(ModelGuidedWorkspacePlanner::new()));
        let receipt = executor.run(&spec(grant(|_| {})), &context);
        match receipt.outcome {
            relay_domain::AttemptOutcome::Failed { reason } => {
                assert!(
                    reason.contains("denied by capability grant"),
                    "the model must see its denial fail the session: {reason}"
                );
            }
            relay_domain::AttemptOutcome::Completed => panic!("denied calls must fail"),
        }
    }

    #[test]
    fn malformed_final_output_fails_with_actionable_reasons() {
        let model = ScriptedModel::new(vec![
            r#"{"outputs":[{"artifact":"NOT_AN_ID","content":{}}]}"#.to_owned(),
        ]);
        let (context, _clock) = context(model);
        let executor = WorkspaceExecutor::new(Arc::new(ModelGuidedWorkspacePlanner::new()));
        let receipt = executor.run(&spec(grant(|_| {})), &context);
        match receipt.outcome {
            relay_domain::AttemptOutcome::Failed { reason } => {
                assert!(reason.contains("malformed final output"), "{reason}");
                assert!(reason.contains("outputs"), "{reason}");
            }
            relay_domain::AttemptOutcome::Completed => panic!("malformed output must fail"),
        }
    }

    #[test]
    fn the_loop_is_bounded_and_never_runs_away() {
        let model = ScriptedModel::new(vec!["not json at all".to_owned()]);
        let (context, _clock) = context(model);
        let executor = WorkspaceExecutor::new(Arc::new(ModelGuidedWorkspacePlanner::with_config(
            ModelGuidedConfig { max_turns: 3 },
        )));
        let receipt = executor.run(&spec(grant(|_| {})), &context);
        match receipt.outcome {
            relay_domain::AttemptOutcome::Failed { reason } => {
                assert!(
                    reason.contains("no final answer within 3 turns"),
                    "{reason}"
                );
            }
            relay_domain::AttemptOutcome::Completed => panic!("a garbage model must not complete"),
        }
    }

    #[test]
    fn verifier_evidence_must_target_the_criterion() {
        let criterion = ArtifactVersionRef::new(
            relay_domain::ArtifactId::parse("art_criterion").unwrap(),
            relay_domain::ArtifactVersion::FIRST,
        );
        let model = ScriptedModel::new(vec![
                r#"{"evidence":[{"kind":"test_run","target":{"work_item":"work_someone-else"},"outcome":"pass","summary":"wrong target"}]}"#
                    .to_owned(),
            ]);
        let (wrong_target_context, _clock) = context(model);
        let executor = VerifierExecutor::new(Arc::new(ModelGuidedVerifierPlanner::for_criterion(
            criterion.clone(),
        )));
        let receipt = executor.run(&spec(grant(|_| {})), &wrong_target_context);
        match receipt.outcome {
            relay_domain::AttemptOutcome::Failed { reason } => {
                assert!(reason.contains("must target criterion"), "{reason}");
            }
            relay_domain::AttemptOutcome::Completed => panic!("wrong target must fail"),
        }

        let model = ScriptedModel::new(vec![
                r#"{"evidence":[{"kind":"test_run","target":{"artifact_version":{"artifact":"art_criterion","version":1}},"outcome":"pass","summary":"all green"}]}"#
                    .to_owned(),
            ]);
        let (verifier_context, _clock) = context(model);
        let executor = VerifierExecutor::new(Arc::new(ModelGuidedVerifierPlanner::for_criterion(
            criterion,
        )));
        let receipt = executor.run(&spec(grant(|_| {})), &verifier_context);
        assert!(matches!(
            receipt.outcome,
            relay_domain::AttemptOutcome::Completed
        ));
        assert!(receipt.evidence.iter().any(|evidence| matches!(
            &evidence.target,
            EvidenceTarget::ArtifactVersion(reference) if reference.artifact.as_str() == "art_criterion"
        )));
    }

    #[test]
    fn verifier_sessions_cannot_publish_outputs() {
        let model = ScriptedModel::new(vec![
            r#"{"outputs":[{"artifact":"art_smuggle","content":{}}]}"#.to_owned(),
        ]);
        let (context, _clock) = context(model);
        let executor = VerifierExecutor::new(Arc::new(ModelGuidedVerifierPlanner::new()));
        let receipt = executor.run(&spec(grant(|_| {})), &context);
        match receipt.outcome {
            relay_domain::AttemptOutcome::Failed { reason } => {
                assert!(
                    reason.contains("verifier sessions cannot publish"),
                    "{reason}"
                );
            }
            relay_domain::AttemptOutcome::Completed => panic!("smuggled outputs must fail"),
        }
    }

    #[test]
    fn prompts_are_plain_data_mentioning_the_protocol_and_tools() {
        let planner = ModelGuidedWorkspacePlanner::new();
        let system = planner.system_prompt();
        assert!(system.contains("tool_calls"));
        assert!(system.contains("graph_changes"));
        let opening = planner.opening_prompt(
            "do the thing",
            &["art_brief@1".to_owned()],
            r#"[{"name":"read-file"}]"#,
        );
        assert!(opening.contains("do the thing"));
        assert!(opening.contains("art_brief@1"));
        assert!(opening.contains("read-file"));
        let observation = planner.observation_prompt(&["read-file: ok".to_owned()], "[]");
        assert!(observation.contains("read-file: ok"));
        let verifier = ModelGuidedVerifierPlanner::for_criterion(ArtifactVersionRef::new(
            relay_domain::ArtifactId::parse("art_c").unwrap(),
            relay_domain::ArtifactVersion::FIRST,
        ));
        assert!(verifier.system_prompt().contains("art_c@1"));
    }

    /// SHA-256 hex over raw text, used ONLY to prove which serialization
    /// the shared digest hashes.
    fn sha256_hex(text: &str) -> String {
        use sha2::{Digest as _, Sha256};
        hex::encode(Sha256::digest(text.as_bytes()))
    }

    fn digest_of_content(content: &ArtifactContent) -> String {
        digest_of(content).expect("canonical digest").to_string()
    }

    #[test]
    fn digests_use_the_one_shared_canonical_definition() {
        // A float-bearing object built with non-sorted key order: canonical
        // JSON sorts keys and uses plain float notation, compact serde_json
        // does not — so the digest must differ from compact-JSON hashing
        // and must equal the canonical form.
        let value =
            serde_json::from_str::<Value>(r#"{"z": 1.0, "a": 2.5, "m": 3}"#).expect("parse");
        let content = ArtifactContent::new(value);
        let canonical = relay_artifacts::canonical_json(&content.0).expect("canonical json");
        let compact = serde_json::to_string(&content.0).expect("compact json");
        assert_ne!(
            canonical, compact,
            "the fixture must actually distinguish the two serializations"
        );
        assert_eq!(
            digest_of_content(&content),
            sha256_hex(&canonical),
            "digests must hash the CANONICAL serialization"
        );
        assert_ne!(
            digest_of_content(&content),
            sha256_hex(&compact),
            "digests must not be compact-JSON hashes"
        );
        assert_eq!(digest_of_content(&content).len(), 64);
    }

    #[test]
    fn digests_are_insertion_order_independent() {
        let first = ArtifactContent::new(
            serde_json::from_str::<Value>(r#"{"a": 1, "b": {"y": 2.0, "x": 3}}"#).expect("parse"),
        );
        let second = ArtifactContent::new(
            serde_json::from_str::<Value>(r#"{"b": {"x": 3, "y": 2.0}, "a": 1}"#).expect("parse"),
        );
        assert_eq!(
            digest_of_content(&first),
            digest_of_content(&second),
            "canonical form must not depend on key insertion order"
        );
        // And different content still yields different commitments.
        let third = ArtifactContent::new(
            serde_json::from_str::<Value>(r#"{"a": 1, "b": {"y": 9.0, "x": 3}}"#).expect("parse"),
        );
        assert_ne!(digest_of_content(&first), digest_of_content(&third));
    }
}
