//! The conversational application service.
//!
//! A conversation turn is a bounded, typed exchange over one workspace:
//! the user's prompt is recorded durably first, a bounded context is built
//! from the transcript tail plus safe graph views, the provider streams
//! visible prose plus typed proposals, and every proposal is validated and
//! applied independently through the existing application services. Model
//! prose never mutates the work graph; model *proposals* only become state
//! after deterministic validation accepts them.
//!
//! This module owns the Relay-side contracts of that flow: the proposal
//! grammar and its runtime decode, the JSON Schema handed to providers, the
//! conversation system prompt, the context budget/assembly rules, and the
//! redaction/bounding every durable or streamed text passes through. The
//! turn orchestration itself ([`start_turn`]) composes these over the
//! session, the provider port, and the command handlers.
//!
//! Error authority: a *failed or cancelled turn* is a successful service
//! call carrying a truthful [`TurnStatus`]; [`AppError`] is reserved for
//! infrastructure failures (store/IO) where no terminal fact could be
//! recorded.

use std::collections::BTreeSet;
use std::sync::mpsc::Sender;

use relay_domain::Speaker;
use relay_domain::{
    AttemptId, ConversationId, DecisionId, DomainFact, MessageId, ProjectGraph, TurnId, WorkItemId,
};
use relay_engine::Evaluation;
use relay_engine::schedule::Autonomy;
use relay_engine::schedule::SchedulingPolicy;
use relay_providers::collect::collect_turn;
use relay_providers::contract::CancelToken;
use relay_providers::contract::FinishReason;
use relay_providers::contract::JsonSchemaObject;
use relay_providers::contract::ModelMessage;
use relay_providers::contract::ModelRequest;
use relay_providers::contract::ProviderErrorKind;
use relay_providers::contract::ProviderStreamEvent;
use relay_providers::contract::StructuredOutputRequest;
use relay_providers::contract::TokenUsage;
use relay_workflows::WorkflowCatalog;
use serde_json::Value;
use serde_json::json;

use crate::error::AppError;
use crate::runtime_routing::TurnRouting;
use crate::session::ProjectSession;

/// The provider purpose marker for conversational turns (routing logs and
/// fixtures key on it).
pub const CONVERSATION_PURPOSE: &str = "conversation";

/// The logical name of the structured output a conversational turn requests
/// (`{"proposals": [...]}` per [`proposal_schema`]).
pub const STRUCTURED_OUTPUT_NAME: &str = "conversation-turn";

/// The wire tags of the five proposal variants, in declaration order. The
/// serde tags, the schema `const`s, and this table must stay identical.
pub const PROPOSAL_TAGS: [&str; 5] = [
    "declare-goal",
    "expand-work",
    "resolve-decision",
    "run-ready-work",
    "request-clarification",
];

/// The inclusive bounds a decoded `run-ready-work` proposal's `max_ticks`
/// must lie within (the service clamps further before running).
pub const MAX_TICKS_MIN: usize = 1;
/// The inclusive upper bound of a decoded `max_ticks`.
pub const MAX_TICKS_MAX: usize = 64;

/// The truncation marker appended when a text is bounded (see
/// [`bound_chars`]).
pub(crate) const TRUNCATION_MARKER: &str = "…[truncated]";

/// Explicit budgets for one conversational turn. Every number is enforced,
/// never advisory: transcript inclusion, context assembly, persisted
/// message text, and streamed fragments are all bounded by these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConversationLimits {
    /// How many transcript messages may enter the model context.
    pub transcript_context_messages: usize,
    /// Byte budget for the transcript messages that enter the context.
    pub transcript_context_bytes: usize,
    /// Per-message character cap for user/assistant text — kept equal to the
    /// domain's message-text cap so no bounded text can fail fact validation.
    pub assistant_max_chars: usize,
    /// Total character budget for everything sent as context (sections +
    /// transcript + prompt). The system prompt is bounded separately.
    pub context_max_chars: usize,
    /// How many typed proposals one turn may carry.
    pub max_proposals: usize,
}

impl Default for ConversationLimits {
    fn default() -> Self {
        ConversationLimits {
            transcript_context_messages: 24,
            transcript_context_bytes: 32_768,
            assistant_max_chars: 16_384,
            context_max_chars: 48_000,
            max_proposals: 8,
        }
    }
}

/// One typed action a model proposed for a turn. Serde shape is the wire
/// contract: internally tagged on `"type"`, kebab-case tags, exactly the
/// tags in [`PROPOSAL_TAGS`].
///
/// A proposal is a *request* — it becomes state only after the
/// deterministic application services accept it (see [`ProposalOutcome`]).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum ConversationProposal {
    /// Declare the project goal when none exists yet.
    DeclareGoal {
        /// The outcome the user wants, in their words.
        outcome: String,
        /// How success is recognized, when stated.
        success_definition: Option<String>,
    },
    /// Apply the next workflow expansion round.
    ExpandWork,
    /// Answer one exact open decision.
    ResolveDecision {
        /// The decision id (`dec_…`).
        decision: String,
        /// The chosen option label (or free-form answer).
        choice: String,
        /// Why, when the model reasoned about it.
        rationale: Option<String>,
    },
    /// Run ready work under a bounded autonomy.
    RunReadyWork {
        /// Autonomy name: `manual`, `safe`, `until-decision`, or
        /// `bounded:<n>`.
        autonomy: Option<String>,
        /// Tick bound (clamped by the service before running).
        max_ticks: Option<usize>,
    },
    /// Ask the user a clarifying question instead of acting.
    RequestClarification {
        /// The question to surface.
        question: String,
    },
}

/// What happened to one proposed action: accepted (and applied through the
/// existing services) or rejected with a safe, bounded reason. Rejections
/// are ordinary answers — a rejected proposal is never coerced into a
/// mutation.
#[derive(Debug, Clone, PartialEq)]
pub struct ProposalOutcome {
    /// The proposal this outcome describes.
    pub proposal: ConversationProposal,
    /// Whether deterministic validation accepted and applied it.
    pub accepted: bool,
    /// A bounded success summary or a safe rejection reason.
    pub detail: String,
}

/// The terminal state of one turn.
#[derive(Debug, Clone, PartialEq)]
pub enum TurnStatus {
    /// The assistant message was recorded and the turn ended normally.
    Completed,
    /// The provider failed; the failure fact is durable.
    Failed {
        /// The safe, redacted, bounded failure message.
        message: String,
    },
    /// The turn was cancelled; the cancellation fact is durable.
    Cancelled {
        /// The safe, redacted, bounded cancellation reason.
        reason: String,
    },
}

/// The typed result of one conversational turn: what the assistant said,
/// what happened to every proposal, and the truthful terminal status. A
/// failed or cancelled turn is a *successful* service call carrying its
/// status; [`AppError`] is reserved for infrastructure failures where no
/// terminal fact could be recorded.
#[derive(Debug, Clone, PartialEq)]
pub struct ConversationTurnResult {
    /// The conversation the turn belongs to.
    pub conversation: ConversationId,
    /// The turn's identity.
    pub turn: TurnId,
    /// The final assistant text (redacted, bounded). Empty for failed and
    /// cancelled turns, which record no assistant message.
    pub assistant_text: String,
    /// What happened to each decoded proposal, in proposal order.
    pub proposals: Vec<ProposalOutcome>,
    /// The terminal state.
    pub status: TurnStatus,
    /// Provider token usage for the model call, when the turn reached the
    /// provider.
    pub usage: Option<TokenUsage>,
}

/// Ephemeral progress events streamed to the live interface while a turn
/// runs. Every text is redacted and bounded; nothing here is durable —
/// durability lives in the conversation facts alone.
#[derive(Debug, Clone, PartialEq)]
pub enum ConversationEvent {
    /// The user message was accepted and the turn started.
    TurnStarted {
        /// The conversation.
        conversation: ConversationId,
        /// The turn.
        turn: TurnId,
        /// The recorded user text (redacted, bounded).
        user_text: String,
    },
    /// A fragment of assistant prose (bounded, redacted).
    AssistantDelta {
        /// The fragment text.
        text: String,
    },
    /// One proposal was decided (accepted or safely rejected).
    ProposalDecided {
        /// The proposal's index in the decoded list.
        index: usize,
        /// Whether deterministic validation accepted it.
        accepted: bool,
        /// The bounded outcome detail.
        detail: String,
    },
    /// One compact bounded line of work activity.
    Activity {
        /// The activity line.
        text: String,
    },
    /// The turn reached a terminal state; the result is durable.
    TurnFinished {
        /// The final turn result.
        result: ConversationTurnResult,
    },
}

/// The tick ceiling the service clamps every `run-ready-work` proposal to
/// (deterministic policy: conversational runs stay small).
pub const RUN_MAX_TICKS: usize = 4;

/// The autonomy a `run-ready-work` proposal runs under when it names none
/// (the engine's default autonomy).
pub const DEFAULT_RUN_AUTONOMY: Autonomy = Autonomy::Safe;

/// How many ready-work lines enter the context block (top-K by the
/// documented (phase, id) tie-break).
pub(crate) const CONTEXT_READY_TOP_K: usize = 8;

/// Characters held back while streaming so no secret can straddle the
/// emitted boundary: every matched shape is longer than a fragment of this
/// size, so a partial secret never reaches display.
pub(crate) const STREAM_HOLDBACK_CHARS: usize = 256;

/// The per-event character cap for streamed assistant fragments.
pub(crate) const STREAM_FRAGMENT_CAP_CHARS: usize = 1_024;

/// The character cap for one proposal detail or activity line.
pub(crate) const DETAIL_CAP_CHARS: usize = 512;

/// Token shapes whose *start* must never be emitted before the full shape
/// has arrived and been redacted (mirrors `relay_tools::redact`'s rules).
const SECRET_STARTS: [&str; 7] = [
    "sk-ant-",
    "sk-",
    "AKIA",
    "ghp_",
    "xox",
    "Bearer ",
    "-----BEGIN",
];

// ---------------------------------------------------------------------------
// Proposal decoding
// ---------------------------------------------------------------------------

