//! The runtime wiring boundary: a thin [`ControllerOps`] trait expressing
//! the workflow-controller surface the stateful CLI commands and the TUI
//! consume, plus [`DirectStoreOps`], a store-backed implementation of the
//! simple paths (initialize, set-mode, set-autonomy, pause/resume, inspect,
//! status/why views, slash-command dispatch). When `relay-runtime` lands it
//! implements this trait with the real controller (model routing, agent
//! loop, approvals, revisions); until then the not-yet-wired operations
//! return an actionable error instead of pretending to run.
//!
//! Event semantics mirror `src/runtime/controller.ts` (the retired
//! TypeScript implementation, removed from the tree 2026-10-14) exactly for
//! the operations implemented here: `set_mode` applies
//! `assert_transition_allowed` and records `mode.entered` with source
//! `user` (or `mode.transition.rejected`), `set_autonomy` records
//! `autonomy.changed` with source `user`, `pause` records `workflow.paused`
//! by `user`, and `inspect` records `artifact.inspected` via `user`.

use crate::errors::CliError;
use relay_core::modes::{AutonomyBound, WorkflowMode};
use relay_core::outcomes::ControllerOutcome;
use relay_core::project_state::ProjectState;
use relay_store::clock::IdGen;
use relay_store::{
    AppendInput, EventRecord, RelayStore, StoreOptions, clock::UuidIdGen, canonical::Json,
};
use relay_ui::{ControllerEvent, StatusView, WhyView};
use std::path::{Path, PathBuf};

/// The result of one controller operation: the outcome plus the state after
/// the operation settled.
#[derive(Debug, Clone)]
pub struct StepResult {
    /// The deterministic outcome.
    pub outcome: ControllerOutcome,
    /// The replayed state after the operation.
    pub state: ProjectState,
}

/// One stored artifact file, as `relay inspect` prints it.
#[derive(Debug, Clone)]
pub struct ArtifactFile {
    /// The parsed frontmatter mapping (insertion order preserved).
    pub frontmatter: Vec<(String, serde_yaml::Value)>,
    /// The Markdown body after the frontmatter.
    pub body: String,
    /// The artifact path as stored (relative to the workspace root).
    pub path: String,
}

/// Not-yet-wired operations report this marker so callers can word their
/// errors consistently.
pub const RUNTIME_NOT_LINKED: &str = "the workflow runtime (relay-runtime) is not linked in this build; \
slash commands (/status, /why, /inspect, /mode, /auto, /pause, /resume) work, model-driven steps do not";

/// The controller surface consumed by the stateful CLI commands and the TUI.
pub trait ControllerOps {
    /// The project id, or `None` before initialization.
    fn project_id(&self) -> Option<String>;

    /// The current state snapshot (uninitialized feeds return `None`).
    fn state(&self) -> Option<ProjectState>;

    /// The `/status` view over the replayed log.
    fn status_view(&mut self) -> Result<StatusView, CliError>;

    /// The `/why` view over the replayed log.
    fn why_view(&mut self) -> Result<WhyView, CliError>;

    /// Initialize a fresh project (`project.initialized`, plus
    /// `session.started` when `with_session` — the `relay run` fresh-project
    /// path appends it, `relay init` does not).
    fn initialize(
        &mut self,
        goal: &str,
        autonomy: &str,
        with_session: bool,
    ) -> Result<StepResult, CliError>;

    /// `relay mode <mode>` semantics.
    fn set_mode(&mut self, mode: WorkflowMode) -> Result<StepResult, CliError>;

    /// `relay auto <bound>` semantics.
    fn set_autonomy(&mut self, bound: AutonomyBound) -> Result<StepResult, CliError>;

    /// `relay pause [reason]` semantics.
    fn pause(&mut self, reason: &str) -> Result<StepResult, CliError>;

    /// `/resume` semantics.
    fn resume(&mut self) -> Result<StepResult, CliError>;

    /// `relay inspect <target>` semantics (resolves kinds, ids, and paths;
    /// records `artifact.inspected`).
    fn inspect(&mut self, target: &str) -> Result<Option<ArtifactFile>, CliError>;

    /// `handleInput`: slash-command dispatch and (once wired) free text.
    fn handle_input(&mut self, input: &str) -> Result<StepResult, CliError>;

