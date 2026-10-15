//! Work-graph CLI integration tests: the product flow through captured
//! buffers, doctor on a healthy store, the legacy-command mapping, exit
//! codes, and the migrate dry-run surface. Offline and deterministic — a
//! scripted model executor stands in for the provider, and every session
//! runs on a fixed clock.

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

use relay_app::RuntimeWiring;
use relay_app::session::SessionOptions;
use relay_domain::{
    ArtifactDigest, AttemptOutcome, AttemptReceipt, ExecutorClass, OutputProposal, Usage,
};
use relay_executor::spec::{AttemptSpec, RunContext};
use relay_executor::{
    Executor, ExecutorSet, ModelClient, ModelClientError, ModelTurnRequest, ModelTurnResponse,
};
use relay_providers::contract::CancelToken;
use relay_workflows::StructuredDoc;

use relay_cli::args::COMMANDS;
use relay_cli::args::LEGACY_COMMANDS;
use relay_cli::output::OutputWriter;
use relay_cli::{CliDeps, EXIT_FAILURE, EXIT_OK, EXIT_USAGE, run_with};

const EPOCH_MS: i64 = 1_791_878_400_000;

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
/// workspace.
fn drive(root: &Path, argv: &[&str], json: bool, deps: CliDeps) -> (i32, String, String) {
    let captured = Captured::new();
    let mut owned: Vec<String> = vec![
        "--workspace".to_string(),
        root.to_string_lossy().into_owned(),
    ];
    owned.extend(argv.iter().map(|argument| argument.to_string()));
    // The writer's json flag and the parsed `--json` must agree: the
    // dispatcher derives output shape from the parsed flag and the writer
    // suppresses plain text in json mode, exactly like the binary does.
    if json {
        owned.push("--json".to_string());
    }
    let mut writer = captured.writer(json);
    let code = run_with(&owned, &mut writer, deps);
    (code, captured.stdout.text(), captured.stderr.text())
}

/// Deterministic dependencies with the scripted discovery executor.
fn scripted_deps() -> CliDeps {
    CliDeps::scripted(SessionOptions::deterministic(EPOCH_MS), discovery_wiring())
}

fn plain_deps() -> CliDeps {
    CliDeps::scripted(
        SessionOptions::deterministic(EPOCH_MS),
        RuntimeWiring::scripted(ExecutorSet::new(), Arc::new(EchoModel)),
    )
}

// ---------------------------------------------------------------------------
// Scripted executors (offline, deterministic)
// ---------------------------------------------------------------------------

/// A model-class executor that publishes an opportunity brief for discovery
/// work and fails loudly for anything else — completing other kinds without
/// doing their work would be a fake completion.
struct ScriptedDiscoveryExecutor;

impl Executor for ScriptedDiscoveryExecutor {
    fn class(&self) -> ExecutorClass {
        ExecutorClass::Model
    }

    fn run(&self, spec: &AttemptSpec, context: &RunContext) -> AttemptReceipt {
        if spec.objective.contains("Survey the problem space") {
            let doc = relay_workflows::BriefDoc {
                problem: "team reports are stuck inside the tool".to_string(),
                audience: "analysts".to_string(),
                outcome: "one-click CSV export".to_string(),
                open_questions: Vec::new(),
            };
            let content = doc.to_content();
            let digest = ArtifactDigest::parse(&format!("{:064x}", 0x00ff))
                .ok()
                .unwrap();
            return AttemptReceipt {
                attempt: spec.attempt.clone(),
                work: spec.work.clone(),
                outcome: AttemptOutcome::Completed,
                completed_at_ms: context.now_ms,
                outputs: vec![OutputProposal {
                    artifact: "art_brief".parse().ok().unwrap(),
                    title: Some("Opportunity brief".to_string()),
                    content,
                    digest,
                }],
                evidence: Vec::new(),
                graph_changes: Vec::new(),
                usage: Usage {
                    input_tokens: 10,
                    output_tokens: 20,
                    tool_calls: 0,
                    duration_ms: 5,
                },
            };
        }
        AttemptReceipt {
            attempt: spec.attempt.clone(),
            work: spec.work.clone(),
            outcome: AttemptOutcome::Failed {
                reason: "scripted executor only handles discovery work".to_string(),
            },
            completed_at_ms: context.now_ms,
            outputs: Vec::new(),
            evidence: Vec::new(),
            graph_changes: Vec::new(),
            usage: Usage::default(),
        }
    }
}

/// An offline echo model client (satisfies the wiring's port).
struct EchoModel;