/// Decode and validate the structured output a conversational turn
/// produced: an object `{"proposals": [<tagged objects>]}`. Fails closed
/// with an actionable [`AppError::Rejected`] on anything malformed —
/// unknown tags, missing/empty required fields, out-of-range `max_ticks`,
/// unknown autonomy names, oversized strings, or more proposals than the
/// limits allow. An empty (or absent-from-result) proposal list is valid:
/// it means the turn only answered.
pub fn decode_proposals(
    value: &Value,
    limits: &ConversationLimits,
) -> Result<Vec<ConversationProposal>, AppError> {
    let Some(items) = value.get("proposals") else {
        return Err(reject(
            "structured output must be an object carrying a \"proposals\" array",
        ));
    };
    let Value::Array(items) = items else {
        return Err(reject(
            "\"proposals\" must be an array of typed proposal objects",
        ));
    };
    if items.len() > limits.max_proposals {
        return Err(reject(&format!(
            "the turn proposed {} action(s) but at most {} are accepted per turn",
            items.len(),
            limits.max_proposals
        )));
    }
    let mut proposals = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        proposals.push(decode_proposal(index, item, limits)?);
    }
    Ok(proposals)
}

/// Decode and bounds-check one proposal element.
fn decode_proposal(
    index: usize,
    item: &Value,
    limits: &ConversationLimits,
) -> Result<ConversationProposal, AppError> {
    let Some(tag) = item.get("type").and_then(Value::as_str) else {
        return Err(reject(&format!(
            "proposals[{index}]: missing the \"type\" tag (expected one of: {})",
            PROPOSAL_TAGS.join(", ")
        )));
    };
    if !PROPOSAL_TAGS.contains(&tag) {
        return Err(reject(&format!(
            "proposals[{index}]: unknown proposal type \"{tag}\" (expected one of: {})",
            PROPOSAL_TAGS.join(", ")
        )));
    }
    let proposal: ConversationProposal = serde_json::from_value(item.clone()).map_err(|error| {
        reject(&format!(
            "proposals[{index}] (\"{tag}\") did not decode: {error}"
        ))
    })?;
    validate_proposal(index, &proposal, limits)?;
    Ok(proposal)
}

/// Field-level bounds for one decoded proposal.
fn validate_proposal(
    index: usize,
    proposal: &ConversationProposal,
    limits: &ConversationLimits,
) -> Result<(), AppError> {
    let cap = limits.assistant_max_chars;
    match proposal {
        ConversationProposal::DeclareGoal {
            outcome,
            success_definition,
        } => {
            require_non_empty(index, "outcome", outcome)?;
            cap_string(index, "outcome", outcome, cap)?;
            if let Some(definition) = success_definition {
                require_non_empty(index, "success_definition", definition)?;
                cap_string(index, "success_definition", definition, cap)?;
            }
        }
        ConversationProposal::ExpandWork => {}
        ConversationProposal::ResolveDecision {
            decision,
            choice,
            rationale,
        } => {
            require_non_empty(index, "decision", decision)?;
            require_non_empty(index, "choice", choice)?;
            cap_string(index, "decision", decision, cap)?;
            cap_string(index, "choice", choice, cap)?;
            if let Some(reason) = rationale {
                require_non_empty(index, "rationale", reason)?;
                cap_string(index, "rationale", reason, cap)?;
            }
        }
        ConversationProposal::RunReadyWork {
            autonomy,
            max_ticks,
        } => {
            if let Some(name) = autonomy {
                require_non_empty(index, "autonomy", name)?;
                if parse_autonomy(name).is_none() {
                    return Err(reject(&format!(
                        "proposals[{index}]: unknown autonomy \"{name}\" (expected manual, safe, \
                         until-decision, or bounded:<n>)"
                    )));
                }
            }
            if let Some(ticks) = max_ticks
                && !(MAX_TICKS_MIN..=MAX_TICKS_MAX).contains(ticks)
            {
                return Err(reject(&format!(
                    "proposals[{index}]: max_ticks {ticks} is outside {MAX_TICKS_MIN}..={MAX_TICKS_MAX}"
                )));
            }
        }
        ConversationProposal::RequestClarification { question } => {
            require_non_empty(index, "question", question)?;
            cap_string(index, "question", question, cap)?;
        }
    }
    Ok(())
}

fn require_non_empty(index: usize, field: &str, value: &str) -> Result<(), AppError> {
    if value.trim().is_empty() {
        return Err(reject(&format!(
            "proposals[{index}]: \"{field}\" must not be empty"
        )));
    }
    Ok(())
}

fn cap_string(index: usize, field: &str, value: &str, cap: usize) -> Result<(), AppError> {
    if value.chars().count() > cap {
        return Err(reject(&format!(
            "proposals[{index}]: \"{field}\" is longer than the {cap}-character message cap"
        )));
    }
    Ok(())
}

fn reject(detail: &str) -> AppError {
    AppError::Rejected(format!("conversation proposals rejected: {detail}"))
}

/// Parse an autonomy name as proposals carry it: `manual`, `safe`,
/// `until-decision`, or `bounded:<n>`. Shared with the view grammar so the
/// model and the interface spell autonomy identically.
pub fn parse_autonomy(text: &str) -> Option<relay_engine::schedule::Autonomy> {
    use relay_engine::schedule::Autonomy;
    match text.trim() {
        "manual" => Some(Autonomy::Manual),
        "safe" => Some(Autonomy::Safe),
        "until-decision" => Some(Autonomy::UntilDecision),
        other => other
            .strip_prefix("bounded:")
            .and_then(|count| count.parse::<u32>().ok())
            .filter(|count| *count > 0)
            .map(Autonomy::Bounded),
    }
}

// ---------------------------------------------------------------------------
// Provider-facing schema and prompt
// ---------------------------------------------------------------------------

/// The JSON Schema handed to providers for [`STRUCTURED_OUTPUT_NAME`]: an
/// object whose `proposals` array holds internally tagged objects with
/// exactly the tags of [`ConversationProposal`]. The schema mirrors
/// `decode_proposal`'s validation; decode remains the authority because
/// models can still produce off-schema output.
pub fn proposal_schema(limits: &ConversationLimits) -> JsonSchemaObject {
    let string_field = || object(json!({ "type": "string" }));
    let optional_string = || object(json!({ "type": ["string", "null"] }));
    let variants = vec![
        variant(
            "declare-goal",
            json!({
                "outcome": string_field(),
                "success_definition": optional_string(),
            }),
            json!(["outcome"]),
        ),
        variant("expand-work", json!({}), json!([])),
        variant(
            "resolve-decision",
            json!({
                "decision": string_field(),
                "choice": string_field(),
                "rationale": optional_string(),
            }),
            json!(["decision", "choice"]),
        ),
        variant(
            "run-ready-work",
            json!({
                "autonomy": optional_string(),
                "max_ticks": object(json!({ "type": ["integer", "null"], "minimum": MAX_TICKS_MIN, "maximum": MAX_TICKS_MAX })),
            }),
            json!([]),
        ),
        variant(
            "request-clarification",
            json!({
                "question": string_field(),
            }),
            json!(["question"]),
        ),
    ];
    object(json!({
        "type": "object",
        "properties": {
            "proposals": {
                "type": "array",
                "maxItems": limits.max_proposals,
                "items": { "oneOf": variants },
            }
        },
        "required": ["proposals"],
        "additionalProperties": false,
    }))
}

/// One internally tagged variant schema: a `type` const plus its fields.
fn variant(tag: &str, fields: Value, required: Value) -> Value {
    let mut properties = object(fields);
    properties.insert(
        "type".to_string(),
        json!({ "const": tag, "type": "string" }),
    );
    let mut required = match required {
        Value::Array(labels) => labels,
        _ => Vec::new(),
    };
    required.insert(0, json!("type"));
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

/// Extract a JSON object from a literal `json!` value. Literal object
/// expressions always match; the fallback keeps the function total.
fn object(value: Value) -> JsonSchemaObject {
    match value {
        Value::Object(map) => map,
        _ => JsonSchemaObject::new(),
    }
}

/// The cap on the conversation system prompt itself — it is context the
/// provider sees, so it is bounded like every other context text.
pub const SYSTEM_PROMPT_CAP_CHARS: usize = 4_000;

/// The system prompt for a conversational turn: bounded, and instructive
/// about the one rule that matters — prose answers, actions only through
/// the typed proposals.
pub fn conversation_system_prompt(limits: &ConversationLimits) -> String {
    let prompt = format!(
        "You are Relay, a conversational development assistant operating inside one workspace. \
         You are given the workspace's Relay work-graph context, the recent conversation, and the \
         user's latest message.\n\n\
         Answer concisely and truthfully in plain prose. You cannot read or write files during a \
         conversation turn; everything you know about the workspace is in the provided context.\n\n\
         Any state change you want must be proposed through the structured output \
         \"{STRUCTURED_OUTPUT_NAME}\" as typed proposals:\n\
         - declare-goal (outcome, success_definition?): only when the context shows no goal yet.\n\
         - expand-work (): apply the next workflow expansion round.\n\
         - resolve-decision (decision, choice, rationale?): answer one exact open decision by its \
         id, choosing one of its declared options.\n\
         - run-ready-work (autonomy?, max_ticks?): run ready work, bounded.\n\
         - request-clarification (question): ask the user instead of guessing.\n\n\
         Propose at most one mutating action per turn (declare-goal, expand-work, \
         resolve-decision, or run-ready-work); at most {max} proposals total. Proposals are \
         validated by deterministic policy and may be rejected — never claim an action happened; \
         the transcript records what was actually applied. Never echo secrets or credentials.",
        max = limits.max_proposals,
    );
    bound_chars(&prompt, SYSTEM_PROMPT_CAP_CHARS)
}

// ---------------------------------------------------------------------------
// Redaction and bounding
// ---------------------------------------------------------------------------

/// Cap `text` at `max_chars` characters (not bytes), keeping the head and
/// appending [`TRUNCATION_MARKER`] when anything was dropped. Unicode-safe:
/// the cut never splits a character. A budget smaller than the marker itself
/// falls back to a hard cut — the cap always holds. A zero budget yields an
/// empty string.
pub(crate) fn bound_chars(text: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    let marker_chars = TRUNCATION_MARKER.chars().count();
    if max_chars < marker_chars {
        return text.chars().take(max_chars).collect();
    }
    let keep = max_chars - marker_chars;
    let head: String = text.chars().take(keep).collect();
    format!("{head}{TRUNCATION_MARKER}")
}

/// Prepare one text for anything durable or streamed: redact secrets first,
/// then bound. Redacting before bounding guarantees the cap can never split
/// a secret out of its match. Every user/assistant message, activity line,
/// streamed fragment, and failure reason passes through here.
pub(crate) fn sanitize_for_record(text: &str, max_chars: usize) -> String {
    bound_chars(&relay_tools::redact::redact_secrets(text), max_chars)
}

// ---------------------------------------------------------------------------
// Context assembly
// ---------------------------------------------------------------------------

/// Per-section caps inside the workspace-context block (characters).
pub(crate) const SECTION_CAPS: (usize, usize, usize) = (4_000, 2_000, 2_000);

/// Per-line cap for decision and ready-work lines inside the context block.
pub(crate) const CONTEXT_LINE_CAP_CHARS: usize = 400;

/// A character floor below which truncating the newest transcript message
/// would be meaningless — it is dropped instead.
const MEANINGFUL_TRUNCATION_CHARS: usize = TRUNCATION_MARKER.len() + 16;

/// Who spoke one transcript entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContextRole {
    /// The user.
    User,
    /// The assistant.
    Assistant,
}

