//! Command-line argument parsing for the work-centric grammar.
//!
//! A small hand-rolled parser (flat tokenizer, two-level commands), chosen
//! over a framework so the legacy-command recognition table lives beside
//! the grammar it maps. The frozen byte-compat grammar that shipped with
//! the stopped compatibility port remains the reference for *what legacy
//! commands must be recognized*; this parser replaces it as the primary
//! surface and recognizes those commands explicitly
//! (`Command::Legacy…` variants) instead of reproducing their behavior.
//!
//! Conventions: `--help`/`-h` and `--version`/`-V` win immediately from
//! anywhere; `--workspace <path>` (or `--workspace=<path>`) selects the
//! project directory (default: current directory); `--json` is accepted for
//! every command; only long options exist.

use std::path::PathBuf;

/// The preview version this binary reports.
pub const VERSION: &str = "0.3.0-preview";

/// The usage text (printed by `--help`).
pub const USAGE: &str = "\
relay — carry product intent from discovery through delivery (work-graph preview)

Usage:
  relay init --goal <text>                        Create a project and declare its goal
  relay project summary                           Goal, phase-grouped progress, recommendation
  relay work ready|blocked|list                   Ready / blocked / all work, with explanations
  relay work inspect <id>                         One work item in full (deps, attempts, evidence)
  relay decisions list                            Decisions, with options and status
  relay decisions resolve <id> --choice <label>   Resolve a decision (optionally --rationale)
  relay artifact inspect <id>[@version]           Structured content + rendered Markdown view
  relay evidence <work-or-criterion>              Evidence attached to a work item or artifact
  relay run [--autonomy <level>] [--provider <name>] [--max-ticks <n>]
                                                  Run ready work (non-interactive; never
                                                  auto-resolves human decisions)
  relay cancel <attempt-id>                       Cancel an active attempt
  relay explain <work-id>                         Why a work item is ready, blocked, or waiting
  relay expand                                    Apply the workflow's next proposed round
  relay import-prd <file>                         Import an edited PRD view as a revision
  relay doctor check|rebuild-projection           Verify / rebuild the local fact store
  relay migrate --workspace <legacy-copy>         Migrate a legacy project copy (--dry-run to plan)
  relay config show|path|get <key>                Inspect effective configuration
  relay tui                                       Interactive terminal interface (q quits)

Options:
  --workspace <path>    Operate on this directory (default: current directory)
  --json                Machine-readable output where supported
  --autonomy <level>    manual | safe | until-decision | bounded:<n>  (default: safe)
  --provider <name>     fake | openai | anthropic  (default: fake, offline)
  --max-ticks <n>       Scheduling waves per `run` (default: 1)
  --no-color            Accepted for compatibility; output is uncolored today
  -h, --help            Show this help
  -V, --version         Show the version

Legacy commands (mapped, not reproduced):
  relay status                  -> relay project summary
  relay why                     -> relay explain <next work> (or project summary)
  relay inspect <artifact>      -> relay artifact inspect <artifact>
  relay run <message>           -> relay init --goal <message> + relay run (when no project)
  relay mode <mode>             -> migration message (no global modes exist)
  relay auto <bound>            -> migration message (autonomy is per-run)
  relay pause [reason]          -> migration message (the graph runs only when you do)
";

/// Doctor actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DoctorAction {
    /// Read-only verification of the fact store.
    Check,
    /// Quarantine and rebuild the SQLite projection from JSONL.
    RebuildProjection,
}

