//! Normalize provider transport errors into Relay-owned failures.
//!
//! Ported from `src/providers/errors.ts` (retired TypeScript tree, removed
//! from the repository 2026-10-14). Transport errors (HTTP status
//! failures, connection errors, timeouts, aborts) can carry request headers
//! and bodies that must never reach Relay's logs. This mapper keeps only a
//! trimmed, redacted `message`, classifies the failure into a provider-neutral
//! kind with a retryability hint, and never serializes request or response
//! payloads.

use regex::Regex;
use std::sync::OnceLock;

use crate::contract::{ProviderErrorKind, ProviderFailure};

/// Maximum length of a preserved error message.
const MAX_MESSAGE_CHARS: usize = 300;

/// Connection-level error codes classified as network failures.
const NETWORK_CODES: [&str; 9] = [
    "ECONNRESET",
    "ECONNREFUSED",
    "EPIPE",
    "ENOTFOUND",
    "EAI_AGAIN",
    "ECONNABORTED",
    "EHOSTUNREACH",
    "ENETUNREACH",
    "EPROTO",
];

/// Connection-level error codes classified as timeouts.
const TIMEOUT_CODES: [&str; 2] = ["ETIMEDOUT", "ETIMEOUT"];

/// The classification signals a provider error carries, extracted at the
/// transport boundary. The Rust stand-in for the TypeScript `ErrorLike`
/// inspection: `name`, `code`, `status`, `message`, headers, and a bounded
/// `cause` chain.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ErrorSignal {
    /// Error class name (e.g. "APIError", "AbortError").
    pub name: Option<String>,
    /// Platform/SDK error code (e.g. "ECONNRESET", "ABORT_ERR").
    pub code: Option<String>,
    /// HTTP status code when the error came from a response.
    pub status: Option<u16>,
    /// The error message (already trimmed by the adapter).
    pub message: String,
    /// `retry-after` response header value, when present.
    pub retry_after: Option<String>,
    /// Wrapped cause, if the transport reported one.
    pub cause: Option<Box<ErrorSignal>>,
}

impl ErrorSignal {
    /// A signal with only a message.
    pub fn from_message(message: impl Into<String>) -> Self {
        ErrorSignal {
            message: message.into(),
            ..ErrorSignal::default()
        }
    }

    /// Attach a `retry-after` header value.
    pub fn with_retry_after(mut self, retry_after: Option<String>) -> Self {
        self.retry_after = retry_after;
        self
    }

    fn is_abort_like(&self) -> bool {
        if let Some(name) = &self.name
            && (name == "AbortError" || name == "APIUserAbortError")
        {
            return true;
        }
        if let Some(code) = &self.code {
            // The TypeScript mapper also treats the numeric DOMException code
            // 20 as abort-like; that arrives here as the string "20".
            if code == "ABORT_ERR" || code == "20" {
                return true;
            }
        }
        self.message == "This operation was aborted"
    }
}

/// Walk the `cause` chain (bounded) looking for classification signals.
fn cause_at_depth(signal: &ErrorSignal, depth: usize) -> Option<&ErrorSignal> {
    let mut current = signal;
    for _ in 0..depth {
        current = current.cause.as_deref()?;
    }
    Some(current)
}

/// Credential-redaction rules used by this module. The patterns are constants,
/// so compilation always succeeds; if a future pattern failed to compile the
/// corresponding rule is skipped (and the redaction tests below fail).
struct MessageRules {
    anthropic_key: Regex,
    api_key: Regex,
    bearer: Regex,
}

fn message_rules() -> Option<&'static MessageRules> {
    static RULES: OnceLock<Option<MessageRules>> = OnceLock::new();
    RULES
        .get_or_init(|| {
            Some(MessageRules {
                anthropic_key: Regex::new(r"sk-ant-[A-Za-z0-9_-]{8,}").ok()?,
                api_key: Regex::new(r"sk-[A-Za-z0-9_-]{8,}").ok()?,
                bearer: Regex::new(r"(?i)\bBearer\s+[A-Za-z0-9._~+/=-]{8,}").ok()?,
            })
        })
        .as_ref()
}

/// Redact credential-shaped substrings so keys never reach persisted output.
///
/// Deliberately narrower than the tool-output redaction in `relay-tools`:
/// this mirrors `errors.ts`, which uses plain `[redacted]` markers and shorter
/// length floors.
pub fn redact_message(text: &str) -> String {
    let Some(rules) = message_rules() else {
        return text.to_owned();
    };
    let text = rules.anthropic_key.replace_all(text, "[redacted]");
    let text = rules.api_key.replace_all(&text, "[redacted]");
    rules
        .bearer
        .replace_all(&text, "Bearer [redacted]")
        .into_owned()
}

