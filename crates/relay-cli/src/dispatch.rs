//! Command dispatch: argv in, exit code out.
//!
//! The dispatcher owns **interface policy only** — how a command's result
//! is rendered (JSON vs human), how errors map onto exit codes (0 success,
//! 1 runtime failure, 2 usage/configuration error), and the legacy-command
//! mapping. Every domain decision comes from `relay-app` handlers over the
//! engine/store/executor ports.
//!
//! All output flows through the [`OutputWriter`] seam; tests capture
//! buffers and drive [`run_with`] in-process.

use std::path::Path;
use std::path::PathBuf;

use relay_app::commands as app;
use relay_app::conversation::{self, ConversationEvent, ConversationLimits, TurnStatus};
use relay_app::runtime_routing::{RuntimeRouting, TurnRouting};
use relay_app::session::{ProjectSession, SessionOptions};
use relay_app::{AppError, RuntimeWiring};
use relay_engine::{Autonomy, SchedulingPolicy};
use relay_providers::contract::CancelToken;

use crate::args::Command;
use crate::args::DoctorAction;
use crate::args::ParseResult;
use crate::args::USAGE;
use crate::args::VERSION;
use crate::args::parse_arguments;
use crate::invocation::TerminalCapability;
use crate::output::OutputWriter;
use crate::render;

/// How a run wires its runtime: production (configuration-driven) or
/// injected (deterministic tests).
#[derive(Clone)]
pub enum RuntimeSelection {
    /// Resolve production wiring from the effective configuration. The
    /// optional provider-connection override is the `--provider` flag: a
    /// configured connection name that every route is forced onto (the
    /// legacy `fake` and `anthropic` spellings are the default
    /// connection names). `None` routes through configuration as-is.
    Production {
        /// The `--provider <connection>` override, when given.
        provider: Option<String>,
    },
    /// Use caller-supplied wiring (tests).
    Injected(RuntimeWiring),
}

/// Injectable dependencies: what varies between the binary and tests.
#[derive(Clone)]
pub struct CliDeps {
    /// Session options (clocks, durability).
    pub session_options: SessionOptions,
    /// Runtime selection for `run`.
    pub runtime: RuntimeSelection,
}

impl CliDeps {
    /// Production dependencies: real clocks, optional provider override.
    pub fn production(provider: Option<&str>) -> Self {
        CliDeps {
            session_options: SessionOptions::production(),
            runtime: RuntimeSelection::Production {
                provider: provider.map(str::to_string),
            },
        }
    }

    /// Deterministic dependencies for tests: fixed clock, injected wiring.
    pub fn scripted(session_options: SessionOptions, wiring: RuntimeWiring) -> Self {
        CliDeps {
            session_options,
            runtime: RuntimeSelection::Injected(wiring),
        }
    }
}

/// Exit code: success.
pub const EXIT_OK: i32 = 0;
/// Exit code: runtime failure.
pub const EXIT_FAILURE: i32 = 1;
/// Exit code: usage or configuration error.
pub const EXIT_USAGE: i32 = 2;

/// The testable entry point with injectable dependencies, treated as a
/// non-interactive invocation (tests capture output).
pub fn run_with(argv: &[String], writer: &mut OutputWriter, deps: CliDeps) -> i32 {
    run_with_capability(argv, writer, deps, TerminalCapability::non_interactive())
}

