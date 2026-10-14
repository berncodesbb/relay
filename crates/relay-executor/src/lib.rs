//! Executor contracts and attempt orchestration for Relay.
//!
//! Executors perform work and return receipts and proposals; they never
//! advance the graph directly. This crate owns:
//!
//! - the [`Executor`] trait and the five executor classes (human, model,
//!   workspace, verifier, system) over the frozen `relay-domain` contracts;
//! - [`AttemptSpec`] / [`RunContext`]: the immutable per-attempt inputs and
//!   the runtime handle (cancellation, model client, tools, budget) an
//!   executor receives;
//! - the [`FactSink`] port: fact append (under the storage writer lock,
//!   per-append) and graph reads, so the orchestrator has no direct store
//!   dependency — the app implements it over `relay-store` in Phase 6, and
//!   [`InMemoryFactSink`] serves deterministic tests;
//! - the [`Orchestrator`]: schedules from `relay_engine::recommend`, appends
//!   `AttemptStarted` (the durable lease) before running anything, never
//!   holds the writer lock across executor work, validates receipts through
//!   `relay_engine::accept_receipt` before appending acceptance facts, and
//!   reconciles abandoned attempts at open;
//! - [`BudgetGuard`]: token/attempt/duration ceilings whose exhaustion fires
//!   the attempt's [`CancelToken`](relay_providers::contract::CancelToken),
//!   which threads into tool execution and the process supervisor;
//! - the [`ModelClient`] port over `relay-providers`' neutral contract, so
//!   no provider type appears in any executor signature. Its production
//!   implementation is [`RoutedModelClient`]: named provider connections
//!   built once from effective configuration, purpose/role route plans
//!   resolved after invocation overrides, and deterministic ordered
//!   fallback for provider availability failures only.
//!
//! Capability enforcement is layered: the domain fold validates grants
//! against work ceilings, the engine refuses overlapping write grants in
//! scheduling, the orchestrator's `AttemptStarted` is the durable lease,
//! and [`relay_tools::GrantGate`] checks every concrete tool call against
//! the lease before the tool body runs.

pub mod budget;
pub mod executors;
pub mod model;
pub mod orchestrator;
pub mod planner;
pub mod routed_model;
pub mod sink;
pub mod spec;

pub use budget::{BudgetExhausted, BudgetGuard};
pub use executors::{
    Executor, ExecutorSet, HumanExecutor, MODEL_REMEDIATION_TURNS, ModelExecutor, SystemExecutor,
    SystemJob, SystemOutcome, VerifierExecutor, VerifierPlanner, WorkspaceExecutor,
    WorkspacePlanner,
};
pub use model::{
    ModelCallFailure, ModelClient, ModelClientError, ModelTurnRequest, ModelTurnResponse,
    ProviderModelClient, RouteAttemptFailure,
};
pub use orchestrator::{
    AbandonedReport, GrantSource, Orchestrator, OrchestratorError, Refusal, TickReport,
    grant_from_work, overlapping_write_conflict,
};
pub use planner::{
    CallRecord, ModelGuidedConfig, ModelGuidedVerifierPlanner, ModelGuidedWorkspacePlanner,
    ToolCallRequest, ToolRunner, ToolSession,
};
pub use routed_model::{
    ConnectionState, PlannedRoute, ProviderRegistry, ResolvedRoutePlan, RoutedModelClient,
};
pub use sink::{FactSink, FactSinkError, InMemoryFactSink};
pub use spec::{
    AttemptSpec, Clock, GraphView, IdSource, JoinHandle, ManualClock, RunContext, Spawn,
    inline_spawn, system_clock, thread_spawn,
};
