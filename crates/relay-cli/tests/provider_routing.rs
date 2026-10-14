//! Phase 5 CLI integration tests: configuration-driven provider routing.
//!
//! Invalid configuration, unknown `--provider` connections, and missing
//! credentials on required connections are usage-class failures (exit 2)
//! that create no project state; the legacy provider spellings remain
//! accepted connection names; and a GLM-style configured connection works
//! end to end against a loopback server driven through the real binary —
//! no test ever contacts an external service.

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
    for name in ["fake", "openai", "anthropic"] {
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
  glm:
    adapter: openai-chat
    baseUrl: https://example.invalid/glm
    apiKeyEnv: RELAY_CLI_TEST_NEVER_SET_KEY_2026_10_15
models:
  reasoning: { provider: glm, model: glm-4.6 }
  architect: { provider: glm, model: glm-4.6 }
  coding: { provider: glm, model: glm-4.6 }
  review: { provider: glm, model: glm-4.6 }
  fast: { provider: glm, model: glm-4.6 }
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
        vec!["run", "--provider", "glm"],
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

/// The legacy `fake`/`openai`/`anthropic` spellings are accepted connection
/// names: `fake` completes offline; `openai` without its credential variable
/// is an actionable configuration failure, not an unknown name.
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
/// inferred fake connection and user-defined named connections.
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
  glm:
    adapter: openai-chat
    baseUrl: https://example.invalid/glm
    apiKeyEnv: GLM_API_KEY
    preset: glm
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
    assert!(out.contains("\"glm\""), "{out}");
    assert!(out.contains("\"fake\""), "the inferred fake renders: {out}");
    assert!(
        out.contains("GLM_API_KEY"),
        "the variable name renders: {out}"
    );
}

// ---------------------------------------------------------------------------
// The GLM-style configured connection, end to end over loopback
// ---------------------------------------------------------------------------

/// One OpenAI-compatible SSE reply carrying a single text chunk plus a
/// finish chunk.
fn glm_sse(reply: &str) -> String {
    let frame = |payload: serde_json::Value| format!("data: {payload}\n\n");
    let text = serde_json::json!({
        "id": "cmpl-loopback",
        "choices": [{ "index": 0, "delta": { "role": "assistant", "content": reply }, "finish_reason": null }],
    });
    let finish = serde_json::json!({
        "id": "cmpl-loopback",
        "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
    });
    format!("{}{}data: [DONE]\n\n", frame(text), frame(finish))
}

/// A GLM-style configured connection, expressed entirely in YAML, serves a
/// real `relay --print` invocation through the loopback server: the reply
/// comes from the loopback, the credential value never appears anywhere,
/// and the wire call hit `/chat/completions`. The GLM preset speaks the
/// `json_object` structured-output mode, so the reply is a JSON object —
/// an empty proposal list, a pure answer turn.
#[test]
fn a_glm_style_connection_serves_print_end_to_end_over_loopback() {
    let server = MockServer::start(CannedResponse {
        status: 200,
        body: glm_sse("{\"proposals\": []}"),
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
  glm:
    adapter: openai-chat
    baseUrl: {base}
    apiKeyEnv: RELAY_CLI_TEST_GLM_LOOPBACK_KEY
    preset: glm
routing:
  intent: {{ provider: glm, model: glm-4.6 }}
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
    let key_value = "sk-cli-loopback-not-a-real-credential";
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_relay"))
        .current_dir(root)
        .arg("--print")
        .arg("hello from the cli loopback test")
        .env("RELAY_CLI_TEST_GLM_LOOPBACK_KEY", key_value)
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

    // The loopback saw the Chat Completions call with the connection's
    // model and the bearer credential from the configured variable.
    let recorded = server.recorded();
    assert_eq!(recorded.len(), 1, "{recorded:?}");
    assert_eq!(recorded[0].method, "POST");
    assert_eq!(recorded[0].path, "/chat/completions");
    assert_eq!(
        recorded[0].header("authorization"),
        Some(&format!("Bearer {key_value}")[..])
    );
    assert_eq!(recorded[0].body["model"], "glm-4.6");
    assert_eq!(recorded[0].body["stream"], serde_json::json!(true));

    // And the turn is durable state created by the turn itself.
    assert!(root.join(STATE_DIR).is_dir());
    let _ = EXIT_FAILURE; // keep the import honest for the suite's convention
}