/// A parsed command.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// `-h`/`--help`.
    Help,
    /// `-V`/`--version`.
    Version,
    /// `init --goal <text>`.
    Init {
        /// The project goal.
        goal: Option<String>,
    },
    /// `project summary`.
    ProjectSummary,
    /// `work ready`.
    WorkReady,
    /// `work blocked`.
    WorkBlocked,
    /// `work list`.
    WorkList,
    /// `work inspect <id>`.
    WorkInspect {
        /// The work id.
        id: String,
    },
    /// `decisions list`.
    DecisionsList,
    /// `decisions resolve <id> --choice <label>`.
    DecisionsResolve {
        /// The decision id.
        id: String,
        /// The chosen option label (or free-form answer).
        choice: String,
        /// Why, when given.
        rationale: Option<String>,
    },
    /// `artifact inspect <id>[@version]` (with optional `--publish`).
    ArtifactInspect {
        /// The artifact reference.
        target: String,
        /// Also publish the rendered view as a blob attachment.
        publish: bool,
    },
    /// `evidence <work-or-criterion>`.
    Evidence {
        /// A work id, decision id, or artifact reference.
        target: String,
    },
    /// `run` (new grammar: no free-text message).
    Run {
        /// `--autonomy`.
        autonomy: Option<String>,
        /// `--provider`.
        provider: Option<String>,
        /// `--max-ticks`.
        max_ticks: usize,
    },
    /// `cancel <attempt-id>`.
    Cancel {
        /// The attempt id.
        attempt: String,
        /// `--reason`.
        reason: Option<String>,
    },
    /// `explain <work-id>`.
    Explain {
        /// The work id.
        work: String,
    },
    /// `expand`.
    Expand,
    /// `import-prd <file>`.
    ImportPrd {
        /// The edited PRD Markdown file.
        path: String,
    },
    /// `doctor check|rebuild-projection`.
    Doctor {
        /// The action.
        action: DoctorAction,
    },
    /// `migrate [--dry-run]` over `--workspace <legacy-copy>`.
    Migrate {
        /// Plan only; stage and switch nothing.
        dry_run: bool,
    },
    /// `config show|path|get <key>`.
    Config {
        /// The config action.
        action: String,
        /// The dotted key for `get`.
        key: Option<String>,
    },
    /// `tui` (interactive; the only command that touches the terminal).
    Tui,
    /// Legacy `status` → project summary.
    LegacyStatus,
    /// Legacy `why` → explain equivalent.
    LegacyWhy,
    /// Legacy `inspect <artifact>` → artifact inspect.
    LegacyInspect {
        /// The artifact the user named.
        target: String,
    },
    /// Legacy `mode <mode>` → migration message.
    LegacyMode {
        /// The mode that was requested.
        mode: String,
    },
    /// Legacy `auto [bound]` → migration message.
    LegacyAuto {
        /// The bound that was requested, when present.
        bound: Option<String>,
    },
    /// Legacy `pause [reason]` → migration message.
    LegacyPause {
        /// The pause reason, when present.
        reason: Option<String>,
    },
    /// Legacy `run [message]` → init + run when no project exists.
    LegacyRun {
        /// The joined positional message.
        message: Option<String>,
    },
}

/// A successful parse: the command, the workspace, and the JSON flag.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedArguments {
    /// The command to dispatch.
    pub command: Command,
    /// The absolute workspace root.
    pub workspace: PathBuf,
    /// Whether `--json` was requested.
    pub json: bool,
}

/// The result of parsing.
#[derive(Debug, Clone, PartialEq)]
pub enum ParseResult {
    /// Parsed successfully.
    Ok(Box<ParsedArguments>),
    /// Rejected; `usage` marks errors that should print the usage hint.
    Err {
        /// The error message.
        error: String,
        /// Whether the usage hint applies.
        usage: bool,
    },
}

impl ParseResult {
    /// The error message, when this is an error.
    pub fn error_message(&self) -> Option<&str> {
        match self {
            ParseResult::Err { error, .. } => Some(error),
            ParseResult::Ok(_) => None,
        }
    }
}

/// Top-level commands of the new grammar (exposed so tests can assert the
/// help text lists every one).
pub const COMMANDS: [&str; 15] = [
    "init",
    "project",
    "work",
    "decisions",
    "artifact",
    "evidence",
    "run",
    "cancel",
    "explain",
    "expand",
    "import-prd",
    "doctor",
    "migrate",
    "config",
    "tui",
];

/// Legacy commands that must be recognized and mapped (never reproduced).
pub const LEGACY_COMMANDS: [&str; 6] = ["status", "why", "inspect", "mode", "auto", "pause"];

/// Flags that consume the next token when not in `--name=value` form.
const VALUE_FLAGS: [&str; 8] = [
    "workspace",
    "goal",
    "choice",
    "rationale",
    "reason",
    "autonomy",
    "provider",
    "max-ticks",
];

/// One parsed flag value.
#[derive(Debug, Clone, PartialEq)]
enum FlagValue {
    /// A boolean flag.
    Boolean(bool),
    /// A string value.
    String(String),
}