/// The testable entry point with an explicit terminal capability: chat
/// opens the interactive driver only when both stdin and stdout are TTYs.
pub fn run_with_capability(
    argv: &[String],
    writer: &mut OutputWriter,
    deps: CliDeps,
    capability: TerminalCapability,
) -> i32 {
    let parsed = match parse_arguments(argv) {
        ParseResult::Ok(parsed) => parsed,
        ParseResult::Err { error, usage } => {
            writer.error(&format!("relay: {error}"));
            if usage {
                writer.error("Run `relay --help` for usage.");
            }
            return EXIT_USAGE;
        }
    };
    let json = parsed.json;
    let workspace: PathBuf = parsed.workspace.clone();
    match parsed.command {
        Command::Help => {
            writer.always(USAGE.trim_end());
            EXIT_OK
        }
        Command::Version => {
            writer.always(&format!("relay {VERSION}"));
            EXIT_OK
        }
        Command::Chat { prompt } => {
            chat_command(writer, &workspace, &deps, capability, prompt.as_deref())
        }
        Command::Print { prompt } => print_command(writer, &workspace, &deps, &prompt),
        Command::Init { goal } => {
            let Some(goal) = goal else {
                writer.error("relay: init needs a goal: `relay init --goal <text>`");
                writer.error("Run `relay --help` for usage.");
                return EXIT_USAGE;
            };
            match app::init(&workspace, &goal, deps.session_options.clone()) {
                Ok(view) => emit(writer, json, &view, render::render_init),
                Err(error) => app_error(writer, error),
            }
        }
        Command::ProjectSummary => summary_command(writer, json, &workspace, &deps),
        Command::WorkReady => session_view(
            writer,
            json,
            &workspace,
            &deps,
            app::ready,
            render::render_work_list,
        ),
        Command::WorkBlocked => session_view(
            writer,
            json,
            &workspace,
            &deps,
            app::blocked,
            render::render_work_list,
        ),
        Command::WorkList => session_view(
            writer,
            json,
            &workspace,
            &deps,
            app::work_list,
            render::render_work_list,
        ),
        Command::WorkInspect { id } => session_view(
            writer,
            json,
            &workspace,
            &deps,
            move |session| app::work_detail(session, &id),
            render::render_work_detail,
        ),
        Command::DecisionsList => session_view(
            writer,
            json,
            &workspace,
            &deps,
            app::decisions,
            render::render_decisions,
        ),
        Command::DecisionsResolve {
            id,
            choice,
            rationale,
        } => session_view(
            writer,
            json,
            &workspace,
            &deps,
            move |session| app::resolve_decision(session, &id, &choice, rationale.as_deref()),
            render::render_resolve,
        ),
        Command::ArtifactInspect { target, publish } => {
            // Publish first (the note goes to stderr so the JSON view on
            // stdout stays parseable), then inspect.
            if publish {
                let session = match ProjectSession::open(&workspace, deps.session_options.clone()) {
                    Ok(session) => session,
                    Err(error) => return app_error(writer, error),
                };
                match app::publish_artifact_view(&session, &target, None) {
                    Ok(published) => writer.error(&render::render_publish(&published)),
                    Err(error) => return app_error(writer, error),
                }
            }
            session_view(
                writer,
                json,
                &workspace,
                &deps,
                move |session| {
                    let (artifact, version) = split_reference(&target);
                    app::artifact_detail(session, &artifact, version, true)
                },
                render::render_artifact,
            )
        }
        Command::Evidence { target } => session_view(
            writer,
            json,
            &workspace,
            &deps,
            move |session| app::evidence(session, &target),
            render::render_evidence,
        ),
        Command::Run {
            autonomy,
            provider,
            max_ticks,
        } => {
            let policy = match parse_autonomy(autonomy.as_deref()) {
                Ok(policy) => policy,
                Err(error) => {
                    writer.error(&format!("relay: {error}"));
                    writer.error("Run `relay --help` for usage.");
                    return EXIT_USAGE;
                }
            };
            let selection = match provider.as_deref() {
                None => deps.runtime.clone(),
                Some(_) if matches!(deps.runtime, RuntimeSelection::Injected(_)) => {
                    deps.runtime.clone()
                }
                Some(name) => RuntimeSelection::Production {
                    provider: Some(name.to_string()),
                },
            };
            run_command(
                writer, json, &workspace, &deps, selection, policy, max_ticks,
            )
        }
        Command::Cancel { attempt, reason } => session_view(
            writer,
            json,
            &workspace,
            &deps,
            move |session| app::cancel_attempt(session, &attempt, reason.as_deref()),
            render::render_cancel,
        ),
        Command::Explain { work } => session_view(
            writer,
            json,
            &workspace,
            &deps,
            move |session| app::explain(session, &work),
            render::render_explain,
        ),
        Command::Expand => session_view(
            writer,
            json,
            &workspace,
            &deps,
            app::expand,
            render::render_expand,
        ),
        Command::ImportPrd { path } => {
            let markdown = match std::fs::read_to_string(&path) {
                Ok(markdown) => markdown,
                Err(error) => {
                    writer.error(&format!("relay: cannot read {path}: {error}"));
                    return EXIT_FAILURE;
                }
            };
            session_view(
                writer,
                json,
                &workspace,
                &deps,
                move |session| app::import_prd_revision(session, &markdown),
                render::render_import,
            )
        }
        Command::Doctor { action } => doctor_command(writer, json, &workspace, &deps, action),
        Command::Migrate { dry_run } => {
            match app::migrate(&workspace, dry_run, &deps.session_options.store) {
                Ok(view) => emit(writer, json, &view, render::render_migrate),
                Err(error) => app_error(writer, error),
            }
        }
        Command::Config { action, key } => {
            config_command(writer, json, &workspace, &action, key.as_deref())
        }
        // `relay tui` is the compatibility alias for the chat interface.
        Command::Tui => chat_command(writer, &workspace, &deps, capability, None),
        // -- legacy mappings ------------------------------------------------
        Command::LegacyStatus => {
            legacy_note(writer, "status", "project summary");
            summary_command(writer, json, &workspace, &deps)
        }
        Command::LegacyWhy => {
            legacy_note(writer, "why", "explain");
            why_command(writer, json, &workspace, &deps)
        }
        Command::LegacyInspect { target } => {
            legacy_note(writer, "inspect", "artifact inspect");
            session_view(
                writer,
                json,
                &workspace,
                &deps,
                move |session| {
                    let (artifact, version) = split_reference(&target);
                    app::artifact_detail(session, &artifact, version, true)
                },
                render::render_artifact,
            )
        }
        Command::LegacyMode { mode } => migration_message(
            writer,
            &format!("`relay mode {mode}`"),
            "the work graph has no global workflow mode to set: work is ready, blocked, or \
             awaiting a decision, derived from facts — never assigned",
            &[
                "relay project summary",
                "relay work ready",
                "relay work blocked",
                "relay explain <work-id>",
            ],
        ),
        Command::LegacyAuto { bound } => {
            let requested = bound
                .as_deref()
                .map(|bound| format!("`relay auto {bound}`"))
                .unwrap_or_else(|| "`relay auto`".to_string());
            migration_message(
                writer,
                &requested,
                "autonomy is a per-run scheduling policy now, not a stored bound",
                &[
                    "relay run --autonomy manual",
                    "relay run --autonomy safe",
                    "relay run --autonomy until-decision",
                    "relay run --autonomy bounded:<n>",
                ],
            )
        }
        Command::LegacyPause { .. } => migration_message(
            writer,
            "`relay pause`",
            "there is nothing to pause: the graph advances only when you run it, and \
             consequential work is gated by human decisions",
            &["relay run", "relay decisions list", "relay project summary"],
        ),
        Command::LegacyRun { message } => {
            legacy_run(writer, json, &workspace, &deps, message.as_deref())
        }
    }
}