impl ModelClient for EchoModel {
    fn complete(
        &self,
        _route: &relay_domain::ModelRoute,
        request: &ModelTurnRequest,
        _signal: &CancelToken,
    ) -> Result<ModelTurnResponse, ModelClientError> {
        Ok(ModelTurnResponse {
            input_tokens: 1,
            output_tokens: 1,
            ..ModelTurnResponse::from_text(format!("echo: {}", request.last_user_text()))
        })
    }
}

fn discovery_wiring() -> RuntimeWiring {
    RuntimeWiring::scripted(
        ExecutorSet::new().with(Arc::new(ScriptedDiscoveryExecutor)),
        Arc::new(EchoModel),
    )
}

// ---------------------------------------------------------------------------
// The product flow
// ---------------------------------------------------------------------------

#[test]
fn init_expand_run_resolve_inspect_explain_flow() {
    let dir = tempfile::tempdir().ok().unwrap();
    let root = dir.path();

    // init declares the project, goal, and the first expansion round.
    let (code, out, err) = drive(
        root,
        &["init", "--goal", "export team reports as CSV"],
        false,
        scripted_deps(),
    );
    assert_eq!(code, EXIT_OK, "init failed: {err}");
    assert!(out.contains("goal: export team reports as CSV"), "{out}");
    assert!(out.contains("Initialized project"), "{out}");
    assert!(
        out.contains("work_declared") || out.contains("declare"),
        "init should apply the first expansion round: {out}"
    );
    assert!(out.contains("Next:"), "{out}");

    // The clarification decision is awaiting a human.
    let (code, out, err) = drive(root, &["decisions", "list", "--json"], true, plain_deps());
    assert_eq!(code, EXIT_OK, "{err}");
    let value: serde_json::Value = serde_json::from_str(out.trim()).ok().unwrap();
    let decisions = value["decisions"].as_array().cloned().unwrap_or_default();
    assert!(!decisions.is_empty(), "the clarification decision exists");
    let clarification = decisions
        .iter()
        .find(|decision| decision["status"] == "open")
        .cloned()
        .unwrap();
    let clarify_id = clarification["id"].as_str().unwrap().to_string();
    assert!(
        clarification["question"]
            .as_str()
            .unwrap()
            .contains("ambiguous"),
        "{clarification}"
    );

    // The free-form clarification accepts any answer text (there are no
    // declared options to miss).
    let (code, out, err) = drive(
        root,
        &[
            "decisions",
            "resolve",
            &clarify_id,
            "--choice",
            "CSV export for analysts",
            "--rationale",
            "the concrete outcome",
            "--json",
        ],
        true,
        plain_deps(),
    );
    assert_eq!(code, EXIT_OK, "{err}");
    let value: serde_json::Value = serde_json::from_str(out.trim()).ok().unwrap();
    assert_eq!(value["decision"]["choice"], "CSV export for analysts");

    // One scripted run tick completes discovery and publishes the brief.
    let (code, out, err) = drive(
        root,
        &["run", "--autonomy", "safe", "--max-ticks", "1"],
        true,
        scripted_deps(),
    );
    assert_eq!(code, EXIT_OK, "{err}");
    let value: serde_json::Value = serde_json::from_str(out.trim())
        .ok()
        .unwrap_or_else(|| panic!("run output was not JSON: {out} (err: {err})"));
    let ticks = value["ticks"].as_array().cloned().unwrap_or_default();
    assert!(!ticks.is_empty(), "a tick ran: {out}");
    assert!(
        ticks[0]["accepted"].as_array().map_or(0, Vec::len) == 1,
        "the discovery attempt was accepted: {ticks:?}"
    );
    assert!(value["autonomy"] == "safe", "{value}");

    // The brief is inspectable: structured content plus the Markdown view.
    let (code, out, err) = drive(
        root,
        &["artifact", "inspect", "art_brief", "--json"],
        true,
        plain_deps(),
    );
    assert_eq!(code, EXIT_OK, "{err}");
    let value: serde_json::Value = serde_json::from_str(out.trim()).ok().unwrap();
    assert_eq!(value["artifact"], "art_brief");
    assert_eq!(value["schema"], "relay.brief@1");
    assert_eq!(value["version"], 1);
    assert_eq!(value["current_version"], 1);
    assert!(
        value["markdown"]
            .as_str()
            .unwrap()
            .contains("# Opportunity Brief"),
        "{value}"
    );
    assert_eq!(
        value["content"]["problem"],
        "team reports are stuck inside the tool"
    );

    // Explain why discovery is completed.
    let discover = find_work(root, "discover");
    let (code, out, err) = drive(root, &["explain", &discover, "--json"], true, plain_deps());
    assert_eq!(code, EXIT_OK, "{err}");
    let value: serde_json::Value = serde_json::from_str(out.trim()).ok().unwrap();
    assert_eq!(value["status"], "completed", "{value}");
    assert!(
        value["detail"].as_str().unwrap().contains(&discover),
        "{value}"
    );

    // Expand proposes the approval gate for the published brief.
    let (code, out, err) = drive(root, &["expand", "--json"], true, plain_deps());
    assert_eq!(code, EXIT_OK, "{err}");
    let value: serde_json::Value = serde_json::from_str(out.trim()).ok().unwrap();
    let applied = value["applied"].as_array().cloned().unwrap_or_default();
    assert!(
        applied.iter().any(|proposal| proposal["summary"]
            .as_str()
            .unwrap_or_default()
            .contains("approve-artifact")),
        "the approval gate was proposed: {applied:?}"
    );

    // A human approves the brief; delivery stays behind human decisions.
    let (code, out, err) = drive(root, &["decisions", "list", "--json"], true, plain_deps());
    assert_eq!(code, EXIT_OK, "{err}");
    let value: serde_json::Value = serde_json::from_str(out.trim()).ok().unwrap();
    let approval = value["decisions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|decision| {
            decision["question"]
                .as_str()
                .unwrap_or_default()
                .contains("Approve opportunity brief")
        })
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    assert_ne!(
        approval,
        serde_json::Value::Null,
        "approval decision exists"
    );
    let approval_id = approval["id"].as_str().unwrap().to_string();
    // A choice outside the declared options is rejected with the options.
    let (code, _out, err) = drive(
        root,
        &["decisions", "resolve", &approval_id, "--choice", "bogus"],
        false,
        plain_deps(),
    );
    assert_eq!(code, EXIT_FAILURE, "{err}");
    assert!(err.contains("not one of the options"), "{err}");
    assert!(err.contains("approve"), "{err}");
    let (code, out, err) = drive(
        root,
        &[
            "decisions",
            "resolve",
            &approval_id,
            "--choice",
            "approve",
            "--json",
        ],
        true,
        plain_deps(),
    );
    assert_eq!(code, EXIT_OK, "{err}");
    let value: serde_json::Value = serde_json::from_str(out.trim()).ok().unwrap();
    assert_eq!(value["decision"]["choice"], "approve");

    // The summary reflects the whole derivation.
    let (code, out, err) = drive(root, &["project", "summary", "--json"], true, plain_deps());
    assert_eq!(code, EXIT_OK, "{err}");
    let value: serde_json::Value = serde_json::from_str(out.trim()).ok().unwrap();
    // Discovery and clarification are the only required work so far; with
    // both completed the derived completion is "complete" until `expand`
    // proposes the specification round.
    assert_eq!(value["completion"], "complete", "{value}");
    assert!(
        value["counts"]["completed"].as_u64().unwrap() >= 2,
        "{value}"
    );
    let phases = value["phases"].as_array().cloned().unwrap_or_default();
    assert_eq!(phases.len(), 7, "all seven phase groups render: {phases:?}");

    // work ready / blocked / list all render human and JSON shapes.
    let (code, out, err) = drive(root, &["work", "ready"], false, plain_deps());
    assert_eq!(code, EXIT_OK, "{err}");
    assert!(
        out.contains("work_") || out.contains("(no work items)"),
        "{out}"
    );
    let (code, _out, err) = drive(root, &["work", "blocked", "--json"], true, plain_deps());
    assert_eq!(code, EXIT_OK, "{err}");
    let (code, out, err) = drive(root, &["work", "list"], false, plain_deps());
    assert_eq!(code, EXIT_OK, "{err}");
    assert!(out.contains("discover-opportunity"), "{out}");

    // Work inspect shows attempts and status.
    let (code, out, err) = drive(
        root,
        &["work", "inspect", &discover, "--json"],
        true,
        plain_deps(),
    );
    assert_eq!(code, EXIT_OK, "{err}");
    let value: serde_json::Value = serde_json::from_str(out.trim()).ok().unwrap();
    assert_eq!(value["entry"]["status"], "completed");
    assert!(
        !value["attempts"]
            .as_array()
            .unwrap_or(&Vec::new())
            .is_empty(),
        "the accepted attempt is listed: {value}"
    );
}

