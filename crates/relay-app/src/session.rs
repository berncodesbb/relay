//! [`ProjectSession`]: one open work-graph project.
//!
//! A session owns the typed fact store for a workspace, keeps the folded
//! [`ProjectGraph`] plus its engine [`Evaluation`] cached, and refreshes
//! that cache after every append (the app never serves a stale projection
//! after reporting a successful write). Opening a session for writing also
//! reconciles abandoned attempts through the orchestrator — never inferring
//! success — so a crashed process leaves no zombie leases behind.

use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;

use relay_domain::{DomainFact, ProjectGraph};
use relay_engine::Evaluation;
use relay_executor::spec::Clock;
use relay_executor::{AbandonedReport, Orchestrator};
use relay_store::fact::record::Integrity;
use relay_store::fact::store::FactOpenReport;
use relay_store::fact::store::FactStoreOptions;
use relay_store::{GraphFactStore, StoreError};

use crate::error::AppError;
use crate::sink::StoreFactSink;

/// Session construction options: the store options (durability, lock
/// timeout, the store's own timestamp source, fault injection) plus the
/// executor-facing clock the orchestrator runs on. The two time sources are
/// separate types by design (storage owns envelope timestamps; the runtime
/// owns attempt timing); the constructors keep them consistent.
#[derive(Clone)]
pub struct SessionOptions {
    /// Options passed straight through to the fact store.
    pub store: FactStoreOptions,
    /// The executor-facing wall-clock source.
    pub clock: Clock,
}

impl SessionOptions {
    /// Production options: real time everywhere, durable appends, default
    /// lock timeout.
    pub fn production() -> Self {
        SessionOptions {
            store: FactStoreOptions::default(),
            clock: relay_executor::system_clock(),
        }
    }

    /// Deterministic options for tests: both clocks pinned at
    /// `epoch_millis`.
    pub fn deterministic(epoch_millis: i64) -> Self {
        SessionOptions {
            store: FactStoreOptions::deterministic(epoch_millis),
            clock: Arc::new(move || epoch_millis),
        }
    }

    /// The time source the session runs on.
    pub fn clock(&self) -> Clock {
        Arc::clone(&self.clock)
    }
}

/// What opening a session found: crash-tail recoveries, projection
/// reconciliation, and abandoned attempts that were concluded (never
/// success) at open.
#[derive(Debug, Clone, Default)]
pub struct SessionOpenReport {
    /// Tail recoveries performed by the store.
    pub recovered_tails: usize,
    /// Whether the SQLite projection replayed a suffix at open.
    pub projection_replayed: bool,
    /// Attempts concluded `Abandoned` by reconciliation.
    pub abandoned: Vec<AbandonedReport>,
}

/// The cached derivation of one revision.
#[derive(Debug, Clone)]
pub struct SessionSnapshot {
    /// The folded graph.
    pub graph: ProjectGraph,
    /// The engine's evaluation of that graph.
    pub evaluation: Evaluation,
}

/// An open project session over `<workspace>/.relay-graph`.
pub struct ProjectSession {
    workspace_root: PathBuf,
    store: Arc<GraphFactStore>,
    clock: Clock,
    state: Mutex<SessionSnapshot>,
}

impl ProjectSession {
    /// Open the existing project in `workspace_root`. Fails with
    /// [`AppError::NotInitialized`] when no fact store (or no
    /// `ProjectCreated` fact) exists. Read paths never create state.
    pub fn open(workspace_root: &Path, options: SessionOptions) -> Result<Self, AppError> {
        let (store, report) = GraphFactStore::open(workspace_root, options.store.clone())
            .map_err(Self::map_open_error)?;
        let (session, abandoned) = Self::finish_open(workspace_root, store, options)?;
        let _ = Self::open_report(report, abandoned);
        Ok(session)
    }

    /// Open the project, creating an empty fact store when none exists. The
    /// caller's first appended facts must include `ProjectCreated` (see
    /// [`crate::commands::init`]); the domain fold enforces it.
    pub fn open_or_create(
        workspace_root: &Path,
        options: SessionOptions,
    ) -> Result<(Self, SessionOpenReport), AppError> {
        let (store, report) = match GraphFactStore::open(workspace_root, options.store.clone()) {
            Ok((store, report)) => (store, report),
            Err(StoreError::Manifest { .. }) => {
                let workspace_name = workspace_name(workspace_root);
                let project = crate::ids::project_id_for(&workspace_name);
                let store = GraphFactStore::create(
                    workspace_root,
                    &project,
                    Integrity::Checksums,
                    options.store.clone(),
                )?;
                (
                    store,
                    FactOpenReport {
                        recovered_tails: Vec::new(),
                        reconcile: None,
                    },
                )
            }
            Err(other) => return Err(Self::map_open_error(other)),
        };
        let (session, abandoned) = Self::finish_open(workspace_root, store, options)?;
        Ok((session, Self::open_report(report, abandoned)))
    }