/// Print a view as JSON or human text; `0` on success.
fn emit<T: serde::Serialize>(
    writer: &mut OutputWriter,
    json: bool,
    view: &T,
    human: impl FnOnce(&T) -> String,
) -> i32 {
    if json {
        match serde_json::to_string_pretty(view) {
            Ok(serialized) => writer.json(&serialized),
            Err(error) => {
                writer.error(&format!("relay: could not serialize the result: {error}"));
                return EXIT_FAILURE;
            }
        }
    } else {
        for line in human(view).lines() {
            writer.text(line);
        }
    }
    EXIT_OK
}

/// Map an [`AppError`] onto message + exit code. Configuration, credential,
/// and route-resolution failures are usage-class (2); runtime failures stay
/// 1.
fn app_error(writer: &mut OutputWriter, error: AppError) -> i32 {
    writer.error(&format!("relay: {}", error.message()));
    if matches!(error, AppError::NotInitialized) {
        writer.error("Initialize the project first: `relay init --goal <text>`.");
    }
    if matches!(error, AppError::Configuration(_)) {
        return EXIT_USAGE;
    }
    EXIT_FAILURE
}

/// Open a session, run one view-returning handler, render it.
fn session_view<T: serde::Serialize>(
    writer: &mut OutputWriter,
    json: bool,
    workspace: &Path,
    deps: &CliDeps,
    handler: impl FnOnce(&ProjectSession) -> Result<T, AppError>,
    human: fn(&T) -> String,
) -> i32 {
    let session = match ProjectSession::open(workspace, deps.session_options.clone()) {
        Ok(session) => session,
        Err(error) => return app_error(writer, error),
    };
    match handler(&session) {
        Ok(view) => emit(writer, json, &view, human),
        Err(error) => app_error(writer, error),
    }
}