    /// One workflow step (agent loop; not wired until relay-runtime lands).
    fn run_step(&mut self) -> Result<StepResult, CliError>;

    /// Drain queued controller UI events (the TUI feed).
    fn take_events(&mut self) -> Vec<ControllerEvent>;

    /// This process's session id.
    fn session_id(&self) -> &str;
}

/// A store-backed [`ControllerOps`]. Opening acquires the writer lock and
/// reconciles the projection, exactly like the TypeScript CLI opening
/// `.relay/state.db` read-write for every stateful command.
pub struct DirectStoreOps {
    workspace: PathBuf,
    store: Option<RelayStore>,
    project_id: Option<String>,
    session_id: String,
    events: Vec<ControllerEvent>,
    options: StoreOptions,
}

/// Whether this workspace has an initialized canonical Relay store (the
/// Rust equivalent of the reference's `hasRelayState`).
pub fn has_relay_state(workspace_root: &Path) -> bool {
    workspace_root.join(".relay").join("manifest.json").is_file()
}

/// Whether a legacy TypeScript `.relay/state.db` exists (migration input).
pub fn has_legacy_state(workspace_root: &Path) -> bool {
    workspace_root.join(".relay").join("state.db").is_file()
}

fn store_error(error: relay_store::StoreError) -> CliError {
    CliError::Internal {
        message: error.to_string(),
    }
}

fn core_error(error: relay_core::CoreError) -> CliError {
    CliError::Internal {
        message: error.to_string(),
    }
}

/// Truncate to `max` characters with an ellipsis, like the controller's
/// `bounded` helper (character-based; ASCII payloads are byte-identical).
pub fn bounded(text: &str, max: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max {
        return text.to_string();
    }
    let kept: String = chars[..max.saturating_sub(1)].iter().collect();
    format!("{kept}…")
}

/// Validate the store's envelopes through relay-core's two-stage decoder.
fn to_relay_events(records: &[EventRecord]) -> Result<Vec<relay_core::RelayEvent>, CliError> {
    let mut events = Vec::with_capacity(records.len());
    for record in records {
        let envelope = &record.envelope;
        let value = serde_json::json!({
            "envelopeVersion": envelope.envelope_version,
            "id": envelope.id,
            "projectId": envelope.project_id,
            "sequence": envelope.sequence,
            "sessionId": envelope.session_id,
            "type": envelope.event_type,
            "schemaVersion": envelope.schema_version,
            "timestamp": envelope.timestamp,
            "correlationId": envelope.correlation_id,
            "causationId": envelope.causation_id,
            "payload": envelope.payload.to_serde(),
            "previousHash": envelope.previous_hash,
            "hash": envelope.hash,
        });
        let parsed: relay_core::EventEnvelope =
            serde_json::from_value(value).map_err(|error| CliError::Internal {
                message: format!("stored event does not decode: {error}"),
            })?;
        events.push(
            relay_core::events::validate_stored_event(&parsed).map_err(core_error)?,
        );
    }
    Ok(events)
}

fn needs_user_from_pause(state: &ProjectState) -> ControllerOutcome {
    match state.paused() {
        None => ControllerOutcome::Continue {
            reason: "not paused".to_string(),
        },
        Some(paused) => ControllerOutcome::NeedsUser {
            question: paused.question.clone().unwrap_or_else(|| {
                format!("workflow is paused: {}", paused.reason)
            }),
            options: if paused.options.is_empty() {
                vec!["/resume".to_string()]
            } else {
                paused.options.clone()
            },
            reason: paused.reason.clone(),
        },
    }
}

impl DirectStoreOps {
    /// Open an existing store (writer mode). Fails with a migrate hint when
    /// only the legacy TypeScript store is present.
    pub fn open(workspace_root: &Path) -> Result<DirectStoreOps, CliError> {
        if !has_relay_state(workspace_root) {
            if has_legacy_state(workspace_root) {
                return Err(CliError::Internal {
                    message: format!(
                        "found a legacy TypeScript .relay/state.db in {}; run `relay migrate` to import it before using this build",
                        workspace_root.display()
                    ),
                });
            }
            return Err(CliError::ProjectNotFound {
                message: format!(
                    "No Relay project in {}. Run relay init.",
                    workspace_root.display()
                ),
            });
        }
        let options = StoreOptions::default();
        let (store, _report) = relay_store::RelayStore::open(workspace_root, options.clone())
            .map_err(store_error)?;
        let session_id = UuidIdGen
            .new_id("ses")
            .map_err(|error| CliError::Internal {
                message: error.to_string(),
            })?;
        let ops = DirectStoreOps {
            workspace: workspace_root.to_path_buf(),
            store: Some(store),
            project_id: None,
            session_id,
            events: Vec::new(),
            options,
        };
        Ok(ops)
    }

