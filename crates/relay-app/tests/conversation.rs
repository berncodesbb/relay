//! Conversational application-service integration tests: the full turn flow
//! (durable user message, bounded context, provider streaming, typed
//! proposal application, terminal facts), chat views, redaction, bounded
//! context, cancellation, provider failure, and the no-writer-lock-across-
//! the-model-call proof. All offline and deterministic.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::ok_expect,
    clippy::err_expect
)] // test-only conveniences, house style

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::mpsc;

use relay_app::conversation::{
    CONVERSATION_PURPOSE, ConversationEvent, ConversationLimits, ConversationProposal,
    ProposalOutcome, TurnStatus, start_turn,
};
use relay_app::conversation_views::{ChatEntryKind, chat_view};
use relay_app::session::{ProjectSession, SessionOptions};
use relay_domain::DomainFact;
use relay_providers::contract::{
    CancelToken, FinishReason, ModelProvider, ModelRequest, ProviderErrorKind, ProviderEventStream,
    ProviderName, ProviderStreamEvent, TokenUsage,
};
use relay_providers::fake::{
    FakeErrorPlan, FakeMatch, FakeProvider, FakeResponse, FakeStep, PurposeMatch,
};
use serde_json::json;

const EPOCH_MS: i64 = 1_791_878_400_000;

fn options() -> SessionOptions {
    SessionOptions::deterministic(EPOCH_MS)
}

fn limits() -> ConversationLimits {
    ConversationLimits::default()
}

/// A provider serving one scripted conversational turn (no proposals by
/// default). Unmatched calls echo the prompt, so a missing script fails
/// loudly in the assertions below.
fn turn_provider(text: &str, proposals: serde_json::Value) -> Arc<dyn ModelProvider> {
    Arc::new(FakeProvider::new(vec![FakeStep {
        r#match: FakeMatch {
            purpose: Some(PurposeMatch::Exact(CONVERSATION_PURPOSE.to_string())),
            ..FakeMatch::default()
        },
        response: FakeResponse {
            text: Some(text.to_string()),
            structured_output: Some(proposals),
            ..FakeResponse::default()
        },
    }]))
}

fn answer_only(text: &str) -> Arc<dyn ModelProvider> {
    turn_provider(text, json!({ "proposals": [] }))
}

/// Run one turn synchronously and collect its events (the sender is dropped
/// when the turn returns, ending the iterator).
fn run_turn(
    session: &ProjectSession,
    provider: Arc<dyn ModelProvider>,
    prompt: &str,
) -> (
    relay_app::conversation::ConversationTurnResult,
    Vec<ConversationEvent>,
) {
    let (sender, receiver) = mpsc::channel();
    let result = start_turn(
        session,
        provider,
        "fake-standard",
        prompt,
        &limits(),
        CancelToken::new(),
        sender,
    )
    .expect("the turn service call succeeds");
    let events: Vec<ConversationEvent> = receiver.iter().collect();
    (result, events)
}

fn facts(session: &ProjectSession) -> Vec<DomainFact> {
    session.store().read_facts().expect("facts read")
}

fn assistant_fact(session: &ProjectSession) -> DomainFact {
    facts(session)
        .into_iter()
        .find(|fact| matches!(fact, DomainFact::AssistantMessageRecorded { .. }))
        .expect("one assistant message fact")
}

// ---------------------------------------------------------------------------
// Fresh workspaces and informational turns
// ---------------------------------------------------------------------------

#[test]
fn a_read_open_of_a_fresh_workspace_creates_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let error = ProjectSession::open(dir.path(), options())
        .err()
        .expect("not initialized");
    assert!(
        matches!(error, relay_app::AppError::NotInitialized),
        "{error}"
    );
    // The read path never creates durable state: no fact log, no manifest,
    // no projection — not a single file. (A bare `locks/` scaffolding
    // directory may appear; that is storage-owner behavior, reported to the
    // lead, and the CLI's lazy-open contract keeps even that from appearing
    // before a first prompt.)
    let root = dir.path().join(".relay-graph");
    if root.exists() {
        fn walk(path: &std::path::Path, created: &mut Vec<String>) {
            if let Ok(entries) = std::fs::read_dir(path) {
                for entry in entries.flatten() {
                    if entry.path().is_dir() {
                        walk(&entry.path(), created);
                    } else {
                        created.push(entry.path().display().to_string());
                    }
                }
            }
        }
        let mut created: Vec<String> = Vec::new();
        walk(&root, &mut created);
        assert!(
            created.is_empty(),
            "the read open created durable state: {created:?}"
        );
    }
}