    /// Assemble the open report from the store's report and the abandoned
    /// attempts concluded at open.
    fn open_report(report: FactOpenReport, abandoned: Vec<AbandonedReport>) -> SessionOpenReport {
        SessionOpenReport {
            recovered_tails: report.recovered_tails.len(),
            projection_replayed: report.reconcile.is_some_and(|outcome| {
                !matches!(
                    outcome,
                    relay_store::fact::projection::FactReconcileOutcome::Matched
                )
            }),
            abandoned,
        }
    }

    /// Shared tail of both open paths: cache the first snapshot, then — when
    /// the store carries a project — reconcile abandoned attempts so no
    /// crashed attempt stays leasable.
    fn finish_open(
        workspace_root: &Path,
        store: GraphFactStore,
        options: SessionOptions,
    ) -> Result<(Self, Vec<AbandonedReport>), AppError> {
        let store = Arc::new(store);
        let clock = options.clock();
        let session = ProjectSession {
            workspace_root: workspace_root.to_path_buf(),
            store: Arc::clone(&store),
            clock,
            state: Mutex::new(SessionSnapshot {
                graph: ProjectGraph::default(),
                evaluation: relay_engine::evaluate(&ProjectGraph::default()),
            }),
        };
        session.refresh()?;
        if session.graph().project_id().is_none() {
            // An empty or pre-init store: reads treat it as uninitialized
            // rather than as a project with nothing in it, and there is no
            // attempt to reconcile.
            return Ok((session, Vec::new()));
        }
        let abandoned = Orchestrator::new(
            Arc::new(StoreFactSink::new(Arc::clone(&store))),
            session.clock(),
        )
        .reconcile_abandoned()
        .map_err(AppError::Executor)?;
        if !abandoned.is_empty() {
            session.refresh()?;
        }
        Ok((session, abandoned))
    }

    fn map_open_error(error: StoreError) -> AppError {
        match error {
            StoreError::Manifest { .. } => AppError::NotInitialized,
            other => AppError::Store(other),
        }
    }

    /// The workspace root this session operates on.
    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    /// The underlying fact store.
    pub fn store(&self) -> Arc<GraphFactStore> {
        Arc::clone(&self.store)
    }

    /// The session's time source.
    pub fn clock(&self) -> Clock {
        Arc::clone(&self.clock)
    }

    /// The current wall-clock reading, in milliseconds.
    pub fn now_ms(&self) -> i64 {
        (self.clock)()
    }

    /// The cached graph snapshot.
    pub fn graph(&self) -> ProjectGraph {
        self.lock_state().graph.clone()
    }

    /// The cached engine evaluation.
    pub fn evaluation(&self) -> Evaluation {
        self.lock_state().evaluation.clone()
    }

    /// Whether the session's store carries an initialized project.
    pub fn is_initialized(&self) -> bool {
        self.lock_state().graph.project_id().is_some()
    }

    /// Fail with [`AppError::NotInitialized`] when the store has no project.
    pub fn require_initialized(&self) -> Result<(), AppError> {
        if self.is_initialized() {
            Ok(())
        } else {
            Err(AppError::NotInitialized)
        }
    }

    /// Re-fold the authoritative timeline and re-evaluate. Called after
    /// every append so callers never see a projection older than the last
    /// acknowledged fact.
    pub fn refresh(&self) -> Result<(), AppError> {
        let graph = self.store.project_graph()?;
        let evaluation = relay_engine::evaluate(&graph);
        *self.lock_state() = SessionSnapshot { graph, evaluation };
        Ok(())
    }

    /// Append already-validated facts and refresh the cached snapshot. The
    /// store's checked append re-validates the whole candidate history under
    /// its writer lock, so nothing reaches disk that does not fold.
    pub(crate) fn append_validated(
        &self,
        facts: Vec<DomainFact>,
    ) -> Result<relay_store::fact::domain::GraphAppendOutcome, AppError> {
        if facts.is_empty() {
            return Ok(self.store.append(&[])?);
        }
        let outcome = self.store.append(&facts)?;
        self.refresh()?;
        Ok(outcome)
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, SessionSnapshot> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
}

/// The final path component of the workspace, or `"workspace"` when the
/// root has no name (e.g. `/`).
pub fn workspace_name(root: &Path) -> String {
    root.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "workspace".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_options_carry_a_system_clock() {
        let options = SessionOptions::production();
        let clock = options.clock();
        // The system clock reads real time; a monotonic source never goes
        // backwards between two reads.
        let first = clock();
        let second = clock();
        assert!(second >= first);
    }

    #[test]
    fn deterministic_options_pin_both_clocks() {
        let options = SessionOptions::deterministic(1_791_878_400_000);
        assert_eq!((options.clock)(), 1_791_878_400_000);
        assert!(options.store.clock.now_iso().contains('T'));
    }

    #[test]
    fn workspace_names_fall_back_to_a_stable_default() {
        assert_eq!(workspace_name(Path::new("/tmp/relay-demo")), "relay-demo");
        assert_eq!(workspace_name(Path::new("/")), "workspace");
    }
}
