//! Fold a provider event stream into a single model turn result.
//!
//! Ported from `src/providers/collect.ts` (retired TypeScript tree, removed
//! from the repository 2026-10-14). The agent loop streams events for
//! live rendering and then needs one aggregated value to decide what happened.
//! [`collect_turn`] is that fold: it reconstructs the ordered assistant block
//! list (text, tool calls, opaque continuations — see the ordering contract
//! on [`ProviderStreamEvent`]), keeps the last reported usage with the
//! terminal event authoritative, captures structured output, and converts
//! `error` events into returned [`ProviderFailure`]s.
//!
//! The fold is strict about termination and bounds: a stream that ends
//! without a terminal `completed` or `error` event, or whose provider-
//! produced content exceeds the contract bounds, fails closed with an
//! [`ProviderErrorKind::InvalidResponse`](crate::contract::ProviderErrorKind)
//! failure rather than defaulting to success.

use crate::contract::{
    AssistantContent, MAX_CONTINUATION_DATA_BYTES, MAX_CONTINUATION_DISCRIMINATOR_CHARS,
    MAX_TEXT_BLOCK_BYTES, ModelTurnResult, ProviderErrorKind, ProviderFailure, ProviderStreamEvent,
    StructuredOutput,
};

/// Aggregate one provider turn from its stream of events.
///
/// Block-bearing events are folded in true order: consecutive text deltas
/// join one open text block, and a complete tool call or continuation
/// closes the open block first, so text → call → text survives as three
/// ordered blocks. The first terminal event ends the fold: a `completed`
/// event returns the turn with its own usage as the authoritative total
/// (earlier `usage` events are provisional snapshots for live views, never
/// replacements for it), and an `error` event returns its failure. A
/// stream that ends without either terminal is an invalid response — there
/// is no implicit success — and provider-produced content above the
/// contract bounds ([`MAX_TEXT_BLOCK_BYTES`], [`MAX_CONTINUATION_DATA_BYTES`],
/// [`MAX_CONTINUATION_DISCRIMINATOR_CHARS`]) fails closed instead of being
/// silently truncated.
pub fn collect_turn(
    stream: impl Iterator<Item = ProviderStreamEvent>,
) -> Result<ModelTurnResult, ProviderFailure> {
    let mut blocks: Vec<AssistantContent> = Vec::new();
    // The currently open text block, closed by any tool-call or continuation
    // block and by the terminal event.
    let mut open_text = String::new();
    let mut structured_output: Option<StructuredOutput> = None;

    for event in stream {
        match event {
            ProviderStreamEvent::TextDelta { text: delta } => {
                open_text.push_str(&delta);
                if open_text.len() > MAX_TEXT_BLOCK_BYTES {
                    return Err(ProviderFailure::new(
                        ProviderErrorKind::InvalidResponse,
                        format!(
                            "assistant text block exceeded the {}-byte bound",
                            MAX_TEXT_BLOCK_BYTES
                        ),
                    ));
                }
            }
            ProviderStreamEvent::ToolCall { call } => {
                close_text_block(&mut blocks, &mut open_text);
                blocks.push(AssistantContent::ToolCall { call });
            }
            ProviderStreamEvent::Continuation {
                discriminator,
                data,
            } => {
                if discriminator.chars().count() > MAX_CONTINUATION_DISCRIMINATOR_CHARS {
                    return Err(ProviderFailure::new(
                        ProviderErrorKind::InvalidResponse,
                        format!(
                            "continuation discriminator exceeded the {}-character bound",
                            MAX_CONTINUATION_DISCRIMINATOR_CHARS
                        ),
                    ));
                }
                if data.len() > MAX_CONTINUATION_DATA_BYTES {
                    return Err(ProviderFailure::new(
                        ProviderErrorKind::InvalidResponse,
                        format!(
                            "continuation block exceeded the {}-byte bound",
                            MAX_CONTINUATION_DATA_BYTES
                        ),
                    ));
                }
                close_text_block(&mut blocks, &mut open_text);
                blocks.push(AssistantContent::Continuation {
                    discriminator,
                    data,
                });
            }
            ProviderStreamEvent::Usage { usage: _ } => {
                // Provisional snapshot: the terminal event's usage is the
                // authoritative total, so the fold does not retain these.
            }
            ProviderStreamEvent::StructuredOutput { name, value } => {
                structured_output = Some(StructuredOutput { name, value });
            }
            ProviderStreamEvent::Completed {
                finish_reason,
                usage: final_usage,
            } => {
                close_text_block(&mut blocks, &mut open_text);
                return Ok(ModelTurnResult {
                    blocks,
                    structured_output,
                    finish_reason,
                    // The completed event carries the authoritative final
                    // usage; earlier usage events are provisional snapshots.
                    usage: final_usage,
                });
            }
            ProviderStreamEvent::Error { error } => return Err(error),
        }
    }

    Err(ProviderFailure::new(
        ProviderErrorKind::InvalidResponse,
        "provider stream ended without a terminal completed or error event",
    ))
}

