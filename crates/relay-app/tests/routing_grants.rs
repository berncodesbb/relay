//! Phase 4 integration tests: work purposes → roles → routes.
//!
//! Configuration-driven routing observed where it is authoritative — in the
//! immutable `ModelRoute` of each attempt's grant: different configured
//! connections and models for discovery, design, implementation,
//! verification, and delivery work, and the explicit `intent` route for
//! conversations, with no global mode state anywhere. Plus the actionable
//! failure for executable custom work kinds without a routing purpose, and
//! layer/override precedence through the real loader.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::ok_expect,
    clippy::err_expect
)] // test-only conveniences, house style

use std::sync::Arc;

use relay_app::RuntimeRouting;
use relay_app::RuntimeWiring;
use relay_app::commands as app;
use relay_app::routing::routing_purpose;
use relay_app::session::{ProjectSession, SessionOptions};
use relay_domain::{
    AttemptOutcome, AttemptReceipt, CompletionRule, DomainFact, ExecutorClass, ScopePath, Usage,
    WorkItem, WorkItemId, WorkKind,
};
use relay_executor::spec::{AttemptSpec, RunContext};
use relay_executor::{
    Executor, ExecutorSet, ModelClient, ModelClientError, ModelTurnRequest, ModelTurnResponse,
};
use relay_providers::contract::CancelToken;

const EPOCH_MS: i64 = 1_791_878_400_000;

/// One connection per routing purpose, so a grant's provider names exactly
/// the purpose its work resolved through.
const PURPOSE_CONFIG: &str = "\
version: 1
budgets: {}
providers:
  fake: { adapter: fake }
  aux-reason: { adapter: fake }
  aux-architect: { adapter: fake }
  aux-coding: { adapter: fake }
  aux-review: { adapter: fake }
  aux-fast: { adapter: fake }
  aux-intent: { adapter: fake }
models:
  reasoning: { provider: aux-reason, model: m-reason }
  architect: { provider: aux-architect, model: m-architect }
  coding: { provider: aux-coding, model: m-coding }
  review: { provider: aux-review, model: m-review }
  fast: { provider: aux-fast, model: m-fast }
routing:
  intent: { provider: aux-intent, model: m-intent }
  discovery: reasoning
  specification: reasoning
  technical-design: architect
  planning: reasoning
  implementation: coding
  verification: review
  delivery: fast
";

fn options() -> SessionOptions {
    SessionOptions::deterministic(EPOCH_MS)
}

/// A model executor that completes whatever it is given with no outputs —
/// the attempts exist for their grants, not their work.
struct CompleteAnyExecutor;

impl Executor for CompleteAnyExecutor {
    fn class(&self) -> ExecutorClass {
        ExecutorClass::Model
    }

    fn run(&self, spec: &AttemptSpec, context: &RunContext) -> AttemptReceipt {
        AttemptReceipt {
            attempt: spec.attempt.clone(),
            work: spec.work.clone(),
            outcome: AttemptOutcome::Completed,
            completed_at_ms: context.now_ms,
            outputs: Vec::new(),
            evidence: Vec::new(),
            graph_changes: Vec::new(),
            usage: Usage::default(),
        }
    }
}

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

/// An offline echo model client whose response carries the granted route as
/// its attempted route (the production client's contract).
fn wiring_for(plan: std::sync::Arc<relay_executor::ResolvedRoutePlan>) -> RuntimeWiring {
    RuntimeWiring::scripted_with_routes(
        ExecutorSet::new().with(Arc::new(CompleteAnyExecutor)),
        Arc::new(EchoModel),
        plan,
    )
}

/// Declare one ready executable work item of a built-in kind, directly
/// through the store (the catalog's kinds, the kernel's receipt rule).
fn declare_ready_work(session: &ProjectSession, id: &str, kind: &str) {
    let mut work = WorkItem::new(
        WorkItemId::parse(id).unwrap(),
        WorkKind::parse(kind).unwrap(),
        ExecutorClass::Model,
        "exercise purpose routing",
        CompletionRule::Receipt,
    );
    work.read_scopes = vec![ScopePath::root()];
    session
        .store()
        .append(&[DomainFact::WorkDeclared { work }])
        .unwrap();
    session.refresh().unwrap();
}

