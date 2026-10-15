//! Deterministic in-process provider for tests and demos.
//!
//! Ported from `src/providers/fake.ts` (retired TypeScript tree, removed from
//! the repository 2026-10-14). The fake provider never touches the
//! network. Callers script per-purpose responses (with optional ordered
//! failures for retry tests); unmatched calls fall back to a simple echo —
//! the last user message, or, for a transcript ending in tool results (a
//! native tool loop between calls), a bounded reflection of those results.
//! It records a call log so tests can assert routing, retries, and
//! cancellation behavior deterministically.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::contract::TokenUsage;
use regex::Regex;
use serde_json::Value;

use crate::contract::{
    AdapterCapabilities, AdapterKind, CancelToken, FinishReason, ModelProvider, ModelRequest,
    ProviderErrorKind, ProviderEventStream, ProviderFailure, ProviderStreamEvent, ToolCall,
};

/// How a scripted step matches the request purpose.
#[derive(Debug, Clone)]
pub enum PurposeMatch {
    /// Exact purpose string.
    Exact(String),
    /// Regular expression tested against the purpose.
    Pattern(Regex),
}

impl PurposeMatch {
    /// Build a regex purpose matcher.
    pub fn pattern(pattern: &str) -> Result<Self, regex::Error> {
        Ok(PurposeMatch::Pattern(Regex::new(pattern)?))
    }

    fn matches(&self, purpose: &str) -> bool {
        match self {
            PurposeMatch::Exact(expected) => expected == purpose,
            PurposeMatch::Pattern(regex) => regex.is_match(purpose),
        }
    }
}

/// Constraints selecting which calls a scripted step answers.
#[derive(Debug, Clone, Default)]
pub struct FakeMatch {
    /// Purpose (exact string or regular expression).
    pub purpose: Option<PurposeMatch>,
    /// Exact model id.
    pub model: Option<String>,
    /// Only match from the Nth call of the purpose onward (1-based).
    pub min_call: Option<u64>,
    /// Only match up to the Nth call of the purpose (1-based).
    pub max_call: Option<u64>,
}

/// A scripted failure, optionally repeated before the step's success payload.
#[derive(Debug, Clone)]
pub struct FakeErrorPlan {
    /// Failure classification.
    pub kind: ProviderErrorKind,
    /// Whether Relay's retry policy may retry it.
    pub retryable: bool,
    /// Failure message.
    pub message: String,
    /// Fail this many times before the step's success response is used.
    pub fail_times: Option<u64>,
}

/// A tool call the fake provider should request.
#[derive(Debug, Clone)]
pub struct FakeToolCallPlan {
    /// Tool name.
    pub name: String,
    /// Tool arguments.
    pub args: Value,
}

/// One scripted response.
#[derive(Debug, Clone, Default)]
pub struct FakeResponse {
    /// Assistant text.
    pub text: Option<String>,
    /// Tool calls to request.
    pub tool_calls: Option<Vec<FakeToolCallPlan>>,
    /// Structured output value.
    pub structured_output: Option<Value>,
    /// Finish reason override.
    pub finish_reason: Option<FinishReason>,
    /// Usage override.
    pub usage: Option<TokenUsage>,
    /// Scripted failure.
    pub error: Option<FakeErrorPlan>,
    /// Delay before responding, in milliseconds.
    pub delay_ms: Option<u64>,
}

/// One scripted step: the constraints and the response it serves.
#[derive(Debug, Clone)]
pub struct FakeStep {
    /// Constraints selecting which calls this step answers.
    pub r#match: FakeMatch,
    /// The response served while the constraints match.
    pub response: FakeResponse,
}

/// Entry in the fake provider's call log.
#[derive(Debug, Clone, PartialEq)]
pub struct FakeCallLogEntry {
    /// Request purpose (as provided; `None` when absent).
    pub purpose: Option<String>,
    /// Request model id.
    pub model: String,
    /// Response text that was served (empty string when none).
    pub text: String,
}

#[derive(Default)]
struct FakeState {
    purpose_counters: HashMap<String, u64>,
    step_failures: HashMap<usize, u64>,
    log: Vec<FakeCallLogEntry>,
    call_seq: u64,
}

