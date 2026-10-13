//! Project state: the deterministic projection of the event log produced by
//! the pure reducer. Ported from `src/core/project-state.ts`. State is only
//! constructible through the reducer (`create_empty_project_state`,
//! `apply_event`, `replay_project`); the field layout serializes to exactly
//! the TypeScript JSON shape (camelCase) so normalized states compare
//! byte-for-byte.

use crate::artifacts::ArtifactKind;
use crate::modes::{AutonomyBound, WorkflowMode};
use crate::outcomes::OutcomeKind;
use crate::payloads::PauseActor;
use serde::Serialize;
use std::collections::BTreeMap;

/// Artifact lifecycle status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ArtifactStatus {
    /// Created but not yet approved.
    Draft,
    /// Explicitly approved by the user at the current version.
    Approved,
    /// Invalidated by an upstream revision; regenerate before relying on it.
    Stale,
}

impl ArtifactStatus {
    /// The wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            ArtifactStatus::Draft => "draft",
            ArtifactStatus::Approved => "approved",
            ArtifactStatus::Stale => "stale",
        }
    }
}

/// One recorded artifact revision.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RevisionRecord {
    /// The version produced by this revision.
    pub version: i64,
    /// When the revision happened (event timestamp).
    pub at: String,
    /// Why the artifact was revised.
    pub reason: String,
}

/// State of one artifact.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactState {
    /// The artifact's id.
    pub(crate) artifact_id: String,
    /// The artifact's kind.
    pub(crate) kind: ArtifactKind,
    /// The artifact's title.
    pub(crate) title: String,
    /// Current version (1 at creation, +1 per revision).
    pub(crate) current_version: i64,
    /// Lifecycle status.
    pub(crate) status: ArtifactStatus,
    /// Path relative to the workspace root, when rendered.
    pub(crate) path: Option<String>,
    /// Creation timestamp.
    pub(crate) created_at: String,
    /// Last change timestamp.
    pub(crate) last_changed_at: String,
    /// Approval timestamp of the current version, if approved.
    pub(crate) approved_at: Option<String>,
    /// Revision history (empty at creation).
    pub(crate) revisions: Vec<RevisionRecord>,
}

impl ArtifactState {
    /// The artifact's id.
    pub fn artifact_id(&self) -> &str {
        &self.artifact_id
    }

    /// The artifact's kind.
    pub fn kind(&self) -> ArtifactKind {
        self.kind
    }

    /// The artifact's title.
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Current version.
    pub fn current_version(&self) -> i64 {
        self.current_version
    }

    /// Lifecycle status.
    pub fn status(&self) -> ArtifactStatus {
        self.status
    }

    /// Path relative to the workspace root, when rendered.
    pub fn path(&self) -> Option<&str> {
        self.path.as_deref()
    }

    /// Revision history.
    pub fn revisions(&self) -> &[RevisionRecord] {
        &self.revisions
    }

    /// Approval timestamp of the current version, if approved.
    pub fn approved_at(&self) -> Option<&str> {
        self.approved_at.as_deref()
    }
}

/// One mode transition record.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransitionRecord {
    /// When the transition happened (event timestamp).
    pub at: String,
    /// The mode left (`None` for the first entry).
    pub from: Option<WorkflowMode>,
    /// The mode entered.
    pub to: WorkflowMode,
    /// Why the transition happened.
    pub reason: String,
    /// Whether the transition was automatic or user-requested.
    pub source: TransitionSource,
}

/// The `source` of a recorded transition (`auto` or `user`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TransitionSource {
    /// Automatic transition.
    Auto,
    /// User-requested transition.
    User,
}

impl TransitionSource {
    /// The wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            TransitionSource::Auto => "auto",
            TransitionSource::User => "user",
        }
    }
}

/// The current pause, when the workflow is paused.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PauseState {
    /// Who paused the workflow.
    pub by: PauseActor,
    /// Why the workflow is paused.
    pub reason: String,
    /// A focused question to answer, when the pause asks one.
    pub question: Option<String>,
    /// Options offered to the user.
    pub options: Vec<String>,
    /// When the pause happened (event timestamp).
    pub at: String,
}

/// The latest recorded transition outcome.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutcomeRecord {
    /// When the outcome was recorded (event timestamp).
    pub at: String,
    /// The outcome kind.
    pub outcome: OutcomeKind,
    /// Target mode for `ADVANCE`.
    pub to: Option<WorkflowMode>,
    /// Stage to route back to for `REVISE`.
    pub revise_stage: Option<WorkflowMode>,
    /// Why the outcome was chosen.
    pub reason: String,
    /// Supporting evidence entries.
    pub evidence: Vec<String>,
}

/// Verification roll-up.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationSummary {
    /// Whether the last run passed (`None` before any run).
    pub passed: Option<bool>,
    /// When the last run happened.
    pub last_run_at: Option<String>,
    /// Criteria that passed.
    pub passed_count: i64,
    /// Criteria that failed.
    pub failed_count: i64,
    /// Criteria not evaluated.
    pub unevaluated_count: i64,
}

/// Run statistics folded from the log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StateStats {
    /// Events folded.
    pub events: i64,
    /// Model calls that completed.
    pub model_calls: i64,
    /// Tool executions.
    pub tool_calls: i64,
    /// `kind: message` of the last model failure.
    pub last_model_error: Option<String>,
}

/// Artifacts keyed by id, preserving insertion (creation) order the way the
/// TypeScript `Record` does; serialization writes the map in insertion order.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ArtifactMap {
    order: Vec<String>,
    map: BTreeMap<String, ArtifactState>,
}

