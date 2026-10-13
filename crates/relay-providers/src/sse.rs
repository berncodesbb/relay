//! Server-sent-events reading and the shared HTTP fetch plumbing used by the
//! wire adapters.
//!
//! Everything here is private to the crate: provider wire types and transport
//! details never cross an adapter boundary. The fetch helper runs the blocking
//! HTTP exchange on a worker thread and forwards parsed SSE messages over a
//! bounded channel so the consumer can observe cancellation between events.

use std::io::{BufRead, BufReader, Read};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};

use crate::contract::ProviderFailure;
use crate::errors::{ErrorSignal, failure_from_reqwest, to_provider_failure};

/// One parsed SSE message (`event:` name plus joined `data:` payload).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SseMessage {
    /// The `event:` field, when the server sent one.
    pub event: Option<String>,
    /// The `data:` payload (multiple data lines joined with `\n`).
    pub data: String,
}

/// Reads SSE messages from a byte stream.
pub(crate) struct SseReader<R> {
    reader: BufReader<R>,
}

impl<R: Read> SseReader<R> {
    /// Wrap a byte stream.
    pub(crate) fn new(reader: R) -> Self {
        SseReader {
            reader: BufReader::with_capacity(16 * 1024, reader),
        }
    }

    /// Read the next complete SSE message, or `None` at clean end of stream.
    pub(crate) fn next_message(&mut self) -> std::io::Result<Option<SseMessage>> {
        let mut event: Option<String> = None;
        let mut data_lines: Vec<String> = Vec::new();
        let mut saw_field = false;

        let mut line: Vec<u8> = Vec::new();
        loop {
            line.clear();
            let read = self.reader.read_until(b'\n', &mut line)?;
            if read == 0 {
                // End of stream: emit a final message only if fields arrived
                // without the terminating blank line.
                if saw_field {
                    return Ok(Some(SseMessage {
                        event: event.take(),
                        data: data_lines.join("\n"),
                    }));
                }
                return Ok(None);
            }
            if line.last() == Some(&b'\n') {
                line.pop();
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
            }
            if line.is_empty() {
                if saw_field {
                    return Ok(Some(SseMessage {
                        event: event.take(),
                        data: data_lines.join("\n"),
                    }));
                }
                continue;
            }
            if line.first() == Some(&b':') {
                // Comment / keep-alive line.
                continue;
            }
            saw_field = true;
            let text = String::from_utf8_lossy(&line).into_owned();
            let (field, value) = match text.find(':') {
                Some(colon) => {
                    let value = &text[colon + 1..];
                    (&text[..colon], value.strip_prefix(' ').unwrap_or(value))
                }
                None => (text.as_str(), ""),
            };
            match field {
                "data" => data_lines.push(value.to_owned()),
                "event" => event = Some(value.to_owned()),
                _ => {}
            }
        }
    }
}

/// Items produced by a background SSE fetch.
pub(crate) enum FetchItem {
    /// One parsed SSE message.
    Message(SseMessage),
    /// The response body ended cleanly.
    Eof,
    /// The exchange failed before or during streaming.
    Failed(ProviderFailure),
}

/// How many parsed messages may sit in the channel before the reader thread
/// blocks (backpressure against a stalled consumer).
const CHANNEL_CAPACITY: usize = 64;

/// Upper bound on an error response body kept for classification.
const MAX_ERROR_BODY_BYTES: u64 = 64 * 1024;

/// Execute a POST that returns an SSE stream on a worker thread, forwarding
/// parsed messages over a bounded channel. HTTP failures are classified with
/// the shared error normalizer (message from the provider's `error.message`
/// field when present, bounded and redacted); transport failures map through
/// the reqwest classifier. The worker is detached: the consumer drops the
/// channel when it stops reading.
pub(crate) fn fetch_sse(request: reqwest::blocking::RequestBuilder) -> Receiver<FetchItem> {
    let (sender, receiver) = sync_channel(CHANNEL_CAPACITY);
    let fallback_sender = sender.clone();
    let spawned = std::thread::Builder::new()
        .name("relay-provider-fetch".into())
        .spawn(move || {
            let outcome = run_fetch(&sender, request);
            if let Some(failure) = outcome {
                let _ = sender.send(FetchItem::Failed(failure));
            } else {
                let _ = sender.send(FetchItem::Eof);
            }
        });
    if spawned.is_err() {
        // Spawning the reader thread failed: report a bounded failure instead
        // of panicking inside a provider stream.
        let _ = fallback_sender.send(FetchItem::Failed(to_provider_failure(
            ErrorSignal::from_message("provider stream worker could not start"),
        )));
    }
    receiver
}