#[test]
fn an_informational_first_turn_never_declares_a_goal() {
    let dir = tempfile::tempdir().unwrap();
    let (session, _) = ProjectSession::open_or_create(dir.path(), options()).unwrap();
    let (result, events) = run_turn(
        &session,
        answer_only("This repo is a Rust CLI."),
        "explain this repository",
    );

    assert_eq!(result.status, TurnStatus::Completed);
    assert_eq!(result.assistant_text, "This repo is a Rust CLI.");
    assert!(result.proposals.is_empty(), "{:?}", result.proposals);
    assert_eq!(result.conversation.as_str(), "cnv_1");
    assert_eq!(result.turn.as_str(), "turn_1-1");
    assert!(result.usage.is_some());

    // Facts: project + conversation + user + assistant — and no goal, no
    // work, no decisions.
    assert!(session.graph().goal().is_none(), "no goal invented");
    assert_eq!(session.graph().works().count(), 0);
    assert_eq!(session.graph().decisions().count(), 0);
    let timeline = facts(&session);
    assert_eq!(timeline.len(), 4, "{timeline:?}");
    assert!(matches!(timeline[0], DomainFact::ProjectCreated { .. }));
    assert!(matches!(
        timeline[1],
        DomainFact::ConversationStarted { .. }
    ));
    assert!(matches!(
        timeline[2],
        DomainFact::UserMessageRecorded { .. }
    ));
    assert!(matches!(
        timeline[3],
        DomainFact::AssistantMessageRecorded { .. }
    ));

    // Events: started, deltas, finished — in that order.
    assert!(matches!(
        events.first(),
        Some(ConversationEvent::TurnStarted { .. })
    ));
    assert!(matches!(
        events.last(),
        Some(ConversationEvent::TurnFinished { .. })
    ));
    assert!(
        events
            .iter()
            .any(|event| matches!(event, ConversationEvent::AssistantDelta { .. }))
    );

    // A second turn continues the same conversation and turn numbering.
    let (second, _) = run_turn(&session, answer_only("It has 8 crates."), "and the crates?");
    assert_eq!(second.conversation.as_str(), "cnv_1");
    assert_eq!(second.turn.as_str(), "turn_1-2");

    // The chat view shows the user/assistant pairs and no goal.
    let view = chat_view(&session, 50);
    assert_eq!(
        view.conversation.as_ref().map(|id| id.as_str()),
        Some("cnv_1")
    );
    assert_eq!(view.goal, None);
    assert_eq!(view.interrupted_turns, 0);
    let kinds: Vec<ChatEntryKind> = view.entries.iter().map(|entry| entry.kind).collect();
    assert_eq!(
        kinds,
        vec![
            ChatEntryKind::User,
            ChatEntryKind::Assistant,
            ChatEntryKind::User,
            ChatEntryKind::Assistant
        ]
    );
}

// ---------------------------------------------------------------------------
// Typed proposals
// ---------------------------------------------------------------------------