/// One transcript entry feeding the context builder. Stage 2 fills these
/// from the graph's transcript tail; the builder only ever sees bounded,
/// ordered entries.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ContextTranscriptEntry {
    /// Who spoke.
    pub role: ContextRole,
    /// What they said (already redacted/bounded per message when durable).
    pub text: String,
}

/// Everything one turn may show the model, assembled by the caller from
/// session reads (stage 2: transcript tail, goal/counts summary, open
/// decisions, ready-work top-K) plus the new prompt.
#[derive(Debug, Clone, Default)]
pub(crate) struct ContextInputs<'a> {
    /// The conversation transcript, oldest first.
    pub transcript: &'a [ContextTranscriptEntry],
    /// The bounded workspace/graph summary block (may be empty).
    pub graph_summary: &'a str,
    /// Open-decision lines (`dec_…: question (options: …)`).
    pub open_decisions: &'a [String],
    /// Ready-work lines (`work_…: objective`), already top-K.
    pub ready_work: &'a [String],
    /// The user's new prompt.
    pub prompt: &'a str,
}

/// What the context builder had to trim, for truthful reporting.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ContextTrim {
    /// Transcript entries that did not make it into the context (window or
    /// budget excluded).
    pub transcript_messages_dropped: usize,
    /// Whether the newest transcript message itself was cut short.
    pub transcript_truncated: bool,
    /// Whether the workspace-context block was cut short.
    pub sections_truncated: bool,
}

/// The assembled provider messages plus the trim report.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct BuiltContext {
    /// The context messages: optional context block, transcript tail in
    /// chronological order, then the prompt.
    pub messages: Vec<ModelMessage>,
    /// What was trimmed to fit the budgets.
    pub trim: ContextTrim,
}

/// Assemble the bounded model context for one turn.
///
/// Deterministic precedence when budgets bind: the prompt always keeps its
/// share (capped by both the per-message and total budgets), then the
/// workspace-context block, then the transcript tail (newest kept first,
/// the newest message truncated rather than dropped when that is the only
/// way to keep any of it). Transcript selection enforces both the message
/// count and the byte budget; the total character budget spans sections +
/// transcript + prompt. Every included text is redacted and bounded.
pub(crate) fn build_context(
    inputs: &ContextInputs<'_>,
    limits: &ConversationLimits,
) -> BuiltContext {
    let mut trim = ContextTrim::default();

    // 1. The prompt: capped by the per-message and total budgets.
    let prompt_cap = limits
        .context_max_chars
        .min(limits.assistant_max_chars)
        .max(1);
    let prompt_text = sanitize_for_record(inputs.prompt, prompt_cap);
    let prompt_chars = prompt_text.chars().count();

    // 2. The workspace-context block, inside what the total budget leaves.
    let sections_budget = limits.context_max_chars.saturating_sub(prompt_chars);
    let mut block = sections_block(inputs, &mut trim);
    if block.chars().count() > sections_budget {
        block = bound_chars(&block, sections_budget);
        trim.sections_truncated = true;
    }
    let block_chars = block.chars().count();

    // 3. The transcript tail: newest first within count, byte, and the
    //    remaining character budgets.
    let transcript_chars = limits
        .context_max_chars
        .saturating_sub(prompt_chars + block_chars);
    let (entries, entry_trim) = select_transcript(inputs.transcript, limits, transcript_chars);
    trim.transcript_messages_dropped = inputs.transcript.len() - entries.len();
    trim.transcript_truncated = entry_trim;

    // 4. Assemble: context block, transcript in chronological order, prompt.
    let mut messages = Vec::with_capacity(entries.len() + 2);
    if !block.is_empty() {
        messages.push(ModelMessage::User { content: block });
    }
    for entry in entries {
        match entry.role {
            ContextRole::User => messages.push(ModelMessage::User {
                content: entry.text,
            }),
            ContextRole::Assistant => messages.push(ModelMessage::Assistant {
                content: vec![relay_providers::contract::AssistantContent::Text {
                    text: entry.text,
                }],
            }),
        }
    }
    messages.push(ModelMessage::User {
        content: prompt_text,
    });
    BuiltContext { messages, trim }
}

/// Render the workspace-context block: summary, open decisions, ready work.
/// Empty sections are omitted; an entirely empty workspace yields an empty
/// block (no context message at all).
fn sections_block(inputs: &ContextInputs<'_>, trim: &mut ContextTrim) -> String {
    let (summary_cap, decisions_cap, ready_cap) = SECTION_CAPS;
    let mut block = String::new();
    let summary = sanitize_for_record(inputs.graph_summary, summary_cap);
    if summary.contains(TRUNCATION_MARKER) {
        trim.sections_truncated = true;
    }
    if !summary.trim().is_empty() {
        block.push_str("Relay workspace context:\n");
        block.push_str(&summary);
        block.push('\n');
    }
    let decisions = line_block(
        "Open decisions (answerable via resolve-decision):",
        inputs.open_decisions,
        decisions_cap,
    );
    if decisions.contains(TRUNCATION_MARKER) {
        trim.sections_truncated = true;
    }
    block.push_str(&decisions);
    let ready = line_block(
        "Ready work (runnable via run-ready-work):",
        inputs.ready_work,
        ready_cap,
    );
    if ready.contains(TRUNCATION_MARKER) {
        trim.sections_truncated = true;
    }
    block.push_str(&ready);
    block
}

/// Render one bulleted section, redacting and bounding each line and the
/// whole block. Returns an empty string when there are no lines.
fn line_block(heading: &str, lines: &[String], cap: usize) -> String {
    if lines.is_empty() {
        return String::new();
    }
    let mut block = format!("\n{heading}\n");
    for line in lines {
        let bounded = sanitize_for_record(line, CONTEXT_LINE_CAP_CHARS);
        block.push_str("- ");
        block.push_str(&bounded);
        block.push('\n');
    }
    bound_chars(&block, cap.max(block.chars().count()))
}

/// Select the transcript tail: at most `limits.transcript_context_messages`
/// entries, newest first, while the byte budget and `chars_budget` hold.
/// The newest entry is truncated (not dropped) when it alone exceeds the
/// budgets but meaningful room remains. Returns the kept entries in
/// chronological order plus whether the newest was truncated.
fn select_transcript(
    transcript: &[ContextTranscriptEntry],
    limits: &ConversationLimits,
    chars_budget: usize,
) -> (Vec<ContextTranscriptEntry>, bool) {
    let window_start = transcript
        .len()
        .saturating_sub(limits.transcript_context_messages);
    let tail = &transcript[window_start..];
    let mut kept_newest_first: Vec<ContextTranscriptEntry> = Vec::new();
    let mut bytes_left = limits.transcript_context_bytes;
    let mut chars_left = chars_budget;
    let mut newest_truncated = false;
    for (position, entry) in tail.iter().rev().enumerate() {
        let text = sanitize_for_record(&entry.text, limits.assistant_max_chars);
        let bytes = text.len();
        let chars = text.chars().count();
        if bytes <= bytes_left && chars <= chars_left {
            bytes_left -= bytes;
            chars_left -= chars;
            kept_newest_first.push(ContextTranscriptEntry {
                role: entry.role,
                text,
            });
            continue;
        }
        // Only the newest entry may be cut short; an older entry that does
        // not fit ends selection (older entries are strictly less relevant).
        if position == 0 {
            // Shrink until BOTH budgets hold after the marker (the marker's
            // multibyte '…' makes a character cap alone insufficient for the
            // byte budget). The floor keeps the cut meaningful.
            let mut room = chars_left.min(bytes_left);
            let mut truncated = bound_chars(&text, room);
            while room >= MEANINGFUL_TRUNCATION_CHARS
                && (truncated.chars().count() > chars_left || truncated.len() > bytes_left)
            {
                room -= 1;
                truncated = bound_chars(&text, room);
            }
            if room >= MEANINGFUL_TRUNCATION_CHARS {
                bytes_left = bytes_left.saturating_sub(truncated.len());
                chars_left = chars_left.saturating_sub(truncated.chars().count());
                newest_truncated = true;
                kept_newest_first.push(ContextTranscriptEntry {
                    role: entry.role,
                    text: truncated,
                });
                continue;
            }
        }
        break;
    }
    kept_newest_first.reverse();
    (kept_newest_first, newest_truncated)
}

// ---------------------------------------------------------------------------
// Turn orchestration
// ---------------------------------------------------------------------------

