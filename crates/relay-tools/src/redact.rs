//! Secret redaction and output bounding shared by every tool.
//!
//! Ported from `src/tools/redact.ts`. Nothing a tool returns may carry
//! credential material: known token shapes are replaced with
//! `[redacted:<kind>]` markers and the literal values of secret-named
//! environment variables never reach recorded output. The value being
//! redacted is deliberately never echoed in any message.

use regex::Regex;
use std::sync::OnceLock;

/// Environment variable NAME patterns considered secret-bearing, as source
/// patterns (case-insensitive, anchored at the final segment boundary).
pub const SECRET_ENV_PATTERNS: [&str; 3] = [
    r"(?i)(^|_)(API_?KEY|TOKENS?|SECRET|PASSWORD|PASSWD|PASSPHRASE|CREDENTIALS?|AUTH|AUTHORIZATION|BEARER)$",
    r"(?i)(^|_)(PRIVATE_?KEY|ACCESS_?KEY|SECRET_?KEY|SIGNING_?KEY|SESSION_?KEY|CLIENT_?SECRET|REFRESH_?TOKEN)$",
    r"(?i)(^|_)(DATABASE_?URL|DB_?URL|CONNECTION_?STRING|DSN)$",
];

/// A literal token shape that must never appear in recorded output.
struct ValuePattern {
    pattern: &'static str,
    kind: &'static str,
}

const SECRET_VALUE_PATTERNS: [ValuePattern; 7] = [
    ValuePattern {
        pattern: r"sk-ant-[A-Za-z0-9_-]{16,}",
        kind: "anthropic-key",
    },
    ValuePattern {
        pattern: r"sk-[A-Za-z0-9_-]{16,}",
        kind: "api-key",
    },
    ValuePattern {
        pattern: r"AKIA[0-9A-Z]{16}",
        kind: "aws-access-key",
    },
    ValuePattern {
        pattern: r"ghp_[A-Za-z0-9]{20,}",
        kind: "github-token",
    },
    ValuePattern {
        pattern: r"xox[bap]-[A-Za-z0-9-]{10,}",
        kind: "slack-token",
    },
    ValuePattern {
        pattern: r"(?i)\bBearer\s+[A-Za-z0-9._-]{20,}",
        kind: "bearer-token",
    },
    ValuePattern {
        pattern: r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----[\s\S]*?(?:-----END [A-Z0-9 ]*PRIVATE KEY-----|$)",
        kind: "private-key",
    },
];

struct CompiledRules {
    env_names: Vec<Regex>,
    values: Vec<(Regex, &'static str)>,
}

fn compiled() -> Option<&'static CompiledRules> {
    static RULES: OnceLock<Option<CompiledRules>> = OnceLock::new();
    RULES
        .get_or_init(|| {
            Some(CompiledRules {
                env_names: SECRET_ENV_PATTERNS
                    .iter()
                    .map(|pattern| Regex::new(pattern).ok())
                    .collect::<Option<Vec<_>>>()?,
                values: SECRET_VALUE_PATTERNS
                    .iter()
                    .map(|rule| {
                        Regex::new(rule.pattern)
                            .ok()
                            .map(|regex| (regex, rule.kind))
                    })
                    .collect::<Option<Vec<_>>>()?,
            })
        })
        .as_ref()
}

/// True when an environment variable NAME looks secret-shaped.
pub fn env_name_looks_secret(name: &str) -> bool {
    match compiled() {
        Some(rules) => rules.env_names.iter().any(|pattern| pattern.is_match(name)),
        None => true, // rules failed to compile: fail closed
    }
}

