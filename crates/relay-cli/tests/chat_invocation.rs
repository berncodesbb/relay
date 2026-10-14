//! Chat-first invocation contract tests: the Phase 0 invocation table at
//! the dispatch seam, over captured buffers (non-interactive by
//! definition) and explicit terminal capabilities. The interactive chat
//! driver itself is exercised separately; nothing here needs a TTY.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::ok_expect,
    clippy::err_expect
)] // test-only conveniences, house style

use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;

use relay_cli::output::OutputWriter;
use relay_cli::{
    CliDeps, EXIT_FAILURE, EXIT_OK, EXIT_USAGE, TerminalCapability, run_with, run_with_capability,
};

// ---------------------------------------------------------------------------
// Captured-buffer harness
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

struct Captured {
    stdout: SharedBuffer,
    stderr: SharedBuffer,
}

impl Captured {
    fn new() -> Self {
        Captured {
            stdout: SharedBuffer::new(),
            stderr: SharedBuffer::new(),
        }
    }

    fn writer(&self, json: bool) -> OutputWriter {
        OutputWriter::captured(
            json,
            Box::new(self.stdout.clone()),
            Box::new(self.stderr.clone()),
        )
    }
}

/// Run one CLI invocation over captured buffers, pinned to `root` as the
/// workspace, as a non-interactive caller.
fn drive(root: &Path, argv: &[&str], json: bool) -> (i32, String, String) {
    drive_with(root, argv, json, TerminalCapability::non_interactive())
}

/// Run one CLI invocation over captured buffers with an explicit terminal
/// capability.
fn drive_with(
    root: &Path,
    argv: &[&str],
    json: bool,
    capability: TerminalCapability,
) -> (i32, String, String) {
    let captured = Captured::new();
    let mut owned: Vec<String> = vec![
        "--workspace".to_string(),
        root.to_string_lossy().into_owned(),
    ];
    owned.extend(argv.iter().map(|argument| argument.to_string()));
    if json {
        owned.push("--json".to_string());
    }
    let mut writer = captured.writer(json);
    let code = run_with_capability(&owned, &mut writer, CliDeps::production(None), capability);
    (code, captured.stdout.text(), captured.stderr.text())
}

// ---------------------------------------------------------------------------
// The invocation table, non-interactive column
// ---------------------------------------------------------------------------

#[test]
fn bare_chat_without_a_tty_is_a_usage_error_with_print_guidance() {
    let dir = tempfile::tempdir().ok().unwrap();
    let root = dir.path();
    for argv in [vec![], vec!["--json"]] {
        let (code, out, err) = drive(root, &argv, false);
        assert_eq!(code, EXIT_USAGE, "{argv:?}: {err}");
        assert!(
            out.is_empty(),
            "{argv:?}: stdout must stay empty (never raw mode): {out}"
        );
        assert!(err.contains("--print"), "{argv:?}: {err}");
        assert!(err.contains("relay --help"), "{argv:?}: {err}");
    }
}

#[test]
fn prompt_form_without_a_tty_is_rejected_with_print_guidance() {
    let dir = tempfile::tempdir().ok().unwrap();
    let root = dir.path();
    let (code, out, err) = drive(root, &["explain this repository"], false);
    assert_eq!(code, EXIT_USAGE, "{err}");
    assert!(
        out.is_empty(),
        "stdout must stay empty (never raw mode): {out}"
    );
    assert!(err.contains("--print"), "{err}");
}

#[test]
fn print_runs_one_scripted_turn_with_a_stable_contract() {
    let dir = tempfile::tempdir().ok().unwrap();
    let root = dir.path();
    // The default fake provider echoes the prompt: a fully deterministic
    // scripted turn. stdout carries the assistant reply and nothing else;
    // a completed turn with no proposals leaves stderr empty.
    let (code, out, err) = drive(root, &["--print", "hello there"], false);
    assert_eq!(code, EXIT_OK, "{err}");
    assert_eq!(out, "hello there\n", "{out}");
    assert!(err.is_empty(), "{err}");
    // The turn is durable: the fresh workspace now carries the project,
    // the conversation, and the turn's facts.
    assert!(
        root.join(".relay-graph").is_dir(),
        "the scripted turn records durable facts"
    );
    // A second scripted turn resumes the same conversation.
    let (code, out, err) = drive(root, &["--print", "second question"], false);
    assert_eq!(code, EXIT_OK, "{err}");
    assert_eq!(out, "second question\n", "{out}");
    // Malformed print invocations are usage errors before anything runs.
    for argv in [vec!["--print"], vec!["--print", "hi", "--json"]] {
        let (code, _out, err) = drive(root, &argv, false);
        assert_eq!(code, EXIT_USAGE, "{argv:?}: {err}");
    }
}