impl FlagValue {
    fn as_str(&self) -> Option<&str> {
        match self {
            FlagValue::Boolean(_) => None,
            FlagValue::String(text) => Some(text),
        }
    }
}

type Flags = Vec<(String, FlagValue)>;

fn flag<'a>(flags: &'a Flags, name: &str) -> Option<&'a FlagValue> {
    flags
        .iter()
        .rev()
        .find(|(candidate, _)| candidate == name)
        .map(|(_, value)| value)
}

/// Split argv into the leading command, positionals, and flags. Help and
/// version win immediately; single-dash tokens other than `-h`/`-V` are
/// rejected.
fn tokenize(argv: &[String]) -> (Option<String>, Vec<String>, Flags, Vec<String>) {
    let mut flags: Flags = Vec::new();
    let mut positionals: Vec<String> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    let mut command: Option<String> = None;
    let mut index = 0;
    while index < argv.len() {
        let token = &argv[index];
        if token == "--help" || token == "-h" {
            return (Some("help".to_string()), positionals, flags, errors);
        }
        if token == "--version" || token == "-V" {
            return (Some("version".to_string()), positionals, flags, errors);
        }
        if token.starts_with("--") {
            let (name, value) = match token.find('=') {
                Some(position) => (
                    token[2..position].to_string(),
                    FlagValue::String(token[position + 1..].to_string()),
                ),
                None => {
                    let name = token.strip_prefix("--").unwrap_or_default().to_string();
                    if VALUE_FLAGS.contains(&name.as_str()) && index + 1 < argv.len() {
                        index += 1;
                        (name, FlagValue::String(argv[index].clone()))
                    } else {
                        (name, FlagValue::Boolean(true))
                    }
                }
            };
            flags.retain(|(candidate, _)| *candidate != name);
            flags.push((name, value));
        } else if token.starts_with('-') && token.chars().count() > 1 {
            errors.push(format!(
                "unknown option \"{token}\" (only long options are supported)"
            ));
        } else if command.is_none() {
            command = Some(token.clone());
        } else {
            positionals.push(token.clone());
        }
        index += 1;
    }
    (command, positionals, flags, errors)
}

fn parse_workspace(flags: &Flags) -> Result<PathBuf, std::io::Error> {
    match flag(flags, "workspace").and_then(FlagValue::as_str) {
        Some(path) if !path.is_empty() => {
            if std::path::Path::new(path).is_absolute() {
                Ok(PathBuf::from(path))
            } else {
                std::env::current_dir().map(|cwd| cwd.join(path))
            }
        }
        _ => std::env::current_dir(),
    }
}

fn parse_max_ticks(flags: &Flags) -> Result<usize, String> {
    match flag(flags, "max-ticks").and_then(FlagValue::as_str) {
        None => Ok(1),
        Some(text) => text
            .parse::<usize>()
            .map_err(|_| format!("--max-ticks must be a positive whole number (got \"{text}\")")),
    }
}

fn exactly_one(positionals: &[String], what: &str) -> Result<String, String> {
    match positionals.first() {
        Some(value) if positionals.len() == 1 => Ok(value.clone()),
        _ => Err(format!("{what} takes exactly one argument")),
    }
}