/// A workspace carrying [`PURPOSE_CONFIG`] and an initialized project.
fn configured_workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("relay.config.yaml"), PURPOSE_CONFIG).unwrap();
    app::init(dir.path(), "export team reports as CSV", options()).unwrap();
    dir
}

/// Every started attempt's grant carries the configured connection, model,
/// and role of its work's routing purpose — one distinct connection per
/// purpose, no global mode state anywhere.
#[test]
fn grants_carry_the_configured_route_of_each_works_purpose() {
    let dir = configured_workspace();
    let session = ProjectSession::open(dir.path(), options()).unwrap();
    let cases = [
        ("work_discover-1", "discover-opportunity"),
        ("work_design-1", "design-approach"),
        ("work_implement-1", "implement-task"),
        ("work_verify-1", "verify-criteria"),
        ("work_deliver-1", "prepare-delivery-evidence"),
    ];
    for (id, kind) in cases {
        declare_ready_work(&session, id, kind);
    }

    let routing = RuntimeRouting::resolve(dir.path(), None).unwrap();
    let wiring = wiring_for(std::sync::Arc::clone(&routing.plan));
    let run = relay_app::runner::run_ticks(
        &session,
        &wiring,
        relay_engine::SchedulingPolicy::default(),
        1,
    )
    .unwrap();
    assert!(
        run.ticks.iter().any(|tick| !tick.started.is_empty()),
        "attempts started: {run:?}"
    );

    // Every grant matches the configured primary of its work's purpose.
    let mut observed: Vec<(String, String, String)> = Vec::new();
    for fact in session.store().read_facts().unwrap() {
        let DomainFact::AttemptStarted { work, grant, .. } = fact else {
            continue;
        };
        let kind = session
            .graph()
            .work(&work)
            .unwrap()
            .work
            .kind
            .as_str()
            .to_string();
        let purpose = routing_purpose(&kind).unwrap_or_else(|| panic!("{kind} maps"));
        let planned = routing.plan.primary(purpose).unwrap();
        assert_eq!(
            grant.model_route.as_ref(),
            Some(&planned.route),
            "work {work} (kind {kind}, purpose {purpose})"
        );
        observed.push((
            planned.route.provider.clone(),
            planned.route.model.clone(),
            planned.route.role.clone().unwrap_or_default(),
        ));
    }
    // Five distinct purposes ran, each on its own connection/model/role.
    observed.sort();
    observed.dedup();
    assert_eq!(
        observed.len(),
        5,
        "one distinct route per purpose: {observed:?}"
    );
    let providers: std::collections::BTreeSet<String> = observed
        .iter()
        .map(|(provider, _, _)| provider.clone())
        .collect();
    assert_eq!(providers.len(), 5, "{providers:?}");

    // And the conversation's explicit intent route is its own, again
    // different from every work purpose.
    let intent = RuntimeRouting::resolve(dir.path(), None)
        .unwrap()
        .turn_routing()
        .unwrap();
    assert_eq!(intent.route.provider, "aux-intent");
    assert_eq!(intent.route.model, "m-intent");
    assert!(
        !observed
            .iter()
            .any(|(provider, model, _)| *provider == "aux-intent" && *model == "m-intent"),
        "intent is not a work purpose's route: {observed:?}"
    );
}

/// A custom executable kind with no routing purpose fails before any
/// orchestrator tick — no state mutation, an actionable diagnostic naming
/// the work and its kind.
#[test]
fn a_custom_executable_kind_fails_before_any_state_mutation() {
    let dir = tempfile::tempdir().unwrap();
    app::init(dir.path(), "ship a tiny tool", options()).unwrap();
    let session = ProjectSession::open(dir.path(), options()).unwrap();
    declare_ready_work(&session, "work_experiment-1", "experiment-maybe");
    let revision = session.graph().revision().get();

    let routing = RuntimeRouting::resolve(dir.path(), None).unwrap();
    let wiring = wiring_for(std::sync::Arc::clone(&routing.plan));
    let error = relay_app::runner::run_ticks(
        &session,
        &wiring,
        relay_engine::SchedulingPolicy::default(),
        4,
    )
    .unwrap_err();
    assert!(
        matches!(error, relay_app::AppError::Configuration(_)),
        "{error}"
    );
    let message = error.message();
    assert!(message.contains("work_experiment-1"), "{message}");
    assert!(message.contains("experiment-maybe"), "{message}");
    assert!(message.contains("routing purpose"), "{message}");
    // Nothing mutated: no attempt started, no revision change.
    assert_eq!(session.graph().revision().get(), revision);
    assert!(
        !session
            .store()
            .read_facts()
            .unwrap()
            .into_iter()
            .any(|fact| matches!(fact, DomainFact::AttemptStarted { .. })),
        "no attempt was started"
    );
}