/// Run one conversational turn end to end.
///
/// Authority flow (plan §"Authority and turn flow"): select or start the
/// latest conversation and record the accepted user message under one short
/// store lock, release it, build the bounded context, call the provider
/// while forwarding redacted delta fragments, decode and apply every typed
/// proposal independently through the existing application services, then
/// append the terminal assistant-message (or failure/cancellation) fact.
/// No writer lock is ever held across the provider call: each append is a
/// discrete short store operation.
///
/// Error authority: a **failed or cancelled** turn returns `Ok` carrying a
/// truthful [`TurnStatus`] plus its durable terminal fact; `Err` is reserved
/// for infrastructure failures (store/IO) where no terminal fact could be
/// recorded. Model prose never mutates the graph — only decoded proposals
/// applied through validated handlers can, at most one mutating proposal per
/// turn.
#[allow(clippy::too_many_arguments)]
pub fn start_turn(
    session: &ProjectSession,
    routing: TurnRouting,
    prompt: &str,
    limits: &ConversationLimits,
    cancel: CancelToken,
    events: Sender<ConversationEvent>,
) -> Result<ConversationTurnResult, AppError> {
    let prompt_text = prompt.trim();
    if prompt_text.is_empty() {
        return Err(AppError::Rejected(
            "a conversation turn needs a non-empty prompt".to_string(),
        ));
    }
    let message_cap = limits
        .assistant_max_chars
        .min(relay_domain::MESSAGE_TEXT_MAX_CHARS)
        .clamp(1, relay_domain::MESSAGE_TEXT_MAX_CHARS);
    let user_text = sanitize_for_record(prompt_text, message_cap);

    // 1. Select or start the conversation, then record the accepted user
    //    message — one checked append under the store's short writer lock.
    let mut facts: Vec<DomainFact> = Vec::new();
    if !session.is_initialized() {
        // A first prompt in a fresh workspace creates the project (and only
        // the project): an informational turn never invents a goal.
        facts.push(DomainFact::ProjectCreated {
            project: crate::ids::project_id_for(&crate::session::workspace_name(
                session.workspace_root(),
            )),
        });
    }
    let graph = session.graph();
    let (conversation, ordinal, turn, user_message) = match graph.latest_conversation() {
        Some(existing) => {
            let ordinal = conversation_ordinal(&graph, &existing);
            let turn = next_turn_id(&graph, &existing, ordinal)?;
            let message = next_message_id(&graph, &existing, ordinal)?;
            (existing, ordinal, turn, message)
        }
        None => {
            let fresh = next_conversation_id(&graph)?;
            let ordinal = conversation_ordinal(&graph, &fresh);
            facts.push(DomainFact::ConversationStarted {
                conversation: fresh.clone(),
                started_at_ms: session.now_ms(),
            });
            let turn = TurnId::parse(&format!("turn_{ordinal}-1"))
                .map_err(|error| AppError::Rejected(error.to_string()))?;
            let message = MessageId::parse(&format!("msg_{ordinal}-1"))
                .map_err(|error| AppError::Rejected(error.to_string()))?;
            (fresh, ordinal, turn, message)
        }
    };
    facts.push(DomainFact::UserMessageRecorded {
        conversation: conversation.clone(),
        turn: turn.clone(),
        message: user_message,
        text: user_text.clone(),
        at_ms: session.now_ms(),
    });
    session.append_validated(facts)?;
    let _ = events.send(ConversationEvent::TurnStarted {
        conversation: conversation.clone(),
        turn: turn.clone(),
        user_text: user_text.clone(),
    });

    // 2. Build the bounded context from the transcript tail (excluding this
    //    turn's own message), safe graph views, and the prompt.
    let graph = session.graph();
    let evaluation = session.evaluation();
    let tail: Vec<ContextTranscriptEntry> = graph
        .transcript_tail(&conversation, limits.transcript_context_messages + 1)
        .into_iter()
        .filter(|entry| entry.turn != turn)
        .map(|entry| ContextTranscriptEntry {
            role: match entry.speaker {
                Speaker::User => ContextRole::User,
                Speaker::Assistant => ContextRole::Assistant,
            },
            text: entry.text,
        })
        .collect();
    let open_decisions = open_decision_lines(&graph);
    let ready_work = ready_work_lines(&graph, &evaluation);
    let inputs = ContextInputs {
        transcript: &tail,
        graph_summary: &graph_summary(&graph, &evaluation),
        open_decisions: &open_decisions,
        ready_work: &ready_work,
        prompt: &user_text,
    };
    let built = build_context(&inputs, limits);

    // 3. Call the provider with the writer lock released, forwarding redacted
    //    delta fragments to the live interface as they arrive. The request
    //    carries the intent route's model; the provider is the route's
    //    connection adapter.
    let request = ModelRequest {
        model: routing.route.model.clone(),
        system: Some(conversation_system_prompt(limits)),
        messages: built.messages,
        tools: None,
        structured_output: Some(StructuredOutputRequest {
            name: STRUCTURED_OUTPUT_NAME.to_string(),
            schema: proposal_schema(limits),
        }),
        max_output_tokens: None,
        temperature: None,
        purpose: Some(CONVERSATION_PURPOSE.to_string()),
        signal: Some(cancel.clone()),
    };
    let forwarder = std::cell::RefCell::new(StreamForwarder::default());
    let observed = routing.provider.stream(&request).inspect(|event| {
        if let ProviderStreamEvent::TextDelta { text } = event {
            forwarder.borrow_mut().push(text, &events);
        }
    });
    let collected = collect_turn(observed);
    forwarder.borrow_mut().flush(&events);

    // 4. Terminal handling: cancellation and provider failure record their
    //    truthful facts and return `Ok` with the status.
    let collected = match collected {
        Ok(turn_result) => {
            if cancel.is_cancelled() || turn_result.finish_reason == FinishReason::Canceled {
                return finish_cancelled(
                    session,
                    &conversation,
                    &turn,
                    &cancel,
                    message_cap,
                    events,
                );
            }
            turn_result
        }
        Err(failure) => {
            if cancel.is_cancelled() || failure.kind == ProviderErrorKind::Canceled {
                return finish_cancelled(
                    session,
                    &conversation,
                    &turn,
                    &cancel,
                    message_cap,
                    events,
                );
            }
            let message = sanitize_for_record(&failure.message, message_cap);
            session.append_validated(vec![DomainFact::ConversationTurnFailed {
                conversation: conversation.clone(),
                turn: turn.clone(),
                safe_message: message.clone(),
                at_ms: session.now_ms(),
            }])?;
            let result = ConversationTurnResult {
                conversation,
                turn,
                assistant_text: String::new(),
                proposals: Vec::new(),
                status: TurnStatus::Failed { message },
                usage: None,
            };
            let _ = events.send(ConversationEvent::TurnFinished {
                result: result.clone(),
            });
            return Ok(result);
        }
    };

    // 5. Decode the typed proposals. Absent structured output is a pure
    //    answer (zero proposals); structured output that does not decode is
    //    a failed turn — the model broke the contract it was given, and the
    //    failure is reported truthfully instead of silently dropping intent.
    let assistant_text = sanitize_for_record(&collected.text, message_cap);
    let proposals = match collected.structured_output {
        Some(output) if output.name == STRUCTURED_OUTPUT_NAME => {
            match decode_proposals(&output.value, limits) {
                Ok(proposals) => proposals,
                Err(error) => {
                    let message = sanitize_for_record(&error.message(), message_cap);
                    session.append_validated(vec![DomainFact::ConversationTurnFailed {
                        conversation: conversation.clone(),
                        turn: turn.clone(),
                        safe_message: message.clone(),
                        at_ms: session.now_ms(),
                    }])?;
                    let result = ConversationTurnResult {
                        conversation,
                        turn,
                        assistant_text: String::new(),
                        proposals: Vec::new(),
                        status: TurnStatus::Failed { message },
                        usage: Some(collected.usage),
                    };
                    let _ = events.send(ConversationEvent::TurnFinished {
                        result: result.clone(),
                    });
                    return Ok(result);
                }
            }
        }
        _ => Vec::new(),
    };

    // 6. Apply every proposal independently; at most one mutating proposal
    //    per turn is accepted, the rest are safely rejected.
    let applied = apply_proposals(session, &routing, proposals, &events);

    // 7. Persist the final assistant message with the ids of what was
    //    actually accepted, then report the completed turn.
    let assistant_message = next_message_id(&session.graph(), &conversation, ordinal)?;
    session.append_validated(vec![DomainFact::AssistantMessageRecorded {
        conversation: conversation.clone(),
        turn: turn.clone(),
        message: assistant_message,
        text: assistant_text.clone(),
        at_ms: session.now_ms(),
        related_attempts: bound_related(applied.attempts),
        related_decisions: bound_related(applied.decisions),
        related_work: bound_related(applied.work),
    }])?;
    let result = ConversationTurnResult {
        conversation,
        turn,
        assistant_text,
        proposals: applied.outcomes,
        status: TurnStatus::Completed,
        usage: Some(collected.usage),
    };
    let _ = events.send(ConversationEvent::TurnFinished {
        result: result.clone(),
    });
    Ok(result)
}

/// Append the durable cancellation fact and finish the turn as cancelled.
fn finish_cancelled(
    session: &ProjectSession,
    conversation: &ConversationId,
    turn: &TurnId,
    cancel: &CancelToken,
    message_cap: usize,
    events: Sender<ConversationEvent>,
) -> Result<ConversationTurnResult, AppError> {
    let reason = sanitize_for_record(
        &cancel
            .reason()
            .unwrap_or_else(|| "cancelled by the user".to_string()),
        message_cap,
    );
    session.append_validated(vec![DomainFact::ConversationTurnCancelled {
        conversation: conversation.clone(),
        turn: turn.clone(),
        reason: reason.clone(),
        at_ms: session.now_ms(),
    }])?;
    let result = ConversationTurnResult {
        conversation: conversation.clone(),
        turn: turn.clone(),
        assistant_text: String::new(),
        proposals: Vec::new(),
        status: TurnStatus::Cancelled { reason },
        usage: None,
    };
    let _ = events.send(ConversationEvent::TurnFinished {
        result: result.clone(),
    });
    Ok(result)
}

// -- id minting --------------------------------------------------------------

/// Mint the next conversation id: `cnv_<n>` where `n` counts the
/// conversations the graph already holds plus one. Deterministic; no
/// randomness anywhere in the service.
pub(crate) fn next_conversation_id(graph: &ProjectGraph) -> Result<ConversationId, AppError> {
    let n = graph.conversations().count() + 1;
    ConversationId::parse(&format!("cnv_{n}"))
        .map_err(|error| AppError::Rejected(error.to_string()))
}

/// A conversation's stable ordinal: its position in the graph's sorted id
/// order, plus one. Unique per conversation, so the turn and message ids
/// derived from it never collide across conversations.
fn conversation_ordinal(graph: &ProjectGraph, conversation: &ConversationId) -> usize {
    graph
        .conversations()
        .filter(|(other, _)| *other < conversation)
        .count()
        + 1
}

/// Mint the next turn id for a conversation: `turn_<c>-<t>` with `t` one past
/// the turns it already holds.
fn next_turn_id(
    graph: &ProjectGraph,
    conversation: &ConversationId,
    ordinal: usize,
) -> Result<TurnId, AppError> {
    let t = graph
        .conversation(conversation)
        .map(|node| node.turns.len() + 1)
        .unwrap_or(1);
    TurnId::parse(&format!("turn_{ordinal}-{t}"))
        .map_err(|error| AppError::Rejected(error.to_string()))
}

/// Mint the next message id for a conversation: `msg_<c>-<m>` with `m` one
/// past the messages (user + assistant) it already holds.
fn next_message_id(
    graph: &ProjectGraph,
    conversation: &ConversationId,
    ordinal: usize,
) -> Result<MessageId, AppError> {
    let m = graph
        .conversation(conversation)
        .map(|node| {
            node.turns
                .values()
                .map(|turn| 1 + usize::from(turn.outcome.is_some()))
                .sum::<usize>()
                + 1
        })
        .unwrap_or(1);
    MessageId::parse(&format!("msg_{ordinal}-{m}"))
        .map_err(|error| AppError::Rejected(error.to_string()))
}