#[test]
fn a_declare_goal_proposal_creates_the_goal_and_expansion() {
    let dir = tempfile::tempdir().unwrap();
    let (session, _) = ProjectSession::open_or_create(dir.path(), options()).unwrap();
    let provider = turn_provider(
        "I will set up the goal and first work.",
        json!({
            "proposals": [
                { "type": "declare-goal", "outcome": "export team reports as CSV",
                  "success_definition": "the export command ships" }
            ]
        }),
    );
    let (result, events) = run_turn(&session, provider, "fix the failing tests");

    assert_eq!(result.status, TurnStatus::Completed);
    assert_eq!(result.proposals.len(), 1);
    assert!(result.proposals[0].accepted, "{:?}", result.proposals[0]);
    assert!(matches!(
        result.proposals[0].proposal,
        ConversationProposal::DeclareGoal { .. }
    ));

    // The goal exists with the proposed outcome, and the same first
    // expansion round `init` applies declared work and a decision.
    let graph = session.graph();
    let goal = graph.goal().expect("goal declared");
    assert_eq!(goal.outcome, "export team reports as CSV");
    assert_eq!(goal.success_definition, "the export command ships");
    assert!(graph.works().count() >= 1, "expansion declared work");
    assert!(
        graph.decisions().count() >= 1,
        "expansion opened a decision"
    );
    assert!(
        session.graph().works().count() >= 1,
        "expansion declared work"
    );
    assert!(
        session.graph().decisions().count() >= 1,
        "expansion opened a decision"
    );

    // The assistant message annotates the ids of what was actually accepted.
    match assistant_fact(&session) {
        DomainFact::AssistantMessageRecorded {
            related_work,
            related_decisions,
            ..
        } => {
            assert!(!related_work.is_empty(), "{related_work:?}");
            assert!(!related_decisions.is_empty(), "{related_decisions:?}");
        }
        other => panic!("expected an assistant fact, got {other:?}"),
    }

    // The decision was surfaced as a ProposalDecided event, and the chat
    // view now shows the goal plus an open-decision entry.
    assert!(events.iter().any(|event| matches!(
        event,
        ConversationEvent::ProposalDecided { accepted: true, .. }
    )));
    let view = chat_view(&session, 50);
    assert_eq!(view.goal.as_deref(), Some("export team reports as CSV"));
    assert_eq!(view.open_decisions.len(), 1);
    assert!(
        view.entries
            .iter()
            .any(|entry| entry.kind == ChatEntryKind::Decision)
    );
}

#[test]
fn a_declare_goal_proposal_is_rejected_when_a_goal_exists() {
    let dir = tempfile::tempdir().unwrap();
    relay_app::commands::init(dir.path(), "ship the existing goal", options()).unwrap();
    let session = ProjectSession::open(dir.path(), options()).unwrap();
    let provider = turn_provider(
        "A goal already exists.",
        json!({
            "proposals": [
                { "type": "declare-goal", "outcome": "a second, conflicting goal" }
            ]
        }),
    );
    let (result, _) = run_turn(&session, provider, "set a new goal");

    // The turn completed; the proposal was rejected with a safe reason and
    // nothing changed.
    assert_eq!(result.status, TurnStatus::Completed);
    let outcome = &result.proposals[0];
    assert!(!outcome.accepted);
    assert!(outcome.detail.contains("already"), "{}", outcome.detail);
    let graph = session.graph();
    let goal = graph.goal().expect("the original goal stands");
    assert_eq!(goal.outcome, "ship the existing goal");
    // No second goal fact was appended: exactly the one `init` recorded.
    assert_eq!(
        facts(&session)
            .iter()
            .filter(|fact| matches!(fact, DomainFact::GoalDeclared { .. }))
            .count(),
        1
    );
}

#[test]
fn resolve_decision_applies_one_exact_decision_and_rejects_unknown_ids() {
    let dir = tempfile::tempdir().unwrap();
    relay_app::commands::init(dir.path(), "export team reports as CSV", options()).unwrap();
    let session = ProjectSession::open(dir.path(), options()).unwrap();

    // Happy path: the exact open decision, one of its declared options.
    let provider = turn_provider(
        "Answering the clarification.",
        json!({
            "proposals": [
                { "type": "resolve-decision", "decision": "dec_clarify-unknowns-1",
                  "choice": "csv export", "rationale": "the user asked for CSV" }
            ]
        }),
    );
    let (result, _) = run_turn(&session, provider, "use csv export");
    assert!(result.proposals[0].accepted, "{:?}", result.proposals[0]);
    let resolved = session
        .graph()
        .decision(&"dec_clarify-unknowns-1".parse().unwrap())
        .cloned()
        .unwrap();
    assert!(resolved.resolution.is_some(), "the decision is resolved");
    match assistant_fact(&session) {
        DomainFact::AssistantMessageRecorded {
            related_decisions, ..
        } => assert_eq!(related_decisions.len(), 1, "{related_decisions:?}"),
        other => panic!("expected an assistant fact, got {other:?}"),
    }

    // Unknown id: rejected, never coerced, and no decision changes.
    let before = session.graph().revision().get();
    let provider = turn_provider(
        "I cannot resolve that.",
        json!({
            "proposals": [
                { "type": "resolve-decision", "decision": "dec_nope", "choice": "anything" }
            ]
        }),
    );
    let (result, _) = run_turn(&session, provider, "resolve dec_nope");
    assert!(!result.proposals[0].accepted);
    assert!(
        result.proposals[0].detail.contains("does not exist"),
        "{}",
        result.proposals[0].detail
    );
    assert!(
        session
            .graph()
            .decision(&"dec_nope".parse().unwrap())
            .is_none()
    );
    // Only the conversation facts for this turn landed.
    assert_eq!(session.graph().revision().get(), before + 2);
}

