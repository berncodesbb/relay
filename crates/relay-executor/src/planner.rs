//! Tool-session planning: the shared execution kernel and the
//! model-guided planners.
//!
//! [`ToolRunner`] is the single execution kernel every planner shares:
//! each call is checked against the attempt's grant ([`GrantGate`]), the
//! budget, and the cancellation signal before the tool body runs. Scripted
//! planners produce calls up front; the model-guided planners
//! ([`ModelGuidedWorkspacePlanner`], [`ModelGuidedVerifierPlanner`]) drive
//! a Relay-owned native tool loop — model turns see only the grant-visible
//! [`ToolSpec`] definitions (built from the same registry the runtime
//! validates against), every native tool call executes through the same
//! kernel and is answered by a correlated [`ModelMessage::ToolResult`],
//! iterations stay bounded by the budget and a turn cap, and the final
//! turn's structured proposals ([`OutputProposal`], [`Evidence`],
//! [`GraphChangeProposal`]) are schema-validated. Malformed final output
//! becomes an actionable remediation turn and, failing that, a `Failed`
//! receipt — never silently coerced.
//!
//! Prompts are plain-data builder methods on the planners (strings in,
//! strings out); tool definitions travel as [`ToolSpec`] values, never as
//! prompt prose.

use std::sync::Arc;

use relay_artifacts::content_digest as canonical_content_digest;
use relay_domain::{
    ArtifactContent, ArtifactDigest, ArtifactId, ArtifactVersionRef, Evidence, EvidenceId,
    EvidenceKind, EvidenceOutcome, EvidenceTarget, GraphChangeProposal, OutputProposal,
};
use relay_providers::contract::{ModelMessage, ToolSpec};
use relay_tools::GrantGate;
use serde::Deserialize;
use serde_json::Value;

use crate::budget::BudgetGuard;
use crate::model::ModelTurnRequest;
use crate::spec::{AttemptSpec, RunContext};

/// How much of a tool's (already bounded and redacted) output is returned
/// to the model as tool-result content. The tool layer bounds output per
/// tool (tens of thousands of characters), which can exceed the provider
/// contract's tool-result ceiling once multibyte characters are involved;
/// 16_000 characters is at most 64_000 bytes, safely below the 65_536-byte
/// [`ModelMessage::tool_result`] bound, and far above the 400-character
/// evidence heads.
const MAX_TOOL_RESULT_TO_MODEL_CHARS: usize = 16_000;

/// One executed tool call, as recorded for the receipt.
#[derive(Debug, Clone, PartialEq)]
pub struct CallRecord {
    /// The provider-assigned id of the native tool call this record
    /// answers, for correlating records with the transcript's tool
    /// results. `None` for scripted (non-model-driven) calls.
    pub call_id: Option<String>,
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

    /// Execute one scripted call through every enforcement layer, recording
    /// it. Native (model-requested) calls go through [`Self::run_native`],
    /// which additionally correlates the record with the provider call id.
    pub fn run(&mut self, call: ToolCallRequest) -> CallRecord {
        self.execute(None, call).0
    }

    /// Execute one native (model-requested) tool call through every
    /// enforcement layer — cancellation, budget, unknown-tool rejection,
    /// grant, argument validation, execution — recording it and returning
    /// the bounded output to hand back to the model as the call's tool
    /// result. Unknown tools are never executed; the returned message
    /// names the tool and the grant-visible surface so the model can
    /// correct itself.
    pub fn run_native(&mut self, call_id: &str, call: ToolCallRequest) -> (CallRecord, String) {
        self.execute(Some(call_id), call)
    }

    /// The shared enforcement chain for scripted and native calls.
    fn execute(&mut self, call_id: Option<&str>, call: ToolCallRequest) -> (CallRecord, String) {
        if self.signal.is_cancelled() {
            return self.stopped(call_id, call, "canceled before the call ran".to_owned());
        }
        if let Err(exhausted) = self.budget.record_attempt() {
            return self.stopped(
                call_id,
                call,
                format!("budget stopped the batch: {exhausted}"),
            );
        }
        // Unknown tools are rejected before the grant: they are never
        // executed, and the error names the visible surface so a model
        // that hallucinated a name can correct itself.
        if self.tools.get(&call.tool).is_err() {
            let output = format!(
                "unknown tool \"{}\"; the tools visible to this attempt are: {}",
                call.tool,
                self.visible_tool_names().join(", ")
            );
            let record = CallRecord {
                call_id: call_id.map(str::to_owned),
                tool: call.tool,
                args: call.args,
                ok: false,
                output_head: head_of(&output, 400),
                affected_files: Vec::new(),
                canceled: false,
                denied_by_grant: false,
            };
            self.records.push(record.clone());
            return (record, output);
        }
        let result = self
            .gate
            .call(&self.tools, &call.tool, &call.args, &self.tool_context);
        let record = CallRecord {
            call_id: call_id.map(str::to_owned),
            denied_by_grant: result.output.starts_with("denied by capability grant"),
            tool: call.tool,
            args: call.args,
            ok: result.ok,
            output_head: head_of(&result.output, 400),
            affected_files: result.affected_files,
            canceled: false,
        };
        let content = head_of(&result.output, MAX_TOOL_RESULT_TO_MODEL_CHARS);
        self.records.push(record.clone());
        (record, content)
    }