/// Keep a related-id list inside the domain's per-list cap, keeping the
/// head (deterministic).
fn bound_related<T>(ids: Vec<T>) -> Vec<T> {
    ids.into_iter()
        .take(relay_domain::TURN_RELATED_MAX)
        .collect()
}

// -- streamed-fragment forwarding --------------------------------------------

/// Ephemeral assistant-fragment forwarding: accumulates raw text, and emits
/// only a *settled* prefix — never the final [`STREAM_HOLDBACK_CHARS`]
/// characters, and never past the start of a secret-shaped token — so a
/// secret split across provider deltas can never be displayed partially.
/// Emission is always of redacted text.
#[derive(Default)]
struct StreamForwarder {
    /// Raw accumulated provider text.
    raw: String,
    /// How many characters of the settled redacted prefix were emitted.
    emitted: usize,
}

impl StreamForwarder {
    /// Account for one delta and emit whatever newly settled.
    fn push(&mut self, delta: &str, events: &Sender<ConversationEvent>) {
        self.raw.push_str(delta);
        let cut = settled_prefix_chars(&self.raw);
        if cut == 0 {
            return;
        }
        let prefix: String = self.raw.chars().take(cut).collect();
        let redacted = relay_tools::redact::redact_secrets(&prefix);
        self.emit_suffix(&redacted, events);
    }

    /// Emit everything still buffered (the turn ended; the full text is
    /// redactable as a whole).
    fn flush(&mut self, events: &Sender<ConversationEvent>) {
        let redacted = relay_tools::redact::redact_secrets(&self.raw);
        self.emit_suffix(&redacted, events);
    }

    /// Emit the not-yet-emitted suffix of a redacted settled text, in
    /// bounded fragments.
    fn emit_suffix(&mut self, redacted: &str, events: &Sender<ConversationEvent>) {
        let total = redacted.chars().count();
        if total <= self.emitted {
            return;
        }
        let suffix: String = redacted.chars().skip(self.emitted).collect();
        self.emitted = total;
        let mut fragment = String::new();
        for character in suffix.chars() {
            fragment.push(character);
            if fragment.chars().count() >= STREAM_FRAGMENT_CAP_CHARS {
                let _ = events.send(ConversationEvent::AssistantDelta {
                    text: std::mem::take(&mut fragment),
                });
            }
        }
        if !fragment.is_empty() {
            let _ = events.send(ConversationEvent::AssistantDelta { text: fragment });
        }
    }
}

/// How many leading characters of `raw` are settled: the total minus the
/// holdback, and never past the last secret-shaped token start.
fn settled_prefix_chars(raw: &str) -> usize {
    let total = raw.chars().count();
    let mut cut = total.saturating_sub(STREAM_HOLDBACK_CHARS);
    for start in SECRET_STARTS {
        if let Some(byte_position) = raw.rfind(start) {
            let char_position = raw[..byte_position].chars().count();
            if char_position < cut {
                cut = char_position;
            }
        }
    }
    cut
}

// -- context feeders -----------------------------------------------------------

/// The bounded workspace/graph summary block for the model context.
pub(crate) fn graph_summary(graph: &ProjectGraph, evaluation: &Evaluation) -> String {
    let counts = crate::views::status_counts(evaluation);
    let mut summary = match graph.goal() {
        Some(goal) => {
            let mut text = format!("goal: {}", goal.outcome);
            if !goal.success_definition.is_empty() {
                text.push_str(&format!("\nsuccess: {}", goal.success_definition));
            }
            text
        }
        None => "goal: none declared yet".to_string(),
    };
    let completion = match evaluation.completion {
        relay_engine::ProjectCompletion::Complete => "complete",
        relay_engine::ProjectCompletion::InProgress { .. } => "in-progress",
    };
    summary.push_str(&format!(
        "\nproject: {} ({completion}, revision {})\nwork: {} completed, {} ready, {} blocked, {} \
         awaiting a human, {} active attempt(s)",
        graph
            .project_id()
            .map(|id| id.to_string())
            .unwrap_or_else(|| "none".to_string()),
        graph.revision().get(),
        counts.completed,
        counts.ready,
        counts.blocked,
        counts.awaiting_human,
        counts.active_attempts,
    ));
    summary
}

/// Open decisions as context lines, most recently opened first.
pub(crate) fn open_decision_lines(graph: &ProjectGraph) -> Vec<String> {
    let mut nodes: Vec<_> = graph
        .decisions()
        .filter(|(_, node)| node.is_open())
        .map(|(_, node)| node)
        .collect();
    nodes.sort_by_key(|node| std::cmp::Reverse(node.decision.opened_at_ms));
    nodes
        .into_iter()
        .map(|node| {
            let options = node
                .decision
                .options
                .iter()
                .map(|option| option.label.clone())
                .collect::<Vec<_>>();
            crate::conversation_views::decision_line(
                node.decision.id.as_str(),
                &node.decision.question,
                &options,
            )
        })
        .collect()
}

/// Ready work as context lines, top-K by the documented (phase, id)
/// deterministic tie-break.
pub(crate) fn ready_work_lines(graph: &ProjectGraph, evaluation: &Evaluation) -> Vec<String> {
    let catalog = WorkflowCatalog::builtin();
    let mut entries: Vec<(usize, String, String)> = evaluation
        .ready
        .iter()
        .filter_map(|entry| {
            graph.work(&entry.work).map(|node| {
                (
                    crate::views::phase_ordinal(&catalog, node.work.kind.as_str()),
                    entry.work.to_string(),
                    node.work.objective.clone(),
                )
            })
        })
        .collect();
    entries.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
    entries
        .into_iter()
        .take(CONTEXT_READY_TOP_K)
        .map(|(_, id, objective)| format!("{id}: {objective}"))
        .collect()
}

// -- proposal application ------------------------------------------------------

/// What applying a turn's proposals produced: the per-proposal outcomes
/// plus the related ids the assistant message annotates itself with.
#[derive(Default)]
struct AppliedTurn {
    outcomes: Vec<ProposalOutcome>,
    attempts: Vec<AttemptId>,
    decisions: Vec<DecisionId>,
    work: Vec<WorkItemId>,
}

/// Whether a proposal mutates the work graph (only one of these is accepted
/// per turn).
fn is_mutating(proposal: &ConversationProposal) -> bool {
    !matches!(proposal, ConversationProposal::RequestClarification { .. })
}

/// Apply every decoded proposal independently through the existing
/// application services. At most one mutating proposal is accepted per turn;
/// later mutating proposals are rejected with `only one mutation per turn`.
/// Every rejection is recorded with a safe reason — never coerced.
fn apply_proposals(
    session: &ProjectSession,
    routing: &TurnRouting,
    proposals: Vec<ConversationProposal>,
    events: &Sender<ConversationEvent>,
) -> AppliedTurn {
    let mut applied = AppliedTurn::default();
    let mut mutation_used = false;
    for (index, proposal) in proposals.into_iter().enumerate() {
        let outcome = if is_mutating(&proposal) && mutation_used {
            rejected_outcome(proposal, "only one mutating proposal per turn is applied")
        } else {
            let outcome = apply_one(session, routing, &proposal, events, &mut applied);
            if outcome.accepted && is_mutating(&proposal) {
                mutation_used = true;
            }
            outcome
        };
        let _ = events.send(ConversationEvent::ProposalDecided {
            index,
            accepted: outcome.accepted,
            detail: outcome.detail.clone(),
        });
        applied.outcomes.push(outcome);
    }
    applied
}