/// Human (decision-completed) work never asks for a route: the pre-check
/// skips it and its grant carries none.
#[test]
fn human_work_needs_no_route_and_gets_none() {
    let dir = configured_workspace();
    let session = ProjectSession::open(dir.path(), options()).unwrap();
    // A decision-gated work item: the human kind's completion rule.
    let decision = session
        .graph()
        .decisions()
        .next()
        .map(|(id, _)| id.clone())
        .unwrap();
    let mut work = WorkItem::new(
        WorkItemId::parse("work_approve-1").unwrap(),
        WorkKind::parse("approve-artifact").unwrap(),
        ExecutorClass::Human,
        "a human approves",
        CompletionRule::Decision { decision },
    );
    work.read_scopes = vec![ScopePath::root()];
    session
        .store()
        .append(&[DomainFact::WorkDeclared { work }])
        .unwrap();
    session.refresh().unwrap();

    // The run passes the pre-check (human work is skipped) and ticks with
    // nothing executable scheduled.
    let routing = RuntimeRouting::resolve(dir.path(), None).unwrap();
    let wiring = wiring_for(std::sync::Arc::clone(&routing.plan));
    let run = relay_app::runner::run_ticks(
        &session,
        &wiring,
        relay_engine::SchedulingPolicy::default(),
        1,
    )
    .unwrap();
    assert!(
        run.ticks.iter().all(|tick| tick.started.is_empty()),
        "the engine never schedules human work: {run:?}"
    );
}

/// Layer precedence through the real loader: the project layer wins over
/// the user layer for the roles it defines, the user layer survives for
/// the keys the project layer leaves alone, and the provider override
/// forces every route onto one connection above both.
#[test]
fn layer_and_override_precedence_hold_end_to_end() {
    let workspace = tempfile::tempdir().unwrap();
    std::fs::write(
        workspace.path().join("relay.config.yaml"),
        "\
version: 1
budgets: {}
models:
  reasoning: { provider: fake, model: project-reason }
routing:
  discovery: reasoning
  specification: reasoning
  technical-design: architect
  planning: reasoning
  implementation: coding
  verification: review
  delivery: fast
",
    )
    .unwrap();
    let user_dir = tempfile::tempdir().unwrap();
    let user_path = user_dir.path().join("user.yaml");
    std::fs::write(
        &user_path,
        "\
version: 1
budgets: {}
models:
  reasoning: { provider: fake, model: user-reason }
  review: { provider: fake, model: user-review }
routing:
  discovery: reasoning
  specification: reasoning
  technical-design: architect
  planning: reasoning
  implementation: coding
  verification: review
  delivery: fast
",
    )
    .unwrap();

    // No override: the project layer's reasoning wins; the user layer's
    // review survives untouched.
    let routing =
        RuntimeRouting::resolve_with_user_config(workspace.path(), Some(&user_path), None).unwrap();
    assert_eq!(
        routing.plan.primary("discovery").unwrap().route.model,
        "project-reason"
    );
    assert_eq!(
        routing.plan.primary("verification").unwrap().route.model,
        "user-review"
    );

    // The provider override sits above every layer: every purpose is forced
    // onto the named connection, models untouched.
    let routing =
        RuntimeRouting::resolve_with_user_config(workspace.path(), Some(&user_path), Some("fake"))
            .unwrap();
    for purpose in routing.plan.purposes() {
        let planned = routing.plan.primary(purpose).unwrap();
        assert_eq!(planned.route.provider, "fake", "{purpose}");
    }
    assert_eq!(
        routing.plan.primary("discovery").unwrap().route.model,
        "project-reason",
        "the override changes the connection, never the model"
    );
}