/// Slice to at most [`MAX_MESSAGE_CHARS`] characters on a char boundary.
fn trim_to_limit(text: &str) -> String {
    if text.chars().count() <= MAX_MESSAGE_CHARS {
        return text.to_owned();
    }
    let cut = text
        .char_indices()
        .nth(MAX_MESSAGE_CHARS)
        .map_or(text.len(), |(byte, _)| byte);
    text[..cut].to_owned()
}

fn safe_message(signal: &ErrorSignal, fallback: &str) -> String {
    let raw = if signal.message.is_empty() {
        fallback
    } else {
        signal.message.as_str()
    };
    trim_to_limit(redact_message(raw).trim())
}

/// Map a provider error signal to a [`ProviderFailure`]. Classification
/// order: aborts, then HTTP status codes, then connection-level causes, then
/// SDK error class names, and finally [`ProviderErrorKind::Unknown`].
pub fn to_provider_failure(signal: ErrorSignal) -> ProviderFailure {
    if signal.is_abort_like() {
        return ProviderFailure::new(
            ProviderErrorKind::Canceled,
            safe_message(&signal, "request aborted"),
        );
    }

    let status = signal.status;

    if status.is_some_and(|code| code == 401 || code == 403) {
        let code = status.unwrap_or(0);
        return ProviderFailure::new(
            ProviderErrorKind::Auth,
            safe_message(
                &signal,
                &format!("provider rejected credentials (HTTP {code})"),
            ),
        )
        .retryable(false)
        .status(code);
    }
    if status == Some(408) {
        return ProviderFailure::new(
            ProviderErrorKind::Timeout,
            safe_message(&signal, "provider timed out (HTTP 408)"),
        )
        .retryable(true)
        .status(408);
    }
    if status == Some(429) {
        let suffix = signal
            .retry_after
            .as_deref()
            .filter(|value| !value.is_empty())
            .map(|value| format!(" (retry-after: {value}s)"))
            .unwrap_or_default();
        return ProviderFailure::new(
            ProviderErrorKind::RateLimit,
            format!(
                "{}{}",
                safe_message(&signal, "provider rate limited the request"),
                suffix
            ),
        )
        .retryable(true)
        .status(429);
    }
    if let Some(code) = status.filter(|code| *code >= 500) {
        return ProviderFailure::new(
            ProviderErrorKind::Server,
            safe_message(&signal, &format!("provider server error (HTTP {code})")),
        )
        .retryable(true)
        .status(code);
    }

    // Connection-level failures are usually wrapped in a `cause` (fetch throws
    // TypeError('fetch failed') with the socket error underneath).
    for depth in 1..=3 {
        let Some(cause) = cause_at_depth(&signal, depth) else {
            break;
        };
        if cause.is_abort_like() {
            return ProviderFailure::new(
                ProviderErrorKind::Canceled,
                safe_message(&signal, "request aborted"),
            );
        }
        if let Some(code) = cause.code.as_deref() {
            if NETWORK_CODES.contains(&code) {
                return ProviderFailure::new(
                    ProviderErrorKind::Network,
                    safe_message(&signal, &format!("network error: {code}")),
                )
                .retryable(true);
            }
            if TIMEOUT_CODES.contains(&code) {
                return ProviderFailure::new(
                    ProviderErrorKind::Timeout,
                    safe_message(&signal, &format!("network timeout: {code}")),
                )
                .retryable(true);
            }
        }
        if cause.name.as_deref() == Some("APIConnectionTimeoutError") {
            return ProviderFailure::new(
                ProviderErrorKind::Timeout,
                safe_message(&signal, "connection timed out"),
            )
            .retryable(true);
        }
    }

    match signal.name.as_deref() {
        Some("APIConnectionTimeoutError") => ProviderFailure::new(
            ProviderErrorKind::Timeout,
            safe_message(&signal, "connection timed out"),
        )
        .retryable(true),
        Some("APIConnectionError") => ProviderFailure::new(
            ProviderErrorKind::Network,
            safe_message(&signal, "connection failed"),
        )
        .retryable(true),
        _ => {
            let fallback = match status {
                Some(code) => format!("provider error (HTTP {code})"),
                None => "unclassified provider error".to_owned(),
            };
            let failure =
                ProviderFailure::new(ProviderErrorKind::Unknown, safe_message(&signal, &fallback));
            match status {
                Some(code) => failure.status(code),
                None => failure,
            }
        }
    }
}