    /// Record a call that never ran because the attempt was canceled or
    /// the budget fired, and return its model-facing output.
    fn stopped(
        &mut self,
        call_id: Option<&str>,
        call: ToolCallRequest,
        output: String,
    ) -> (CallRecord, String) {
        let record = CallRecord {
            call_id: call_id.map(str::to_owned),
            tool: call.tool,
            args: call.args,
            ok: false,
            output_head: output.clone(),
            affected_files: Vec::new(),
            canceled: true,
            denied_by_grant: false,
        };
        self.records.push(record.clone());
        (record, output)
    }

    /// Execute a scripted batch.
    pub fn run_all(&mut self, calls: Vec<ToolCallRequest>) -> Vec<CallRecord> {
        for call in calls {
            self.run(call);
        }
        self.records.clone()
    }

    /// The grant-visible tool surface as native tool definitions: name,
    /// description, and parameters schema straight from the registry the
    /// runtime validates against — the same definitions, never a parallel
    /// serialization for prompts.
    pub fn visible_tools(&self) -> Vec<ToolSpec> {
        self.gate
            .visible_tools(&self.tools)
            .into_iter()
            .map(|tool| ToolSpec {
                name: tool.name.clone(),
                description: tool.description.clone(),
                parameters: relay_tools::tool_json_schema(&tool),
            })
            .collect()
    }

