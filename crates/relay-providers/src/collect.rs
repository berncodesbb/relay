//! Fold a provider event stream into a single model turn result.
//!
//! Ported from `src/providers/collect.ts` (retired TypeScript tree, removed
//! from the repository 2026-10-14). The agent loop streams events for
//! live rendering and then needs one aggregated value to decide what happened.
//! [`collect_turn`] is that fold: it concatenates text deltas, gathers tool
//! calls in arrival order, keeps the last reported usage, captures structured
//! output, and converts `error` events into returned [`ProviderFailure`]s.

use crate::contract::TokenUsage;

use crate::contract::{
    FinishReason, ModelTurnResult, ProviderFailure, ProviderStreamEvent, StructuredOutput, ToolCall,
};

/// Aggregate one provider turn from its stream of events.
///
/// Returns the failure carried by the first `error` event; a stream that ends
/// without a `completed` event defaults to finish reason `stop` and zero
/// usage, exactly like the TypeScript fold.
pub fn collect_turn(
    stream: impl Iterator<Item = ProviderStreamEvent>,
) -> Result<ModelTurnResult, ProviderFailure> {
    let mut text = String::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    let mut usage: Option<TokenUsage> = None;
    let mut structured_output: Option<StructuredOutput> = None;
    let mut finish_reason: Option<FinishReason> = None;

    for event in stream {
        match event {
            ProviderStreamEvent::TextDelta { text: delta } => {
                text.push_str(&delta);
            }
            ProviderStreamEvent::ToolCall { call } => {
                tool_calls.push(call);
            }
            ProviderStreamEvent::Usage { usage: reported } => {
                usage = Some(reported);
            }
            ProviderStreamEvent::StructuredOutput { name, value } => {
                structured_output = Some(StructuredOutput { name, value });
            }
            ProviderStreamEvent::Completed {
                finish_reason: reason,
                usage: final_usage,
            } => {
                finish_reason = Some(reason);
                // The completed event carries the authoritative final usage.
                usage = Some(final_usage);
            }
            ProviderStreamEvent::Error { error } => return Err(error),
        }
    }

    Ok(ModelTurnResult {
        text,
        tool_calls,
        structured_output,
        finish_reason: finish_reason.unwrap_or(FinishReason::Stop),
        usage: usage.unwrap_or(TokenUsage {
            input_tokens: 0,
            output_tokens: 0,
        }),
    })
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::contract::ProviderErrorKind;
    use serde_json::json;

    fn usage(input: i64, output: i64) -> TokenUsage {
        TokenUsage {
            input_tokens: input,
            output_tokens: output,
        }
    }

    #[test]
    fn concatenates_text_deltas_in_order() {
        let result = collect_turn(
            [
                ProviderStreamEvent::TextDelta {
                    text: "Hello ".into(),
                },
                ProviderStreamEvent::TextDelta { text: "wor".into() },
                ProviderStreamEvent::TextDelta { text: "ld".into() },
                ProviderStreamEvent::Completed {
                    finish_reason: FinishReason::Stop,
                    usage: usage(1, 2),
                },
            ]
            .into_iter(),
        )
        .expect("stream completes");
        assert_eq!(result.text, "Hello world");
        assert_eq!(result.finish_reason, FinishReason::Stop);
    }

    #[test]
    fn collects_tool_calls_in_arrival_order() {
        let result = collect_turn(
            [
                ProviderStreamEvent::TextDelta {
                    text: "thinking".into(),
                },
                ProviderStreamEvent::ToolCall {
                    call: ToolCall {
                        id: "a".into(),
                        name: "first".into(),
                        arguments: json!({ "n": 1 }),
                    },
                },
                ProviderStreamEvent::ToolCall {
                    call: ToolCall {
                        id: "b".into(),
                        name: "second".into(),
                        arguments: json!({ "n": 2 }),
                    },
                },
                ProviderStreamEvent::Completed {
                    finish_reason: FinishReason::ToolCalls,
                    usage: usage(0, 0),
                },
            ]
            .into_iter(),
        )
        .expect("stream completes");
        assert_eq!(
            result
                .tool_calls
                .iter()
                .map(|call| call.id.as_str())
                .collect::<Vec<_>>(),
            ["a", "b"]
        );
        assert_eq!(result.finish_reason, FinishReason::ToolCalls);
    }

    #[test]
    fn uses_completed_usage_over_earlier_usage_events() {
        let result = collect_turn(
            [
                ProviderStreamEvent::Usage { usage: usage(5, 5) },
                ProviderStreamEvent::Usage { usage: usage(9, 7) },
                ProviderStreamEvent::Completed {
                    finish_reason: FinishReason::Stop,
                    usage: usage(12, 8),
                },
            ]
            .into_iter(),
        )
        .expect("stream completes");
        assert_eq!(result.usage, usage(12, 8));
    }

    #[test]
    fn captures_structured_output_events() {
        let result = collect_turn(
            [
                ProviderStreamEvent::TextDelta {
                    text: "{\"kind\":\"intent\"}".into(),
                },
                ProviderStreamEvent::StructuredOutput {
                    name: "intent-proposal".into(),
                    value: json!({ "kind": "intent" }),
                },
                ProviderStreamEvent::Completed {
                    finish_reason: FinishReason::Stop,
                    usage: usage(1, 1),
                },
            ]
            .into_iter(),
        )
        .expect("stream completes");
        assert_eq!(
            result.structured_output,
            Some(StructuredOutput {
                name: "intent-proposal".into(),
                value: json!({ "kind": "intent" })
            })
        );
    }

    #[test]
    fn defaults_finish_reason_and_usage_when_the_stream_ends_without_completion() {
        let result = collect_turn(
            [ProviderStreamEvent::TextDelta {
                text: "partial".into(),
            }]
            .into_iter(),
        )
        .expect("stream completes");
        assert_eq!(result.finish_reason, FinishReason::Stop);
        assert_eq!(result.usage, usage(0, 0));
        assert_eq!(result.text, "partial");
    }

    #[test]
    fn returns_the_failure_carried_by_error_events() {
        let failure = ProviderFailure::new(ProviderErrorKind::RateLimit, "too many requests")
            .retryable(true)
            .status(429);
        let error = collect_turn(
            [
                ProviderStreamEvent::TextDelta { text: "x".into() },
                ProviderStreamEvent::Error {
                    error: failure.clone(),
                },
            ]
            .into_iter(),
        )
        .expect_err("error events fail the turn");
        assert_eq!(error, failure);
    }
}