impl ArtifactMap {
    /// Look up an artifact by id.
    pub fn get(&self, artifact_id: &str) -> Option<&ArtifactState> {
        self.map.get(artifact_id)
    }

    /// Whether an artifact with this id exists.
    pub fn contains(&self, artifact_id: &str) -> bool {
        self.map.contains_key(artifact_id)
    }

    /// Insert or replace an artifact, recording insertion order for new ids.
    pub(crate) fn insert(&mut self, artifact_id: String, artifact: ArtifactState) {
        if !self.map.contains_key(&artifact_id) {
            self.order.push(artifact_id.clone());
        }
        self.map.insert(artifact_id, artifact);
    }

    /// Iterate artifacts in insertion (creation) order.
    pub(crate) fn iter(&self) -> impl Iterator<Item = (&String, &ArtifactState)> {
        self.order
            .iter()
            .filter_map(|id| self.map.get(id).map(|artifact| (id, artifact)))
    }
}

impl Serialize for ArtifactMap {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(self.order.len()))?;
        for (id, artifact) in self.iter() {
            map.serialize_entry(id, artifact)?;
        }
        map.end()
    }
}

/// The deterministic projection of one project's event log. Only the reducer
/// constructs non-empty states; read fields through the accessors or
/// `serde_json::to_value`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectState {
    pub(crate) project_id: String,
    pub(crate) goal: String,
    pub(crate) workspace_root: Option<String>,
    pub(crate) initialized_at: String,
    pub(crate) current_mode: WorkflowMode,
    pub(crate) autonomy: AutonomyBound,
    pub(crate) paused: Option<PauseState>,
    pub(crate) artifacts: ArtifactMap,
    /// Latest artifact id per kind.
    pub(crate) artifacts_by_kind: BTreeMap<ArtifactKind, String>,
    pub(crate) transitions: Vec<TransitionRecord>,
    pub(crate) rejected_transitions: Vec<RejectedTransition>,
    pub(crate) last_outcome: Option<OutcomeRecord>,
    pub(crate) stats: StateStats,
    pub(crate) stale_artifacts: Vec<String>,
    pub(crate) verification: VerificationSummary,
    pub(crate) last_activity_at: String,
}

/// A policy-rejected mode transition record.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RejectedTransition {
    /// When the rejection happened (event timestamp).
    pub at: String,
    /// The mode the transition was attempted from.
    pub from: WorkflowMode,
    /// The mode the transition targeted.
    pub to: WorkflowMode,
    /// Why policy rejected the transition.
    pub reason: String,
}

/// The empty state every replay starts from.
pub fn create_empty_project_state() -> ProjectState {
    ProjectState {
        project_id: String::new(),
        goal: String::new(),
        workspace_root: None,
        initialized_at: String::new(),
        current_mode: WorkflowMode::Discovery,
        autonomy: AutonomyBound::NextCheckpoint,
        paused: None,
        artifacts: ArtifactMap::default(),
        artifacts_by_kind: BTreeMap::new(),
        transitions: Vec::new(),
        rejected_transitions: Vec::new(),
        last_outcome: None,
        stats: StateStats {
            events: 0,
            model_calls: 0,
            tool_calls: 0,
            last_model_error: None,
        },
        stale_artifacts: Vec::new(),
        verification: VerificationSummary {
            passed: None,
            last_run_at: None,
            passed_count: 0,
            failed_count: 0,
            unevaluated_count: 0,
        },
        last_activity_at: String::new(),
    }
}

impl ProjectState {
    /// The project id (empty before initialization).
    pub fn project_id(&self) -> &str {
        &self.project_id
    }

    /// The product goal.
    pub fn goal(&self) -> &str {
        &self.goal
    }

    /// The workspace root, once initialized.
    pub fn workspace_root(&self) -> Option<&str> {
        self.workspace_root.as_deref()
    }

    /// Initialization timestamp.
    pub fn initialized_at(&self) -> &str {
        &self.initialized_at
    }

    /// The current workflow mode.
    pub fn current_mode(&self) -> WorkflowMode {
        self.current_mode
    }

    /// The current autonomy bound.
    pub fn autonomy(&self) -> AutonomyBound {
        self.autonomy
    }

    /// The current pause, when paused.
    pub fn paused(&self) -> Option<&PauseState> {
        self.paused.as_ref()
    }

    /// All artifacts, keyed by id.
    pub fn artifact(&self, artifact_id: &str) -> Option<&ArtifactState> {
        self.artifacts.get(artifact_id)
    }

    /// Latest artifact id for a kind.
    pub fn artifact_id_by_kind(&self, kind: ArtifactKind) -> Option<&str> {
        self.artifacts_by_kind.get(&kind).map(String::as_str)
    }

    /// Recorded transitions, oldest first.
    pub fn transitions(&self) -> &[TransitionRecord] {
        &self.transitions
    }

    /// Recorded rejected transitions (capped at 50).
    pub fn rejected_transitions(&self) -> &[RejectedTransition] {
        &self.rejected_transitions
    }

    /// The latest recorded outcome, if any.
    pub fn last_outcome(&self) -> Option<&OutcomeRecord> {
        self.last_outcome.as_ref()
    }

    /// Folded run statistics.
    pub fn stats(&self) -> &StateStats {
        &self.stats
    }

    /// Ids of stale artifacts, in the order they went stale.
    pub fn stale_artifacts(&self) -> &[String] {
        &self.stale_artifacts
    }

    /// The verification roll-up.
    pub fn verification(&self) -> &VerificationSummary {
        &self.verification
    }

    /// Timestamp of the last folded event.
    pub fn last_activity_at(&self) -> &str {
        &self.last_activity_at
    }
}
