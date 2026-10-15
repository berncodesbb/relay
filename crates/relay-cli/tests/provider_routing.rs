//! Phase 5 CLI integration tests: configuration-driven provider routing.
//!
//! Invalid configuration, unknown `--provider` connections, and missing
//! credentials on required connections are usage-class failures (exit 2)
//! that create no project state; the legacy provider spellings remain
//! accepted connection names; and an Anthropic Messages-compatible
//! configured connection works end to end against a loopback server driven
//! through the real binary — no test ever contacts an external service.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::ok_expect,
    clippy::err_expect
)] // test-only conveniences, house style

mod common;

use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;

use common::{CannedResponse, MockServer};
use relay_cli::output::OutputWriter;
use relay_cli::{CliDeps, EXIT_FAILURE, EXIT_OK, EXIT_USAGE, run_with};

/// The fact-store directory a failed preflight must never create.
const STATE_DIR: &str = ".relay-graph";

// ---------------------------------------------------------------------------
// Captured-buffer harness (same shape as the sibling suites)
// ---------------------------------------------------------------------------

/// A shareable in-memory sink so the harness can read what the CLI wrote.
#[derive(Clone, Default)]
struct SharedBuffer(Arc<Mutex<Vec<u8>>>);

impl SharedBuffer {
    fn new() -> Self {
        SharedBuffer::default()
    }

    fn text(&self) -> String {
        let guard = self.0.lock().unwrap_or_else(|poison| poison.into_inner());
        String::from_utf8_lossy(&guard).into_owned()
    }
}

impl std::io::Write for SharedBuffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Run one CLI invocation over captured buffers, pinned to `root`.
fn drive(root: &Path, argv: &[&str]) -> (i32, String, String) {
    let stdout = SharedBuffer::new();
    let stderr = SharedBuffer::new();
    let mut writer =
        OutputWriter::captured(false, Box::new(stdout.clone()), Box::new(stderr.clone()));
    let mut owned: Vec<String> = vec![
        "--workspace".to_string(),
        root.to_string_lossy().into_owned(),
    ];
    owned.extend(argv.iter().map(|argument| argument.to_string()));
    let code = run_with(&owned, &mut writer, CliDeps::production(None));
    (code, stdout.text(), stderr.text())
}

fn write_config(root: &Path, text: &str) {
    std::fs::write(root.join("relay.config.yaml"), text).unwrap();
}

// ---------------------------------------------------------------------------
// Preflight failures: exit 2, no state
// ---------------------------------------------------------------------------

/// An invalid relay.config.yaml fails `run` and `--print` with the usage
/// exit code and creates no `.relay-graph` state.
#[test]
fn an_invalid_configuration_fails_run_and_print_before_any_state() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_config(root, "version: 1\nbudgets: {}\nrouting: [not, a, map]\n");
    for argv in [vec!["run"], vec!["--print", "hello"]] {
        let (code, out, err) = drive(root, &argv);
        assert_eq!(code, EXIT_USAGE, "{argv:?}: {err}");
        assert!(out.is_empty(), "{argv:?}: {out}");
        assert!(err.contains("relay.config.yaml"), "{argv:?}: {err}");
        assert!(
            !root.join(STATE_DIR).exists(),
            "{argv:?}: no state may be created"
        );
    }
}

/// An unknown `--provider` connection is a usage error listing the
/// configured connections, before any state exists.
#[test]
fn an_unknown_provider_connection_is_a_usage_error_listing_connections() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let (code, _out, err) = drive(root, &["run", "--provider", "nonsense"]);
    assert_eq!(code, EXIT_USAGE, "{err}");
    assert!(err.contains("\"nonsense\""), "{err}");
    for name in ["fake", "anthropic"] {
        assert!(err.contains(name), "the configured names are listed: {err}");
    }
    assert!(!root.join(STATE_DIR).exists(), "no state was created");
}