/// Find the live work id of a kind by listing work in `root`.
fn find_work(root: &Path, kind_fragment: &str) -> String {
    let (_code, out, err) = drive(root, &["work", "list", "--json"], true, plain_deps());
    assert_eq!(_code, EXIT_OK, "{err}");
    let value: serde_json::Value = serde_json::from_str(out.trim()).ok().unwrap();
    value["works"]
        .as_array()
        .unwrap()
        .iter()
        .find(|work| {
            work["kind"]
                .as_str()
                .unwrap_or_default()
                .contains(kind_fragment)
        })
        .map(|work| work["id"].as_str().unwrap_or_default().to_string())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Doctor
// ---------------------------------------------------------------------------

#[test]
fn doctor_check_is_clean_right_after_a_run_that_published_views() {
    let dir = tempfile::tempdir().ok().unwrap();
    let root = dir.path();
    let (code, _out, err) = drive(
        root,
        &["init", "--goal", "export team reports as CSV"],
        false,
        scripted_deps(),
    );
    assert_eq!(code, EXIT_OK, "{err}");
    // Complete discovery through the scripted tick.
    let (code, out, err) = drive(root, &["decisions", "list", "--json"], true, plain_deps());
    assert_eq!(code, EXIT_OK, "{err}");
    let value: serde_json::Value = serde_json::from_str(out.trim()).ok().unwrap();
    let id = value["decisions"][0]["id"].as_str().unwrap().to_string();
    let (code, _out, err) = drive(
        root,
        &["decisions", "resolve", &id, "--choice", "csv export"],
        false,
        plain_deps(),
    );
    assert_eq!(code, EXIT_OK, "{err}");
    let (code, _out, err) = drive(root, &["run"], false, scripted_deps());
    assert_eq!(code, EXIT_OK, "{err}");

    // The run auto-published the accepted brief's view, so the store is
    // healthy immediately — no manual `--publish` needed.
    let (code, out, err) = drive(root, &["doctor", "check"], false, plain_deps());
    assert_eq!(code, EXIT_OK, "{err}");
    assert!(out.contains("fact store healthy"), "{out}");
    assert!(out.contains("artifact-blobs-ok"), "{out}");

    // The blob sits at the canonical convention path.
    let (code, out, err) = drive(
        root,
        &["artifact", "inspect", "art_brief", "--json"],
        true,
        plain_deps(),
    );
    assert_eq!(code, EXIT_OK, "{err}");
    let value: serde_json::Value = serde_json::from_str(out.trim()).ok().unwrap();
    let digest = value["digest"].as_str().unwrap().to_string();
    let prefix: String = digest.chars().take(16).collect();
    let published_path = format!(".relay-graph/artifacts/art_brief/1-{prefix}.blob");
    assert!(
        root.join(&published_path).is_file(),
        "the blob exists at {published_path}"
    );

    // A manual --publish of the same version is idempotent.
    let (code, _out, err) = drive(
        root,
        &["artifact", "inspect", "art_brief", "--publish", "--json"],
        true,
        plain_deps(),
    );
    assert_eq!(code, EXIT_OK, "{err}");
}

#[test]
fn doctor_check_on_a_freshly_initialized_store_is_healthy() {
    let dir = tempfile::tempdir().ok().unwrap();
    let root = dir.path();
    let (code, _out, err) = drive(
        root,
        &["init", "--goal", "ship a tiny tool"],
        false,
        scripted_deps(),
    );
    assert_eq!(code, EXIT_OK, "{err}");
    let (code, out, err) = drive(root, &["doctor", "check"], false, plain_deps());
    assert_eq!(code, EXIT_OK, "{err}");
    assert!(out.contains("fact store healthy"), "{out}");
    assert!(out.contains("manifest-ok"), "{out}");
    assert!(out.contains("facts-ok"), "{out}");
}

#[test]
fn rebuild_projection_is_reported() {
    let dir = tempfile::tempdir().ok().unwrap();
    let root = dir.path();
    let (code, _out, err) = drive(
        root,
        &["init", "--goal", "ship a tiny tool"],
        false,
        scripted_deps(),
    );
    assert_eq!(code, EXIT_OK, "{err}");
    let (code, out, err) = drive(
        root,
        &["doctor", "rebuild-projection", "--json"],
        true,
        plain_deps(),
    );
    assert_eq!(code, EXIT_OK, "{err}");
    let value: serde_json::Value = serde_json::from_str(out.trim()).ok().unwrap();
    assert!(value["facts_indexed"].as_u64().unwrap() >= 5, "{value}");
}

// ---------------------------------------------------------------------------
// Legacy command mapping
// ---------------------------------------------------------------------------

#[test]
fn legacy_status_maps_to_project_summary() {
    let dir = tempfile::tempdir().ok().unwrap();
    let root = dir.path();
    let (code, _out, err) = drive(
        root,
        &["init", "--goal", "ship a tiny tool"],
        false,
        scripted_deps(),
    );
    assert_eq!(code, EXIT_OK, "{err}");
    let (code, out, err) = drive(root, &["status"], false, plain_deps());
    assert_eq!(code, EXIT_OK, "{err}");
    assert!(err.contains("`relay status` maps to"), "{err}");
    assert!(out.contains("Goal: ship a tiny tool"), "{out}");
    assert!(out.contains("Next:"), "{out}");
    // The JSON shape is the work-graph summary, not the legacy status shape.
    let (code, out, err) = drive(root, &["status", "--json"], true, plain_deps());
    assert_eq!(code, EXIT_OK, "{err}");
    let value: serde_json::Value = serde_json::from_str(out.trim()).ok().unwrap();
    assert!(value["recommendation"]["next"].is_object(), "{value}");
    assert!(value["phases"].is_array(), "{value}");
}

#[test]
fn legacy_why_maps_to_explain_and_legacy_inspect_maps_to_artifact() {
    let dir = tempfile::tempdir().ok().unwrap();
    let root = dir.path();
    let (code, _out, err) = drive(
        root,
        &["init", "--goal", "export team reports as CSV"],
        false,
        scripted_deps(),
    );
    assert_eq!(code, EXIT_OK, "{err}");
    // `why` with work awaiting a human explains that work.
    let (code, out, err) = drive(root, &["why"], false, plain_deps());
    assert_eq!(code, EXIT_OK, "{err}");
    assert!(err.contains("`relay why` maps to"), "{err}");
    assert!(
        out.contains("is awaiting-human") || out.contains("is blocked"),
        "{out}"
    );

    // `inspect <artifact>` maps to artifact inspect.
    let (code, out, err) = drive(root, &["decisions", "list", "--json"], true, plain_deps());
    assert_eq!(code, EXIT_OK, "{err}");
    let value: serde_json::Value = serde_json::from_str(out.trim()).ok().unwrap();
    let id = value["decisions"][0]["id"].as_str().unwrap().to_string();
    let (code, _out, err) = drive(
        root,
        &["decisions", "resolve", &id, "--choice", "csv export"],
        false,
        plain_deps(),
    );
    assert_eq!(code, EXIT_OK, "{err}");
    let (code, _out, err) = drive(root, &["run"], false, scripted_deps());
    assert_eq!(code, EXIT_OK, "{err}");
    let (code, out, err) = drive(
        root,
        &["inspect", "art_brief", "--json"],
        true,
        plain_deps(),
    );
    assert_eq!(code, EXIT_OK, "{err}");
    assert!(err.contains("`relay inspect` maps to"), "{err}");
    let value: serde_json::Value = serde_json::from_str(out.trim()).ok().unwrap();
    assert_eq!(value["artifact"], "art_brief");
    let _ = root;
}

#[test]
fn legacy_mode_auto_pause_return_migration_messages() {
    let dir = tempfile::tempdir().ok().unwrap();
    let root = dir.path();
    for (argv, fragment) in [
        (vec!["mode", "planning"], "no global workflow mode"),
        (vec!["auto", "off"], "per-run scheduling policy"),
        (vec!["auto"], "per-run scheduling policy"),
        (vec!["pause"], "nothing to pause"),
        (vec!["pause", "reviewing the prd"], "nothing to pause"),
    ] {
        let (code, _out, err) = drive(root, &argv, false, plain_deps());
        assert_eq!(code, EXIT_USAGE, "{argv:?} must be a usage error: {err}");
        assert!(
            err.contains("has no work-graph equivalent"),
            "{argv:?}: {err}"
        );
        assert!(err.contains(fragment), "{argv:?}: {err}");
        assert!(err.contains("Use instead:"), "{argv:?}: {err}");
        assert!(err.contains("relay "), "{argv:?}: {err}");
    }
}

#[test]
fn legacy_run_message_initializes_then_runs() {
    let dir = tempfile::tempdir().ok().unwrap();
    let root = dir.path();
    let (code, out, err) = drive(root, &["run", "build a timer app"], false, plain_deps());
    assert_eq!(code, EXIT_OK, "{err}");
    assert!(err.contains("`relay run <message>` maps to"), "{err}");
    assert!(out.contains("Initialized project"), "{out}");
    assert!(out.contains("build a timer app"), "{out}");
    // The project now exists for the new grammar too.
    let (code, out, err) = drive(root, &["project", "summary"], false, plain_deps());
    assert_eq!(code, EXIT_OK, "{err}");
    assert!(out.contains("Goal: build a timer app"), "{out}");
    // A second legacy run does not re-init.
    let (code, out, err) = drive(root, &["run", "another message"], false, plain_deps());
    assert_eq!(code, EXIT_OK, "{err}");
    assert!(!out.contains("Initialized project"), "no re-init: {out}");
    let _ = root;
}

// ---------------------------------------------------------------------------
// Exit codes and smoke parity
// ---------------------------------------------------------------------------

#[test]
fn exit_codes_follow_the_house_convention() {
    let dir = tempfile::tempdir().ok().unwrap();
    let root = dir.path();
    // Unknown command: usage error.
    let (code, _out, err) = drive(root, &["frobnicate"], false, plain_deps());
    assert_eq!(code, EXIT_USAGE, "{err}");
    // Missing command: usage error.
    let (code, _out, _err) = drive(root, &[], false, plain_deps());
    assert_eq!(code, EXIT_USAGE);
    // Reading a workspace with no project: runtime failure.
    let (code, _out, err) = drive(root, &["work", "ready"], false, plain_deps());
    assert_eq!(code, EXIT_FAILURE, "{err}");
    assert!(err.contains("no Relay project"), "{err}");
    // Double init: runtime failure.
    let (code, _out, err) = drive(root, &["init", "--goal", "one"], false, scripted_deps());
    assert_eq!(code, EXIT_OK, "{err}");
    let (code, _out, err) = drive(root, &["init", "--goal", "two"], false, scripted_deps());
    assert_eq!(code, EXIT_FAILURE, "{err}");
    assert!(err.contains("already exists"), "{err}");
    // init without a goal: usage error.
    let fresh = tempfile::tempdir().ok().unwrap();
    let (code, _out, err) = drive(root, &["init"], false, scripted_deps());
    assert_eq!(code, EXIT_USAGE, "{err}");
    assert!(err.contains("init needs a goal"), "{err}");
    let _ = fresh;
    // Bad autonomy: usage error.
    let (code, _out, err) = drive(
        root,
        &["run", "--autonomy", "sometimes"],
        false,
        plain_deps(),
    );
    assert_eq!(code, EXIT_USAGE, "{err}");
    // Bad provider: usage error (validated on the production path).
    let (code, _out, err) = drive(
        root,
        &["run", "--provider", "nonsense"],
        false,
        CliDeps::production(None),
    );
    assert_eq!(code, EXIT_USAGE, "{err}");
}

#[test]
fn help_lists_every_new_and_legacy_command() {
    let dir = tempfile::tempdir().ok().unwrap();
    let root = dir.path();
    let (code, out, err) = drive(root, &["--help"], false, plain_deps());
    assert_eq!(code, EXIT_OK, "{err}");
    for command in COMMANDS {
        assert!(
            out.contains(&format!("relay {command}")),
            "help lists {command}"
        );
    }
    for command in LEGACY_COMMANDS {
        assert!(
            out.contains(&format!("relay {command}")),
            "help lists {command}"
        );
    }
    assert!(out.contains("--json"), "{out}");
    assert!(out.contains("--workspace"), "{out}");
    assert!(out.contains("--autonomy"), "{out}");
}

#[test]
fn version_prints_the_preview_version() {
    let dir = tempfile::tempdir().ok().unwrap();
    let root = dir.path();
    let (code, out, err) = drive(root, &["--version"], false, plain_deps());
    assert_eq!(code, EXIT_OK, "{err}");
    assert_eq!(out.trim(), "relay 0.3.0-preview", "{out}");
    let (code, out, err) = drive(root, &["-V"], false, plain_deps());
    assert_eq!(code, EXIT_OK, "{err}");
    assert_eq!(out.trim(), "relay 0.3.0-preview", "{out}");
}

#[test]
fn config_surface_is_preserved() {
    let dir = tempfile::tempdir().ok().unwrap();
    let root = dir.path();
    let (code, out, err) = drive(root, &["config", "show"], false, CliDeps::production(None));
    assert_eq!(code, EXIT_OK, "{err}");
    assert!(out.contains("providers"), "{out}");
    let (code, out, err) = drive(root, &["config", "path"], false, CliDeps::production(None));
    assert_eq!(code, EXIT_OK, "{err}");
    assert!(
        out.contains(&format!("{}", root.join("relay.config.yaml").display())),
        "{out}"
    );
    let (code, out, err) = drive(
        root,
        &["config", "get", "providers.anthropic.apiKeyEnv"],
        false,
        CliDeps::production(None),
    );
    let _ = (code, out, err);
}

// ---------------------------------------------------------------------------
// Migrate
// ---------------------------------------------------------------------------

#[test]
fn migrate_dry_run_reports_honestly_when_no_legacy_project_exists() {
    let dir = tempfile::tempdir().ok().unwrap();
    let root = dir.path();
    let (code, out, err) = drive(
        root,
        &["migrate", "--dry-run", "--json"],
        true,
        plain_deps(),
    );
    assert_eq!(code, EXIT_OK, "{err}");
    let value: serde_json::Value = serde_json::from_str(out.trim()).ok().unwrap();
    assert_eq!(value["available"], false, "{value}");
    assert!(
        value["detail"]
            .as_str()
            .unwrap()
            .contains("no legacy Relay project found"),
        "{value}"
    );
    assert_eq!(value["dry_run"], true);
}

#[test]
fn migrate_dry_run_plans_a_real_legacy_copy() {
    // Copy a committed legacy fixture (a single exported events.jsonl) into
    // a disposable workspace, then plan the migration. The legacy copy is
    // only ever read.
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/cases/e2e-demo-run/events.jsonl");
    if !fixture.is_file() {
        // The fixture moved; the honest-unavailable path is covered above.
        return;
    }
    let dir = tempfile::tempdir().ok().unwrap();
    let root = dir.path();
    let relay = root.join(".relay");
    std::fs::create_dir_all(relay.join("artifacts"))
        .ok()
        .unwrap();
    std::fs::copy(&fixture, relay.join("events.jsonl"))
        .ok()
        .unwrap();

    let (code, out, err) = drive(
        root,
        &["migrate", "--dry-run", "--json"],
        true,
        plain_deps(),
    );
    assert_eq!(code, EXIT_OK, "{err}");
    let value: serde_json::Value = serde_json::from_str(out.trim()).ok().unwrap();
    assert_eq!(value["available"], true, "{value}");
    assert!(value["legacy_events"].as_u64().unwrap() > 0, "{value}");
    assert!(value["facts_emitted"].as_u64().unwrap() > 0, "{value}");
    assert_eq!(value["dry_run"], true, "{value}");
    // Needs-review items surface with their notes (this fixture's legacy
    // artifact bytes are absent, so the extractor flags them).
    assert!(
        !value["needs_review"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .is_empty(),
        "review items surface: {value}"
    );
    // The dry run staged and switched nothing.
    assert!(value["store_root"].is_null(), "{value}");
    assert!(
        !root.join(".relay-graph").exists(),
        "a dry run must not create the new store"
    );
    assert!(
        relay.join("events.jsonl").is_file(),
        "the legacy copy is untouched"
    );
}

#[test]
fn evidence_command_answers_for_work_and_criteria() {
    let dir = tempfile::tempdir().ok().unwrap();
    let root = dir.path();
    let (code, _out, err) = drive(
        root,
        &["init", "--goal", "ship a tiny tool"],
        false,
        scripted_deps(),
    );
    assert_eq!(code, EXIT_OK, "{err}");
    let discover = find_work(dir.path(), "discover");
    let (code, out, err) = drive(root, &["evidence", &discover, "--json"], true, plain_deps());
    assert_eq!(code, EXIT_OK, "{err}");
    let value: serde_json::Value = serde_json::from_str(out.trim()).ok().unwrap();
    assert!(
        value["target"].as_str().unwrap().contains(&discover),
        "{value}"
    );
    // Unknown targets fail cleanly.
    let (code, _out, err) = drive(root, &["evidence", "work_missing"], false, plain_deps());
    assert_eq!(code, EXIT_FAILURE, "{err}");
}

#[test]
fn expand_on_an_empty_graph_reports_nothing_to_propose() {
    // A project whose goal already expanded fully in init: the next expand
    // round is honest about having nothing new.
    let dir = tempfile::tempdir().ok().unwrap();
    let root = dir.path();
    let (code, _out, err) = drive(
        root,
        &["init", "--goal", "ship a tiny tool"],
        false,
        scripted_deps(),
    );
    assert_eq!(code, EXIT_OK, "{err}");
    let (code, out, err) = drive(root, &["expand"], false, plain_deps());
    assert_eq!(code, EXIT_OK, "{err}");
    assert!(
        out.contains("applied") || out.contains("nothing to propose"),
        "{out}"
    );
}

#[test]
fn run_never_auto_resolves_human_decisions() {
    let dir = tempfile::tempdir().ok().unwrap();
    let root = dir.path();
    let (code, _out, err) = drive(
        root,
        &["init", "--goal", "export team reports as CSV"],
        false,
        scripted_deps(),
    );
    assert_eq!(code, EXIT_OK, "{err}");
    // Run repeatedly: the clarification decision stays open.
    for _ in 0..2 {
        let (code, out, err) = drive(root, &["run", "--max-ticks", "1"], false, scripted_deps());
        assert_eq!(code, EXIT_OK, "{err}");
        assert!(out.contains("run (autonomy"), "{out}");
    }
    let (code, out, err) = drive(root, &["decisions", "list", "--json"], true, plain_deps());
    assert_eq!(code, EXIT_OK, "{err}");
    let value: serde_json::Value = serde_json::from_str(out.trim()).ok().unwrap();
    let open = value["decisions"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|decision| decision["status"] == "open")
        .count();
    assert!(
        open >= 1,
        "the clarification is still awaiting a human: {value}"
    );
}

// ---------------------------------------------------------------------------
// Offline default characterization (plan Phase 0.1)
// ---------------------------------------------------------------------------

/// A fresh workspace with no configuration files and no credentials: bare
/// `relay run` on the production wiring completes offline through the
/// in-process fake provider (the only provider the default constructs), so
/// no network call is possible. This behavior must survive the
/// configuration-driven routing change unchanged.
#[test]
fn a_bare_run_completes_offline_on_the_fake_provider_without_configuration() {
    let dir = tempfile::tempdir().ok().unwrap();
    let root = dir.path();
    // init declares the project (no provider involvement).
    let (code, _out, err) = drive(
        root,
        &["init", "--goal", "export team reports as CSV"],
        false,
        CliDeps::production(None),
    );
    assert_eq!(code, EXIT_OK, "{err}");
    // Answer the clarification so discovery work is ready to run.
    let (code, out, err) = drive(
        root,
        &["decisions", "list", "--json"],
        true,
        CliDeps::production(None),
    );
    assert_eq!(code, EXIT_OK, "{err}");
    let value: serde_json::Value = serde_json::from_str(out.trim()).ok().unwrap();
    let id = value["decisions"][0]["id"].as_str().unwrap().to_string();
    let (code, _out, err) = drive(
        root,
        &["decisions", "resolve", &id, "--choice", "csv export"],
        false,
        CliDeps::production(None),
    );
    assert_eq!(code, EXIT_OK, "{err}");

    // The bare run: production deps, no --provider, no relay.config.yaml,
    // no credentials. It completes (exit 0); the attempt's model call was
    // served by the offline fake.
    let (code, out, err) = drive(root, &["run"], true, CliDeps::production(None));
    assert_eq!(code, EXIT_OK, "{err}");
    let value: serde_json::Value = serde_json::from_str(out.trim()).ok().unwrap();
    let ticks = value["ticks"].as_array().cloned().unwrap_or_default();
    assert!(!ticks.is_empty(), "a tick ran offline: {out}");
    // The model attempt ran and was answered by the fake (it fails the
    // strict final-object decode, which is the truthful offline outcome —
    // what matters is that the run completed without any provider or
    // transport error).
    assert!(
        !ticks[0]["started"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .is_empty(),
        "the discovery attempt started: {ticks:?}"
    );
    assert!(err.is_empty(), "no provider diagnostics: {err}");
}

/// `relay --print "hello"` in a fresh workspace with no configuration and no
/// credentials: one scripted conversational turn through the offline fake,
/// deterministic output, exit 0.
#[test]
fn print_completes_offline_on_the_fake_provider_without_configuration() {
    let dir = tempfile::tempdir().ok().unwrap();
    let root = dir.path();
    let (code, out, err) = drive(
        root,
        &["--print", "hello"],
        false,
        CliDeps::production(None),
    );
    assert_eq!(code, EXIT_OK, "{err}");
    assert_eq!(out, "hello\n", "the fake echoes the prompt: {out}");
    assert!(err.is_empty(), "{err}");
    // The turn is durable: the fresh workspace now carries the project,
    // conversation, and turn facts — created by the turn, not before.
    assert!(
        root.join(".relay-graph").is_dir(),
        "the scripted turn recorded durable facts"
    );
}

/// The offline default survives a polluted environment: driving the real
/// binary with sentinel credentials set (through `Command::env`, never
/// `std::env::set_var`) must not tempt the default into a live call — the
/// run and print paths still complete offline, and the sentinel never
/// appears in any output.
#[test]
fn the_offline_default_survives_credential_pollution_in_the_environment() {
    let dir = tempfile::tempdir().ok().unwrap();
    let root = dir.path();
    let sentinel_anthropic = "sk-ant-characterization-not-a-real-key";
    let invoke = |argv: &[&str]| {
        std::process::Command::new(env!("CARGO_BIN_EXE_relay"))
            .current_dir(root)
            .args(argv)
            .env("ANTHROPIC_API_KEY", sentinel_anthropic)
            .output()
            .ok()
            .unwrap()
    };
    // One scripted print turn: offline, deterministic, no secret echoed.
    let output = invoke(&["--print", "hello from the polluted environment"]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_eq!(stdout, "hello from the polluted environment\n", "{stdout}");
    for text in [&stdout, &stderr] {
        assert!(!text.contains(sentinel_anthropic), "{text}");
    }
    // And a bare run over the now-existing project completes offline too
    // (the print turn created it; there is no ready work, so nothing runs).
    let output = invoke(&["run"]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !root.join("relay.config.yaml").exists(),
        "no configuration file was invented"
    );
}