    /// Create a fresh store (writer mode) for `relay init` and the
    /// fresh-project `relay run` path.
    pub fn create(workspace_root: &Path) -> Result<DirectStoreOps, CliError> {
        let options = StoreOptions::default();
        let project_id = UuidIdGen
            .new_id("prj")
            .map_err(|error| CliError::Internal {
                message: error.to_string(),
            })?;
        let session_id = UuidIdGen
            .new_id("ses")
            .map_err(|error| CliError::Internal {
                message: error.to_string(),
            })?;
        let store = RelayStore::create(workspace_root, &project_id, options.clone())
            .map_err(store_error)?;
        Ok(DirectStoreOps {
            workspace: workspace_root.to_path_buf(),
            store: Some(store),
            project_id: Some(project_id),
            session_id,
            events: Vec::new(),
            options,
        })
    }

    /// The workspace this store serves.
    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    fn store(&self) -> Result<&RelayStore, CliError> {
        self.store.as_ref().ok_or_else(|| CliError::Internal {
            message: "the store handle was already dropped".to_string(),
        })
    }

    /// Replay the whole log into state.
    pub fn replay(&self) -> Result<(Vec<relay_core::RelayEvent>, ProjectState), CliError> {
        let records = self.store()?.read_all().map_err(store_error)?;
        let events = to_relay_events(&records)?;
        let state = relay_core::replay_project(&events).map_err(core_error)?;
        Ok((events, state))
    }

    /// The existing project's goal, when the store holds one (the
    /// `relay init` idempotence check).
    pub fn existing_goal(&self) -> Result<Option<String>, CliError> {
        if self.store.is_none() {
            return Ok(None);
        }
        let (_, state) = self.replay()?;
        if state.project_id().is_empty() {
            Ok(None)
        } else {
            Ok(Some(state.goal().to_string()))
        }
    }

    fn append(&self, event_type: &str, payload: serde_json::Value) -> Result<(), CliError> {
        let payload = Json::from_serde(&payload).map_err(store_error)?;
        self.store()?
            .append_batch(&[AppendInput {
                event_type: event_type.to_string(),
                payload,
                session_id: Some(self.session_id.clone()),
                timestamp: None,
                correlation_id: None,
                causation_id: None,
            }])
            .map_err(store_error)?;
        Ok(())
    }

    fn finish(
        &mut self,
        outcome: ControllerOutcome,
        state: ProjectState,
    ) -> Result<StepResult, CliError> {
        let outcome_for_event = outcome.clone();
        self.events.push(outcome_event(&outcome_for_event));
        Ok(StepResult { outcome, state })
    }

    fn require_initialized(&self) -> Result<(), CliError> {
        if self.store.is_none() {
            return Err(CliError::NotInitialized {
                detail: "this operation requires an initialized project".to_string(),
            });
        }
        Ok(())
    }

    fn emit(&mut self, event: ControllerEvent) {
        self.events.push(event);
    }
}

/// Map a controller outcome to the TUI outcome event payload.
fn outcome_event(outcome: &ControllerOutcome) -> ControllerEvent {
    match outcome {
        ControllerOutcome::Continue { reason } => ControllerEvent::Outcome {
            outcome: relay_ui::OutcomeValue::Fields {
                kind: Some("CONTINUE".to_string()),
                reason: Some(reason.clone()),
            },
        },
        ControllerOutcome::Advance { to, reason } => ControllerEvent::Outcome {
            outcome: relay_ui::OutcomeValue::Fields {
                kind: Some("ADVANCE".to_string()),
                reason: Some(format!("{reason} (to {})", to.as_str())),
            },
        },
        ControllerOutcome::NeedsUser { reason, .. } => ControllerEvent::Outcome {
            outcome: relay_ui::OutcomeValue::Fields {
                kind: Some("NEEDS_USER".to_string()),
                reason: Some(reason.clone()),
            },
        },
        ControllerOutcome::Revise { stage, reason } => ControllerEvent::Outcome {
            outcome: relay_ui::OutcomeValue::Fields {
                kind: Some("REVISE".to_string()),
                reason: Some(format!("{reason} (revise {})", stage.as_str())),
            },
        },
    }
}