/// A missing credential on a connection the routing requires is a
/// usage-class failure naming the environment variable, and creates no
/// state — for `run` and for `--print` alike.
#[test]
fn a_missing_credential_on_a_required_connection_fails_before_state() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_config(
        root,
        "\
version: 1
budgets: {}
providers:
  claude-gateway:
    adapter: anthropic
    baseUrl: https://example.invalid/claude
    apiKeyEnv: RELAY_CLI_TEST_NEVER_SET_KEY_2026_10_15
models:
  reasoning: { provider: claude-gateway, model: claude-sonnet-5-5 }
  architect: { provider: claude-gateway, model: claude-opus-5-5 }
  coding: { provider: claude-gateway, model: claude-sonnet-5-5 }
  review: { provider: claude-gateway, model: claude-sonnet-5-5 }
  fast: { provider: claude-gateway, model: claude-haiku-4-5 }
routing:
  intent: fast
  discovery: reasoning
  specification: reasoning
  technical-design: architect
  planning: reasoning
  implementation: coding
  verification: review
  delivery: fast
",
    );
    for argv in [
        vec!["run"],
        vec!["--print", "hello"],
        vec!["run", "--provider", "claude-gateway"],
    ] {
        let (code, _out, err) = drive(root, &argv);
        assert_eq!(code, EXIT_USAGE, "{argv:?}: {err}");
        assert!(
            err.contains("RELAY_CLI_TEST_NEVER_SET_KEY_2026_10_15"),
            "{argv:?}: the variable name is the actionable hint: {err}"
        );
        assert!(!err.contains("sk-"), "{argv:?}: {err}");
        assert!(
            !root.join(STATE_DIR).exists(),
            "{argv:?}: no state may be created"
        );
    }
}

// ---------------------------------------------------------------------------
// Provider spellings and the offline default
// ---------------------------------------------------------------------------

/// The legacy `fake`/`anthropic` spellings are accepted connection names:
/// `fake` completes offline; `anthropic` without its credential variable is
/// an actionable configuration failure, not an unknown name.
#[test]
fn the_legacy_provider_spellings_remain_accepted_connections() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let (code, _out, err) = drive(root, &["init", "--goal", "ship a tiny tool"]);
    assert_eq!(code, EXIT_OK, "{err}");

    let (code, out, err) = drive(root, &["run", "--provider", "fake"]);
    assert_eq!(code, EXIT_OK, "{err}");
    assert!(out.contains("run (autonomy"), "{out}");

    let (code, _out, err) = drive(root, &["run", "--provider", "anthropic"]);
    assert_eq!(code, EXIT_USAGE, "{err}");
    assert!(err.contains("ANTHROPIC_API_KEY"), "{err}");
    assert!(
        !err.contains("unknown provider connection"),
        "the spelling is accepted: {err}"
    );
}

/// `relay config show` renders the effective configuration including the
/// default fake connection and user-defined named connections.
#[test]
fn config_show_renders_named_connections() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_config(
        root,
        "\
version: 1
budgets: {}
providers:
  claude-gateway:
    adapter: anthropic
    baseUrl: https://example.invalid/claude
    apiKeyEnv: CLAUDE_GATEWAY_API_KEY
routing:
  intent: fast
  discovery: reasoning
  specification: reasoning
  technical-design: architect
  planning: reasoning
  implementation: coding
  verification: review
  delivery: fast
",
    );
    let (code, out, err) = drive(root, &["config", "show"]);
    assert_eq!(code, EXIT_OK, "{err}");
    assert!(out.contains("\"providers\""), "{out}");
    assert!(out.contains("\"claude-gateway\""), "{out}");
    assert!(out.contains("\"fake\""), "the default fake renders: {out}");
    assert!(
        out.contains("CLAUDE_GATEWAY_API_KEY"),
        "the variable name renders: {out}"
    );
}

// ---------------------------------------------------------------------------
// The Anthropic Messages-compatible configured connection, end to end over
// loopback
// ---------------------------------------------------------------------------