/// Move the accumulated open text block, if any, into the block list.
fn close_text_block(blocks: &mut Vec<AssistantContent>, open_text: &mut String) {
    if !open_text.is_empty() {
        blocks.push(AssistantContent::Text {
            text: std::mem::take(open_text),
        });
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::contract::{FinishReason, ModelMessage, TokenUsage, ToolCall};
    use serde_json::json;

    /// A completed call for stream-folding tests.
    fn call(id: &str, name: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            name: name.into(),
            arguments: json!({}),
        }
    }

    fn usage(input: i64, output: i64) -> TokenUsage {
        TokenUsage {
            input_tokens: input,
            output_tokens: output,
        }
    }

    fn completed(reason: FinishReason) -> ProviderStreamEvent {
        ProviderStreamEvent::Completed {
            finish_reason: reason,
            usage: usage(1, 1),
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
        assert_eq!(result.text(), "Hello world");
        assert_eq!(result.finish_reason, FinishReason::Stop);
        assert_eq!(result.usage, usage(1, 2));
    }

    #[test]
    fn preserves_interleaved_text_and_tool_block_order() {
        let result = collect_turn(
            [
                ProviderStreamEvent::TextDelta {
                    text: "checking ".into(),
                },
                ProviderStreamEvent::TextDelta {
                    text: "first".into(),
                },
                ProviderStreamEvent::ToolCall {
                    call: call("a", "read-file"),
                },
                ProviderStreamEvent::TextDelta {
                    text: "then ".into(),
                },
                ProviderStreamEvent::TextDelta {
                    text: "second".into(),
                },
                completed(FinishReason::ToolCalls),
            ]
            .into_iter(),
        )
        .expect("stream completes");
        // The authoritative order interleaves; the accessors are views.
        let kinds: Vec<&str> = result
            .blocks
            .iter()
            .map(|block| match block {
                AssistantContent::Text { .. } => "text",
                AssistantContent::ToolCall { .. } => "tool-call",
                AssistantContent::Continuation { .. } => "continuation",
            })
            .collect();
        assert_eq!(kinds, ["text", "tool-call", "text"]);
        assert_eq!(
            result.blocks,
            vec![
                AssistantContent::Text {
                    text: "checking first".into()
                },
                AssistantContent::ToolCall {
                    call: call("a", "read-file")
                },
                AssistantContent::Text {
                    text: "then second".into()
                },
            ]
        );
        assert_eq!(result.text(), "checking firstthen second");
    }

    #[test]
    fn collects_tool_calls_in_stable_arrival_order() {
        let result = collect_turn(
            [
                ProviderStreamEvent::ToolCall {
                    call: call("a", "first"),
                },
                ProviderStreamEvent::TextDelta { text: "mid".into() },
                ProviderStreamEvent::ToolCall {
                    call: call("b", "second"),
                },
                ProviderStreamEvent::ToolCall {
                    call: call("c", "third"),
                },
                completed(FinishReason::ToolCalls),
            ]
            .into_iter(),
        )
        .expect("stream completes");
        assert_eq!(
            result
                .tool_calls()
                .iter()
                .map(|call| call.id.as_str())
                .collect::<Vec<_>>(),
            ["a", "b", "c"]
        );
        assert_eq!(result.finish_reason, FinishReason::ToolCalls);
    }

    #[test]
    fn continuation_blocks_keep_their_place_and_are_bounded() {
        let result = collect_turn(
            [
                ProviderStreamEvent::TextDelta {
                    text: "before".into(),
                },
                ProviderStreamEvent::Continuation {
                    discriminator: "thinking".into(),
                    data: "opaque".into(),
                },
                ProviderStreamEvent::TextDelta {
                    text: "after".into(),
                },
                completed(FinishReason::Stop),
            ]
            .into_iter(),
        )
        .expect("stream completes");
        assert_eq!(result.text(), "beforeafter", "continuation is not text");
        assert!(matches!(
            result.blocks[1],
            AssistantContent::Continuation { .. }
        ));

        // The data bound itself is accepted at exactly the limit...
        let at_limit = collect_turn(
            [
                ProviderStreamEvent::Continuation {
                    discriminator: "thinking".into(),
                    data: "x".repeat(MAX_CONTINUATION_DATA_BYTES),
                },
                completed(FinishReason::Stop),
            ]
            .into_iter(),
        );
        assert!(at_limit.is_ok(), "the bound itself is allowed");

        // ...and one byte over fails closed as an invalid response.
        let error = collect_turn(
            [
                ProviderStreamEvent::Continuation {
                    discriminator: "thinking".into(),
                    data: "x".repeat(MAX_CONTINUATION_DATA_BYTES + 1),
                },
                completed(FinishReason::Stop),
            ]
            .into_iter(),
        )
        .expect_err("oversized continuation data fails");
        assert_eq!(error.kind, ProviderErrorKind::InvalidResponse);
        assert!(error.message.contains("continuation"), "{}", error.message);

        // Over-long discriminators fail the same way.
        let error = collect_turn(
            [
                ProviderStreamEvent::Continuation {
                    discriminator: "d".repeat(MAX_CONTINUATION_DISCRIMINATOR_CHARS + 1),
                    data: "x".into(),
                },
                completed(FinishReason::Stop),
            ]
            .into_iter(),
        )
        .expect_err("oversized discriminator fails");
        assert_eq!(error.kind, ProviderErrorKind::InvalidResponse);
        assert!(error.message.contains("discriminator"), "{}", error.message);
    }

    #[test]
    fn text_blocks_above_the_bound_fail_closed() {
        let error = collect_turn(
            [
                ProviderStreamEvent::TextDelta {
                    text: "x".repeat(MAX_TEXT_BLOCK_BYTES + 1),
                },
                completed(FinishReason::Stop),
            ]
            .into_iter(),
        )
        .expect_err("oversized text block fails");
        assert_eq!(error.kind, ProviderErrorKind::InvalidResponse);
        assert!(error.message.contains("text block"), "{}", error.message);
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
    fn a_stream_without_a_terminal_event_is_an_invalid_response() {
        let error = collect_turn(
            [ProviderStreamEvent::TextDelta {
                text: "partial".into(),
            }]
            .into_iter(),
        )
        .expect_err("missing terminal event fails");
        assert_eq!(error.kind, ProviderErrorKind::InvalidResponse);
        assert!(!error.retryable);
        assert!(
            error
                .message
                .contains("without a terminal completed or error event"),
            "{}",
            error.message
        );

        // Even an entirely empty stream is a failure, not a silent success.
        let error = collect_turn(std::iter::empty()).expect_err("empty stream fails");
        assert_eq!(error.kind, ProviderErrorKind::InvalidResponse);

        // Usage snapshots alone do not terminate a stream either.
        let error = collect_turn([ProviderStreamEvent::Usage { usage: usage(3, 3) }].into_iter())
            .expect_err("usage alone does not complete");
        assert_eq!(error.kind, ProviderErrorKind::InvalidResponse);
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

    #[test]
    fn mid_stream_cancellation_surfaces_as_the_canceled_failure() {
        let failure = ProviderFailure::new(
            ProviderErrorKind::Canceled,
            "request aborted before completion",
        );
        let error = collect_turn(
            [
                ProviderStreamEvent::TextDelta { text: "x".into() },
                ProviderStreamEvent::Error {
                    error: failure.clone(),
                },
            ]
            .into_iter(),
        )
        .expect_err("canceled mid-stream fails the turn");
        assert_eq!(error, failure);
    }

    #[test]
    fn tool_result_round_trips_the_call_id_through_the_transcript() {
        // Correlation contract: the id a ToolCall block carries is the id
        // the answering ToolResult must return.
        let result = collect_turn(
            [
                ProviderStreamEvent::ToolCall {
                    call: ToolCall {
                        id: "tu_42".into(),
                        name: "read-file".into(),
                        arguments: json!({ "path": "a" }),
                    },
                },
                completed(FinishReason::ToolCalls),
            ]
            .into_iter(),
        )
        .expect("stream completes");
        let call = &result.tool_calls()[0];
        let answer = ModelMessage::tool_result(&call.id, "file body", false).expect("bounded");
        match answer {
            ModelMessage::ToolResult {
                tool_call_id,
                content,
                is_error,
            } => {
                assert_eq!(tool_call_id, "tu_42", "the id round-trips");
                assert_eq!(content, "file body");
                assert!(!is_error);
            }
            other => panic!("expected a tool result, got {other:?}"),
        }
    }

    #[test]
    fn empty_turns_produce_an_empty_block_list() {
        let result = collect_turn(
            [ProviderStreamEvent::Completed {
                finish_reason: FinishReason::Stop,
                usage: TokenUsage::default(),
            }]
            .into_iter(),
        )
        .expect("stream completes");
        assert!(result.blocks.is_empty());
        assert_eq!(result.text(), "");
        assert!(result.tool_calls().is_empty());
    }
}