impl ControllerOps for DirectStoreOps {
    fn project_id(&self) -> Option<String> {
        self.project_id.clone()
    }

    fn state(&self) -> Option<ProjectState> {
        self.replay().ok().map(|(_, state)| state)
    }

    fn status_view(&mut self) -> Result<StatusView, CliError> {
        self.require_initialized()?;
        let (events, state) = self.replay()?;
        if state.project_id().is_empty() {
            return Err(CliError::NotInitialized {
                detail: "initialize a project (or describe your goal) before reading state."
                    .to_string(),
            });
        }
        Ok(relay_ui::build_status_view(&state, &events, Some(&self.session_id)))
    }

    fn why_view(&mut self) -> Result<WhyView, CliError> {
        self.require_initialized()?;
        let (events, state) = self.replay()?;
        if state.project_id().is_empty() {
            return Err(CliError::NotInitialized {
                detail: "initialize a project (or describe your goal) before reading state."
                    .to_string(),
            });
        }
        Ok(relay_ui::build_why_view(&state, &events))
    }

    fn initialize(
        &mut self,
        goal: &str,
        autonomy: &str,
        with_session: bool,
    ) -> Result<StepResult, CliError> {
        if self.store.is_some() && self.existing_goal()?.is_some() {
            let project_id = self
                .project_id
                .clone()
                .unwrap_or_else(|| "unknown".to_string());
            return Err(CliError::ProjectAlreadyInitialized { project_id });
        }
        let trimmed = goal.trim();
        if trimmed.is_empty() {
            return Err(CliError::Internal {
                message: "initialize requires a non-empty goal describing what this project should deliver."
                    .to_string(),
            });
        }
        let effective_autonomy = if autonomy.trim().is_empty() {
            "next-checkpoint"
        } else {
            autonomy
        };
        if self.store.is_none() {
            let workspace = self.workspace.clone();
            *self = DirectStoreOps::create(&workspace)?;
        }
        self.append(
            "project.initialized",
            serde_json::json!({
                "goal": trimmed,
                "workspaceRoot": self.workspace.display().to_string(),
                "initialAutonomy": effective_autonomy,
            }),
        )?;
        if with_session {
            self.append(
                "session.started",
                serde_json::json!({
                    "sessionId": self.session_id,
                    "interactive": false,
                }),
            )?;
        }
        let (_, state) = self.replay()?;
        self.emit(ControllerEvent::Session {
            session_id: self.session_id.clone(),
        });
        self.emit(ControllerEvent::Mode {
            mode: state.current_mode().as_str().to_string(),
            previous: None,
            reason: "project initialized".to_string(),
        });
        self.finish(
            ControllerOutcome::Continue {
                reason: "project initialized".to_string(),
            },
            state,
        )
    }

    fn set_mode(&mut self, mode: WorkflowMode) -> Result<StepResult, CliError> {
        self.require_initialized()?;
        let (_, state) = self.replay()?;
        if state.project_id().is_empty() {
            return Err(CliError::NotInitialized {
                detail: "this operation requires an initialized project".to_string(),
            });
        }
        match relay_core::policies::transitions::assert_transition_allowed(&state, mode) {
            Err(rejection) => {
                self.append(
                    "mode.transition.rejected",
                    serde_json::json!({
                        "from": state.current_mode().as_str(),
                        "to": mode.as_str(),
                        "reason": bounded(&rejection.message, 2000),
                    }),
                )?;
                let (_, state) = self.replay()?;
                self.finish(
                    ControllerOutcome::NeedsUser {
                        question: rejection.message.clone(),
                        options: vec!["/status".to_string(), "/resume".to_string()],
                        reason: format!("transition to {} rejected by policy", mode.as_str()),
                    },
                    state,
                )
            }
            Ok(()) => {
                let from = state.current_mode();
                self.append(
                    "mode.entered",
                    serde_json::json!({
                        "from": from.as_str(),
                        "to": mode.as_str(),
                        "reason": "user override",
                        "source": "user",
                    }),
                )?;
                let (_, state) = self.replay()?;
                self.emit(ControllerEvent::Mode {
                    mode: mode.as_str().to_string(),
                    previous: Some(from.as_str().to_string()),
                    reason: "user override".to_string(),
                });
                self.finish(
                    ControllerOutcome::Continue {
                        reason: format!("mode set to {} by user override", mode.as_str()),
                    },
                    state,
                )
            }
        }
    }