/// Apply one proposal, extending `applied` with what it touched.
fn apply_one(
    session: &ProjectSession,
    routing: &TurnRouting,
    proposal: &ConversationProposal,
    events: &Sender<ConversationEvent>,
    applied: &mut AppliedTurn,
) -> ProposalOutcome {
    match proposal {
        ConversationProposal::DeclareGoal {
            outcome,
            success_definition,
        } => {
            if session.graph().goal().is_some() {
                return rejected_outcome(
                    proposal.clone(),
                    "a goal already exists; a goal is declared once",
                );
            }
            match crate::commands::declare_goal(session, outcome, success_definition.as_deref()) {
                Ok(view) => {
                    applied.work.extend(parse_ids(&view.work));
                    applied.decisions.extend(parse_ids(&view.decisions));
                    accepted_outcome(
                        proposal.clone(),
                        format!(
                            "goal \"{}\" declared; {} work item(s) and {} decision(s) opened",
                            view.goal.id,
                            view.work.len(),
                            view.decisions.len()
                        ),
                    )
                }
                Err(error) => rejected_outcome(proposal.clone(), &error.message()),
            }
        }
        ConversationProposal::ExpandWork => {
            if session.graph().goal().is_none() {
                return rejected_outcome(proposal.clone(), "expand-work requires a declared goal");
            }
            let before = targets_snapshot(&session.graph());
            match crate::commands::expand(session) {
                Ok(view) => {
                    let (work, decisions) = new_targets(&before, &session.graph());
                    applied.work.extend(work.clone());
                    applied.decisions.extend(decisions.clone());
                    accepted_outcome(
                        proposal.clone(),
                        format!(
                            "expansion applied {} proposal(s), rejected {}; {} work item(s), {} \
                             decision(s) declared",
                            view.applied.len(),
                            view.rejected.len(),
                            work.len(),
                            decisions.len()
                        ),
                    )
                }
                Err(error) => rejected_outcome(proposal.clone(), &error.message()),
            }
        }
        ConversationProposal::ResolveDecision {
            decision,
            choice,
            rationale,
        } => {
            if session.graph().goal().is_none() {
                return rejected_outcome(
                    proposal.clone(),
                    "resolve-decision requires a declared goal",
                );
            }
            match crate::commands::resolve_decision(session, decision, choice, rationale.as_deref())
            {
                Ok(view) => {
                    if let Ok(id) = DecisionId::parse(decision) {
                        applied.decisions.push(id);
                    }
                    let work: Vec<WorkItemId> = parse_ids(&view.completed_work);
                    applied.work.extend(work.clone());
                    accepted_outcome(
                        proposal.clone(),
                        format!(
                            "decision {} resolved with \"{}\"; {} work item(s) completed",
                            view.decision.id,
                            view.decision
                                .choice
                                .clone()
                                .unwrap_or_else(|| choice.clone()),
                            work.len()
                        ),
                    )
                }
                Err(error) => rejected_outcome(proposal.clone(), &error.message()),
            }
        }
        ConversationProposal::RunReadyWork {
            autonomy,
            max_ticks,
        } => {
            if session.graph().goal().is_none() {
                return rejected_outcome(
                    proposal.clone(),
                    "run-ready-work requires a declared goal",
                );
            }
            let ticks = max_ticks.unwrap_or(1).clamp(1, RUN_MAX_TICKS);
            let autonomy = autonomy
                .as_deref()
                .and_then(parse_autonomy)
                .unwrap_or(DEFAULT_RUN_AUTONOMY);
            let _ = events.send(ConversationEvent::Activity {
                text: sanitize_for_record(
                    &format!(
                        "running ready work (up to {ticks} tick(s), {} autonomy)",
                        crate::views::autonomy_name(autonomy)
                    ),
                    DETAIL_CAP_CHARS,
                ),
            });
            // Proposal-driven runs use the same production wiring `relay
            // run` uses: the routed model client and the purpose-resolved
            // plan carried by this turn's routing. Grants therefore record
            // the connection name (and role) each work item's purpose
            // resolves to — never the adapter kind.
            let wiring = routing.run_wiring.clone();
            let run = match crate::runner::run_ticks(
                session,
                &wiring,
                SchedulingPolicy {
                    autonomy,
                    ..SchedulingPolicy::default()
                },
                ticks,
            ) {
                Ok(view) => view,
                Err(error) => return rejected_outcome(proposal.clone(), &error.message()),
            };
            let started = run
                .ticks
                .iter()
                .flat_map(|tick| tick.started.iter().cloned());
            let accepted = run
                .ticks
                .iter()
                .flat_map(|tick| tick.accepted.iter().cloned());
            let attempts: Vec<AttemptId> = started
                .chain(accepted)
                .filter_map(|id| AttemptId::parse(&id).ok())
                .collect();
            applied.attempts.extend(attempts);
            let _ = events.send(ConversationEvent::Activity {
                text: sanitize_for_record(
                    &format!(
                        "run finished: {} tick(s), {} attempt(s) accepted, {} failed, {} awaiting \
                         a human",
                        run.ticks.len(),
                        run.ticks
                            .iter()
                            .map(|tick| tick.accepted.len())
                            .sum::<usize>(),
                        run.ticks
                            .iter()
                            .map(|tick| tick.failed.len())
                            .sum::<usize>(),
                        run.awaiting_human.len()
                    ),
                    DETAIL_CAP_CHARS,
                ),
            });
            accepted_outcome(
                proposal.clone(),
                format!(
                    "ran {} tick(s); {} attempt(s) accepted, {} awaiting a human",
                    run.ticks.len(),
                    run.ticks
                        .iter()
                        .map(|tick| tick.accepted.len())
                        .sum::<usize>(),
                    run.awaiting_human.len()
                ),
            )
        }
        ConversationProposal::RequestClarification { question } => accepted_outcome(
            proposal.clone(),
            format!(
                "asking: {}",
                sanitize_for_record(question, DETAIL_CAP_CHARS)
            ),
        ),
    }
}

/// A bounded accepted outcome.
fn accepted_outcome(proposal: ConversationProposal, detail: String) -> ProposalOutcome {
    ProposalOutcome {
        proposal,
        accepted: true,
        detail: sanitize_for_record(&detail, DETAIL_CAP_CHARS),
    }
}

/// A bounded, safe rejection outcome — never a coercion.
fn rejected_outcome(proposal: ConversationProposal, reason: &str) -> ProposalOutcome {
    ProposalOutcome {
        proposal,
        accepted: false,
        detail: sanitize_for_record(reason, DETAIL_CAP_CHARS),
    }
}

/// The work/decision id sets of a graph (for before/after diffing).
fn targets_snapshot(graph: &ProjectGraph) -> (BTreeSet<WorkItemId>, BTreeSet<DecisionId>) {
    (
        graph.works().map(|(id, _)| id.clone()).collect(),
        graph.decisions().map(|(id, _)| id.clone()).collect(),
    )
}

/// Work and decision ids `after` holds that `before` did not.
fn new_targets(
    before: &(BTreeSet<WorkItemId>, BTreeSet<DecisionId>),
    after: &ProjectGraph,
) -> (Vec<WorkItemId>, Vec<DecisionId>) {
    let (after_work, after_decisions) = targets_snapshot(after);
    let work: Vec<WorkItemId> = after_work.difference(&before.0).cloned().collect();
    let decisions: Vec<DecisionId> = after_decisions.difference(&before.1).cloned().collect();
    (work, decisions)
}

/// Parse a list of id strings, skipping anything malformed.
fn parse_ids<T: std::str::FromStr>(values: &[String]) -> Vec<T> {
    values
        .iter()
        .filter_map(|value| value.parse::<T>().ok())
        .collect()
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)] // test-only, house style
mod tests {
    use super::*;
    use serde_json::json;

    fn user(text: &str) -> ContextTranscriptEntry {
        ContextTranscriptEntry {
            role: ContextRole::User,
            text: text.to_string(),
        }
    }

    fn assistant(text: &str) -> ContextTranscriptEntry {
        ContextTranscriptEntry {
            role: ContextRole::Assistant,
            text: text.to_string(),
        }
    }

    fn text_of(message: &ModelMessage) -> String {
        match message {
            ModelMessage::User { content } => content.clone(),
            ModelMessage::Assistant { content } => content
                .iter()
                .filter_map(|block| match block {
                    relay_providers::contract::AssistantContent::Text { text } => {
                        Some(text.clone())
                    }
                    _ => None,
                })
                .collect(),
            ModelMessage::ToolResult { content, .. } => content.clone(),
        }
    }

    // -- limits --------------------------------------------------------------

    #[test]
    fn default_limits_match_the_documented_budgets() {
        let limits = ConversationLimits::default();
        assert_eq!(limits.transcript_context_messages, 24);
        assert_eq!(limits.transcript_context_bytes, 32_768);
        assert_eq!(limits.assistant_max_chars, 16_384);
        assert_eq!(limits.context_max_chars, 48_000);
        assert_eq!(limits.max_proposals, 8);
    }

    // -- decode_proposals ----------------------------------------------------

    #[test]
    fn decodes_every_valid_proposal_kind_with_fields() {
        let value = json!({
            "proposals": [
                { "type": "declare-goal", "outcome": "fix the failing tests",
                  "success_definition": "cargo test is green" },
                { "type": "expand-work" },
                { "type": "resolve-decision", "decision": "dec_1", "choice": "approve",
                  "rationale": "the brief is ready" },
                { "type": "run-ready-work", "autonomy": "bounded:2", "max_ticks": 4 },
                { "type": "request-clarification", "question": "which test suite?" }
            ]
        });
        let proposals =
            decode_proposals(&value, &ConversationLimits::default()).expect("all five decode");
        assert_eq!(proposals.len(), 5);
        assert_eq!(
            proposals[0],
            ConversationProposal::DeclareGoal {
                outcome: "fix the failing tests".to_string(),
                success_definition: Some("cargo test is green".to_string()),
            }
        );
        assert_eq!(proposals[1], ConversationProposal::ExpandWork);
        assert_eq!(
            proposals[4],
            ConversationProposal::RequestClarification {
                question: "which test suite?".to_string()
            }
        );
    }

    #[test]
    fn an_empty_proposal_list_is_a_pure_answer() {
        let proposals =
            decode_proposals(&json!({ "proposals": [] }), &ConversationLimits::default())
                .expect("empty list decodes");
        assert!(proposals.is_empty());
    }

    #[test]
    fn unknown_type_tags_are_rejected_with_the_expected_tags() {
        let error = decode_proposals(
            &json!({ "proposals": [{ "type": "delete-everything" }] }),
            &ConversationLimits::default(),
        )
        .expect_err("unknown tag rejected");
        let message = error.message();
        assert!(
            message.contains("unknown proposal type \"delete-everything\""),
            "{message}"
        );
        for tag in PROPOSAL_TAGS {
            assert!(message.contains(tag), "{message}");
        }
    }

    #[test]
    fn missing_fields_and_tags_are_rejected() {
        let missing_field = decode_proposals(
            &json!({ "proposals": [{ "type": "declare-goal" }] }),
            &ConversationLimits::default(),
        )
        .expect_err("outcome is required");
        assert!(
            missing_field.message().contains("did not decode"),
            "{}",
            missing_field.message()
        );

        let missing_tag = decode_proposals(
            &json!({ "proposals": [{ "outcome": "x" }] }),
            &ConversationLimits::default(),
        )
        .expect_err("type tag is required");
        assert!(
            missing_tag.message().contains("missing the \"type\" tag"),
            "{}",
            missing_tag.message()
        );
    }

    #[test]
    fn over_limit_proposal_counts_are_rejected() {
        let limits = ConversationLimits {
            max_proposals: 2,
            ..ConversationLimits::default()
        };
        let error = decode_proposals(
            &json!({
                "proposals": [
                    { "type": "expand-work" },
                    { "type": "expand-work" },
                    { "type": "expand-work" }
                ]
            }),
            &limits,
        )
        .expect_err("three proposals exceed the cap");
        assert!(error.message().contains("at most 2"), "{}", error.message());
    }

    #[test]
    fn empty_strings_are_rejected_for_required_fields() {
        for value in [
            json!({ "proposals": [{ "type": "declare-goal", "outcome": "" }] }),
            json!({ "proposals": [{ "type": "declare-goal", "outcome": "ok",
                                    "success_definition": "  " }] }),
            json!({ "proposals": [{ "type": "resolve-decision", "decision": " ", "choice": "a" }] }),
            json!({ "proposals": [{ "type": "resolve-decision", "decision": "dec_1", "choice": "" }] }),
            json!({ "proposals": [{ "type": "request-clarification", "question": "" }] }),
        ] {
            let error = decode_proposals(&value, &ConversationLimits::default())
                .expect_err("empty strings fail closed");
            assert!(
                error.message().contains("must not be empty"),
                "{}",
                error.message()
            );
        }
    }

    #[test]
    fn max_ticks_must_be_in_range() {
        let limits = ConversationLimits::default();
        for bad in [0usize, 65] {
            let error = decode_proposals(
                &json!({ "proposals": [{ "type": "run-ready-work", "max_ticks": bad }] }),
                &limits,
            )
            .expect_err("out-of-range max_ticks rejected");
            assert!(error.message().contains("max_ticks"), "{}", error.message());
        }
        for good in [1usize, 64] {
            decode_proposals(
                &json!({ "proposals": [{ "type": "run-ready-work", "max_ticks": good }] }),
                &limits,
            )
            .expect("in-range max_ticks accepted");
        }
    }