/// The deterministic fake provider.
#[derive(Clone)]
pub struct FakeProvider {
    steps: Arc<[FakeStep]>,
    fallback_response: Option<Arc<FakeResponse>>,
    state: Arc<Mutex<FakeState>>,
}

impl FakeProvider {
    /// A provider with scripted steps and no fallback response (unmatched
    /// calls echo the last user message).
    pub fn new(steps: Vec<FakeStep>) -> Self {
        FakeProvider {
            steps: steps.into(),
            fallback_response: None,
            state: Arc::new(Mutex::new(FakeState::default())),
        }
    }

    /// A provider with scripted steps and a fallback response for unmatched
    /// calls.
    pub fn with_fallback(steps: Vec<FakeStep>, fallback: FakeResponse) -> Self {
        FakeProvider {
            steps: steps.into(),
            fallback_response: Some(Arc::new(fallback)),
            state: Arc::new(Mutex::new(FakeState::default())),
        }
    }

    /// A fake provider that echoes the last user message.
    pub fn echo() -> Self {
        Self::new(Vec::new())
    }

    /// The fake's capability profile: the `fake` adapter defaults. The fake
    /// is maximally permissive and performs no protocol translation, so the
    /// profile is a constant, never a construction option.
    pub fn capabilities(&self) -> AdapterCapabilities {
        AdapterCapabilities::defaults_for(AdapterKind::Fake)
    }

    /// Ordered log of every call this provider served.
    pub fn call_log(&self) -> Vec<FakeCallLogEntry> {
        self.lock_state().log.clone()
    }

    /// Call counts keyed by purpose (absent purposes share the "(none)" key).
    pub fn calls_by_purpose(&self) -> HashMap<String, u64> {
        self.lock_state().purpose_counters.clone()
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, FakeState> {
        // The fake is deterministic test support; a poisoned lock means a test
        // already panicked, so recovering the inner state keeps serving calls.
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    /// Find the first step whose purpose/model/minCall constraints match,
    /// tracking per-step failure counts so `error.fail_times` fails N times
    /// before the step's success response takes over.
    fn match_step(&self, request: &ModelRequest, count: u64) -> FakeResponse {
        for (index, step) in self.steps.iter().enumerate() {
            let purpose_matched = match &step.r#match.purpose {
                None => true,
                Some(matcher) => request
                    .purpose
                    .as_deref()
                    .is_some_and(|purpose| matcher.matches(purpose)),
            };
            let model_matched = step
                .r#match
                .model
                .as_deref()
                .is_none_or(|model| model == request.model);
            let call_matched = step.r#match.min_call.unwrap_or(0) <= count
                && count <= step.r#match.max_call.unwrap_or(u64::MAX);
            if !purpose_matched || !model_matched || !call_matched {
                continue;
            }

            if let Some(error) = &step.response.error {
                let mut state = self.lock_state();
                let failed = state.step_failures.get(&index).copied().unwrap_or(0);
                let fail_times = error.fail_times.unwrap_or(1);
                if failed < fail_times {
                    state.step_failures.insert(index, failed + 1);
                    let mut failing = step.response.clone();
                    failing.error = Some(error.clone());
                    return failing;
                }
                // Failures exhausted: serve the step's success payload.
                let mut success = step.response.clone();
                success.error = None;
                return success;
            }
            return step.response.clone();
        }
        if let Some(fallback) = &self.fallback_response {
            return (**fallback).clone();
        }
        FakeResponse {
            text: Some(echo_text(request)),
            ..FakeResponse::default()
        }
    }
}

impl ModelProvider for FakeProvider {
    fn name(&self) -> AdapterKind {
        AdapterKind::Fake
    }

    fn models(&self) -> Vec<String> {
        vec!["fake-standard".into(), "fake-mini".into()]
    }