#[test]
fn run_ready_work_runs_a_bounded_number_of_ticks() {
    let dir = tempfile::tempdir().unwrap();
    relay_app::commands::init(dir.path(), "export team reports as CSV", options()).unwrap();
    let session = ProjectSession::open(dir.path(), options()).unwrap();
    relay_app::commands::resolve_decision(&session, "dec_clarify-unknowns-1", "csv export", None)
        .unwrap();

    // The conversation step serves the turn; the fallback serves the
    // model-executor attempt (purpose-less route call) with the protocol's
    // final object, exactly like the fake-provider run test in
    // app_service.rs.
    let final_object = json!({
        "outputs": [{
            "artifact": "art_brief",
            "title": "Opportunity brief",
            "content": {
                "schema": "relay.brief@1",
                "problem": "reports are stuck",
                "audience": "analysts",
                "outcome": "one-click CSV export",
                "open_questions": []
            }
        }]
    });
    let provider = Arc::new(FakeProvider::with_fallback(
        vec![FakeStep {
            r#match: FakeMatch {
                purpose: Some(PurposeMatch::Exact(CONVERSATION_PURPOSE.to_string())),
                ..FakeMatch::default()
            },
            response: FakeResponse {
                text: Some("Running the discovery work.".to_string()),
                structured_output: Some(json!({
                    "proposals": [
                        { "type": "run-ready-work", "autonomy": "bounded:2", "max_ticks": 64 }
                    ]
                })),
                ..FakeResponse::default()
            },
        }],
        FakeResponse {
            text: Some(final_object.to_string()),
            ..FakeResponse::default()
        },
    ));
    let (result, events) = run_turn(&session, provider, "run what is ready");

    assert!(result.proposals[0].accepted, "{:?}", result.proposals[0]);
    // max_ticks 64 was clamped to the service ceiling.
    assert!(
        result.proposals[0].detail.contains("tick"),
        "{}",
        result.proposals[0].detail
    );
    // Activity lines bracket the long work.
    let activities: Vec<&String> = events
        .iter()
        .filter_map(|event| match event {
            ConversationEvent::Activity { text } => Some(text),
            _ => None,
        })
        .collect();
    assert!(activities.len() >= 2, "{activities:?}");
    assert!(
        activities[0].contains("running ready work"),
        "{activities:?}"
    );

    // The run accepted an attempt and the assistant message cites it.
    assert!(!session.evaluation().completed.is_empty(), "work completed");
    match assistant_fact(&session) {
        DomainFact::AssistantMessageRecorded {
            related_attempts, ..
        } => assert!(!related_attempts.is_empty(), "{related_attempts:?}"),
        other => panic!("expected an assistant fact, got {other:?}"),
    }
    // The discovery brief published end to end.
    let brief = relay_app::commands::artifact_detail(&session, "art_brief", None, false).unwrap();
    assert_eq!(brief.schema, Some("relay.brief@1".to_string()));
}