    fn set_autonomy(&mut self, bound: AutonomyBound) -> Result<StepResult, CliError> {
        self.require_initialized()?;
        let (_, state) = self.replay()?;
        if state.project_id().is_empty() {
            return Err(CliError::NotInitialized {
                detail: "this operation requires an initialized project".to_string(),
            });
        }
        let reason = format!("autonomy set to {}", bound.as_str());
        self.append(
            "autonomy.changed",
            serde_json::json!({
                "from": state.autonomy().as_str(),
                "to": bound.as_str(),
                "source": "user",
            }),
        )?;
        let (_, state) = self.replay()?;
        self.emit(ControllerEvent::Notice {
            message: reason.clone(),
        });
        self.finish(ControllerOutcome::Continue { reason }, state)
    }

    fn pause(&mut self, reason: &str) -> Result<StepResult, CliError> {
        self.require_initialized()?;
        let (_, state) = self.replay()?;
        if state.project_id().is_empty() {
            return Err(CliError::NotInitialized {
                detail: "this operation requires an initialized project".to_string(),
            });
        }
        if let Some(paused) = state.paused() {
            let outcome = ControllerOutcome::NeedsUser {
                question: paused.question.clone().unwrap_or_else(|| {
                    format!("workflow is paused: {}", paused.reason)
                }),
                options: if paused.options.is_empty() {
                    vec!["/resume".to_string()]
                } else {
                    paused.options.clone()
                },
                reason: paused.reason.clone(),
            };
            return self.finish(outcome, state);
        }
        let trimmed = reason.trim();
        let effective = if trimmed.is_empty() {
            "requested by user"
        } else {
            trimmed
        };
        self.append(
            "workflow.paused",
            serde_json::json!({
                "by": "user",
                "reason": bounded(effective, 2000),
                "question": null,
                "options": [],
            }),
        )?;
        let (_, state) = self.replay()?;
        self.finish(
            ControllerOutcome::NeedsUser {
                question: effective.to_string(),
                options: vec!["/resume".to_string()],
                reason: "paused by user".to_string(),
            },
            state,
        )
    }

    fn resume(&mut self) -> Result<StepResult, CliError> {
        self.require_initialized()?;
        let (_, state) = self.replay()?;
        if state.project_id().is_empty() {
            return Err(CliError::NotInitialized {
                detail: "this operation requires an initialized project".to_string(),
            });
        }
        if state.paused().is_none() {
            self.emit(ControllerEvent::Notice {
                message: "the workflow is not paused".to_string(),
            });
            return self.finish(
                ControllerOutcome::Continue {
                    reason: "the workflow is not paused".to_string(),
                },
                state,
            );
        }
        self.append(
            "workflow.resumed",
            serde_json::json!({
                "by": "user",
                "note": "resumed by user",
            }),
        )?;
        let (_, state) = self.replay()?;
        self.emit(ControllerEvent::Notice {
            message: "workflow resumed".to_string(),
        });
        self.finish(
            ControllerOutcome::Continue {
                reason: "workflow resumed".to_string(),
            },
            state,
        )
    }

