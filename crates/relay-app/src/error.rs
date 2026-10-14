//! Application-service errors.
//!
//! Every command handler returns [`Result<_, AppError>`]; the interface
//! crates (CLI/TUI) own how these map onto exit codes and user text. The
//! variants are user-facing categories, not internal panics: a rejected
//! proposal, an unknown id, or an unopened project are ordinary answers,
//! never crashes.

/// Why an application command failed.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// The workspace has no work-graph project yet (`relay init` first).
    #[error("no Relay project in this workspace: run `relay init --goal <text>` first")]
    NotInitialized,
    /// The workspace already carries a project.
    #[error("a Relay project already exists in this workspace ({project})")]
    AlreadyInitialized {
        /// The existing project identity.
        project: String,
    },
    /// A referenced id does not exist in the graph.
    #[error("{kind} \"{id}\" does not exist in this project")]
    UnknownTarget {
        /// What kind of thing was looked up (work, decision, attempt, …).
        kind: &'static str,
        /// The id as the user gave it.
        id: String,
    },
    /// A decision was resolved with a choice that is not one of its options.
    #[error(
        "choice \"{choice}\" is not one of the options declared by decision {decision}: {options}"
    )]
    InvalidChoice {
        /// The decision.
        decision: String,
        /// The offending choice.
        choice: String,
        /// The declared option labels, comma-joined.
        options: String,
    },
    /// Deterministic policy rejected the requested change.
    #[error("rejected: {0}")]
    Rejected(String),
    /// Effective configuration, credentials, or route resolution failed
    /// before any project state was created or changed. Usage/configuration
    /// class: the interface maps this onto its usage exit code, distinct
    /// from runtime failures.
    #[error("configuration error: {0}")]
    Configuration(String),
    /// The durable store failed (IO, corruption, lock, projection).
    #[error("store failure: {0}")]
    Store(#[source] relay_store::StoreError),
    /// Orchestration failed outright (missing executor, sink failure).
    #[error("orchestration failure: {0}")]
    Executor(#[source] relay_executor::OrchestratorError),
    /// A controlled Markdown import did not parse or validate.
    #[error("import failed: {0}")]
    Import(#[source] relay_workflows::ImportError),
    /// Reading or writing a workspace file failed.
    #[error("io failure: {0}")]
    Io(#[source] std::io::Error),
}

impl From<relay_store::StoreError> for AppError {
    fn from(error: relay_store::StoreError) -> Self {
        AppError::Store(error)
    }
}

impl From<relay_executor::OrchestratorError> for AppError {
    fn from(error: relay_executor::OrchestratorError) -> Self {
        AppError::Executor(error)
    }
}

impl From<relay_workflows::ImportError> for AppError {
    fn from(error: relay_workflows::ImportError) -> Self {
        AppError::Import(error)
    }
}

impl From<std::io::Error> for AppError {
    fn from(error: std::io::Error) -> Self {
        AppError::Io(error)
    }
}

impl AppError {
    /// The single-line user-facing message (already actionable).
    pub fn message(&self) -> String {
        self.to_string()
    }
}