fn run_fetch(
    sender: &SyncSender<FetchItem>,
    request: reqwest::blocking::RequestBuilder,
) -> Option<ProviderFailure> {
    let response = match request.send() {
        Ok(response) => response,
        Err(error) => return Some(failure_from_reqwest(&error)),
    };
    let status = response.status();
    if !status.is_success() {
        let retry_after = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .map(|value| value.to_owned());
        let body = bounded_body(response);
        return Some(to_provider_failure(ErrorSignal {
            status: Some(status.as_u16()),
            message: provider_error_message(&body),
            retry_after,
            ..ErrorSignal::default()
        }));
    }
    let mut reader = SseReader::new(response);
    loop {
        match reader.next_message() {
            Ok(Some(message)) => {
                if sender.send(FetchItem::Message(message)).is_err() {
                    // Consumer dropped: stop reading.
                    return None;
                }
            }
            Ok(None) => return None,
            Err(error) => {
                let failure = to_provider_failure(ErrorSignal {
                    name: Some("APIConnectionError".into()),
                    message: error.to_string(),
                    ..ErrorSignal::default()
                });
                return Some(failure);
            }
        }
    }
}

/// Read at most [`MAX_ERROR_BODY_BYTES`] of an error response, decoded
/// lossily so a binary body cannot crash classification.
fn bounded_body(response: reqwest::blocking::Response) -> String {
    let mut limited = response.take(MAX_ERROR_BODY_BYTES);
    let mut bytes = Vec::new();
    let _ = limited.read_to_end(&mut bytes);
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Extract the provider's `error.message` field from an error response body,
/// bounded by the message trim in the failure normalizer. Bodies are never
/// included wholesale.
fn provider_error_message(body: &str) -> String {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        return String::new();
    };
    let message = value
        .get("error")
        .and_then(|error| error.get("message"))
        .and_then(|message| message.as_str())
        .unwrap_or("");
    message.to_owned()
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    fn reader(body: &str) -> Vec<SseMessage> {
        let mut sse = SseReader::new(body.as_bytes());
        let mut messages = Vec::new();
        while let Ok(Some(message)) = sse.next_message() {
            messages.push(message);
        }
        messages
    }

    #[test]
    fn parses_data_and_event_fields() {
        let messages = reader("event: message_start\ndata: {\"a\":1}\n\ndata: [DONE]\n\n");
        assert_eq!(
            messages,
            vec![
                SseMessage {
                    event: Some("message_start".into()),
                    data: "{\"a\":1}".into()
                },
                SseMessage {
                    event: None,
                    data: "[DONE]".into()
                },
            ]
        );
    }

    #[test]
    fn joins_multiple_data_lines_and_handles_crlf() {
        let messages = reader("data: one\r\ndata: two\r\n\r\n");
        assert_eq!(
            messages,
            vec![SseMessage {
                event: None,
                data: "one\ntwo".into()
            }]
        );
    }

    #[test]
    fn skips_comments_and_ignores_unknown_fields() {
        let messages = reader(": keep-alive\nid: 7\ndata: x\n\n");
        assert_eq!(
            messages,
            vec![SseMessage {
                event: None,
                data: "x".into()
            }]
        );
    }

    #[test]
    fn emits_trailing_message_without_final_blank_line() {
        let messages = reader("data: tail");
        assert_eq!(
            messages,
            vec![SseMessage {
                event: None,
                data: "tail".into()
            }]
        );
    }

    #[test]
    fn extracts_provider_error_message_from_bodies() {
        assert_eq!(
            provider_error_message(
                r#"{"error":{"message":"Incorrect API key","type":"invalid_request_error"}}"#
            ),
            "Incorrect API key"
        );
        assert_eq!(provider_error_message("plain text"), "");
        assert_eq!(provider_error_message("{\"error\":{}}"), "");
    }
}