fn summary_command(writer: &mut OutputWriter, json: bool, workspace: &Path, deps: &CliDeps) -> i32 {
    session_view(
        writer,
        json,
        workspace,
        deps,
        app::summary,
        render::render_summary,
    )
}

/// Conversational chat: the interactive driver on a fully interactive
/// terminal, an actionable usage error everywhere else — printed before
/// any terminal setup, so non-TTY callers never see raw mode.
/// What dispatch does with a conversational invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChatPlan {
    /// Open the interactive chat driver.
    OpenInterface,
    /// Reject with the scripted-turn guidance (never raw mode).
    RejectScripted,
}

/// Decide the chat plan from the terminal capability alone (pure, tested).
pub(crate) fn chat_plan(capability: TerminalCapability) -> ChatPlan {
    if capability.interactive() {
        ChatPlan::OpenInterface
    } else {
        ChatPlan::RejectScripted
    }
}

/// Conversational chat: the interactive driver on a fully interactive
/// terminal, an actionable usage error everywhere else — printed before
/// any terminal setup, so non-TTY callers never see raw mode.
fn chat_command(
    writer: &mut OutputWriter,
    workspace: &Path,
    deps: &CliDeps,
    capability: TerminalCapability,
    initial_prompt: Option<&str>,
) -> i32 {
    if let ChatPlan::RejectScripted = chat_plan(capability) {
        writer.error("relay: conversational chat needs an interactive terminal (stdin and stdout)");
        writer.error("For one scripted conversational turn: relay --print \"<prompt>\"");
        writer.error("Run `relay --help` for usage.");
        return EXIT_USAGE;
    }
    // Routing resolves before any terminal setup and before any state: an
    // invalid configuration or unavailable intent connection is a plain
    // usage-class message, never a raw-mode surprise or a half-created
    // workspace.
    let routing = match conversational_routing(workspace, &deps.runtime) {
        Ok(routing) => routing,
        Err(error) => return app_error(writer, error),
    };
    crate::tui::run_chat(workspace, deps, routing, initial_prompt)
}

/// Resolve the conversational turn routing: the same selection as `run` —
/// the production wiring's provider-connection override when one was
/// given; injected test wirings carry run-command executors, so their
/// scripted turns use the configuration's default routing (the offline
/// fake profile in a fresh workspace).
pub(crate) fn conversational_routing(
    workspace: &Path,
    runtime: &RuntimeSelection,
) -> Result<TurnRouting, AppError> {
    match runtime {
        RuntimeSelection::Production { provider } => {
            RuntimeRouting::resolve(workspace, provider.as_deref())?.turn_routing()
        }
        RuntimeSelection::Injected(_) => RuntimeRouting::resolve(workspace, None)?.turn_routing(),
    }
}