#[test]
fn a_clarification_is_accepted_without_touching_the_graph() {
    let dir = tempfile::tempdir().unwrap();
    relay_app::commands::init(dir.path(), "export team reports as CSV", options()).unwrap();
    let session = ProjectSession::open(dir.path(), options()).unwrap();
    let before = session.graph().revision().get();
    let provider = turn_provider(
        "Which reports first?",
        json!({
            "proposals": [ { "type": "request-clarification", "question": "Which reports first?" } ]
        }),
    );
    let (result, _) = run_turn(&session, provider, "export the reports");

    assert_eq!(result.status, TurnStatus::Completed);
    let outcome = &result.proposals[0];
    assert!(outcome.accepted, "{outcome:?}");
    assert!(
        outcome.detail.contains("Which reports first?"),
        "{outcome:?}"
    );
    // Only this turn's conversation facts landed: the conversation start
    // (first turn in this workspace), the user message, the assistant
    // message — and no graph mutation.
    assert_eq!(session.graph().revision().get(), before + 3);
}

#[test]
fn partial_rejection_applies_the_valid_and_reports_the_invalid() {
    let dir = tempfile::tempdir().unwrap();
    relay_app::commands::init(dir.path(), "export team reports as CSV", options()).unwrap();
    let session = ProjectSession::open(dir.path(), options()).unwrap();
    let provider = turn_provider(
        "One worked, one did not.",
        json!({
            "proposals": [
                { "type": "resolve-decision", "decision": "dec_wrong", "choice": "nope" },
                { "type": "request-clarification", "question": "meanwhile: which format?" },
                { "type": "expand-work" },
                { "type": "expand-work" }
            ]
        }),
    );
    let (result, _) = run_turn(&session, provider, "do several things");

    let outcomes: Vec<&ProposalOutcome> = result.proposals.iter().collect();
    assert_eq!(outcomes.len(), 4);
    // Unknown decision: rejected.
    assert!(!outcomes[0].accepted);
    // Clarification: accepted, and it does not consume the mutation slot.
    assert!(outcomes[1].accepted);
    // First mutating proposal: accepted.
    assert!(outcomes[2].accepted, "{:?}", outcomes[2]);
    // Second mutating proposal: the one-mutation-per-turn policy rejects it.
    assert!(!outcomes[3].accepted);
    assert!(
        outcomes[3].detail.contains("only one mutating proposal"),
        "{}",
        outcomes[3].detail
    );
    assert_eq!(result.status, TurnStatus::Completed);
}

#[test]
fn mutations_require_a_goal() {
    let dir = tempfile::tempdir().unwrap();
    let (session, _) = ProjectSession::open_or_create(dir.path(), options()).unwrap();
    let provider = turn_provider(
        "There is nothing to run yet.",
        json!({
            "proposals": [
                { "type": "expand-work" },
                { "type": "run-ready-work", "max_ticks": 2 },
                { "type": "resolve-decision", "decision": "dec_1", "choice": "a" }
            ]
        }),
    );
    let (result, _) = run_turn(&session, provider, "go");

    for outcome in &result.proposals {
        assert!(!outcome.accepted, "{outcome:?}");
        assert!(
            outcome.detail.contains("requires a declared goal"),
            "{}",
            outcome.detail
        );
    }
    assert_eq!(session.graph().works().count(), 0);
}

// ---------------------------------------------------------------------------
// Failure, cancellation, recovery
// ---------------------------------------------------------------------------

#[test]
fn a_provider_failure_records_a_truthful_failed_turn() {
    let dir = tempfile::tempdir().unwrap();
    let (session, _) = ProjectSession::open_or_create(dir.path(), options()).unwrap();
    let provider = Arc::new(FakeProvider::new(vec![FakeStep {
        r#match: FakeMatch {
            purpose: Some(PurposeMatch::Exact(CONVERSATION_PURPOSE.to_string())),
            ..FakeMatch::default()
        },
        response: FakeResponse {
            error: Some(FakeErrorPlan {
                kind: ProviderErrorKind::Server,
                retryable: false,
                message: "provider exploded (sk-ant-secretkeyintext00000)".to_string(),
                fail_times: None,
            }),
            ..FakeResponse::default()
        },
    }]));
    let (result, events) = run_turn(&session, provider, "hello?");

    // A failed turn is a successful service call with a Failed status.
    assert!(matches!(result.status, TurnStatus::Failed { .. }));
    assert_eq!(
        result.assistant_text, "",
        "no assistant message is invented"
    );
    assert!(result.proposals.is_empty());
    assert!(result.usage.is_none());
    match &result.status {
        TurnStatus::Failed { message } => {
            assert!(message.contains("provider exploded"), "{message}");
            // The failure message was redacted before it became durable.
            assert!(
                !message.contains("sk-ant-secretkeyintext00000"),
                "{message}"
            );
            assert!(message.contains("[redacted:anthropic-key]"), "{message}");
        }
        other => panic!("expected a failure status, got {other:?}"),
    }
    // The terminal fact is durable, and it is the LAST fact.
    let timeline = facts(&session);
    assert!(matches!(
        timeline.last(),
        Some(DomainFact::ConversationTurnFailed { .. })
    ));
    // No assistant message fact exists.
    assert!(
        !timeline
            .iter()
            .any(|fact| matches!(fact, DomainFact::AssistantMessageRecorded { .. }))
    );
    assert!(matches!(
        events.last(),
        Some(ConversationEvent::TurnFinished { .. })
    ));
    // The chat view renders the failure entry.
    let view = chat_view(&session, 50);
    assert!(
        view.entries
            .iter()
            .any(|entry| entry.kind == ChatEntryKind::Failed)
    );
}