    fn stream(&self, request: &ModelRequest) -> ProviderEventStream<'_> {
        let purpose_key = request
            .purpose
            .clone()
            .unwrap_or_else(|| "(none)".to_owned());
        let count = {
            let mut state = self.lock_state();
            let next = state
                .purpose_counters
                .get(&purpose_key)
                .copied()
                .unwrap_or(0)
                + 1;
            state.purpose_counters.insert(purpose_key.clone(), next);
            next
        };

        let response = self.match_step(request, count);
        {
            let mut state = self.lock_state();
            state.log.push(FakeCallLogEntry {
                purpose: request.purpose.clone(),
                model: request.model.clone(),
                text: response.text.clone().unwrap_or_default(),
            });
        }

        let provider = self.clone();
        let request = request.clone();
        Box::new(
            std::iter::once_with(move || {
                fake_turn_events(provider, request, purpose_key, response)
            })
            .flatten(),
        )
    }
}

/// Produce the events for one fake turn as a concrete `Vec`.
fn fake_turn_events(
    provider: FakeProvider,
    request: ModelRequest,
    purpose_key: String,
    response: FakeResponse,
) -> Vec<ProviderStreamEvent> {
    if let Some(delay_ms) = response.delay_ms.filter(|ms| *ms > 0)
        && let Some(reason) = sleep_cancellable(delay_ms, request.signal.as_ref())
    {
        let reason_text = match reason {
            Some(reason) if !reason.is_empty() => format!(": {reason}"),
            _ => String::new(),
        };
        return vec![ProviderStreamEvent::Error {
            error: ProviderFailure::new(
                ProviderErrorKind::Canceled,
                format!(
                    "fake provider call for purpose \"{purpose_key}\" was canceled during its delay{reason_text}"
                ),
            ),
        }];
    }

    if let Some(error) = &response.error {
        return vec![ProviderStreamEvent::Error {
            error: ProviderFailure::new(error.kind, error.message.clone())
                .retryable(error.retryable),
        }];
    }

    let mut events = Vec::new();

    // Split text into a few deltas so consumers exercise delta folding.
    let text = response.text.clone().unwrap_or_default();
    if !text.is_empty() {
        for part in split_text(&text, 3) {
            events.push(ProviderStreamEvent::TextDelta { text: part });
        }
    }

    let call_seq = {
        let mut state = provider.lock_state();
        state.call_seq += 1;
        state.call_seq
    };
    for (index, call) in response
        .tool_calls
        .clone()
        .unwrap_or_default()
        .into_iter()
        .enumerate()
    {
        events.push(ProviderStreamEvent::ToolCall {
            call: ToolCall {
                id: format!("fake_{}_{}", call_seq, index + 1),
                name: call.name,
                arguments: call.args,
            },
        });
    }

    if let (Some(value), Some(requested)) =
        (&response.structured_output, &request.structured_output)
    {
        events.push(ProviderStreamEvent::StructuredOutput {
            name: requested.name.clone(),
            value: value.clone(),
        });
    }

    let usage = response.usage.unwrap_or(TokenUsage {
        input_tokens: 100,
        output_tokens: 50,
    });
    events.push(ProviderStreamEvent::Usage { usage });

    let finish_reason = response.finish_reason.unwrap_or(
        if response
            .tool_calls
            .as_ref()
            .is_some_and(|calls| !calls.is_empty())
        {
            FinishReason::ToolCalls
        } else {
            FinishReason::Stop
        },
    );
    events.push(ProviderStreamEvent::Completed {
        finish_reason,
        usage,
    });
    events
}