/// Redact known secret values from text using the given environment entries
/// (`(name, value)` pairs): literal values of secret-named variables (empty
/// values are skipped) plus common token shapes. Deterministic variant of
/// [`redact_secrets`] for tests and callers that own the environment.
pub fn redact_secrets_with_env(text: &str, env: &[(String, String)]) -> String {
    let Some(rules) = compiled() else {
        return text.to_owned();
    };
    let mut env_values: Vec<(&str, &str)> = env
        .iter()
        .filter(|(name, value)| !value.is_empty() && env_name_looks_secret(name))
        .map(|(_, value)| (value.as_str(), "[redacted:env]"))
        .collect();
    // Longer values first so overlapping secrets never leak a remainder.
    env_values.sort_by_key(|(value, _)| std::cmp::Reverse(value.len()));

    let mut redacted = text.to_owned();
    for (value, marker) in env_values {
        redacted = redacted.replace(value, marker);
    }
    for (pattern, kind) in &rules.values {
        let marker = format!("[redacted:{kind}]");
        redacted = pattern.replace_all(&redacted, marker.as_str()).into_owned();
    }
    redacted
}

/// Redact known secret values from text, reading secret-named environment
/// values from the process environment.
pub fn redact_secrets(text: &str) -> String {
    redact_secrets_with_env(text, &std::env::vars().collect::<Vec<_>>())
}

/// Cap text at `max_chars` (characters), keeping the head and marking what
/// was dropped. Returns a [`BoundOutputError`] for non-positive budgets.
pub fn bound_output(text: &str, max_chars: usize) -> Result<String, BoundOutputError> {
    if max_chars == 0 {
        return Err(BoundOutputError);
    }
    let total = text.chars().count();
    if total <= max_chars {
        return Ok(text.to_owned());
    }
    let mut keep = max_chars.saturating_sub(1);
    let mut marker = truncation_marker(total, keep);
    while keep > 0 && keep + marker.chars().count() > max_chars {
        keep -= 1;
        marker = truncation_marker(total, keep);
    }
    if keep + marker.chars().count() > max_chars {
        return Ok(marker.chars().take(max_chars).collect());
    }
    let head: String = text.chars().take(keep).collect();
    Ok(format!("{head}{marker}"))
}

fn truncation_marker(total: usize, keep: usize) -> String {
    format!("\n…[truncated {} chars]", total - keep)
}

/// A non-positive `bound_output` budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("boundOutput requires a positive maxChars")]
pub struct BoundOutputError;