    #[test]
    fn autonomy_names_are_validated_at_decode_time() {
        let limits = ConversationLimits::default();
        let error = decode_proposals(
            &json!({ "proposals": [{ "type": "run-ready-work", "autonomy": "fast" }] }),
            &limits,
        )
        .expect_err("unknown autonomy rejected");
        assert!(
            error.message().contains("unknown autonomy \"fast\""),
            "{}",
            error.message()
        );

        for good in ["manual", "safe", "until-decision", "bounded:3"] {
            decode_proposals(
                &json!({ "proposals": [{ "type": "run-ready-work", "autonomy": good }] }),
                &limits,
            )
            .expect("known autonomy accepted");
        }
    }

    #[test]
    fn malformed_envelopes_fail_closed() {
        let limits = ConversationLimits::default();
        for value in [
            json!("proposals"),
            json!({}),
            json!({ "proposals": { "type": "expand-work" } }),
            json!({ "proposals": ["expand-work"] }),
        ] {
            assert!(
                decode_proposals(&value, &limits).is_err(),
                "malformed envelope rejected: {value}"
            );
        }
    }

    #[test]
    fn oversized_proposal_strings_are_rejected() {
        let limits = ConversationLimits {
            assistant_max_chars: 8,
            ..ConversationLimits::default()
        };
        let error = decode_proposals(
            &json!({ "proposals": [{ "type": "declare-goal", "outcome": "123456789" }] }),
            &limits,
        )
        .expect_err("over-cap string rejected");
        assert!(
            error.message().contains("8-character message cap"),
            "{}",
            error.message()
        );
    }

    // -- schema / serde agreement --------------------------------------------

    #[test]
    fn serde_tags_match_the_schema_constants() {
        let limits = ConversationLimits::default();
        let schema = proposal_schema(&limits);
        let variants = schema["properties"]["proposals"]["items"]["oneOf"]
            .as_array()
            .expect("oneOf variants");
        let consts: Vec<&str> = variants
            .iter()
            .map(|variant| {
                variant["properties"]["type"]["const"]
                    .as_str()
                    .expect("const tag")
            })
            .collect();
        assert_eq!(consts, PROPOSAL_TAGS, "schema tags equal the wire tags");

        let samples = vec![
            ConversationProposal::DeclareGoal {
                outcome: "o".to_string(),
                success_definition: None,
            },
            ConversationProposal::ExpandWork,
            ConversationProposal::ResolveDecision {
                decision: "dec_1".to_string(),
                choice: "a".to_string(),
                rationale: None,
            },
            ConversationProposal::RunReadyWork {
                autonomy: None,
                max_ticks: None,
            },
            ConversationProposal::RequestClarification {
                question: "q".to_string(),
            },
        ];
        for sample in samples {
            let encoded = serde_json::to_value(&sample).expect("serializes");
            let tag = encoded["type"].as_str().expect("tagged");
            assert!(PROPOSAL_TAGS.contains(&tag), "tag {tag} is a wire tag");
            // Round trip through the decode path, as the service will.
            let decoded =
                decode_proposals(&json!({ "proposals": [encoded] }), &limits).expect("decodes");
            assert_eq!(decoded, vec![sample]);
        }
        assert_eq!(
            schema["properties"]["proposals"]["maxItems"],
            json!(limits.max_proposals)
        );
    }

    // -- autonomy ------------------------------------------------------------

    #[test]
    fn autonomy_parsing_matches_the_view_grammar() {
        use relay_engine::schedule::Autonomy;
        assert_eq!(parse_autonomy("manual"), Some(Autonomy::Manual));
        assert_eq!(parse_autonomy("safe"), Some(Autonomy::Safe));
        assert_eq!(
            parse_autonomy("until-decision"),
            Some(Autonomy::UntilDecision)
        );
        assert_eq!(parse_autonomy("bounded:3"), Some(Autonomy::Bounded(3)));
        for bad in ["", "fast", "bounded:0", "bounded:x", "bounded:-1"] {
            assert_eq!(parse_autonomy(bad), None, "{bad} is not an autonomy");
        }
    }

    // -- system prompt ---------------------------------------------------------

    #[test]
    fn the_system_prompt_is_bounded_and_names_the_contract() {
        let limits = ConversationLimits::default();
        let prompt = conversation_system_prompt(&limits);
        assert!(prompt.chars().count() <= SYSTEM_PROMPT_CAP_CHARS);
        assert!(prompt.contains(STRUCTURED_OUTPUT_NAME), "{prompt}");
        for tag in PROPOSAL_TAGS {
            assert!(prompt.contains(tag), "prompt names {tag}: {prompt}");
        }
        assert!(prompt.contains("concisely"), "{prompt}");
    }

    // -- bounding / redaction -------------------------------------------------

    #[test]
    fn bounding_keeps_the_head_and_marks_the_cut_on_a_char_boundary() {
        assert_eq!(bound_chars("short", 32), "short");
        assert_eq!(bound_chars("abc", 0), "");
        assert_eq!(bound_chars("abcdefghij", 20), "abcdefghij");
        let cut = bound_chars("abcdefghijklmnopqrstuvwxyz", 15);
        assert_eq!(cut, "abc…[truncated]");
        assert_eq!(cut.chars().count(), 15);
        // The cap always holds, even below the marker's own length: a hard
        // cut replaces the marker there.
        let tight = bound_chars("abcdefghij", 7);
        assert_eq!(tight, "abcdefg");
        assert!(tight.chars().count() <= 7);
        // Multibyte characters are never split.
        let unicode = "日".repeat(20);
        let cut = bound_chars(&unicode, 15);
        assert_eq!(cut, "日日日…[truncated]");
        assert!(cut.chars().count() <= 15);
    }

    #[test]
    fn sanitization_redacts_before_bounding() {
        let dirty = "key sk-ant-0123456789abcdefghijklmnop then a lot of trailing text";
        let clean = sanitize_for_record(dirty, 40);
        assert!(
            !clean.contains("sk-ant-0123456789abcdefghijklmnop"),
            "{clean}"
        );
        assert!(clean.contains("[redacted:anthropic-key]"), "{clean}");
        assert!(clean.chars().count() <= 40, "{clean}");
    }

    // -- build_context ---------------------------------------------------------

    #[test]
    fn context_keeps_the_newest_entries_within_the_message_window() {
        let transcript: Vec<ContextTranscriptEntry> = (0..30)
            .map(|index| {
                if index % 2 == 0 {
                    user(&format!("message {index}"))
                } else {
                    assistant(&format!("reply {index}"))
                }
            })
            .collect();
        let inputs = ContextInputs {
            transcript: &transcript,
            graph_summary: "goal: ship it",
            open_decisions: &[],
            ready_work: &[],
            prompt: "what is next?",
        };
        let built = build_context(&inputs, &ConversationLimits::default());
        // context block + 24 newest transcript entries + prompt
        assert_eq!(built.messages.len(), 26, "{built:?}");
        assert_eq!(built.trim.transcript_messages_dropped, 6);
        assert!(!built.trim.transcript_truncated);
        let first = text_of(&built.messages[1]);
        assert!(first.contains("message 6"), "oldest kept is #6: {first}");
        let last = text_of(&built.messages[24]);
        assert!(last.contains("reply 29"), "newest kept is #29: {last}");
        let prompt = text_of(built.messages.last().expect("prompt present"));
        assert_eq!(prompt, "what is next?");
        let context = text_of(&built.messages[0]);
        assert!(context.contains("Relay workspace context:"), "{context}");
        assert!(context.contains("goal: ship it"), "{context}");
    }

    #[test]
    fn byte_budget_drops_older_transcript_entries_and_reports_it() {
        let transcript = vec![
            user(&"x".repeat(2048)),
            user(&"y".repeat(2048)),
            user("tiny newest"),
        ];
        let limits = ConversationLimits {
            transcript_context_bytes: 4_096,
            ..ConversationLimits::default()
        };
        let inputs = ContextInputs {
            transcript: &transcript,
            graph_summary: "",
            open_decisions: &[],
            ready_work: &[],
            prompt: "go",
        };
        let built = build_context(&inputs, &limits);
        assert!(built.trim.transcript_messages_dropped >= 1, "{built:?}");
        let last_transcript = text_of(&built.messages[built.messages.len() - 2]);
        assert!(last_transcript.contains("tiny newest"), "{built:?}");
        // The included transcript bytes stay inside the byte budget.
        let included: usize = built.messages[1..built.messages.len() - 1]
            .iter()
            .map(|message| text_of(message).len())
            .sum();
        assert!(included <= limits.transcript_context_bytes, "{built:?}");
    }

    #[test]
    fn total_char_budget_trims_the_transcript_not_the_prompt() {
        let limits = ConversationLimits {
            context_max_chars: 300,
            assistant_max_chars: 100,
            transcript_context_bytes: 100_000,
            transcript_context_messages: 24,
            max_proposals: 8,
        };
        let transcript: Vec<ContextTranscriptEntry> = (0..10)
            .map(|index| user(&format!("entry {index} with some words")))
            .collect();
        let inputs = ContextInputs {
            transcript: &transcript,
            graph_summary: &"s".repeat(200),
            open_decisions: &["dec_1: which database? (options: sqlite, postgres)".to_string()],
            ready_work: &[],
            prompt: &"p".repeat(100),
        };
        let built = build_context(&inputs, &limits);
        let total: usize = built
            .messages
            .iter()
            .map(|m| text_of(m).chars().count())
            .sum();
        assert!(total <= limits.context_max_chars, "{built:?}");
        let prompt = text_of(built.messages.last().expect("prompt kept"));
        assert_eq!(prompt.chars().count(), 100, "the prompt keeps its share");
    }

    #[test]
    fn an_oversized_newest_message_is_truncated_not_dropped() {
        let limits = ConversationLimits {
            transcript_context_bytes: 500,
            context_max_chars: 5_000,
            ..ConversationLimits::default()
        };
        // Chronological order: the huge message is the newest entry.
        let transcript = vec![
            user("dropped old"),
            assistant("kept"),
            user(&"z".repeat(5_000)),
        ];
        let inputs = ContextInputs {
            transcript: &transcript,
            graph_summary: "",
            open_decisions: &[],
            ready_work: &[],
            prompt: "go",
        };
        let built = build_context(&inputs, &limits);
        assert!(built.trim.transcript_truncated, "{built:?}");
        // The truncated newest consumed the byte budget, so both older
        // entries no longer fit and are dropped.
        assert_eq!(built.trim.transcript_messages_dropped, 2, "{built:?}");
        let newest = text_of(&built.messages[0]);
        assert!(newest.contains(TRUNCATION_MARKER), "{newest}");
        assert!(newest.len() <= limits.transcript_context_bytes, "{newest}");
    }