    /// The names of the grant-visible tools, for unknown-tool errors.
    fn visible_tool_names(&self) -> Vec<String> {
        self.gate
            .visible_tools(&self.tools)
            .into_iter()
            .map(|tool| tool.name.clone())
            .collect()
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

    /// The opening turn prompt: objective and inputs (plain data). Tool
    /// definitions travel as native tool definitions on the request, never
    /// as prompt text.
    pub fn opening_prompt(&self, objective: &str, inputs: &[String]) -> String {
        opening_prompt(objective, inputs)
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
    pub fn opening_prompt(&self, objective: &str, inputs: &[String]) -> String {
        opening_prompt(objective, inputs)
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

/// The workspace session system prompt: the bounded-grant and native-tool
/// preamble plus the shared final-object protocol.
fn workspace_system_prompt() -> String {
    format!(
        "You are a careful workspace agent working inside a bounded capability \
         grant. Tools are provided as native tool definitions alongside this \
         instruction: call them directly when you need them, and never \
         describe or encode a tool call in your text. Every call is checked \
         against the grant before it runs, and its result returns to you as a \
         tool result. Work step by step, then stop with the final \
         object.\n\n{}",
        protocol_contract()
    )
}

/// The final-object protocol shared by every model session (workspace,
/// verifier, and model-class): how to finish. Tool calling is native and
/// needs no textual instructions.
pub(crate) fn protocol_contract() -> String {
    "To finish, reply with EXACTLY ONE JSON object as your entire visible reply:\n\
     {\"outputs\":[{\"artifact\":\"art_…\",\"title\":\"…\",\"content\":{…}}],\n\
     \x20              \"evidence\":[{\"kind\":\"inspection|test_run|command_output|static_analysis|review|human_statement\",\n\
     \x20                          \"target\":{\"work_item\":\"work_…\"}|{\"artifact_version\":{\"artifact\":\"art_…\",\"version\":1}},\n\
     \x20                          \"outcome\":\"pass|fail|info\",\"summary\":\"…\",\"payload\":{…}}],\n\
     \x20              \"graph_changes\":[…]}\n\
     All three final arrays are optional but at least one entry must exist. \
     Ids must use their exact prefixed forms; malformed entries are rejected."
        .to_owned()
}

fn opening_prompt(objective: &str, inputs: &[String]) -> String {
    let inputs = if inputs.is_empty() {
        "none".to_owned()
    } else {
        inputs.join(", ")
    };
    format!(
        "Objective: {objective}\nInputs: {inputs}\n\n\
         Begin. Use the provided tools as needed, then reply with the final object."
    )
}

/// The model-guided loop shared by both planners: a Relay-owned transcript
/// of user, assistant, and tool-result turns replayed in full to every
/// model call.
///
/// Per turn: the model receives the system prompt, the whole transcript,
/// and the grant-visible native tool definitions. Its ordered blocks are
/// appended verbatim as the assistant turn (opaque continuation blocks
/// included — they are in-memory transcript state, never persisted). Every
/// native tool call is answered strictly sequentially in arrival order
/// through [`ToolRunner`] (cancellation, budget, unknown-tool rejection,
/// grant, argument validation, execution) with a correlated tool result
/// marked success or error. A turn that carries tool calls is never a
/// final candidate: the tools run and the loop continues. A turn with no
/// tool calls is a candidate final turn — its text must be the final JSON
/// object, and a parse failure becomes a remediation user turn. The loop
/// is bounded by the turn cap and the budget.
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
    let tools = runner.visible_tools();
    let tool_definitions = (!tools.is_empty()).then_some(tools);
    let mut transcript: Vec<ModelMessage> = vec![ModelMessage::user_text(opening_prompt(
        &spec.objective,
        &spec
            .inputs
            .iter()
            .map(|input| input.to_string())
            .collect::<Vec<_>>(),
    ))];
    for _turn in 0..config.max_turns {
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
        let request = ModelTurnRequest {
            system: Some(guidance.system_prompt()),
            messages: transcript.clone(),
            tools: tool_definitions.clone(),
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
        // The assistant turn is preserved verbatim — block order, visible
        // text, and any opaque continuation blocks.
        transcript.push(ModelMessage::Assistant {
            content: response.blocks.clone(),
        });
        let calls = response.tool_calls();
        if !calls.is_empty() {
            // A tool-carrying turn is never a final candidate: the calls
            // run sequentially in arrival order and every one is answered
            // by a correlated tool result before the next model turn.
            for call in calls {
                let (record, output) = runner.run_native(
                    &call.id,
                    ToolCallRequest {
                        tool: call.name,
                        args: call.arguments,
                    },
                );
                transcript.push(tool_result_message(&call.id, &output, !record.ok));
            }
            continue;
        }
        // No tool calls: the visible text is the candidate final object.
        let turn_value: Result<Value, String> = match serde_json::from_str(response.text().trim()) {
            Ok(Value::Object(turn_value)) => Ok(Value::Object(turn_value)),
            Ok(_) => Err("the reply was JSON but not an object".to_owned()),
            Err(error) => Err(error.to_string()),
        };
        match turn_value {
            Ok(turn_value) => {
                extract_final(spec, context, &turn_value, &guidance, &mut session);
                return finalize_session(session, runner);
            }
            Err(reason) => {
                transcript.push(ModelMessage::user_text(malformed_final_note(&reason)));
            }
        }
    }
    session
        .failures
        .push(format!("no final answer within {} turns", config.max_turns));
    finalize_session(session, runner)
}

/// The tool-result message answering one native call: bounded content and
/// an error flag covering every way the call did not succeed (tool
/// failure, denial, unknown tool, malformed arguments, cancellation,
/// budget stop).
fn tool_result_message(call_id: &str, content: &str, is_error: bool) -> ModelMessage {
    match ModelMessage::tool_result(
        call_id,
        head_of(content, MAX_TOOL_RESULT_TO_MODEL_CHARS),
        is_error,
    ) {
        Ok(message) => message,
        // Unreachable by construction: the head is far below the contract's
        // tool-result bound. Fail closed with a bounded notice rather than
        // panicking or dropping the correlation.
        Err(_) => ModelMessage::user_text(format!(
            "tool result for call \"{call_id}\" exceeded the bound and was dropped"
        )),
    }
}

/// Attach the runner's records to the session (single exit path).
fn finalize_session(mut session: ToolSession, runner: &ToolRunner) -> ToolSession {
    session.records = runner.records().to_vec();
    session
}

/// The remediation note for a turn whose text was not the final JSON
/// object. It addresses only the final object: tool calls are native and
/// never need textual remediation.
pub(crate) fn malformed_final_note(reason: &str) -> String {
    format!(
        "your previous reply was not the final JSON object ({reason}); call tools if \
         you still need them, otherwise reply with exactly the final \
         {{\"outputs\":…,\"evidence\":…,\"graph_changes\":…}} object"
    )
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

    /// A model that replays scripted turns of ordered blocks in order,
    /// records every request it served (so tests can assert exactly what
    /// the loop forwarded), and can cancel the attempt signal while
    /// serving its first turn. Calls beyond the script repeat the last
    /// turn.
    struct TurnsModel {
        turns: Mutex<std::collections::VecDeque<ModelTurnResponse>>,
        requests: Mutex<Vec<ModelTurnRequest>>,
        cancel_on_first_turn: bool,
    }

    impl TurnsModel {
        fn new(turns: Vec<ModelTurnResponse>) -> Self {
            TurnsModel {
                turns: Mutex::new(turns.into()),
                requests: Mutex::new(Vec::new()),
                cancel_on_first_turn: false,
            }
        }

        fn cancel_on_first_turn(mut turns: Vec<ModelTurnResponse>) -> Self {
            turns.truncate(1);
            TurnsModel {
                turns: Mutex::new(turns.into()),
                requests: Mutex::new(Vec::new()),
                cancel_on_first_turn: true,
            }
        }

        fn served_requests(&self) -> Vec<ModelTurnRequest> {
            self.requests
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .clone()
        }
    }

    impl ModelClient for TurnsModel {
        fn complete(
            &self,
            _route: &ModelRoute,
            request: &ModelTurnRequest,
            signal: &CancelToken,
        ) -> Result<ModelTurnResponse, ModelClientError> {
            self.requests
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .push(request.clone());
            if self.cancel_on_first_turn
                && self
                    .requests
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner())
                    .len()
                    == 1
            {
                signal.cancel("test canceled between turns");
            }
            let mut turns = self
                .turns
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            let response = if turns.len() > 1 {
                turns
                    .pop_front()
                    .unwrap_or_else(|| ModelTurnResponse::from_text(""))
            } else {
                turns
                    .front()
                    .cloned()
                    .unwrap_or_else(|| ModelTurnResponse::from_text(""))
            };
            Ok(response)
        }
    }

    fn text_block(text: &str) -> relay_providers::contract::AssistantContent {
        relay_providers::contract::AssistantContent::Text {
            text: text.to_owned(),
        }
    }

    fn call_block(
        id: &str,
        tool: &str,
        args: serde_json::Value,
    ) -> relay_providers::contract::AssistantContent {
        relay_providers::contract::AssistantContent::ToolCall {
            call: relay_providers::contract::ToolCall {
                id: id.to_owned(),
                name: tool.to_owned(),
                arguments: args,
            },
        }
    }

    fn turn(blocks: Vec<relay_providers::contract::AssistantContent>) -> ModelTurnResponse {
        ModelTurnResponse {
            blocks,
            input_tokens: 1,
            output_tokens: 1,
            routes_attempted: Vec::new(),
        }
    }

    fn final_turn(json: &str) -> ModelTurnResponse {
        turn(vec![text_block(json)])
    }

    /// The tool-result messages of a recorded request, in transcript
    /// order.
    fn tool_results_of(
        request: &ModelTurnRequest,
    ) -> Vec<&relay_providers::contract::ModelMessage> {
        request
            .messages
            .iter()
            .filter(|message| {
                matches!(
                    message,
                    relay_providers::contract::ModelMessage::ToolResult { .. }
                )
            })
            .collect()
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

    fn context(model: Arc<TurnsModel>) -> (RunContext, Arc<ManualClock>) {
        context_with_model(model)
    }

    fn context_with_budget(
        model: Arc<TurnsModel>,
        budget: Budget,
    ) -> (RunContext, Arc<ManualClock>) {
        build_context(model, budget)
    }

    fn context_with_model(model: Arc<dyn ModelClient>) -> (RunContext, Arc<ManualClock>) {
        build_context(model, Budget::default())
    }

    fn build_context(
        model: Arc<dyn ModelClient>,
        budget: Budget,
    ) -> (RunContext, Arc<ManualClock>) {
        let clock = Arc::new(ManualClock::new(1_050));
        let signal = CancelToken::new();
        let budget = Arc::new(BudgetGuard::new(
            budget,
            1_000,
            Arc::clone(&clock).into_clock(),
            signal.clone(),
        ));
        let context = RunContext {
            workspace_root: std::env::temp_dir(),
            signal,
            model,
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
    fn native_tool_calls_execute_through_the_gate_and_correlate_results() {
        let model = Arc::new(TurnsModel::new(vec![
            turn(vec![
                text_block("writing the file first"),
                call_block(
                    "c1",
                    "write-file",
                    serde_json::json!({
                        "path": "src/added.rs",
                        "content": "pub fn f() {}\n"
                    }),
                ),
            ]),
            final_turn(
                r#"{"outputs":[{"artifact":"art_added","title":"Added","content":{"file":"src/added.rs"}}],
                    "evidence":[{"kind":"inspection","target":{"work_item":"work_planner"},"outcome":"info","summary":"added the file"}],
                    "graph_changes":[]}"#,
            ),
        ]));
        let workspace = tempfile::Builder::new()
            .prefix("relay-planner-")
            .tempdir()
            .expect("temp dir")
            .keep();
        let (mut context, _clock) = context(Arc::clone(&model));
        context.workspace_root = workspace.clone();
        let executor = WorkspaceExecutor::new(Arc::new(ModelGuidedWorkspacePlanner::new()));
        let receipt = executor.run(&spec(grant(|_| {})), &context);
        assert!(
            matches!(receipt.outcome, relay_domain::AttemptOutcome::Completed),
            "{:?}",
            receipt.outcome
        );
        // The native call really ran through the gate.
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

        // The loop replayed a full transcript and answered the call by id.
        let requests = model.served_requests();
        assert_eq!(requests.len(), 2, "one model turn per loop turn");
        let second = &requests[1];
        assert_eq!(second.messages.len(), 3, "user + assistant + tool result");
        // The assistant turn keeps its blocks verbatim, interleaved order
        // included.
        assert_eq!(
            second.messages[1],
            relay_providers::contract::ModelMessage::Assistant {
                content: vec![
                    text_block("writing the file first"),
                    call_block(
                        "c1",
                        "write-file",
                        serde_json::json!({
                            "path": "src/added.rs",
                            "content": "pub fn f() {}\n"
                        }),
                    ),
                ],
            }
        );
        // The tool result matches the call id, is a success, and carries
        // the bounded tool output.
        let results = tool_results_of(second);
        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0],
            &relay_providers::contract::ModelMessage::tool_result(
                "c1",
                "created src/added.rs (14 bytes)",
                false
            )
            .expect("bounded")
        );
        // Every model turn carried the grant-visible native tool
        // definitions, never a tool list in prose.
        for request in &requests {
            let tools = request.tools.as_ref().expect("tools are sent natively");
            assert!(
                tools.iter().any(|tool| tool.name == "write-file"
                    && tool.description.contains("Create a NEW text file")),
                "specs come from the registry definitions"
            );
            assert!(!request.last_user_text().contains("write-file"));
        }
    }

    #[test]
    fn unknown_tools_get_error_results_naming_the_visible_surface_and_never_execute() {
        let model = Arc::new(TurnsModel::new(vec![
            turn(vec![call_block(
                "c9",
                "definitely-not-a-tool",
                serde_json::json!({ "path": "src/evil.rs" }),
            )]),
            final_turn(
                r#"{"evidence":[{"kind":"inspection","target":{"work_item":"work_planner"},"outcome":"info","summary":"recovered"}]}"#,
            ),
        ]));
        let workspace = tempfile::Builder::new()
            .prefix("relay-planner-unknown-")
            .tempdir()
            .expect("temp dir")
            .keep();
        let (mut context, _clock) = context(Arc::clone(&model));
        context.workspace_root = workspace.clone();
        let executor = WorkspaceExecutor::new(Arc::new(ModelGuidedWorkspacePlanner::new()));
        let receipt = executor.run(&spec(grant(|_| {})), &context);
        // The hallucinated name never executed...
        assert!(!workspace.join("src/evil.rs").exists());
        // ...the session continued to a final turn...
        assert_eq!(model.served_requests().len(), 2);
        // ...and the error result names the tool and the visible surface.
        let requests = model.served_requests();
        let results = tool_results_of(&requests[1]);
        assert_eq!(results.len(), 1);
        match results[0] {
            relay_providers::contract::ModelMessage::ToolResult {
                tool_call_id,
                content,
                is_error,
            } => {
                assert_eq!(tool_call_id, "c9");
                assert!(is_error, "an unknown tool is an error result");
                assert!(content.contains("unknown tool"), "{content}");
                assert!(content.contains("definitely-not-a-tool"), "{content}");
                assert!(content.contains("read-file"), "{content}");
            }
            other => panic!("expected a tool result, got {other:?}"),
        }
        // The failed record keeps the attempt from ever completing.
        match receipt.outcome {
            relay_domain::AttemptOutcome::Failed { reason } => {
                assert!(reason.contains("unknown tool"), "{reason}");
            }
            relay_domain::AttemptOutcome::Completed => {
                panic!("an unknown tool must not become success")
            }
        }
    }

    #[test]
    fn grant_denials_fail_the_session_with_the_denial_reason() {
        let model = Arc::new(TurnsModel::new(vec![
            turn(vec![call_block(
                "c1",
                "run-command",
                serde_json::json!({ "command": "/bin/rm", "args": ["-rf", "."] }),
            )]),
            final_turn(
                r#"{"evidence":[{"kind":"inspection","target":{"work_item":"work_planner"},"outcome":"info","summary":"done"}]}"#,
            ),
        ]));
        let (context, _clock) = context(Arc::clone(&model));
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
        // The denial also reached the model as an error tool result.
        let requests = model.served_requests();
        let results = tool_results_of(&requests[1]);
        match results[0] {
            relay_providers::contract::ModelMessage::ToolResult {
                tool_call_id,
                content,
                is_error,
            } => {
                assert_eq!(tool_call_id, "c1");
                assert!(is_error, "a denied call is an error result");
                assert!(content.contains("denied by capability grant"), "{content}");
            }
            other => panic!("expected a tool result, got {other:?}"),
        }
    }

    #[test]
    fn malformed_arguments_get_error_results_without_execution() {
        let model = Arc::new(TurnsModel::new(vec![
            turn(vec![call_block(
                "c1",
                "write-file",
                // `path` must be a string; a number fails the schema.
                serde_json::json!({ "path": 123, "content": "nope" }),
            )]),
            final_turn(
                r#"{"evidence":[{"kind":"inspection","target":{"work_item":"work_planner"},"outcome":"info","summary":"recovered"}]}"#,
            ),
        ]));
        let workspace = tempfile::Builder::new()
            .prefix("relay-planner-args-")
            .tempdir()
            .expect("temp dir")
            .keep();
        let (mut context, _clock) = context(Arc::clone(&model));
        context.workspace_root = workspace.clone();
        let executor = WorkspaceExecutor::new(Arc::new(ModelGuidedWorkspacePlanner::new()));
        let receipt = executor.run(&spec(grant(|_| {})), &context);
        // Schema-violating arguments never reach the tool body.
        assert!(!workspace.join("123").exists());
        let requests = model.served_requests();
        let results = tool_results_of(&requests[1]);
        match results[0] {
            relay_providers::contract::ModelMessage::ToolResult {
                content, is_error, ..
            } => {
                assert!(is_error, "malformed arguments are an error result");
                assert!(content.contains("invalid arguments"), "{content}");
            }
            other => panic!("expected a tool result, got {other:?}"),
        }
        match receipt.outcome {
            relay_domain::AttemptOutcome::Failed { reason } => {
                assert!(reason.contains("invalid arguments"), "{reason}");
            }
            relay_domain::AttemptOutcome::Completed => panic!("bad arguments must fail"),
        }
    }

    #[test]
    fn multiple_calls_run_sequentially_with_correlated_results() {
        let model = Arc::new(TurnsModel::new(vec![
            turn(vec![
                call_block(
                    "c1",
                    "write-file",
                    serde_json::json!({ "path": "src/one.rs", "content": "one\n" }),
                ),
                call_block(
                    "c2",
                    "write-file",
                    serde_json::json!({ "path": "src/two.rs", "content": "two\n" }),
                ),
            ]),
            final_turn(
                r#"{"evidence":[{"kind":"inspection","target":{"work_item":"work_planner"},"outcome":"info","summary":"both written"}]}"#,
            ),
        ]));
        let workspace = tempfile::Builder::new()
            .prefix("relay-planner-multi-")
            .tempdir()
            .expect("temp dir")
            .keep();
        let (mut context, _clock) = context(Arc::clone(&model));
        context.workspace_root = workspace.clone();
        let executor = WorkspaceExecutor::new(Arc::new(ModelGuidedWorkspacePlanner::new()));
        let receipt = executor.run(&spec(grant(|_| {})), &context);
        assert!(workspace.join("src/one.rs").exists());
        assert!(workspace.join("src/two.rs").exists());
        assert_eq!(receipt.usage.tool_calls, 2);
        // Results preserve arrival order and each matches its own call id.
        let requests = model.served_requests();
        let results = tool_results_of(&requests[1]);
        assert_eq!(results.len(), 2);
        assert_eq!(
            results[0],
            &relay_providers::contract::ModelMessage::tool_result(
                "c1",
                "created src/one.rs (4 bytes)",
                false
            )
            .expect("bounded")
        );
        assert_eq!(
            results[1],
            &relay_providers::contract::ModelMessage::tool_result(
                "c2",
                "created src/two.rs (4 bytes)",
                false
            )
            .expect("bounded")
        );
    }

    #[test]
    fn guided_sessions_run_against_a_real_provider_stream() {
        use crate::model::ProviderModelClient;
        use relay_providers::fake::{
            FakeMatch, FakeProvider, FakeResponse, FakeStep, FakeToolCallPlan,
        };
        // Scripted by call count: the first provider turn requests a
        // native write-file call (the fake mints the call id and streams
        // the blocks), the second carries the final object as text.
        let provider = FakeProvider::new(vec![
            FakeStep {
                r#match: FakeMatch {
                    max_call: Some(1),
                    ..FakeMatch::default()
                },
                response: FakeResponse {
                    tool_calls: Some(vec![FakeToolCallPlan {
                        name: "write-file".into(),
                        args: serde_json::json!({
                            "path": "src/from-fake.rs",
                            "content": "from the fake provider\n"
                        }),
                    }]),
                    ..FakeResponse::default()
                },
            },
            FakeStep {
                r#match: FakeMatch {
                    min_call: Some(2),
                    max_call: Some(2),
                    ..FakeMatch::default()
                },
                response: FakeResponse {
                    text: Some(
                        r#"{"outputs":[{"artifact":"art_fake","content":{"file":"src/from-fake.rs"}}],"evidence":[],"graph_changes":[]}"#
                            .into(),
                    ),
                    ..FakeResponse::default()
                },
            },
        ]);
        let workspace = tempfile::Builder::new()
            .prefix("relay-planner-fake-")
            .tempdir()
            .expect("temp dir")
            .keep();
        let (mut context, _clock) =
            context_with_model(Arc::new(ProviderModelClient::new(Arc::new(provider))));
        context.workspace_root = workspace.clone();
        let executor = WorkspaceExecutor::new(Arc::new(ModelGuidedWorkspacePlanner::new()));
        let receipt = executor.run(&spec(grant(|_| {})), &context);
        assert!(
            matches!(receipt.outcome, relay_domain::AttemptOutcome::Completed),
            "{:?}",
            receipt.outcome
        );
        assert!(workspace.join("src/from-fake.rs").exists());
        assert_eq!(receipt.outputs.len(), 1);
        assert_eq!(receipt.outputs[0].artifact.as_str(), "art_fake");
    }

    #[test]
    fn interleaved_text_and_tool_blocks_are_replayed_verbatim() {
        let blocks = vec![
            text_block("before the call "),
            call_block(
                "c1",
                "write-file",
                serde_json::json!({ "path": "src/kept.rs", "content": "kept\n" }),
            ),
            text_block("after the call"),
            relay_providers::contract::AssistantContent::Continuation {
                discriminator: "thinking".into(),
                data: "opaque-payload".into(),
            },
        ];
        let model = Arc::new(TurnsModel::new(vec![
            turn(blocks.clone()),
            final_turn(
                r#"{"evidence":[{"kind":"inspection","target":{"work_item":"work_planner"},"outcome":"info","summary":"done"}]}"#,
            ),
        ]));
        let workspace = tempfile::Builder::new()
            .prefix("relay-planner-order-")
            .tempdir()
            .expect("temp dir")
            .keep();
        let (mut context, _clock) = context(Arc::clone(&model));
        context.workspace_root = workspace.clone();
        let executor = WorkspaceExecutor::new(Arc::new(ModelGuidedWorkspacePlanner::new()));
        let receipt = executor.run(&spec(grant(|_| {})), &context);
        assert!(
            matches!(receipt.outcome, relay_domain::AttemptOutcome::Completed),
            "{:?}",
            receipt.outcome
        );
        // The next turn's transcript carries the assistant blocks
        // verbatim — interleaved order and the opaque continuation
        // included, with the tool result following the assistant turn.
        let requests = model.served_requests();
        let second = &requests[1];
        assert_eq!(
            second.messages[1],
            relay_providers::contract::ModelMessage::Assistant { content: blocks }
        );
        assert_eq!(tool_results_of(second).len(), 1);
    }

    #[test]
    fn a_turn_mixing_final_text_and_tool_calls_executes_the_tools() {
        // The turn carries BOTH a valid final object as text and a native
        // tool call: the calls execute, the text is not treated as final,
        // and the final object is requested on a later turn.
        let model = Arc::new(TurnsModel::new(vec![
            turn(vec![
                text_block(
                    r#"{"outputs":[{"artifact":"art_from_text","content":{}}],"evidence":[],"graph_changes":[]}"#,
                ),
                call_block(
                    "c1",
                    "write-file",
                    serde_json::json!({ "path": "src/ambig.rs", "content": "ambig\n" }),
                ),
            ]),
            final_turn(
                r#"{"outputs":[{"artifact":"art_final","content":{"file":"src/ambig.rs"}}],"evidence":[],"graph_changes":[]}"#,
            ),
        ]));
        let workspace = tempfile::Builder::new()
            .prefix("relay-planner-ambig-")
            .tempdir()
            .expect("temp dir")
            .keep();
        let (mut context, _clock) = context(model);
        context.workspace_root = workspace.clone();
        let executor = WorkspaceExecutor::new(Arc::new(ModelGuidedWorkspacePlanner::new()));
        let receipt = executor.run(&spec(grant(|_| {})), &context);
        // The tool really ran and the FINAL turn's proposal won.
        assert!(workspace.join("src/ambig.rs").exists());
        assert_eq!(receipt.outputs.len(), 1);
        assert_eq!(receipt.outputs[0].artifact.as_str(), "art_final");
    }

    #[test]
    fn malformed_final_text_gets_a_remediation_turn_and_the_loop_continues() {
        let model = Arc::new(TurnsModel::new(vec![
            final_turn("not json at all"),
            final_turn(
                r#"{"evidence":[{"kind":"inspection","target":{"work_item":"work_planner"},"outcome":"info","summary":"recovered"}]}"#,
            ),
        ]));
        let (context, _clock) = context(Arc::clone(&model));
        let executor = WorkspaceExecutor::new(Arc::new(ModelGuidedWorkspacePlanner::new()));
        let receipt = executor.run(&spec(grant(|_| {})), &context);
        assert!(
            matches!(receipt.outcome, relay_domain::AttemptOutcome::Completed),
            "{:?}",
            receipt.outcome
        );
        // The remediation note rode the transcript as a user turn.
        let requests = model.served_requests();
        assert_eq!(requests.len(), 2);
        let second = &requests[1];
        assert_eq!(second.messages.len(), 3, "opening + bad assistant + note");
        match &second.messages[2] {
            relay_providers::contract::ModelMessage::User { content } => {
                let note = content[0].text();
                assert!(note.contains("was not the final JSON object"), "{note}");
                assert!(!note.contains("tool_calls"), "{note}");
            }
            other => panic!("expected a remediation user turn, got {other:?}"),
        }
    }

    #[test]
    fn cancellation_between_calls_stops_the_session() {
        // The model cancels the attempt while serving its first (tool
        // calling) turn: the pending call is answered with a canceled
        // error result, no further model turn happens.
        let model = Arc::new(TurnsModel::cancel_on_first_turn(vec![turn(vec![
            call_block(
                "c1",
                "write-file",
                serde_json::json!({ "path": "src/gone.rs", "content": "gone\n" }),
            ),
        ])]));
        let workspace = tempfile::Builder::new()
            .prefix("relay-planner-cancel-")
            .tempdir()
            .expect("temp dir")
            .keep();
        let (mut context, _clock) = context(Arc::clone(&model));
        context.workspace_root = workspace.clone();
        let executor = WorkspaceExecutor::new(Arc::new(ModelGuidedWorkspacePlanner::new()));
        let receipt = executor.run(&spec(grant(|_| {})), &context);
        assert!(!workspace.join("src/gone.rs").exists());
        assert_eq!(
            model.served_requests().len(),
            1,
            "no model turn after cancellation"
        );
        match receipt.outcome {
            relay_domain::AttemptOutcome::Failed { reason } => {
                assert!(reason.contains("canceled"), "{reason}");
            }
            relay_domain::AttemptOutcome::Completed => panic!("canceled work must not complete"),
        }
    }

    #[test]
    fn budget_exhaustion_between_calls_stops_the_session() {
        let model = Arc::new(TurnsModel::new(vec![
            turn(vec![call_block(
                "c1",
                "write-file",
                serde_json::json!({ "path": "src/budget.rs", "content": "budget\n" }),
            )]),
            final_turn(
                r#"{"evidence":[{"kind":"inspection","target":{"work_item":"work_planner"},"outcome":"info","summary":"never"}]}"#,
            ),
        ]));
        let (context, _clock) = context_with_budget(
            model,
            Budget {
                max_attempts: Some(1),
                ..Budget::default()
            },
        );
        let executor = WorkspaceExecutor::new(Arc::new(ModelGuidedWorkspacePlanner::new()));
        let receipt = executor.run(&spec(grant(|_| {})), &context);
        match receipt.outcome {
            relay_domain::AttemptOutcome::Failed { reason } => {
                assert!(reason.contains("canceled or budget-stopped"), "{reason}");
            }
            relay_domain::AttemptOutcome::Completed => {
                panic!("budget-stopped work must not complete")
            }
        }
    }

    #[test]
    fn visible_tools_are_grant_filtered_registry_definitions() {
        let registry =
            create_default_tool_registry(&relay_config::ToolSettings::default()).expect("registry");
        let write_definition = registry.get("write-file").expect("write-file");
        // Full grant: write-file is visible with the registry's own schema.
        let (full_context, _clock) = context(Arc::new(TurnsModel::new(vec![final_turn("{}")])));
        let runner = ToolRunner::new(&spec(grant(|_| {})), &full_context);
        let tools = runner.visible_tools();
        let write = tools
            .iter()
            .find(|tool| tool.name == "write-file")
            .expect("write-file visible");
        assert_eq!(write.description, write_definition.description);
        assert_eq!(
            write.parameters,
            relay_tools::tool_json_schema(&write_definition),
            "the model sees exactly the schema the runtime validates against"
        );
        // A read-only grant never even shows write tools.
        let (read_context, _clock) = context(Arc::new(TurnsModel::new(vec![final_turn("{}")])));
        let runner = ToolRunner::new(
            &spec(grant(|value| {
                value
                    .operations
                    .retain(|operation| *operation != Operation::Write);
            })),
            &read_context,
        );
        assert!(
            runner
                .visible_tools()
                .iter()
                .all(|tool| tool.name != "write-file"),
            "write tools must be invisible without the write operation"
        );
    }

    #[test]
    fn tool_result_content_is_bounded_for_the_model() {
        let message = tool_result_message("c1", &"x".repeat(100_000), false);
        match message {
            relay_providers::contract::ModelMessage::ToolResult {
                content, is_error, ..
            } => {
                assert_eq!(content.chars().count(), MAX_TOOL_RESULT_TO_MODEL_CHARS);
                assert!(!is_error);
            }
            other => panic!("expected a tool result, got {other:?}"),
        }
        let message = tool_result_message("c2", "failed", true);
        match message {
            relay_providers::contract::ModelMessage::ToolResult {
                content, is_error, ..
            } => {
                assert_eq!(content, "failed");
                assert!(is_error);
            }
            other => panic!("expected a tool result, got {other:?}"),
        }
    }

    #[test]
    fn malformed_final_output_fails_with_actionable_reasons() {
        let model = Arc::new(TurnsModel::new(vec![final_turn(
            r#"{"outputs":[{"artifact":"NOT_AN_ID","content":{}}]}"#,
        )]));
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
        let model = Arc::new(TurnsModel::new(vec![final_turn("not json at all")]));
        let (context, _clock) = context(Arc::clone(&model));
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
        // Every remediation retry was a real model turn.
        assert_eq!(model.served_requests().len(), 3);
    }

    #[test]
    fn verifier_evidence_must_target_the_criterion() {
        let criterion = ArtifactVersionRef::new(
            relay_domain::ArtifactId::parse("art_criterion").unwrap(),
            relay_domain::ArtifactVersion::FIRST,
        );
        let model = Arc::new(TurnsModel::new(vec![final_turn(
            r#"{"evidence":[{"kind":"test_run","target":{"work_item":"work_someone-else"},"outcome":"pass","summary":"wrong target"}]}"#,
        )]));
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

        let model = Arc::new(TurnsModel::new(vec![final_turn(
            r#"{"evidence":[{"kind":"test_run","target":{"artifact_version":{"artifact":"art_criterion","version":1}},"outcome":"pass","summary":"all green"}]}"#,
        )]));
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
        let model = Arc::new(TurnsModel::new(vec![final_turn(
            r#"{"outputs":[{"artifact":"art_smuggle","content":{}}]}"#,
        )]));
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
    fn prompts_carry_no_textual_tool_protocol() {
        let planner = ModelGuidedWorkspacePlanner::new();
        let system = planner.system_prompt();
        // The final-object protocol stays; the synthetic tool protocol is
        // gone from every prompt surface.
        assert!(system.contains("graph_changes"));
        for prompt in [
            system.as_str(),
            planner
                .opening_prompt("do the thing", &["art_brief@1".to_owned()])
                .as_str(),
            ModelGuidedVerifierPlanner::new().system_prompt().as_str(),
            ModelGuidedVerifierPlanner::for_criterion(ArtifactVersionRef::new(
                relay_domain::ArtifactId::parse("art_c").unwrap(),
                relay_domain::ArtifactVersion::FIRST,
            ))
            .system_prompt()
            .as_str(),
            crate::executors::ModelExecutor::system_prompt().as_str(),
            crate::executors::ModelExecutor::opening_prompt("obj", &[]).as_str(),
            crate::executors::ModelExecutor::remediation_prompt(&["bad".to_owned()]).as_str(),
            malformed_final_note("x").as_str(),
        ] {
            assert!(!prompt.contains("tool_calls"), "{prompt}");
            assert!(!prompt.contains("\"parameters\""), "{prompt}");
            assert!(!prompt.contains("Tools you may use"), "{prompt}");
        }
        let opening = planner.opening_prompt("do the thing", &["art_brief@1".to_owned()]);
        assert!(opening.contains("do the thing"));
        assert!(opening.contains("art_brief@1"));
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