/// Parse `argv` (without the program name).
pub fn parse_arguments(argv: &[String]) -> ParseResult {
    let (command, positionals, flags, errors) = tokenize(argv);
    if !errors.is_empty() {
        return ParseResult::Err {
            error: errors.join("; "),
            usage: true,
        };
    }
    let Some(command) = command else {
        return ParseResult::Err {
            error: "no command given".to_string(),
            usage: true,
        };
    };
    let workspace = match parse_workspace(&flags) {
        Ok(workspace) => workspace,
        Err(_) => PathBuf::from("."),
    };
    let json = flag(&flags, "json") == Some(&FlagValue::Boolean(true));
    let dry_run = flag(&flags, "dry-run") == Some(&FlagValue::Boolean(true));

    if command == "help" || command == "version" {
        let parsed = if command == "help" {
            Command::Help
        } else {
            Command::Version
        };
        return ParseResult::Ok(Box::new(ParsedArguments {
            command: parsed,
            workspace,
            json,
        }));
    }

    let parsed = match command.as_str() {
        "init" => Command::Init {
            goal: flag(&flags, "goal")
                .and_then(FlagValue::as_str)
                .map(str::to_string)
                .or_else(|| non_empty(positionals.join(" "))),
        },
        "project" => match positionals.first().map(String::as_str) {
            Some("summary") if positionals.len() == 1 => Command::ProjectSummary,
            _ => {
                return usage_error("project supports: summary");
            }
        },
        "work" => match positionals.first().map(String::as_str) {
            Some("ready") if positionals.len() == 1 => Command::WorkReady,
            Some("blocked") if positionals.len() == 1 => Command::WorkBlocked,
            Some("list") if positionals.len() == 1 => Command::WorkList,
            Some("inspect") => {
                let rest = positionals[1..].to_vec();
                let id = match exactly_one(&rest, "work inspect") {
                    Ok(id) => id,
                    Err(error) => return usage_error(error),
                };
                Command::WorkInspect { id }
            }
            _ => {
                return usage_error("work supports: ready, blocked, list, inspect <id>");
            }
        },
        "decisions" => match positionals.first().map(String::as_str) {
            Some("list") if positionals.len() == 1 => Command::DecisionsList,
            Some("resolve") => {
                let rest = positionals[1..].to_vec();
                let id = match exactly_one(&rest, "decisions resolve") {
                    Ok(id) => id,
                    Err(error) => return usage_error(error),
                };
                let Some(choice) = flag(&flags, "choice").and_then(FlagValue::as_str) else {
                    return usage_error("decisions resolve requires --choice <label>");
                };
                Command::DecisionsResolve {
                    id,
                    choice: choice.to_string(),
                    rationale: flag(&flags, "rationale")
                        .and_then(FlagValue::as_str)
                        .map(str::to_string),
                }
            }
            _ => {
                return usage_error("decisions supports: list, resolve <id> --choice <label>");
            }
        },
        "artifact" => match positionals.first().map(String::as_str) {
            Some("inspect") => {
                let rest = positionals[1..].to_vec();
                let target = match exactly_one(&rest, "artifact inspect") {
                    Ok(target) => target,
                    Err(error) => return usage_error(error),
                };
                Command::ArtifactInspect {
                    target,
                    publish: flag(&flags, "publish") == Some(&FlagValue::Boolean(true)),
                }
            }
            _ => {
                return usage_error("artifact supports: inspect <id>[@version]");
            }
        },
        "evidence" => {
            let target = match exactly_one(&positionals, "evidence") {
                Ok(target) => target,
                Err(error) => return usage_error(error),
            };
            Command::Evidence { target }
        }
        "run" => {
            if !positionals.is_empty() {
                // Legacy shape: `relay run <message...>`.
                Command::LegacyRun {
                    message: non_empty(positionals.join(" ")),
                }
            } else {
                let max_ticks = match parse_max_ticks(&flags) {
                    Ok(value) => value,
                    Err(error) => return usage_error(error),
                };
                Command::Run {
                    autonomy: flag(&flags, "autonomy")
                        .and_then(FlagValue::as_str)
                        .map(str::to_string),
                    provider: flag(&flags, "provider")
                        .and_then(FlagValue::as_str)
                        .map(str::to_string),
                    max_ticks: max_ticks.max(1),
                }
            }
        }
        "cancel" => {
            let attempt = match exactly_one(&positionals, "cancel") {
                Ok(attempt) => attempt,
                Err(error) => return usage_error(error),
            };
            Command::Cancel {
                attempt,
                reason: flag(&flags, "reason")
                    .and_then(FlagValue::as_str)
                    .map(str::to_string),
            }
        }
        "explain" => {
            let work = match exactly_one(&positionals, "explain") {
                Ok(work) => work,
                Err(error) => return usage_error(error),
            };
            Command::Explain { work }
        }
        "expand" if positionals.is_empty() => Command::Expand,
        "import-prd" => {
            let path = match exactly_one(&positionals, "import-prd") {
                Ok(path) => path,
                Err(error) => return usage_error(error),
            };
            Command::ImportPrd { path }
        }
        "doctor" => match positionals.first().map(String::as_str) {
            Some(action) if positionals.len() == 1 => match action {
                "check" => Command::Doctor {
                    action: DoctorAction::Check,
                },
                "rebuild-projection" => Command::Doctor {
                    action: DoctorAction::RebuildProjection,
                },
                other => {
                    return usage_error(format!(
                        "doctor action must be check or rebuild-projection (got \"{other}\")"
                    ));
                }
            },
            _ => return usage_error("doctor supports: check, rebuild-projection"),
        },
        "migrate" if positionals.is_empty() => Command::Migrate { dry_run },
        "config" => {
            let action = positionals
                .first()
                .cloned()
                .unwrap_or_else(|| "show".to_string());
            if action != "show" && action != "path" && action != "get" {
                return usage_error(format!(
                    "config action must be show, path, or get (got \"{action}\")"
                ));
            }
            let key = positionals.get(1).cloned();
            if action == "get" && key.is_none() {
                return usage_error("config get requires a key".to_string());
            }
            Command::Config { action, key }
        }
        "tui" if positionals.is_empty() => Command::Tui,
        // Legacy commands: recognized and mapped in dispatch.
        "status" if positionals.is_empty() => Command::LegacyStatus,
        "why" if positionals.is_empty() => Command::LegacyWhy,
        "inspect" => {
            let target = match exactly_one(&positionals, "inspect") {
                Ok(target) => target,
                Err(error) => return usage_error(error),
            };
            Command::LegacyInspect { target }
        }
        "mode" => {
            let mode = match exactly_one(&positionals, "mode") {
                Ok(mode) => mode,
                Err(error) => return usage_error(error),
            };
            Command::LegacyMode { mode }
        }
        "auto" => Command::LegacyAuto {
            bound: positionals.first().cloned().or_else(|| {
                flag(&flags, "auto")
                    .and_then(FlagValue::as_str)
                    .map(str::to_string)
            }),
        },
        "pause" => Command::LegacyPause {
            reason: non_empty(positionals.join(" ")),
        },
        other => {
            return usage_error(format!(
                "unknown command \"{other}\"; the legacy commands {} are mapped where honest and \
                 refused with guidance elsewhere",
                LEGACY_COMMANDS.join(", ")
            ));
        }
    };

    ParseResult::Ok(Box::new(ParsedArguments {
        command: parsed,
        workspace,
        json,
    }))
}