/// `--print "<prompt>"`: one bounded conversational turn. Identical on a
/// TTY and off it — no terminal mode is ever entered. The final assistant
/// text goes to stdout; proposal outcomes, failures, and cancellation
/// reasons go to stderr. Exit codes: 0 completed, 1 failed/cancelled or
/// infrastructure error, 2 malformed invocation or invalid routing.
fn print_command(writer: &mut OutputWriter, workspace: &Path, deps: &CliDeps, prompt: &str) -> i32 {
    let prompt = prompt.trim();
    if prompt.is_empty() {
        writer.error("relay: --print requires a prompt");
        writer.error("Run `relay --help` for usage.");
        return EXIT_USAGE;
    }
    // Routing resolves before the session opens: an invalid configuration,
    // unknown connection, or unavailable intent connection fails here with
    // the usage exit code and creates no project state.
    let routing = match conversational_routing(workspace, &deps.runtime) {
        Ok(routing) => routing,
        Err(error) => return app_error(writer, error),
    };
    // The turn durably records its facts, so the session (and on a fresh
    // workspace the project and conversation) is created here, not before.
    let (session, _report) =
        match ProjectSession::open_or_create(workspace, deps.session_options.clone()) {
            Ok(opened) => opened,
            Err(error) => return app_error(writer, error),
        };
    let (events, drain) = std::sync::mpsc::channel::<ConversationEvent>();
    let turn = conversation::start_turn(
        &session,
        routing,
        prompt,
        &ConversationLimits::default(),
        CancelToken::new(),
        events,
    );
    // The sender drops when start_turn returns, ending this drain. Nothing
    // streams live in --print; draining just guarantees no buffered event
    // is silently dropped while the turn ran.
    for event in drain.try_iter() {
        let _ = event;
    }
    match turn {
        Ok(result) => match result.status {
            TurnStatus::Completed => {
                for line in result.assistant_text.trim().lines() {
                    writer.text(line);
                }
                // Proposal outcomes are turn diagnostics: stdout stays the
                // clean, scriptable assistant reply.
                for outcome in &result.proposals {
                    let verb = if outcome.accepted {
                        "applied"
                    } else {
                        "rejected"
                    };
                    writer.error(&format!("relay: proposal {verb}: {}", outcome.detail));
                }
                EXIT_OK
            }
            TurnStatus::Failed { message } => {
                writer.error(&format!("relay: the conversational turn failed: {message}"));
                EXIT_FAILURE
            }
            TurnStatus::Cancelled { reason } => {
                writer.error(&format!(
                    "relay: the conversational turn was cancelled: {reason}"
                ));
                EXIT_FAILURE
            }
        },
        // Infrastructure failure: no terminal fact was recorded; the app
        // error mapping already prints the actionable hint.
        Err(error) => app_error(writer, error),
    }
}

fn doctor_command(
    writer: &mut OutputWriter,
    json: bool,
    workspace: &Path,
    deps: &CliDeps,
    action: DoctorAction,
) -> i32 {
    match action {
        DoctorAction::Check => {
            let view = app::doctor_check(workspace, &deps.session_options.store);
            let code = emit(writer, json, &view, render::render_doctor);
            if view.healthy { code } else { EXIT_FAILURE }
        }
        DoctorAction::RebuildProjection => session_view(
            writer,
            json,
            workspace,
            deps,
            app::rebuild_projection,
            render::render_rebuild,
        ),
    }
}

fn run_command(
    writer: &mut OutputWriter,
    json: bool,
    workspace: &Path,
    deps: &CliDeps,
    selection: RuntimeSelection,
    policy: SchedulingPolicy,
    max_ticks: usize,
) -> i32 {
    // Production wiring resolves from the effective configuration and
    // preflights its routes before the session opens: invalid
    // configuration, an unknown `--provider` connection, or a missing
    // credential on a route's primary connection is a usage-class failure
    // that creates no project state. Provider *transport* failures stay
    // runtime failures (exit 1) inside the run.
    let wiring = match selection {
        RuntimeSelection::Production { provider } => {
            match RuntimeRouting::resolve(workspace, provider.as_deref()) {
                Ok(routing) => match routing.preflight_purposes() {
                    Ok(()) => routing.into_wiring(),
                    Err(error) => return app_error(writer, error),
                },
                Err(error) => return app_error(writer, error),
            }
        }
        RuntimeSelection::Injected(wiring) => wiring,
    };
    let session = match ProjectSession::open(workspace, deps.session_options.clone()) {
        Ok(session) => session,
        Err(error) => return app_error(writer, error),
    };
    match relay_app::runner::run_ticks(&session, &wiring, policy, max_ticks) {
        Ok(view) => emit(writer, json, &view, render::render_run),
        Err(error) => app_error(writer, error),
    }
}