#[test]
fn a_pre_cancelled_token_records_a_cancelled_turn() {
    let dir = tempfile::tempdir().unwrap();
    let (session, _) = ProjectSession::open_or_create(dir.path(), options()).unwrap();
    let (sender, receiver) = mpsc::channel();
    let cancel = CancelToken::new();
    cancel.cancel("user pressed ctrl-c");
    let result = start_turn(
        &session,
        answer_only("too late"),
        "fake-standard",
        "long question",
        &limits(),
        cancel,
        sender,
    )
    .unwrap();
    drop(receiver);

    assert!(matches!(result.status, TurnStatus::Cancelled { .. }));
    assert_eq!(result.assistant_text, "");
    match &result.status {
        TurnStatus::Cancelled { reason } => assert_eq!(reason, "user pressed ctrl-c"),
        other => panic!("expected a cancelled status, got {other:?}"),
    }
    let timeline = facts(&session);
    match timeline.last() {
        Some(DomainFact::ConversationTurnCancelled { reason, .. }) => {
            assert_eq!(reason, "user pressed ctrl-c")
        }
        other => panic!("expected a cancellation fact, got {other:?}"),
    }
    let view = chat_view(&session, 50);
    assert!(
        view.entries
            .iter()
            .any(|entry| entry.kind == ChatEntryKind::Cancelled)
    );
}

#[test]
fn an_interrupted_turn_is_derived_never_answered() {
    let dir = tempfile::tempdir().unwrap();
    let (session, _) = ProjectSession::open_or_create(dir.path(), options()).unwrap();
    // One completed turn, then a crash between the user message and any
    // terminal fact (appended directly, simulating a crashed process).
    run_turn(&session, answer_only("first answer"), "first question");
    session
        .store()
        .append(&[DomainFact::UserMessageRecorded {
            conversation: "cnv_1".parse().unwrap(),
            turn: "turn_1-2".parse().unwrap(),
            message: "msg_1-3".parse().unwrap(),
            text: "interrupted question".to_string(),
            at_ms: EPOCH_MS + 50,
        }])
        .unwrap();
    session.refresh().unwrap();

    let view = chat_view(&session, 50);
    assert_eq!(view.interrupted_turns, 1);
    let interrupted = view
        .entries
        .iter()
        .find(|entry| entry.kind == ChatEntryKind::Interrupted)
        .expect("an interrupted entry");
    assert!(
        interrupted.text.contains("no reply was recorded"),
        "{}",
        interrupted.text
    );
    // The next turn continues the numbering past the interrupted one.
    let (result, _) = run_turn(&session, answer_only("back again"), "back again?");
    assert_eq!(result.turn.as_str(), "turn_1-3");
}