fn usage_error(message: impl Into<String>) -> ParseResult {
    ParseResult::Err {
        error: message.into(),
        usage: true,
    }
}

fn non_empty(text: String) -> Option<String> {
    if text.is_empty() { None } else { Some(text) }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> ParseResult {
        let argv: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        parse_arguments(&argv)
    }

    #[test]
    fn help_and_version_win_from_anywhere() {
        assert!(matches!(parse(&["--help"]), ParseResult::Ok(p) if p.command == Command::Help));
        assert!(matches!(parse(&["-h"]), ParseResult::Ok(p) if p.command == Command::Help));
        assert!(
            matches!(parse(&["--version"]), ParseResult::Ok(p) if p.command == Command::Version)
        );
        assert!(matches!(
            parse(&["work", "ready", "-V"]),
            ParseResult::Ok(p) if p.command == Command::Version
        ));
    }

    #[test]
    fn usage_documents_every_new_command() {
        for command in COMMANDS {
            assert!(USAGE.contains(&format!("relay {command}")), "{command}");
        }
    }

    #[test]
    fn usage_documents_every_legacy_command() {
        for command in LEGACY_COMMANDS {
            assert!(USAGE.contains(&format!("relay {command}")), "{command}");
        }
    }

    #[test]
    fn parses_the_new_grammar() {
        assert!(matches!(
            parse(&["init", "--goal", "ship csv export"]),
            ParseResult::Ok(p) if matches!(&p.command, Command::Init { goal } if goal.as_deref() == Some("ship csv export"))
        ));
        assert!(matches!(
            parse(&["project", "summary"]),
            ParseResult::Ok(p) if p.command == Command::ProjectSummary
        ));
        assert!(matches!(
            parse(&["work", "ready"]),
            ParseResult::Ok(p) if p.command == Command::WorkReady
        ));
        assert!(matches!(
            parse(&["work", "inspect", "work_discover-1"]),
            ParseResult::Ok(p) if matches!(&p.command, Command::WorkInspect { id } if id == "work_discover-1")
        ));
        assert!(matches!(
            parse(&[
                "decisions", "resolve", "dec_approve-1", "--choice", "approve",
                "--rationale", "matches the goal"
            ]),
            ParseResult::Ok(p) if matches!(&p.command, Command::DecisionsResolve { id, choice, rationale }
                if id == "dec_approve-1" && choice == "approve"
                    && rationale.as_deref() == Some("matches the goal"))
        ));
        assert!(matches!(
            parse(&["artifact", "inspect", "art_brief@2", "--publish"]),
            ParseResult::Ok(p) if matches!(&p.command, Command::ArtifactInspect { target, publish }
                if target == "art_brief@2" && *publish)
        ));
        assert!(matches!(
            parse(&["run", "--autonomy", "bounded:3", "--provider", "fake", "--max-ticks", "2"]),
            ParseResult::Ok(p) if matches!(&p.command, Command::Run { autonomy, provider, max_ticks }
                if autonomy.as_deref() == Some("bounded:3")
                    && provider.as_deref() == Some("fake") && *max_ticks == 2)
        ));
        assert!(matches!(
            parse(&["doctor", "rebuild-projection"]),
            ParseResult::Ok(p) if matches!(&p.command, Command::Doctor { action: DoctorAction::RebuildProjection })
        ));
        assert!(matches!(
            parse(&["migrate", "--dry-run"]),
            ParseResult::Ok(p) if matches!(&p.command, Command::Migrate { dry_run: true })
        ));
    }

    #[test]
    fn decisions_resolve_requires_a_choice() {
        let ParseResult::Err { error, usage } = parse(&["decisions", "resolve", "dec_1"]) else {
            panic!("expected an error");
        };
        assert!(error.contains("--choice"), "{error}");
        assert!(usage);
    }

    #[test]
    fn recognizes_legacy_commands() {
        assert!(
            matches!(parse(&["status"]), ParseResult::Ok(p) if p.command == Command::LegacyStatus)
        );
        assert!(matches!(parse(&["why"]), ParseResult::Ok(p) if p.command == Command::LegacyWhy));
        assert!(matches!(
            parse(&["inspect", "prd"]),
            ParseResult::Ok(p) if matches!(&p.command, Command::LegacyInspect { target } if target == "prd")
        ));
        assert!(matches!(
            parse(&["mode", "planning"]),
            ParseResult::Ok(p) if matches!(&p.command, Command::LegacyMode { mode } if mode == "planning")
        ));
        assert!(matches!(
            parse(&["auto", "off"]),
            ParseResult::Ok(p) if matches!(&p.command, Command::LegacyAuto { bound } if bound.as_deref() == Some("off"))
        ));
        assert!(matches!(
            parse(&["pause", "reviewing", "the", "brief"]),
            ParseResult::Ok(p) if matches!(&p.command, Command::LegacyPause { reason }
                if reason.as_deref() == Some("reviewing the brief"))
        ));
        // `run` with a positional message is the legacy shape.
        assert!(matches!(
            parse(&["run", "build a timer app"]),
            ParseResult::Ok(p) if matches!(&p.command, Command::LegacyRun { message }
                if message.as_deref() == Some("build a timer app"))
        ));
        // `run` without a message is the new command.
        assert!(matches!(
            parse(&["run", "--autonomy", "safe"]),
            ParseResult::Ok(p) if matches!(&p.command, Command::Run { .. })
        ));
    }

    #[test]
    fn rejects_unknown_commands_and_short_options() {
        assert!(matches!(parse(&[]), ParseResult::Err { usage: true, .. }));
        let ParseResult::Err { error, .. } = parse(&["frobnicate"]) else {
            panic!("expected an error");
        };
        assert!(error.contains("frobnicate"));
        let ParseResult::Err { error, .. } = parse(&["status", "-x"]) else {
            panic!("expected an error");
        };
        assert!(error.contains("-x"));
    }

    #[test]
    fn json_flag_applies_to_every_command() {
        for args in [
            vec!["project", "summary", "--json"],
            vec!["work", "ready", "--json"],
            vec!["explain", "work_x", "--json"],
        ] {
            assert!(
                matches!(parse(&args), ParseResult::Ok(p) if p.json),
                "{args:?}"
            );
        }
    }

    #[test]
    fn workspace_flag_resolves_relative_to_cwd() {
        let ParseResult::Ok(parsed) = parse(&["--workspace", "sub/dir", "status"]) else {
            panic!("expected a parse");
        };
        assert!(parsed.workspace.ends_with("sub/dir"));
        assert!(parsed.workspace.is_absolute());
    }
}