/// Reduce environment variable entries to bare names for logging. Accepts
/// either plain names or `NAME=value` strings; values are never returned.
pub fn sanitize_env_for_record(env: &[String]) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for entry in env {
        let name = match entry.split_once('=') {
            Some((name, _)) => name,
            None => entry.as_str(),
        }
        .trim();
        if !name.is_empty() && !names.iter().any(|seen| seen == name) {
            names.push(name.to_owned());
        }
    }
    names
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn redacts_the_literal_value_of_secret_named_environment_variables() {
        let output = redact_secrets_with_env(
            "password=hunter2-super-secret-value and more text",
            &env(&[("RELAY_TEST_SECRET", "hunter2-super-secret-value")]),
        );
        assert!(output.contains("[redacted:env]"), "{output}");
        assert!(!output.contains("hunter2-super-secret-value"));
    }

    #[test]
    fn redacts_values_containing_regular_expression_metacharacters() {
        let output = redact_secrets_with_env(
            "token a.b*c(d)e here",
            &env(&[("RELAY_TEST_SECRET", "a.b*c(d)e")]),
        );
        assert_eq!(output, "token [redacted:env] here");
    }

    #[test]
    fn ignores_empty_environment_variable_values() {
        let output = redact_secrets_with_env("unchanged text", &env(&[("RELAY_TEST_SECRET", "")]));
        assert_eq!(output, "unchanged text");
    }

    #[test]
    fn redacts_openai_style_keys() {
        let output = redact_secrets_with_env("key: sk-proj-abcdefghijklmnop1234567890", &[]);
        assert_eq!(output, "key: [redacted:api-key]");
    }

    #[test]
    fn redacts_anthropic_style_keys() {
        let output = redact_secrets_with_env("key: sk-ant-api03-abcdefghijklmnop1234567890", &[]);
        assert_eq!(output, "key: [redacted:anthropic-key]");
    }

    #[test]
    fn redacts_aws_access_keys() {
        let output = redact_secrets_with_env("aws AKIAIOSFODNN7EXAMPLE", &[]);
        assert_eq!(output, "aws [redacted:aws-access-key]");
    }

    #[test]
    fn redacts_github_and_slack_tokens() {
        assert_eq!(
            redact_secrets_with_env("gh ghp_abcdefghijklmnopqrstuvwxyz123456", &[]),
            "gh [redacted:github-token]"
        );
        assert_eq!(
            redact_secrets_with_env("slack xoxb-1234567890abcdef", &[]),
            "slack [redacted:slack-token]"
        );
    }

    #[test]
    fn redacts_bearer_tokens() {
        let output = redact_secrets_with_env(
            "Authorization: Bearer abcdefghijklmnopqrstuvwxyz123456",
            &[],
        );
        assert_eq!(output, "Authorization: [redacted:bearer-token]");
        assert!(!output.contains("abcdefghijklmnopqrstuvwxyz123456"));
    }

    #[test]
    fn redacts_private_key_blocks_including_their_body() {
        let pem = "-----BEGIN RSA PRIVATE KEY-----\nMIIEowIBAAKCAQEAabcdefghijklmnop\n-----END RSA PRIVATE KEY-----";
        let output = redact_secrets_with_env(&format!("cert:\n{pem}\n"), &[]);
        assert_eq!(output, "cert:\n[redacted:private-key]\n");
    }

    #[test]
    fn leaves_clean_text_untouched() {
        let text = "const x = 1; console.log(\"task-manager\"); // node:fs";
        assert_eq!(redact_secrets_with_env(text, &[]), text);
    }

    #[test]
    fn secret_name_detection_covers_the_reference_patterns() {
        for name in [
            "API_KEY",
            "MY_TOKEN",
            "TOKENS",
            "CLIENT_SECRET",
            "PASSWORD",
            "PASSWD",
            "PASSPHRASE",
            "CREDENTIALS",
            "AUTH",
            "AUTHORIZATION",
            "BEARER",
            "PRIVATE_KEY",
            "ACCESS_KEY",
            "SECRET_KEY",
            "SIGNING_KEY",
            "SESSION_KEY",
            "CLIENT_SECRET",
            "REFRESH_TOKEN",
            "DATABASE_URL",
            "DB_URL",
            "CONNECTION_STRING",
            "DSN",
        ] {
            assert!(env_name_looks_secret(name), "{name} should look secret");
        }
        for name in ["PATH", "HOME", "LANG", "CI", "RELAY_UNLISTED_VAR", "AUTHOR"] {
            assert!(
                !env_name_looks_secret(name),
                "{name} should not look secret"
            );
        }
    }

    #[test]
    fn longer_env_values_win_over_shorter_overlapping_ones() {
        let output = redact_secrets_with_env(
            "leak abcdefgh secret",
            &env(&[("SHORT_SECRET", "abc"), ("LONG_SECRET", "abcdefgh")]),
        );
        assert_eq!(output, "leak [redacted:env] secret");
    }

    #[test]
    fn bound_output_returns_short_text_unchanged() {
        assert_eq!(bound_output("hello", 100).expect("bounded"), "hello");
    }

    #[test]
    fn bound_output_truncates_long_text_with_a_marker_reporting_dropped_chars() {
        let text = "a".repeat(500);
        let output = bound_output(&text, 100).expect("bounded");
        assert_eq!(output.chars().count(), 100);
        let marker_start = output.find("\n…[truncated ").expect("marker present");
        let kept = marker_start;
        assert!(text.starts_with(&output[..kept]));
    }

    #[test]
    fn bound_output_never_exceeds_the_budget() {
        for max in [1usize, 5, 17, 64, 1000] {
            let output = bound_output(&"x".repeat(10_000), max).expect("bounded");
            assert!(output.chars().count() <= max, "{max}");
        }
    }

    #[test]
    fn bound_output_refuses_non_positive_budgets() {
        assert!(bound_output("x", 0).is_err());
    }

    #[test]
    fn sanitize_env_for_record_reduces_entries_to_bare_deduplicated_names() {
        let input: Vec<String> = ["FOO", "BAR=1", " BAZ ", "", "FOO=2"]
            .iter()
            .map(|value| (*value).to_owned())
            .collect();
        assert_eq!(
            sanitize_env_for_record(&input),
            vec!["FOO", "BAR", "BAZ"]
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>()
        );
    }
}