#[test]
fn the_chat_view_surfaces_decisions_before_any_conversation_exists() {
    let dir = tempfile::tempdir().unwrap();
    relay_app::commands::init(dir.path(), "export team reports as CSV", options()).unwrap();
    let session = ProjectSession::open(dir.path(), options()).unwrap();

    let view = chat_view(&session, 50);
    assert_eq!(view.conversation, None, "no conversation was invented");
    assert_eq!(view.goal.as_deref(), Some("export team reports as CSV"));
    assert_eq!(view.interrupted_turns, 0);
    assert_eq!(view.open_decisions.len(), 1);
    assert_eq!(
        view.open_decisions[0].decision.as_str(),
        "dec_clarify-unknowns-1"
    );
    // The open decision is rendered as a synthetic entry even with no
    // transcript yet.
    assert_eq!(view.entries.len(), 1);
    assert_eq!(view.entries[0].kind, ChatEntryKind::Decision);
    assert!(view.entries[0].turn.is_none());
    assert!(
        view.entries[0].text.contains("dec_clarify-unknowns-1"),
        "{}",
        view.entries[0].text
    );
}

// ---------------------------------------------------------------------------
// Safety: redaction, locks, budgets
// ---------------------------------------------------------------------------

#[test]
fn secrets_are_redacted_before_persistence_and_display() {
    let dir = tempfile::tempdir().unwrap();
    let (session, _) = ProjectSession::open_or_create(dir.path(), options()).unwrap();
    let secret = "sk-ant-0123456789abcdefghijklmnop";
    let provider = turn_provider(
        &format!("I saw a key {secret} in the config."),
        json!({ "proposals": [] }),
    );
    let (result, events) = run_turn(
        &session,
        provider,
        &format!("here is my key {secret} for testing"),
    );

    // The durable user message is redacted.
    let timeline = facts(&session);
    let user_text = timeline
        .iter()
        .find_map(|fact| match fact {
            DomainFact::UserMessageRecorded { text, .. } => Some(text.clone()),
            _ => None,
        })
        .unwrap();
    assert!(!user_text.contains(secret), "{user_text}");
    assert!(
        user_text.contains("[redacted:anthropic-key]"),
        "{user_text}"
    );
    // The durable assistant message is redacted.
    assert!(
        !result.assistant_text.contains(secret),
        "{}",
        result.assistant_text
    );
    match assistant_fact(&session) {
        DomainFact::AssistantMessageRecorded { text, .. } => {
            assert!(!text.contains(secret), "{text}")
        }
        other => panic!("expected an assistant fact, got {other:?}"),
    }
    // Every streamed fragment is redacted too.
    for event in &events {
        if let ConversationEvent::AssistantDelta { text } = event {
            assert!(!text.contains(secret), "{text}");
        }
    }
    // And the rendered view.
    let view = chat_view(&session, 50);
    for entry in &view.entries {
        assert!(!entry.text.contains(secret), "{}", entry.text);
    }
}

#[test]
fn no_writer_lock_is_held_across_the_model_call() {
    let dir = tempfile::tempdir().unwrap();
    let (session, _) = ProjectSession::open_or_create(dir.path(), options()).unwrap();
    let observed: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

    /// A provider that probes the store from inside the model call: both a
    /// reader open and a writer open with a short lock timeout. If the
    /// application held the writer lock across the provider call, the writer
    /// probe would time out.
    struct LockProbingProvider {
        workspace: PathBuf,
        observed: Arc<Mutex<Option<String>>>,
    }
    impl ModelProvider for LockProbingProvider {
        fn name(&self) -> ProviderName {
            ProviderName::Fake
        }
        fn models(&self) -> Vec<String> {
            vec!["fake-standard".to_string()]
        }
        fn stream(&self, _request: &ModelRequest) -> ProviderEventStream<'_> {
            let mut store_options = options().store;
            store_options.lock_timeout = std::time::Duration::from_millis(250);
            let reader = match relay_store::GraphFactStore::open_read_only(
                &self.workspace,
                store_options.clone(),
            ) {
                Ok(store) => format!(
                    "reader:{}",
                    store.read_facts().map(|facts| facts.len()).unwrap_or(0)
                ),
                Err(error) => format!("reader-open-failed: {error}"),
            };
            let writer = match relay_store::GraphFactStore::open(&self.workspace, store_options) {
                Ok(_) => "writer:acquired".to_string(),
                Err(error) => format!("writer-failed: {error}"),
            };
            *self.observed.lock().unwrap() = Some(format!("{reader}; {writer}"));
            Box::new(
                vec![
                    ProviderStreamEvent::TextDelta {
                        text: "probe done".to_string(),
                    },
                    ProviderStreamEvent::Completed {
                        finish_reason: FinishReason::Stop,
                        usage: TokenUsage {
                            input_tokens: 1,
                            output_tokens: 1,
                        },
                    },
                ]
                .into_iter(),
            )
        }
    }

    let provider = Arc::new(LockProbingProvider {
        workspace: dir.path().to_path_buf(),
        observed: Arc::clone(&observed),
    });
    let (result, _) = run_turn(&session, provider, "probe the lock");
    assert_eq!(result.status, TurnStatus::Completed);
    let probe = observed.lock().unwrap().clone().expect("the probe ran");
    assert!(
        probe.starts_with("reader:") && probe.contains("writer:acquired"),
        "the store stayed usable during the model call: {probe}"
    );
}