    fn inspect(&mut self, target: &str) -> Result<Option<ArtifactFile>, CliError> {
        self.require_initialized()?;
        let (events, state) = self.replay()?;
        if state.project_id().is_empty() {
            return Err(CliError::NotInitialized {
                detail: "this operation requires an initialized project".to_string(),
            });
        }
        let trimmed = target.trim();
        // Resolve an artifact kind to the latest stored artifact of that
        // kind, then an artifact id to its stored path, then try the target
        // as a path relative to the workspace root.
        let mut resolved = trimmed.to_string();
        if let Some(kind) = relay_core::ArtifactKind::parse(trimmed) {
            if let Some(latest) = state.artifact_id_by_kind(kind) {
                resolved = latest.to_string();
            }
        }
        let mut found_path: Option<PathBuf> = None;
        let mut artifact_id: Option<String> = None;
        if let Some(artifact) = state.artifact(&resolved) {
            if let Some(path) = artifact.path() {
                found_path = Some(self.workspace.join(path));
                artifact_id = Some(resolved.clone());
            }
        }
        if found_path.is_none() {
            let candidate = self.workspace.join(trimmed);
            if candidate.is_file() {
                // Identify the artifact by frontmatter id when present.
                if let Some((frontmatter, _)) = read_markdown(&candidate) {
                    artifact_id = frontmatter
                        .iter()
                        .find(|(key, _)| key == "id")
                        .and_then(|(_, value)| value.as_str())
                        .map(str::to_string);
                }
                found_path = Some(candidate);
            }
        }
        if found_path.is_none() {
            // A bare artifact id whose file is missing also falls through.
            if let Some(artifact) = state.artifact(trimmed) {
                if let Some(stored) = artifact.path() {
                    let candidate = self.workspace.join(stored);
                    if candidate.is_file() {
                        found_path = Some(candidate);
                        artifact_id = Some(trimmed.to_string());
                    }
                }
            }
            if found_path.is_none() {
                return Ok(None);
            }
        }
        let path = found_path.unwrap_or_else(|| PathBuf::from(trimmed));
        let Some((frontmatter, body)) = read_markdown(&path) else {
            return Ok(None);
        };
        let id = artifact_id
            .or_else(|| {
                frontmatter
                    .iter()
                    .find(|(key, _)| key == "id")
                    .and_then(|(_, value)| value.as_str())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| resolved.clone());
        self.append(
            "artifact.inspected",
            serde_json::json!({ "artifactId": id, "via": "user" }),
        )?;
        let display_path = path
            .strip_prefix(&self.workspace)
            .map(|relative| relative.display().to_string())
            .unwrap_or_else(|_| path.display().to_string());
        // Silence the unused-events warning; the replay validates the log.
        let _ = events.len();
        Ok(Some(ArtifactFile {
            frontmatter,
            body,
            path: display_path,
        }))
    }

    fn handle_input(&mut self, input: &str) -> Result<StepResult, CliError> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return self.finish_needs_user_empty();
        }
        if trimmed.starts_with('/') {
            let Some(command) = relay_core::parse_slash_command(trimmed) else {
                let preview: String = trimmed.chars().take(120).collect();
                return self.finish(
                    ControllerOutcome::NeedsUser {
                        question: format!("unknown or malformed command: {preview}"),
                        options: vec![
                            "/status".to_string(),
                            "/why".to_string(),
                            "/inspect <artifact>".to_string(),
                            "/help".to_string(),
                        ],
                        reason: "unknown command".to_string(),
                    },
                    self.replay()?.1,
                );
            };
            return match command {
                relay_core::ParsedCommand::Status => {
                    let view = self.status_view()?;
                    let paused = match &view.paused {
                        None => "running".to_string(),
                        Some(paused) => format!("paused ({})", paused.by),
                    };
                    let message = format!(
                        "status: {} ({}/{}) · {} artifacts · {} · next: {}",
                        view.mode.as_str(),
                        view.progress.stage,
                        view.progress.total,
                        view.artifacts.len(),
                        paused,
                        view.next_action
                    );
                    let (_, state) = self.replay()?;
                    self.emit(ControllerEvent::Notice { message });
                    self.finish(
                        ControllerOutcome::Continue {
                            reason: "status requested".to_string(),
                        },
                        state,
                    )
                }
                relay_core::ParsedCommand::Why => {
                    let view = self.why_view()?;
                    let last = match &view.last_outcome {
                        None => "none recorded".to_string(),
                        Some(outcome) => {
                            format!("{} — {}", outcome.outcome, outcome.reason)
                        }
                    };
                    let message = format!(
                        "why: mode {}; last outcome {last}{}",
                        view.mode.as_str(),
                        match &view.paused_reason {
                            None => String::new(),
                            Some(reason) => format!("; paused: {reason}"),
                        }
                    );
                    let (_, state) = self.replay()?;
                    self.emit(ControllerEvent::Notice { message });
                    self.finish(
                        ControllerOutcome::Continue {
                            reason: "explanation requested".to_string(),
                        },
                        state,
                    )
                }
                relay_core::ParsedCommand::Inspect { target } => {
                    let found = self.inspect(&target)?;
                    let Some(found) = found else {
                        return self.finish(
                            ControllerOutcome::NeedsUser {
                                question: format!("no artifact matched \"{target}\""),
                                options: vec!["/status".to_string()],
                                reason: "inspect found no artifact".to_string(),
                            },
                            self.replay()?.1,
                        );
                    };
                    let notice = format!(
                        "{}:\n{}{}",
                        found.path,
                        &found.body.chars().take(400).collect::<String>(),
                        if found.body.chars().count() > 400 { "…" } else { "" }
                    );
                    let (_, state) = self.replay()?;
                    self.emit(ControllerEvent::Notice { message: notice });
                    self.finish(
                        ControllerOutcome::Continue {
                            reason: format!("inspected {}", found.path),
                        },
                        state,
                    )
                }
                relay_core::ParsedCommand::Mode { mode } => self.set_mode(mode),
                relay_core::ParsedCommand::Auto { bound } => self.set_autonomy(bound),
                relay_core::ParsedCommand::Pause { reason } => self.pause(&reason),
                relay_core::ParsedCommand::Resume => self.resume(),
                relay_core::ParsedCommand::Approve { .. }
                | relay_core::ParsedCommand::Revise { .. } => Err(CliError::Internal {
                    message: format!(
                        "approval and revision flows need the controller; {RUNTIME_NOT_LINKED}"
                    ),
                }),
            };
        }
        Err(CliError::Internal {
            message: format!("conversational input needs the agent loop; {RUNTIME_NOT_LINKED}"),
        })
    }

    fn run_step(&mut self) -> Result<StepResult, CliError> {
        Err(CliError::Internal {
            message: RUNTIME_NOT_LINKED.to_string(),
        })
    }

    fn take_events(&mut self) -> Vec<ControllerEvent> {
        std::mem::take(&mut self.events)
    }

    fn session_id(&self) -> &str {
        &self.session_id
    }
}