fn config_command(
    writer: &mut OutputWriter,
    json: bool,
    workspace: &Path,
    action: &str,
    key: Option<&str>,
) -> i32 {
    let options = relay_config::load::LoadOptions {
        workspace_root: workspace.to_path_buf(),
        user_config_path: None,
        overrides: None,
    };
    let loaded = match relay_config::load::load_config(options) {
        Ok(loaded) => loaded,
        Err(error) => {
            writer.error(&format!("relay: configuration is invalid: {error}"));
            return EXIT_USAGE;
        }
    };
    match action {
        "path" => {
            let project = workspace.join(relay_config::load::PROJECT_CONFIG_FILENAME);
            let user = relay_config::load::user_config_path()
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_else(|| "(none)".to_string());
            if json {
                let value = serde_json::json!({
                    "project": project.to_string_lossy(),
                    "user": user,
                });
                writer.json(&serde_json::to_string_pretty(&value).unwrap_or_default());
            } else {
                writer.text(&format!("project: {}", project.display()));
                writer.text(&format!("user: {user}"));
            }
            EXIT_OK
        }
        "get" => {
            let Some(key) = key else {
                writer.error("relay: config get requires a key");
                return EXIT_USAGE;
            };
            match relay_config::inspect::lookup_dotted(&loaded.config, key) {
                Some(value) => {
                    writer.json(&serde_json::to_string_pretty(&value).unwrap_or_default());
                    EXIT_OK
                }
                None => {
                    writer.error(&format!("relay: configuration key \"{key}\" is not set"));
                    EXIT_FAILURE
                }
            }
        }
        _ => match relay_config::inspect::effective_value(&loaded.config) {
            Ok(value) => {
                writer.json(&serde_json::to_string_pretty(&value).unwrap_or_default());
                EXIT_OK
            }
            Err(error) => {
                writer.error(&format!(
                    "relay: could not render the configuration: {error}"
                ));
                EXIT_FAILURE
            }
        },
    }
}

/// `relay why`: explain the work behind the next action, or fall back to
/// the summary when nothing is selected.
fn why_command(writer: &mut OutputWriter, json: bool, workspace: &Path, deps: &CliDeps) -> i32 {
    let session = match ProjectSession::open(workspace, deps.session_options.clone()) {
        Ok(session) => session,
        Err(error) => return app_error(writer, error),
    };
    let summary = match app::summary(&session) {
        Ok(summary) => summary,
        Err(error) => return app_error(writer, error),
    };
    let target_work = next_action_work(&session, &summary);
    match target_work {
        Some(work) => match app::explain(&session, &work) {
            Ok(view) => emit(writer, json, &view, render::render_explain),
            Err(error) => app_error(writer, error),
        },
        None => emit(writer, json, &summary, render::render_summary),
    }
}

/// The work id the next action concerns: the ready work it names, or the
/// work gated by the decision it names. Derived only from the summary's
/// engine-made recommendation.
fn next_action_work(
    session: &ProjectSession,
    summary: &relay_app::views::SummaryView,
) -> Option<String> {
    let next = &summary.recommendation.next;
    match next.kind.as_str() {
        "ready-work" => next.target.clone(),
        "awaiting-human" => {
            let decision = relay_domain::DecisionId::parse(next.target.as_deref()?).ok()?;
            session.graph().works().find_map(|(work, node)| {
                matches!(
                    &node.work.completion,
                    relay_domain::CompletionRule::Decision { decision: rule } if rule == &decision
                )
                .then(|| work.to_string())
            })
        }
        _ => None,
    }
}