#[test]
fn misspelled_commands_never_become_prompts() {
    let dir = tempfile::tempdir().ok().unwrap();
    let root = dir.path();
    let (code, out, err) = drive(root, &["statusx"], false);
    assert_eq!(code, EXIT_USAGE, "{err}");
    assert!(out.is_empty(), "{out}");
    assert!(err.contains("did you mean `status`?"), "{err}");
    let (code, _out, err) = drive(root, &["decision"], false);
    assert_eq!(code, EXIT_USAGE, "{err}");
    assert!(err.contains("did you mean `decisions`?"), "{err}");
}

// ---------------------------------------------------------------------------
// Precedence and regressions
// ---------------------------------------------------------------------------

#[test]
fn help_and_version_win_over_every_chat_form() {
    let dir = tempfile::tempdir().ok().unwrap();
    let root = dir.path();
    let (code, out, err) = drive(root, &["--help"], false);
    assert_eq!(code, EXIT_OK, "{err}");
    assert!(out.contains("Usage:"), "{out}");
    assert!(out.contains("relay --print"), "{out}");
    let (code, out, err) = drive(root, &["frobnicate", "-V"], false);
    assert_eq!(code, EXIT_OK, "{err}");
    assert_eq!(out.trim(), "relay 0.3.0-preview", "{out}");
    let (code, out, err) = drive(root, &["--print", "hi", "--help"], false);
    assert_eq!(code, EXIT_OK, "{err}");
    assert!(out.contains("Usage:"), "{out}");
}

#[test]
fn existing_commands_keep_their_non_interactive_behavior() {
    let dir = tempfile::tempdir().ok().unwrap();
    let root = dir.path();
    // Unknown command with extra arguments keeps today's error.
    let (code, _out, err) = drive(root, &["frobnicate", "extra"], false);
    assert_eq!(code, EXIT_USAGE, "{err}");
    // Reading an uninitialized workspace keeps the runtime failure.
    let (code, _out, err) = drive(root, &["project", "summary"], false);
    assert_eq!(code, EXIT_FAILURE, "{err}");
    assert!(err.contains("no Relay project"), "{err}");
    // `relay tui` is the compatibility alias for chat: without a full TTY
    // it fails with the actionable terminal error before any setup.
    let (code, out, err) = drive(root, &["tui"], false);
    assert_eq!(code, EXIT_USAGE, "{err}");
    assert!(out.is_empty(), "{out}");
    assert!(err.contains("interactive terminal"), "{err}");
}

// ---------------------------------------------------------------------------
// The capability seam
// ---------------------------------------------------------------------------

#[test]
fn a_half_capability_is_still_non_interactive() {
    let dir = tempfile::tempdir().ok().unwrap();
    let root = dir.path();
    // One TTY stream is not enough: both bare chat and the prompt form
    // get the usage guidance (the fully-interactive branch is the chat
    // driver, exercised without a terminal in the lib tests — driving it
    // here would touch the harness's real terminal).
    for argv in [vec![], vec!["a prompt"]] {
        let half = TerminalCapability {
            stdin_tty: true,
            stdout_tty: false,
        };
        let (code, out, err) = drive_with(root, &argv, false, half);
        assert_eq!(code, EXIT_USAGE, "{argv:?}: {err}");
        assert!(out.is_empty(), "{argv:?}: {out}");
        assert!(err.contains("--print"), "{argv:?}: {err}");
    }
}

#[test]
fn run_with_defaults_to_non_interactive_semantics() {
    let dir = tempfile::tempdir().ok().unwrap();
    let root = dir.path();
    let captured = Captured::new();
    let mut writer = captured.writer(false);
    let argv: Vec<String> = vec![
        "--workspace".to_string(),
        root.to_string_lossy().into_owned(),
    ];
    let code = run_with(&argv, &mut writer, CliDeps::production(None));
    assert_eq!(code, EXIT_USAGE);
    assert!(captured.stderr.text().contains("--print"));
}