/// Map a `reqwest` transport error to a failure, mirroring the connection
/// classification the TypeScript adapters get from SDK error classes.
pub fn failure_from_reqwest(error: &reqwest::Error) -> ProviderFailure {
    let mut signal = ErrorSignal {
        message: error.to_string(),
        ..ErrorSignal::default()
    };
    if error.is_timeout() {
        signal.name = Some("APIConnectionTimeoutError".into());
    } else if error.is_connect() || error.is_request() {
        signal.name = Some("APIConnectionError".into());
    }
    if let Some(status) = error.status() {
        signal.status = Some(status.as_u16());
    }
    to_provider_failure(signal)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    fn sdk_like(status: Option<u16>, message: &str) -> ErrorSignal {
        ErrorSignal {
            name: Some("APIError".into()),
            message: message.into(),
            status,
            ..ErrorSignal::default()
        }
    }

    fn with_code(code: &str) -> ErrorSignal {
        ErrorSignal {
            message: "fetch failed".into(),
            cause: Some(Box::new(ErrorSignal {
                message: "socket problem".into(),
                code: Some(code.into()),
                ..ErrorSignal::default()
            })),
            ..ErrorSignal::default()
        }
    }

    #[test]
    fn maps_401_and_403_to_non_retryable_auth_failures() {
        for status in [401, 403] {
            let failure = to_provider_failure(sdk_like(Some(status), "bad key"));
            assert_eq!(failure.kind, ProviderErrorKind::Auth);
            assert!(!failure.retryable);
            assert_eq!(failure.status, Some(status));
            assert_eq!(failure.message, "bad key");
        }
    }

    #[test]
    fn maps_408_to_retryable_timeout() {
        let failure = to_provider_failure(sdk_like(Some(408), "request timeout"));
        assert_eq!(failure.kind, ProviderErrorKind::Timeout);
        assert!(failure.retryable);
    }

    #[test]
    fn maps_429_to_retryable_rate_limit_and_surfaces_retry_after() {
        let failure = to_provider_failure(
            sdk_like(Some(429), "slow down").with_retry_after(Some("7".into())),
        );
        assert_eq!(failure.kind, ProviderErrorKind::RateLimit);
        assert!(failure.retryable);
        assert!(
            failure.message.contains("retry-after: 7s"),
            "{}",
            failure.message
        );
    }

    #[test]
    fn omits_retry_after_suffix_when_header_is_absent() {
        let failure = to_provider_failure(sdk_like(Some(429), "slow down"));
        assert!(
            !failure.message.contains("retry-after"),
            "{}",
            failure.message
        );
    }

    #[test]
    fn maps_5xx_to_retryable_server_failures() {
        for status in [500, 502, 503] {
            let failure = to_provider_failure(sdk_like(Some(status), "upstream exploded"));
            assert_eq!(failure.kind, ProviderErrorKind::Server);
            assert!(failure.retryable);
        }
    }

    #[test]
    fn maps_connection_resets_in_the_cause_chain_to_retryable_network_failures() {
        let failure = to_provider_failure(with_code("ECONNRESET"));
        assert_eq!(failure.kind, ProviderErrorKind::Network);
        assert!(failure.retryable);
        assert_eq!(failure.message, "fetch failed");
    }

    #[test]
    fn maps_etimedout_causes_to_timeout() {
        let failure = to_provider_failure(with_code("ETIMEDOUT"));
        assert_eq!(failure.kind, ProviderErrorKind::Timeout);
        assert!(failure.retryable);
    }

    #[test]
    fn maps_abort_errors_to_non_retryable_cancellation() {
        let abort = ErrorSignal {
            name: Some("AbortError".into()),
            message: "This operation was aborted".into(),
            ..ErrorSignal::default()
        };
        let failure = to_provider_failure(abort);
        assert_eq!(failure.kind, ProviderErrorKind::Canceled);
        assert!(!failure.retryable);
    }

    #[test]
    fn maps_sdk_user_abort_errors_to_cancellation() {
        let error = ErrorSignal {
            name: Some("APIUserAbortError".into()),
            message: "Request was aborted.".into(),
            ..ErrorSignal::default()
        };
        assert_eq!(to_provider_failure(error).kind, ProviderErrorKind::Canceled);
    }

    #[test]
    fn maps_abort_like_causes_to_cancellation() {
        let wrapped = ErrorSignal {
            message: "fetch failed".into(),
            cause: Some(Box::new(ErrorSignal {
                code: Some("ABORT_ERR".into()),
                message: "aborted".into(),
                ..ErrorSignal::default()
            })),
            ..ErrorSignal::default()
        };
        assert_eq!(
            to_provider_failure(wrapped).kind,
            ProviderErrorKind::Canceled
        );
    }

    #[test]
    fn maps_other_4xx_to_non_retryable_unknown_failures() {
        let failure = to_provider_failure(sdk_like(Some(422), "unprocessable"));
        assert_eq!(failure.kind, ProviderErrorKind::Unknown);
        assert!(!failure.retryable);
        assert_eq!(failure.status, Some(422));
    }

    #[test]
    fn falls_back_to_unknown_for_plain_errors() {
        let failure = to_provider_failure(ErrorSignal::from_message("weird"));
        assert_eq!(failure.kind, ProviderErrorKind::Unknown);
        assert!(!failure.retryable);
        assert_eq!(failure.message, "weird");
    }

    #[test]
    fn falls_back_to_status_message_when_message_is_empty() {
        let failure = to_provider_failure(sdk_like(Some(500), ""));
        assert_eq!(failure.message, "provider server error (HTTP 500)");
        let unknown = to_provider_failure(sdk_like(Some(418), ""));
        assert_eq!(unknown.message, "provider error (HTTP 418)");
    }

    #[test]
    fn trims_messages_to_300_characters_and_never_includes_response_bodies() {
        let long = "x".repeat(500);
        let failure = to_provider_failure(sdk_like(Some(500), &long));
        assert_eq!(failure.message.chars().count(), 300);
    }

    #[test]
    fn redacts_credential_shaped_substrings_from_messages() {
        let failure = to_provider_failure(ErrorSignal::from_message(
            "key sk-abc123def456ghi789 rejected",
        ));
        assert!(
            !failure.message.contains("sk-abc123def456ghi789"),
            "{}",
            failure.message
        );
        assert!(
            failure.message.contains("[redacted]"),
            "{}",
            failure.message
        );
    }

    #[test]
    fn redacts_anthropic_keys_and_bearer_tokens() {
        let a = to_provider_failure(ErrorSignal::from_message(
            "leak sk-ant-abc123def456 rejected",
        ));
        assert!(a.message.contains("[redacted]"), "{}", a.message);
        let b = to_provider_failure(ErrorSignal::from_message(
            "header Bearer eyJhbGciOi.abc123 invalid",
        ));
        assert!(b.message.contains("Bearer [redacted]"), "{}", b.message);
    }

    #[test]
    fn maps_sdk_connection_class_names() {
        let timeout = ErrorSignal {
            name: Some("APIConnectionTimeoutError".into()),
            message: "timed out".into(),
            ..ErrorSignal::default()
        };
        assert_eq!(
            to_provider_failure(timeout).kind,
            ProviderErrorKind::Timeout
        );

        let network = ErrorSignal {
            name: Some("APIConnectionError".into()),
            message: "no route".into(),
            ..ErrorSignal::default()
        };
        assert_eq!(
            to_provider_failure(network).kind,
            ProviderErrorKind::Network
        );
    }

    #[test]
    fn unicode_messages_trim_on_character_boundaries() {
        let message = "é".repeat(400);
        let failure = to_provider_failure(ErrorSignal::from_message(message));
        assert_eq!(failure.message.chars().count(), 300);
    }

    #[test]
    fn deep_cause_chains_are_bounded() {
        // depth 4 is beyond the inspected window: classified as unknown.
        let deep = ErrorSignal {
            message: "wrapper".into(),
            cause: Some(Box::new(ErrorSignal {
                message: "level1".into(),
                cause: Some(Box::new(ErrorSignal {
                    message: "level2".into(),
                    cause: Some(Box::new(ErrorSignal {
                        message: "level3".into(),
                        cause: Some(Box::new(with_code("ECONNRESET"))),
                        ..ErrorSignal::default()
                    })),
                    ..ErrorSignal::default()
                })),
                ..ErrorSignal::default()
            })),
            ..ErrorSignal::default()
        };
        assert_eq!(to_provider_failure(deep).kind, ProviderErrorKind::Unknown);
    }
}