    #[test]
    fn every_context_text_is_redacted() {
        let transcript = vec![user("my key is sk-ant-0123456789abcdefghijklmnop")];
        let inputs = ContextInputs {
            transcript: &transcript,
            graph_summary: "token sk-ant-0123456789abcdefghijklmnop in summary",
            open_decisions: &["dec_1: which? (options: a, b)".to_string()],
            ready_work: &["work_1: use sk-ant-0123456789abcdefghijklmnop".to_string()],
            prompt: "here: sk-ant-0123456789abcdefghijklmnop",
        };
        let built = build_context(&inputs, &ConversationLimits::default());
        for message in &built.messages {
            let text = text_of(message);
            assert!(
                !text.contains("sk-ant-0123456789abcdefghijklmnop"),
                "{text}"
            );
        }
    }

    #[test]
    fn section_blocks_are_truncated_and_reported() {
        let inputs = ContextInputs {
            transcript: &[],
            graph_summary: &"s".repeat(SECTION_CAPS.0 + 5_000),
            open_decisions: &[],
            ready_work: &[],
            prompt: "go",
        };
        let built = build_context(&inputs, &ConversationLimits::default());
        assert!(built.trim.sections_truncated, "{built:?}");
        let context = text_of(&built.messages[0]);
        assert!(context.contains(TRUNCATION_MARKER), "{context}");
    }

    #[test]
    fn empty_inputs_produce_a_prompt_only_context() {
        let inputs = ContextInputs {
            transcript: &[],
            graph_summary: "",
            open_decisions: &[],
            ready_work: &[],
            prompt: "hello",
        };
        let built = build_context(&inputs, &ConversationLimits::default());
        assert_eq!(built.messages.len(), 1);
        assert_eq!(text_of(&built.messages[0]), "hello");
        assert_eq!(built.trim, ContextTrim::default());
    }

    #[test]
    fn transcript_roles_map_onto_provider_messages() {
        let transcript = vec![user("u1"), assistant("a1"), user("u2")];
        let inputs = ContextInputs {
            transcript: &transcript,
            graph_summary: "",
            open_decisions: &[],
            ready_work: &[],
            prompt: "now",
        };
        let built = build_context(&inputs, &ConversationLimits::default());
        assert_eq!(built.messages.len(), 4);
        // No context block (empty workspace): the transcript leads.
        assert!(matches!(built.messages[0], ModelMessage::User { .. }));
        assert!(matches!(built.messages[1], ModelMessage::Assistant { .. }));
        assert!(matches!(built.messages[2], ModelMessage::User { .. }));
        assert!(matches!(built.messages[3], ModelMessage::User { .. }));
        assert_eq!(text_of(&built.messages[2]), "u2");
    }

    // -- id minting ----------------------------------------------------------

    fn graph_from(facts: Vec<DomainFact>) -> ProjectGraph {
        ProjectGraph::from_facts(&facts).expect("the fixture folds")
    }

    fn conversation_fixture(conversation: &str, turns: &[(&str, bool)]) -> Vec<DomainFact> {
        let mut facts = vec![
            DomainFact::ProjectCreated {
                project: "prj_demo".parse().expect("project id"),
            },
            DomainFact::ConversationStarted {
                conversation: conversation.parse().expect("conversation id"),
                started_at_ms: 1_000,
            },
        ];
        let slug = conversation.trim_start_matches("cnv_");
        for (index, (turn, completed)) in turns.iter().enumerate() {
            let message_number = 2 * index + 1;
            facts.push(DomainFact::UserMessageRecorded {
                conversation: conversation.parse().expect("conversation id"),
                turn: turn.parse().expect("turn id"),
                message: format!("msg_f{slug}-{message_number}")
                    .parse()
                    .expect("message id"),
                text: format!("question {index}"),
                at_ms: 1_000 + index as i64,
            });
            if *completed {
                facts.push(DomainFact::AssistantMessageRecorded {
                    conversation: conversation.parse().expect("conversation id"),
                    turn: turn.parse().expect("turn id"),
                    message: format!("msg_f{slug}-{}", message_number + 1)
                        .parse()
                        .expect("message id"),
                    text: format!("answer {index}"),
                    at_ms: 1_100 + index as i64,
                    related_attempts: Vec::new(),
                    related_decisions: Vec::new(),
                    related_work: Vec::new(),
                });
            }
        }
        facts
    }

    #[test]
    fn conversation_turn_and_message_ids_are_deterministic_and_unique() {
        // One conversation with one completed and one open turn.
        let graph = graph_from(conversation_fixture(
            "cnv_1",
            &[("turn_1-1", true), ("turn_1-2", false)],
        ));
        let cnv = "cnv_1".parse().expect("id");
        let ordinal = conversation_ordinal(&graph, &cnv);
        assert_eq!(ordinal, 1);
        assert_eq!(
            next_turn_id(&graph, &cnv, ordinal)
                .expect("turn id")
                .as_str(),
            "turn_1-3"
        );
        // Two prior messages (user + assistant of turn 1) plus the open
        // user message of turn 2 → the next message is number 4.
        assert_eq!(
            next_message_id(&graph, &cnv, ordinal)
                .expect("message id")
                .as_str(),
            "msg_1-4"
        );

        // A second conversation gets its own ordinal, so its ids never
        // collide with the first conversation's.
        let mut facts = conversation_fixture("cnv_1", &[("turn_1-1", true)]);
        facts.extend(
            conversation_fixture("cnv_2", &[("turn_2-1", true)])
                .into_iter()
                .skip(1), // one ProjectCreated per graph
        );
        let graph = graph_from(facts);
        assert_eq!(
            next_conversation_id(&graph)
                .expect("conversation id")
                .as_str(),
            "cnv_3"
        );
        for (conversation, expected_ordinal) in [("cnv_1", 1usize), ("cnv_2", 2)] {
            assert_eq!(
                conversation_ordinal(&graph, &conversation.parse().expect("id")),
                expected_ordinal
            );
        }
    }

    // -- streamed-fragment forwarding ---------------------------------------

    fn forwarded(deltas: &[&str]) -> Vec<String> {
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut forwarder = StreamForwarder::default();
        for delta in deltas {
            forwarder.push(delta, &sender);
        }
        forwarder.flush(&sender);
        drop(sender);
        receiver
            .iter()
            .map(|event: ConversationEvent| match event {
                ConversationEvent::AssistantDelta { text } => text,
                _ => String::new(),
            })
            .collect()
    }

    #[test]
    fn a_secret_split_across_deltas_is_never_streamed_partially() {
        let secret = "sk-ant-0123456789abcdefghijklmnop";
        let deltas = [
            "the key is sk-ant-0123", // a partial secret arrives first
            "456789abcdefghijklmnop and more text follows",
        ];
        let fragments = forwarded(&deltas);
        let joined = fragments.concat();
        assert!(
            joined.contains("[redacted:anthropic-key]"),
            "the whole secret was redacted: {joined}"
        );
        assert!(!joined.contains(secret), "{joined}");
        // No intermediate fragment ever carried a secret prefix either.
        for fragment in &fragments {
            assert!(!fragment.contains("sk-ant"), "{fragment}");
            assert!(!fragment.contains("0123456789"), "{fragment}");
        }
    }

    #[test]
    fn streamed_fragments_are_bounded_and_ordered() {
        let long = "x".repeat(STREAM_FRAGMENT_CAP_CHARS * 3 + 10);
        let fragments = forwarded(&[&long]);
        assert!(fragments.len() >= 3, "{fragments:?}");
        assert!(
            fragments
                .iter()
                .all(|fragment| fragment.chars().count() <= STREAM_FRAGMENT_CAP_CHARS),
            "{fragments:?}"
        );
        assert_eq!(fragments.concat().chars().count(), long.chars().count());
        // Plain text streams through unchanged.
        assert_eq!(forwarded(&["hello ", "world"]).concat(), "hello world");
    }

    #[test]
    fn settled_prefix_holds_back_the_tail_and_secret_starts() {
        let plain = "x".repeat(STREAM_HOLDBACK_CHARS + 100);
        assert_eq!(
            settled_prefix_chars(&plain),
            100,
            "only the head beyond the holdback settles"
        );
        // A secret start inside the visible region cuts the settled prefix
        // to before it, so the shape can only ever be emitted redacted.
        let with_secret = format!("{}sk-ant-tail{}", "y".repeat(300), "z".repeat(400));
        assert_eq!(settled_prefix_chars(&with_secret), 300);
        // A secret start beyond the holdback needs no extra cut: 600 + 11
        // characters minus the holdback.
        let distant = format!("{}sk-ant-tail", "y".repeat(600));
        assert_eq!(
            settled_prefix_chars(&distant),
            600 + 11 - STREAM_HOLDBACK_CHARS
        );
    }

    // -- context feeders ------------------------------------------------------

    #[test]
    fn the_graph_summary_and_decision_lines_are_bounded_and_redacted() {
        let graph = graph_from(vec![
            DomainFact::ProjectCreated {
                project: "prj_demo".parse().expect("project id"),
            },
            DomainFact::GoalDeclared {
                goal: relay_domain::Goal::new(
                    "goal_demo".parse().expect("goal id"),
                    "export reports as CSV",
                    relay_domain::Provenance {
                        source: relay_domain::ProvenanceSource::User,
                        statement: None,
                        at_ms: 1_000,
                    },
                    1_000,
                ),
            },
        ]);
        let evaluation = relay_engine::evaluate(&graph);
        let summary = graph_summary(&graph, &evaluation);
        assert!(summary.contains("goal: export reports as CSV"), "{summary}");
        assert!(summary.contains("revision"), "{summary}");
        assert!(summary.contains("awaiting a human"), "{summary}");
        // A goal-less graph says so plainly.
        let bare = graph_from(vec![DomainFact::ProjectCreated {
            project: "prj_demo".parse().expect("project id"),
        }]);
        let bare_evaluation = relay_engine::evaluate(&bare);
        assert!(
            graph_summary(&bare, &bare_evaluation).contains("none declared yet"),
            "no goal is invented in context"
        );
        assert!(open_decision_lines(&bare).is_empty());
        assert!(ready_work_lines(&bare, &bare_evaluation).is_empty());
    }
}