/// `relay run <message>`: init when no project exists, then run.
fn legacy_run(
    writer: &mut OutputWriter,
    json: bool,
    workspace: &Path,
    deps: &CliDeps,
    message: Option<&str>,
) -> i32 {
    let existing = ProjectSession::open(workspace, deps.session_options.clone());
    let initialized = existing
        .as_ref()
        .map(|session| session.is_initialized())
        .unwrap_or(false);
    if initialized {
        legacy_note(
            writer,
            "run <message>",
            "run (the goal is graph state now; free-text messages no longer start projects)",
        );
        return run_command(
            writer,
            json,
            workspace,
            deps,
            deps.runtime.clone(),
            SchedulingPolicy::default(),
            1,
        );
    }
    let Some(message) = message else {
        writer.error("relay: no Relay project in this workspace");
        writer.error("Initialize one first: `relay init --goal <text>`.");
        return EXIT_FAILURE;
    };
    legacy_note(writer, "run <message>", "init --goal <message> + run");
    match app::init(workspace, message, deps.session_options.clone()) {
        Ok(view) => {
            let _ = emit(writer, json, &view, render::render_init);
        }
        Err(error) => return app_error(writer, error),
    }
    run_command(
        writer,
        json,
        workspace,
        deps,
        deps.runtime.clone(),
        SchedulingPolicy::default(),
        1,
    )
}

/// Note a legacy mapping on stderr (stdout stays parseable in `--json`
/// mode; in human mode the note leads the output).
fn legacy_note(writer: &mut OutputWriter, from: &str, to: &str) {
    writer.error(&format!(
        "note: `relay {from}` maps to `{to}` in the work-graph interface; the output shape \
         differs from the legacy command"
    ));
}

fn migration_message(
    writer: &mut OutputWriter,
    requested: &str,
    why: &str,
    instead: &[&str],
) -> i32 {
    writer.error(&format!("relay: {requested} has no work-graph equivalent."));
    writer.error(&format!("  {why}."));
    writer.error("  Use instead:");
    for command in instead {
        writer.error(&format!("    {command}"));
    }
    EXIT_USAGE
}

fn parse_autonomy(text: Option<&str>) -> Result<SchedulingPolicy, String> {
    let autonomy = match text {
        None => Autonomy::Safe,
        Some("manual") => Autonomy::Manual,
        Some("safe") => Autonomy::Safe,
        Some("until-decision") => Autonomy::UntilDecision,
        Some(bounded) => {
            let Some(limit) = bounded.strip_prefix("bounded:") else {
                return Err(format!(
                    "unknown autonomy \"{bounded}\" (expected manual, safe, until-decision, or \
                     bounded:<n>)"
                ));
            };
            let limit: u32 = limit.parse().map_err(|_| {
                format!("bounded autonomy needs a number of attempts (got \"{limit}\")")
            })?;
            Autonomy::Bounded(limit.max(1))
        }
    };
    Ok(SchedulingPolicy {
        autonomy,
        max_concurrent_writers: 1,
    })
}

fn split_reference(target: &str) -> (String, Option<u32>) {
    match target.split_once('@') {
        Some((artifact, version)) => (artifact.to_string(), version.parse::<u32>().ok()),
        None => (target.to_string(), None),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn chat_opens_only_on_a_fully_interactive_terminal() {
        assert_eq!(
            chat_plan(TerminalCapability {
                stdin_tty: true,
                stdout_tty: true
            }),
            ChatPlan::OpenInterface
        );
        for (stdin_tty, stdout_tty) in [(true, false), (false, true), (false, false)] {
            assert_eq!(
                chat_plan(TerminalCapability {
                    stdin_tty,
                    stdout_tty
                }),
                ChatPlan::RejectScripted,
                "stdin={stdin_tty} stdout={stdout_tty}"
            );
        }
    }

    #[test]
    fn the_conversational_routing_follows_the_run_selection() {
        // A unique scratch directory (std-only: lib unit tests have no
        // dev-dependencies), removed before asserting so a failure cannot
        // leak it.
        let root = std::env::temp_dir().join(format!(
            "relay-cli-dispatch-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&root).ok().unwrap();
        let routing =
            conversational_routing(&root, &RuntimeSelection::Production { provider: None })
                .ok()
                .unwrap();
        std::fs::remove_dir_all(&root).ok();
        // The offline default: the intent route resolves onto the fake
        // connection with its default model.
        assert_eq!(routing.route.provider, "fake");
        assert_eq!(routing.route.model, "fake-standard");
        assert_eq!(routing.provider.name().as_str(), "fake");
    }
}