/// Sleep that gives up promptly when the token cancels, returning the
/// cancellation reason.
fn sleep_cancellable(ms: u64, signal: Option<&CancelToken>) -> Option<Option<String>> {
    let deadline = Instant::now() + Duration::from_millis(ms);
    loop {
        if let Some(token) = signal
            && token.is_cancelled()
        {
            return Some(token.reason());
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn last_user_text(request: &ModelRequest) -> String {
    request
        .messages
        .iter()
        .rev()
        .find_map(|message| match message {
            crate::contract::ModelMessage::User { .. } => Some(message.text()),
            _ => None,
        })
        .unwrap_or_default()
}

/// What the default echo serves for a request. A transcript that ends with
/// tool results (a native tool loop between calls) echoes a bounded
/// reflection of those results, so tests can assert exactly which
/// tool-result messages reached the provider — call id, error flag, and a
/// content head, newest last. The budget is spent newest-first, so the
/// oldest results are dropped first and the reflection never exceeds the
/// whole-echo bound. Any other transcript echoes the last user message as
/// before.
fn echo_text(request: &ModelRequest) -> String {
    use crate::contract::ModelMessage;
    let mut lines: Vec<String> = Vec::new();
    let mut budget = ECHO_TOOL_RESULT_TOTAL_CHARS;
    for message in request
        .messages
        .iter()
        .rev()
        .take_while(|message| matches!(message, ModelMessage::ToolResult { .. }))
    {
        if lines.len() >= ECHO_TOOL_RESULT_LIMIT {
            break;
        }
        let ModelMessage::ToolResult {
            tool_call_id,
            content,
            is_error,
        } = message
        else {
            break;
        };
        let line = format!(
            "tool-result {tool_call_id} error={is_error}: {}",
            head_chars(content, ECHO_TOOL_RESULT_HEAD_CHARS)
        );
        let cost = line.chars().count() + 1; // the joining newline
        if cost > budget {
            break;
        }
        budget -= cost;
        lines.push(line);
    }
    if lines.is_empty() {
        return last_user_text(request);
    }
    lines.reverse();
    lines.join("\n")
}

/// How many trailing tool results the echo reflects (oldest dropped first).
const ECHO_TOOL_RESULT_LIMIT: usize = 8;
/// Per-result content head in the echo reflection.
const ECHO_TOOL_RESULT_HEAD_CHARS: usize = 120;
/// Whole-echo bound.
const ECHO_TOOL_RESULT_TOTAL_CHARS: usize = 400;

/// The first `limit` characters of a text.
fn head_chars(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
}

/// Split text into at most `parts` similarly sized chunks.
fn split_text(text: &str, parts: usize) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= 1 {
        return vec![text.to_owned()];
    }
    let size = chars.len().div_ceil(parts).max(1);
    chars
        .chunks(size)
        .map(|chunk| chunk.iter().collect())
        .collect()
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::collect::collect_turn;
    use crate::contract::{ModelMessage, StructuredOutput};

    fn base_request() -> ModelRequest {
        let mut request = ModelRequest::simple("fake-standard", "hello");
        request.purpose = Some("test".into());
        request
    }

    fn request_for(purpose: &str) -> ModelRequest {
        base_request().with_purpose(purpose)
    }

    fn step(purpose: &str, text: &str) -> FakeStep {
        FakeStep {
            r#match: FakeMatch {
                purpose: Some(PurposeMatch::Exact(purpose.into())),
                ..FakeMatch::default()
            },
            response: FakeResponse {
                text: Some(text.into()),
                ..FakeResponse::default()
            },
        }
    }

    #[test]
    fn routes_by_exact_purpose_string_and_regex() {
        let provider = FakeProvider::new(vec![
            step("intent", "intent response"),
            FakeStep {
                r#match: FakeMatch {
                    purpose: Some(PurposeMatch::pattern("^mode:").expect("valid regex")),
                    ..FakeMatch::default()
                },
                response: FakeResponse {
                    text: Some("mode response".into()),
                    ..FakeResponse::default()
                },
            },
        ]);

        let intent = collect_turn(provider.stream(&request_for("intent"))).expect("ok");
        let mode = collect_turn(provider.stream(&request_for("mode:discovery"))).expect("ok");
        assert_eq!(intent.text(), "intent response");
        assert_eq!(mode.text(), "mode response");
        let calls = provider.calls_by_purpose();
        assert_eq!(calls.get("intent"), Some(&1));
        assert_eq!(calls.get("mode:discovery"), Some(&1));
    }

    #[test]
    fn matches_model_and_min_call_constraints_in_order() {
        let provider = FakeProvider::with_fallback(
            vec![
                FakeStep {
                    r#match: FakeMatch {
                        model: Some("fake-mini".into()),
                        ..FakeMatch::default()
                    },
                    response: FakeResponse {
                        text: Some("from mini".into()),
                        ..FakeResponse::default()
                    },
                },
                FakeStep {
                    r#match: FakeMatch {
                        purpose: Some(PurposeMatch::Exact("p".into())),
                        min_call: Some(2),
                        ..FakeMatch::default()
                    },
                    response: FakeResponse {
                        text: Some("second-plus call".into()),
                        ..FakeResponse::default()
                    },
                },
            ],
            FakeResponse {
                text: Some("fallback".into()),
                ..FakeResponse::default()
            },
        );

        let first = collect_turn(provider.stream(&request_for("p"))).expect("ok");
        let second = collect_turn(provider.stream(&request_for("p"))).expect("ok");
        let mut third_request = request_for("p");
        third_request.model = "fake-mini".into();
        let third = collect_turn(provider.stream(&third_request)).expect("ok");

        assert_eq!(first.text(), "fallback");
        assert_eq!(second.text(), "second-plus call");
        assert_eq!(third.text(), "from mini");
        assert_eq!(provider.calls_by_purpose().get("p"), Some(&3));
    }

    #[test]
    fn scopes_steps_to_a_call_window_with_max_call() {
        let provider = FakeProvider::with_fallback(
            vec![
                FakeStep {
                    r#match: FakeMatch {
                        purpose: Some(PurposeMatch::Exact("seq".into())),
                        max_call: Some(1),
                        ..FakeMatch::default()
                    },
                    response: FakeResponse {
                        text: Some("first only".into()),
                        ..FakeResponse::default()
                    },
                },
                FakeStep {
                    r#match: FakeMatch {
                        purpose: Some(PurposeMatch::Exact("seq".into())),
                        min_call: Some(2),
                        max_call: Some(2),
                        ..FakeMatch::default()
                    },
                    response: FakeResponse {
                        text: Some("second only".into()),
                        ..FakeResponse::default()
                    },
                },
            ],
            FakeResponse {
                text: Some("later".into()),
                ..FakeResponse::default()
            },
        );

        let mut results = Vec::new();
        for _ in 0..3 {
            let turn = collect_turn(provider.stream(&request_for("seq"))).expect("ok");
            results.push(turn.text());
        }
        assert_eq!(results, ["first only", "second only", "later"]);
    }

    #[test]
    fn emits_tool_calls_with_unique_ids_default_usage_and_tool_calls_finish_reason() {
        let provider = FakeProvider::with_fallback(
            Vec::new(),
            FakeResponse {
                tool_calls: Some(vec![
                    FakeToolCallPlan {
                        name: "read-file".into(),
                        args: serde_json::json!({ "path": "a.ts" }),
                    },
                    FakeToolCallPlan {
                        name: "read-file".into(),
                        args: serde_json::json!({ "path": "b.ts" }),
                    },
                ]),
                ..FakeResponse::default()
            },
        );
        let result = collect_turn(provider.stream(&request_for("x"))).expect("ok");
        assert_eq!(
            result
                .tool_calls()
                .iter()
                .map(|call| call.name.as_str())
                .collect::<Vec<_>>(),
            ["read-file", "read-file"]
        );
        let ids: std::collections::HashSet<_> =
            result.tool_calls().iter().map(|c| c.id.clone()).collect();
        assert_eq!(ids.len(), 2);
        assert_eq!(result.finish_reason, FinishReason::ToolCalls);
        assert_eq!(result.usage.input_tokens, 100);
        assert_eq!(result.usage.output_tokens, 50);
    }

    #[test]
    fn emits_structured_output_only_when_the_request_asks_for_it() {
        let provider = FakeProvider::with_fallback(
            Vec::new(),
            FakeResponse {
                structured_output: Some(serde_json::json!({ "mode": "planning" })),
                ..FakeResponse::default()
            },
        );

        let without = collect_turn(provider.stream(&request_for("x"))).expect("ok");
        assert!(without.structured_output.is_none());

        let mut with_schema = request_for("x");
        with_schema.structured_output = Some(crate::contract::StructuredOutputRequest {
            name: "intent-proposal".into(),
            schema: serde_json::Map::from_iter([(
                "type".to_owned(),
                Value::String("object".into()),
            )]),
        });
        let with_result = collect_turn(provider.stream(&with_schema)).expect("ok");
        assert_eq!(
            with_result.structured_output,
            Some(StructuredOutput {
                name: "intent-proposal".into(),
                value: serde_json::json!({ "mode": "planning" })
            })
        );
    }

    #[test]
    fn fails_fail_times_times_with_the_scripted_error_then_succeeds() {
        let provider = FakeProvider::new(vec![FakeStep {
            r#match: FakeMatch {
                purpose: Some(PurposeMatch::Exact("flaky".into())),
                ..FakeMatch::default()
            },
            response: FakeResponse {
                text: Some("finally ok".into()),
                error: Some(FakeErrorPlan {
                    kind: ProviderErrorKind::Server,
                    retryable: true,
                    message: "boom".into(),
                    fail_times: Some(2),
                }),
                ..FakeResponse::default()
            },
        }]);

        for expected_kind in [ProviderErrorKind::Server, ProviderErrorKind::Server] {
            let error = collect_turn(provider.stream(&request_for("flaky"))).expect_err("fails");
            assert_eq!(error.kind, expected_kind);
            assert!(error.retryable);
            assert_eq!(error.message, "boom");
        }
        let third = collect_turn(provider.stream(&request_for("flaky"))).expect("ok");
        assert_eq!(third.text(), "finally ok");
        assert_eq!(provider.calls_by_purpose().get("flaky"), Some(&3));
    }

    #[test]
    fn records_a_call_log_with_purpose_model_and_response_text() {
        let provider = FakeProvider::new(vec![step("a", "alpha")]);
        collect_turn(provider.stream(&request_for("a"))).expect("ok");
        let mut mini = request_for("a");
        mini.model = "fake-mini".into();
        collect_turn(provider.stream(&mini)).expect("ok");
        assert_eq!(
            provider.call_log(),
            vec![
                FakeCallLogEntry {
                    purpose: Some("a".into()),
                    model: "fake-standard".into(),
                    text: "alpha".into()
                },
                FakeCallLogEntry {
                    purpose: Some("a".into()),
                    model: "fake-mini".into(),
                    text: "alpha".into()
                },
            ]
        );
    }

    #[test]
    fn cancels_during_delay_ms_when_the_signal_aborts() {
        let token = CancelToken::new();
        let provider = FakeProvider::with_fallback(
            Vec::new(),
            FakeResponse {
                text: Some("slow".into()),
                delay_ms: Some(10_000),
                ..FakeResponse::default()
            },
        );
        let watcher = token.clone();
        let canceller = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(15));
            watcher.cancel("user");
        });
        let mut request = request_for("x");
        request.signal = Some(token);
        let error = collect_turn(provider.stream(&request)).expect_err("canceled");
        canceller.join().expect("canceller finishes");
        assert_eq!(error.kind, ProviderErrorKind::Canceled);
        assert!(!error.retryable);
        assert!(
            error.message.contains("canceled during its delay: user"),
            "{}",
            error.message
        );
    }

    #[test]
    fn splits_text_across_multiple_deltas_so_folding_is_exercised() {
        let provider = FakeProvider::with_fallback(
            Vec::new(),
            FakeResponse {
                text: Some("abcdefghij".into()),
                ..FakeResponse::default()
            },
        );
        let mut deltas = Vec::new();
        for event in provider.stream(&request_for("x")) {
            if let ProviderStreamEvent::TextDelta { text } = event {
                deltas.push(text);
            }
        }
        assert!(deltas.len() >= 2, "{deltas:?}");
        assert_eq!(deltas.concat(), "abcdefghij");
    }

    #[test]
    fn echoes_the_last_user_message_when_no_step_matches() {
        let provider = FakeProvider::new(vec![step("other", "nope")]);
        let mut request = request_for("x");
        request.messages = vec![
            ModelMessage::user_text("first"),
            ModelMessage::Assistant {
                content: vec![crate::contract::AssistantContent::Text {
                    text: "middle".into(),
                }],
            },
            ModelMessage::user_text("last question"),
        ];
        let result = collect_turn(provider.stream(&request)).expect("ok");
        assert_eq!(result.text(), "last question");

        let echo = FakeProvider::echo();
        let echoed = collect_turn(echo.stream(&request_for("y"))).expect("ok");
        assert_eq!(echoed.text(), "hello");
    }

    #[test]
    fn echoes_trailing_tool_results_for_tool_loop_transcripts() {
        // A native tool loop sends assistant tool-use turns followed by
        // tool results; the echo reflects exactly what reached the
        // provider — ids, error flags, content heads, newest last.
        let provider = FakeProvider::new(Vec::new());
        let mut request = request_for("x");
        request.messages = vec![
            ModelMessage::user_text("opening"),
            ModelMessage::Assistant {
                content: vec![crate::contract::AssistantContent::ToolCall {
                    call: ToolCall {
                        id: "fake_1_1".into(),
                        name: "write-file".into(),
                        arguments: serde_json::json!({ "path": "src/a.rs" }),
                    },
                }],
            },
            ModelMessage::tool_result("fake_1_1", "wrote src/a.rs", false).expect("bounded"),
            ModelMessage::tool_result("fake_1_2", "denied by capability grant", true)
                .expect("bounded"),
        ];
        let result = collect_turn(provider.stream(&request)).expect("ok");
        assert_eq!(
            result.text(),
            "tool-result fake_1_1 error=false: wrote src/a.rs\n\
             tool-result fake_1_2 error=true: denied by capability grant"
        );
    }

    #[test]
    fn the_tool_result_echo_is_bounded() {
        let provider = FakeProvider::new(Vec::new());
        let mut request = request_for("x");
        let mut messages = vec![ModelMessage::user_text("opening")];
        for index in 0..12 {
            messages.push(
                ModelMessage::tool_result(
                    format!("fake_9_{index}"),
                    format!("{} output for call {index}", "x".repeat(500)),
                    false,
                )
                .expect("bounded"),
            );
        }
        request.messages = messages;
        let result = collect_turn(provider.stream(&request)).expect("ok");
        let text = result.text();
        assert!(
            text.chars().count() <= ECHO_TOOL_RESULT_TOTAL_CHARS,
            "{text}"
        );
        // The budget is spent newest-first: the oldest results drop out
        // and each reflected content is headed.
        assert!(text.contains("fake_9_11"), "{text}");
        assert!(text.contains("fake_9_10"), "{text}");
        assert!(!text.contains("fake_9_0"), "{text}");
        assert!(!text.contains("fake_9_9"), "{text}");
        assert!(!text.contains("output for call"), "{text}");
    }

    #[test]
    fn honors_scripted_finish_reason_and_usage_overrides() {
        let provider = FakeProvider::with_fallback(
            Vec::new(),
            FakeResponse {
                text: Some("trunc".into()),
                finish_reason: Some(FinishReason::Length),
                usage: Some(TokenUsage {
                    input_tokens: 5,
                    output_tokens: 6,
                }),
                ..FakeResponse::default()
            },
        );
        let result = collect_turn(provider.stream(&request_for("x"))).expect("ok");
        assert_eq!(result.finish_reason, FinishReason::Length);
        assert_eq!(result.usage.input_tokens, 5);
        assert_eq!(result.usage.output_tokens, 6);
    }

    #[test]
    fn reports_name_and_models() {
        let provider = FakeProvider::echo();
        assert_eq!(provider.name(), AdapterKind::Fake);
        assert_eq!(provider.models(), vec!["fake-standard", "fake-mini"]);
    }

    #[test]
    fn reports_the_maximally_permissive_fake_capability_defaults() {
        let provider = FakeProvider::echo();
        assert_eq!(
            provider.capabilities(),
            AdapterCapabilities::defaults_for(AdapterKind::Fake)
        );
        assert_eq!(
            provider.capabilities().tool_calls,
            crate::contract::FeatureSupport::Supported
        );
        assert_eq!(
            provider.capabilities().structured_output,
            crate::contract::StructuredOutputMode::JsonSchema
        );
    }
}