impl DirectStoreOps {
    fn finish_needs_user_empty(&mut self) -> Result<StepResult, CliError> {
        let (_, state) = self.replay()?;
        self.finish(
            ControllerOutcome::NeedsUser {
                question: "say something — describe a goal, answer a question, or use a /command"
                    .to_string(),
                options: vec!["/status".to_string(), "/why".to_string()],
                reason: "empty input".to_string(),
            },
            state,
        )
    }
}

/// Parse `---`-delimited YAML frontmatter plus the body from a Markdown
/// artifact file. Returns `None` for files without valid frontmatter.
fn read_markdown(path: &Path) -> Option<(Vec<(String, serde_yaml::Value)>, String)> {
    let text = std::fs::read_to_string(path).ok()?;
    let after_open = text.strip_prefix("---\n")?;
    let close = after_open.find("\n---")?;
    let frontmatter_text = &after_open[..close];
    let body = after_open[close + 4..].strip_prefix('\n').unwrap_or(&after_open[close + 4..]);
    let mapping: serde_yaml::Mapping = serde_yaml::from_str(frontmatter_text).ok()?;
    let pairs: Vec<(String, serde_yaml::Value)> = mapping
        .into_iter()
        .filter_map(|(key, value)| match key {
            serde_yaml::Value::String(key) => Some((key, value)),
            _ => None,
        })
        .collect();
    Some((pairs, body.to_string()))
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn bounded_truncates_with_an_ellipsis() {
        assert_eq!(bounded("short", 10), "short");
        assert_eq!(bounded("aaaaabbbbb", 5), "aaaa…");
    }

    #[test]
    fn read_markdown_parses_frontmatter_and_body() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.md");
        std::fs::write(
            &file,
            "---\nid: art_1\nkind: prd\n---\n\n# Title\n\nBody text.\n",
        )
        .unwrap();
        let (frontmatter, body) = read_markdown(&file).unwrap();
        assert_eq!(frontmatter[0].0, "id");
        assert_eq!(frontmatter[0].1.as_str(), Some("art_1"));
        assert!(body.contains("# Title"));
        assert!(body.contains("Body text."));

        let no_frontmatter = dir.path().join("b.md");
        std::fs::write(&no_frontmatter, "# Just markdown\n").unwrap();
        assert!(read_markdown(&no_frontmatter).is_none());
    }
}