/// One Anthropic Messages SSE reply carrying a single text block.
fn anthropic_sse(reply: &str) -> String {
    let frame = |payload: serde_json::Value| format!("event: stream\ndata: {payload}\n\n");
    let mut body = frame(serde_json::json!({
        "type": "message_start",
        "message": { "usage": { "input_tokens": 1, "output_tokens": 0 } },
    }));
    body += &frame(serde_json::json!({
        "type": "content_block_start",
        "index": 0,
        "content_block": { "type": "text" },
    }));
    body += &frame(serde_json::json!({
        "type": "content_block_delta",
        "index": 0,
        "delta": { "type": "text_delta", "text": reply },
    }));
    body += &frame(serde_json::json!({
        "type": "content_block_stop",
        "index": 0,
    }));
    body += &frame(serde_json::json!({
        "type": "message_delta",
        "delta": { "stop_reason": "end_turn" },
        "usage": { "output_tokens": 1 },
    }));
    body += &frame(serde_json::json!({ "type": "message_stop" }));
    body
}

/// An Anthropic Messages-compatible connection, expressed entirely in YAML,
/// serves a real `relay --print` invocation through the loopback server: the
/// reply comes from the loopback, the credential value never appears
/// anywhere, and the wire call hit `/v1/messages` with the connection's
/// model, the `x-api-key` credential header, and the required
/// `anthropic-version` header.
#[test]
fn an_anthropic_connection_serves_print_end_to_end_over_loopback() {
    // The turn requests schema-constrained structured output; the Anthropic
    // adapter instructs the model to answer with only a JSON object, so the
    // canned reply is one — an empty proposal list, a pure answer turn.
    let server = MockServer::start(CannedResponse {
        status: 200,
        body: anthropic_sse("{\"proposals\": []}"),
    });
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_config(
        root,
        &format!(
            "\
version: 1
budgets: {{}}
providers:
  claude-gateway:
    adapter: anthropic
    baseUrl: {base}
    apiKeyEnv: RELAY_CLI_TEST_ANTHROPIC_LOOPBACK_KEY
routing:
  intent: {{ provider: claude-gateway, model: claude-sonnet-5-5 }}
  discovery: reasoning
  specification: reasoning
  technical-design: architect
  planning: reasoning
  implementation: coding
  verification: review
  delivery: fast
",
            base = server.base_url
        ),
    );
    let key_value = "sk-ant-loopback-not-a-real-credential";
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_relay"))
        .current_dir(root)
        .arg("--print")
        .arg("hello from the cli loopback test")
        .env("RELAY_CLI_TEST_ANTHROPIC_LOOPBACK_KEY", key_value)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        output.status.success(),
        "stderr: {stderr} (stdout: {stdout})"
    );
    assert_eq!(stdout, "{\"proposals\": []}\n", "{stdout}");
    assert!(
        stderr.is_empty(),
        "a clean turn leaves no diagnostics: {stderr}"
    );
    // The credential value never appears in either stream.
    assert!(!stdout.contains(key_value), "{stdout}");
    assert!(!stderr.contains(key_value), "{stderr}");

    // The loopback saw the Messages call with the connection's model, the
    // credential from the configured variable, and the required version
    // header.
    let recorded = server.recorded();
    assert_eq!(recorded.len(), 1, "{recorded:?}");
    assert_eq!(recorded[0].method, "POST");
    assert_eq!(recorded[0].path, "/v1/messages");
    assert_eq!(
        recorded[0].header("x-api-key"),
        Some(key_value),
        "the credential header carries the configured variable's value"
    );
    assert!(
        recorded[0].header("anthropic-version").is_some(),
        "the Messages version header is required: {:?}",
        recorded[0].headers
    );
    assert_eq!(recorded[0].body["model"], "claude-sonnet-5-5");

    // And the turn is durable state created by the turn itself.
    assert!(root.join(STATE_DIR).is_dir());
    let _ = EXIT_FAILURE; // keep the import honest for the suite's convention
}