#[test]
fn the_context_respects_every_configured_budget() {
    let dir = tempfile::tempdir().unwrap();
    let (session, _) = ProjectSession::open_or_create(dir.path(), options()).unwrap();

    /// Records every request it serves, then completes without proposals.
    struct RecordingProvider {
        requests: Mutex<Vec<ModelRequest>>,
    }
    impl ModelProvider for RecordingProvider {
        fn name(&self) -> ProviderName {
            ProviderName::Fake
        }
        fn models(&self) -> Vec<String> {
            vec!["fake-standard".to_string()]
        }
        fn stream(&self, request: &ModelRequest) -> ProviderEventStream<'_> {
            self.requests.lock().unwrap().push(request.clone());
            Box::new(
                vec![
                    ProviderStreamEvent::TextDelta {
                        text: "noted".to_string(),
                    },
                    ProviderStreamEvent::Completed {
                        finish_reason: FinishReason::Stop,
                        usage: TokenUsage::default(),
                    },
                ]
                .into_iter(),
            )
        }
    }
    let recorder = Arc::new(RecordingProvider {
        requests: Mutex::new(Vec::new()),
    });

    // Six prior turns build a transcript of twelve messages.
    for index in 0..6 {
        run_turn(
            &session,
            answer_only(&format!("answer number {index}")),
            &format!("question number {index}"),
        );
    }

    // A final turn under deliberately tiny budgets.
    let tiny = ConversationLimits {
        transcript_context_messages: 3,
        transcript_context_bytes: 400,
        assistant_max_chars: 16_384,
        context_max_chars: 600,
        max_proposals: 8,
    };
    let (sender, receiver) = mpsc::channel();
    let result = start_turn(
        &session,
        Arc::clone(&recorder) as Arc<dyn ModelProvider>,
        "fake-standard",
        "summarize",
        &tiny,
        CancelToken::new(),
        sender,
    )
    .unwrap();
    drop(receiver);
    assert_eq!(result.status, TurnStatus::Completed);

    let requests = recorder.requests.lock().unwrap().clone();
    let request = requests.last().expect("the turn reached the provider");
    // At most: context block + 3 transcript entries + the prompt.
    assert!(request.messages.len() <= 5, "{}", request.messages.len());
    // The configured total character budget holds across all messages
    // (counting assistant content, which `text()` alone would skip).
    let total: usize = request
        .messages
        .iter()
        .map(|message| match message {
            relay_providers::contract::ModelMessage::User { content }
            | relay_providers::contract::ModelMessage::ToolResult { content, .. } => {
                content.chars().count()
            }
            relay_providers::contract::ModelMessage::Assistant { content } => content
                .iter()
                .map(|block| match block {
                    relay_providers::contract::AssistantContent::Text { text } => {
                        text.chars().count()
                    }
                    _ => 0,
                })
                .sum(),
        })
        .sum();
    assert!(total <= tiny.context_max_chars, "{total}");
    // The system prompt is present and bounded.
    let system = request.system.as_deref().unwrap_or_default();
    assert!(!system.is_empty());
    assert!(system.chars().count() <= 4_000);
    // The structured-output contract is what the service documents.
    let structured = request.structured_output.as_ref().expect("requested");
    assert_eq!(structured.name, "conversation-turn");
    assert_eq!(request.purpose.as_deref(), Some(CONVERSATION_PURPOSE));
}
