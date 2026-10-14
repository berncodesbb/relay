//! Property-style tests over the work-graph kernel: cycle rejection,
//! order-independent evaluation, targeted invalidation, exact-version
//! enforcement, receipt revision matching, duplicate rejection, and
//! non-overlapping write scheduling. All deterministic and offline.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::ok_expect,
    clippy::err_expect
)] // test-only conveniences, house style

mod common;

use common::*;
use relay_domain::{
    DependencyEdge, DependencyKind, DomainFact, EvidenceOutcome, EvidenceTarget,
    GraphChangeProposal, InputRef, InvalidationCause, InvalidationTarget, ProjectGraph,
};
use relay_engine::{self, Autonomy, SchedulingPolicy, WorkStatus};

/// A small linear history used by several properties: a requirements chain
/// (req → design → impl) plus one independent work item.
fn chain_history() -> Vec<DomainFact> {
    let mut facts = vec![
        project("prj_prop"),
        goal("goal_prop", "ship a small improvement"),
    ];

    let req = model_work("work_req", "define-requirements", "write the requirement");
    let mut design = model_work("work_design", "technical-design", "design the change");
    design.inputs = vec![InputRef::Exact(version_ref("art_req", 1))];
    design.dependencies = vec![DependencyEdge::new(
        work("work_design"),
        work("work_req"),
        DependencyKind::Requires,
    )];
    let mut imp = model_work("work_build", "implement", "implement the change");
    imp.inputs = vec![InputRef::Exact(version_ref("art_design", 1))];
    imp.dependencies = vec![DependencyEdge::new(
        work("work_build"),
        work("work_design"),
        DependencyKind::Requires,
    )];
    let other = model_work("work_other", "research", "unrelated research");

    facts.push(work_declared(req));
    facts.push(work_declared(design));
    facts.push(work_declared(imp));
    facts.push(work_declared(other));

    // Everyone completes: req publishes art_req v1 …
    facts.push(started(
        &fold(&facts),
        "att_req",
        "work_req",
        grant_read("grant_req", &["."]),
    ));
    let graph = fold(&facts);
    let acceptance = relay_engine::accept_receipt(
        &graph,
        &completed_receipt(
            "att_req",
            "work_req",
            vec![output("art_req", "requirements", 1)],
            vec![],
            vec![],
        ),
    )
    .ok()
    .expect("req receipt accepted");
    facts.extend(acceptance.facts);

    let graph = fold(&facts);
    facts.push(started(
        &graph,
        "att_design",
        "work_design",
        grant_read("grant_design", &["."]),
    ));
    let graph = fold(&facts);
    let acceptance = relay_engine::accept_receipt(
        &graph,
        &completed_receipt(
            "att_design",
            "work_design",
            vec![output("art_design", "design", 2)],
            vec![],
            vec![],
        ),
    )
    .ok()
    .expect("design receipt accepted");
    facts.extend(acceptance.facts);

    let graph = fold(&facts);
    facts.push(started(
        &graph,
        "att_impl",
        "work_build",
        grant_read("grant_impl", &["."]),
    ));
    let graph = fold(&facts);
    let acceptance = relay_engine::accept_receipt(
        &graph,
        &completed_receipt(
            "att_impl",
            "work_build",
            vec![output("art_impl", "implementation", 3)],
            vec![],
            vec![],
        ),
    )
    .ok()
    .expect("impl receipt accepted");
    facts.extend(acceptance.facts);

    let graph = fold(&facts);
    facts.push(started(
        &graph,
        "att_other",
        "work_other",
        grant_read("grant_other", &["."]),
    ));
    let graph = fold(&facts);
    let acceptance = relay_engine::accept_receipt(
        &graph,
        &completed_receipt(
            "att_other",
            "work_other",
            vec![output("art_other", "unrelated notes", 4)],
            vec![],
            vec![],
        ),
    )
    .ok()
    .expect("other receipt accepted");
    facts.extend(acceptance.facts);
    facts
}

// ---------------------------------------------------------------------------
// Cycles
// ---------------------------------------------------------------------------

#[test]
fn dependency_cycles_are_rejected_by_fold_and_proposals() {
    let base = vec![
        project("prj_cycle"),
        goal("goal_cycle", "acyclic forever"),
        work_declared(model_work("work_c1", "research", "one")),
        work_declared(model_work("work_c2", "research", "two")),
        work_declared(model_work("work_c3", "research", "three")),
        requires("work_c2", "work_c1"),
        requires("work_c3", "work_c2"),
    ];
    let graph = fold(&base);

    // Closing the loop is rejected by the fold.
    let mut with_cycle = base.clone();
    with_cycle.push(requires("work_c1", "work_c3"));
    let error = ProjectGraph::from_facts(&with_cycle)
        .err()
        .expect("fold must reject the cycle");
    assert!(matches!(
        error,
        relay_domain::FactError::DependencyCycle { .. }
    ));

    let rejection = relay_engine::validate_graph_change(
        &graph,
        &GraphChangeProposal::DeclareDependency {
            edge: DependencyEdge::new(work("work_c1"), work("work_c3"), DependencyKind::Requires),
        },
    )
    .err()
    .expect("proposal must be rejected");
    assert!(matches!(rejection, relay_engine::Rejected::Cycle { .. }));

    // Self-dependency is rejected too.
    let rejection = relay_engine::validate_graph_change(
        &graph,
        &GraphChangeProposal::DeclareDependency {
            edge: DependencyEdge::new(work("work_c1"), work("work_c1"), DependencyKind::Requires),
        },
    )
    .err()
    .expect("self dependency must be rejected");
    assert!(matches!(
        rejection,
        relay_engine::Rejected::InvalidState { .. }
    ));

    // The accepted edge order stays legal: a fresh acyclic edge passes
    // (c3 → c1 keeps the three-node chain acyclic).
    let applied = relay_engine::validate_graph_change(
        &graph,
        &GraphChangeProposal::DeclareDependency {
            edge: DependencyEdge::new(work("work_c3"), work("work_c1"), DependencyKind::Informs),
        },
    )
    .ok()
    .expect("informs edge from c3 to c1 is acyclic");
    assert_eq!(applied.facts.len(), 1);
}

// ---------------------------------------------------------------------------
// Order independence
// ---------------------------------------------------------------------------

#[test]
fn evaluation_is_order_independent_for_independent_facts() {
    // Two clusters with no references between them.
    let cluster_a = vec![
        work_declared(model_work("work_a1", "research", "A1")),
        work_declared(model_work("work_a2", "research", "A2")),
        requires("work_a2", "work_a1"),
    ];
    let cluster_b = vec![
        work_declared(model_work("work_b1", "research", "B1")),
        work_declared(model_work("work_b2", "research", "B2")),
        informs("work_b2", "work_b1"),
    ];

    // Reference statuses from one canonical order.
    let canonical = {
        let mut facts = vec![
            project("prj_order"),
            goal("goal_order", "order does not matter"),
        ];
        facts.extend(cluster_a.iter().cloned());
        facts.extend(cluster_b.iter().cloned());
        status_map(&fold(&facts))
    };

    // All six interleavings of 3+3 fact sequences.
    let mut lcg = Lcg::new(7);
    for _ in 0..64 {
        let mut facts = vec![
            project("prj_order"),
            goal("goal_order", "order does not matter"),
        ];
        let merged = interleave(&cluster_a, &cluster_b, &mut lcg);
        facts.extend(merged);
        let graph = fold(&facts);
        let statuses = status_map(&graph);
        assert_eq!(
            statuses, canonical,
            "shuffled independent facts must evaluate identically"
        );
    }
}

// ---------------------------------------------------------------------------
// Targeted invalidation
// ---------------------------------------------------------------------------

#[test]
fn invalidation_is_targeted_to_actual_dependents() {
    let facts = chain_history();
    let graph = fold(&facts);
    // Sanity: everything completed, nothing invalid.
    for id in ["work_req", "work_design", "work_build", "work_other"] {
        assert_eq!(status_map(&graph)[&work(id)], WorkStatus::Completed, "{id}");
    }

    // The requirement changes: only its actual dependents go invalid.
    let mut changed = graph.clone();
    apply(&mut changed, &publish("art_req", 2, 9));
    let statuses = status_map(&changed);
    let invalid: Vec<&relay_domain::WorkItemId> = statuses
        .iter()
        .filter(|(_, status)| matches!(status, WorkStatus::Invalid { .. }))
        .map(|(id, _)| id)
        .collect();
    assert_eq!(invalid.len(), 2, "exactly the dependents: {invalid:?}");
    assert!(invalid.contains(&&work("work_design")));
    assert!(invalid.contains(&&work("work_build")));
    assert_eq!(statuses[&work("work_req")], WorkStatus::Completed);
    assert_eq!(statuses[&work("work_other")], WorkStatus::Completed);

    // The derived artifact version and any evidence on it are invalid too.
    let design_ref = version_ref("art_design", 1);
    assert!(matches!(
        relay_engine::status::version_status(&changed, &design_ref),
        relay_engine::VersionStatus::Invalid { .. }
    ));

    // Republishing an artifact nobody depends on invalidates nothing.
    let mut unrelated = graph.clone();
    apply(&mut unrelated, &publish("art_other", 2, 10));
    let statuses = status_map(&unrelated);
    assert!(
        statuses
            .values()
            .all(|status| !matches!(status, WorkStatus::Invalid { .. }))
    );
}

// ---------------------------------------------------------------------------
// Exact-version references
// ---------------------------------------------------------------------------

#[test]
fn exact_version_references_are_enforced() {
    let mut facts = vec![project("prj_pin"), goal("goal_pin", "pin your inputs")];
    let mut pinned = model_work("work_pinned", "research", "read the brief");
    pinned.inputs = vec![InputRef::Exact(version_ref("art_pin", 1))];
    let mut floating = model_work("work_floating", "research", "read whatever is current");
    floating.inputs = vec![InputRef::LatestPublished(
        "art_pin".parse().ok().expect("valid artifact id"),
    )];
    facts.push(work_declared(pinned));
    facts.push(work_declared(floating));

    // Unresolved inputs block rather than invalidate.
    let graph = fold(&facts);
    let statuses = status_map(&graph);
    for id in ["work_pinned", "work_floating"] {
        assert!(
            matches!(statuses[&work(id)], WorkStatus::Blocked { .. }),
            "unpublished input must block {id}"
        );
    }

    // Publishing v1 satisfies both.
    let mut graph = graph;
    apply(&mut graph, &publish("art_pin", 1, 1));
    let statuses = status_map(&graph);
    assert_eq!(statuses[&work("work_pinned")], WorkStatus::Ready);
    assert_eq!(statuses[&work("work_floating")], WorkStatus::Ready);

    // A v2 publication invalidates only the exact pinner.
    apply(&mut graph, &publish("art_pin", 2, 2));
    let statuses = status_map(&graph);
    assert!(matches!(
        statuses[&work("work_pinned")],
        WorkStatus::Invalid { .. }
    ));
    assert_eq!(statuses[&work("work_floating")], WorkStatus::Ready);

    // Version monotonicity fails closed at fold time.
    let facts = vec![
        project("prj_pin2"),
        publish("art_pin", 1, 1),
        publish("art_pin", 3, 3),
    ];
    let error = ProjectGraph::from_facts(&facts)
        .err()
        .expect("version skip must fail");
    assert!(matches!(
        error,
        relay_domain::FactError::SequenceViolation { .. }
    ));
}

// ---------------------------------------------------------------------------
// Receipt revision matching
// ---------------------------------------------------------------------------

#[test]
fn stale_receipts_are_rejected_only_for_material_changes() {
    let mut facts = vec![
        project("prj_stale"),
        goal("goal_stale", "fresh receipts only"),
    ];
    facts.push(publish("art_in", 1, 1));
    let mut reader = model_work("work_reader", "research", "consume art_in@1");
    reader.inputs = vec![InputRef::Exact(version_ref("art_in", 1))];
    facts.push(work_declared(reader));
    facts.push(started(
        &fold(&facts),
        "att_reader",
        "work_reader",
        grant_read("grant_reader", &["."]),
    ));
    let graph = fold(&facts);

    // An unrelated fact landing mid-attempt is immaterial: acceptance holds.
    let mut graph = graph.clone();
    apply(
        &mut graph,
        &work_declared(model_work("work_unrelated", "research", "elsewhere")),
    );
    let acceptance = relay_engine::accept_receipt(
        &graph,
        &completed_receipt(
            "att_reader",
            "work_reader",
            vec![output("art_out", "notes", 4)],
            vec![],
            vec![],
        ),
    );
    assert!(
        acceptance.is_ok(),
        "immaterial changes must not reject: {acceptance:?}"
    );

    // A material change (the pinned input superseded) rejects with a typed
    // stale revision naming the change.
    let mut stale = graph.clone();
    apply(&mut stale, &publish("art_in", 2, 5));
    let rejection = relay_engine::accept_receipt(
        &stale,
        &completed_receipt(
            "att_reader",
            "work_reader",
            vec![output("art_out", "notes", 4)],
            vec![],
            vec![],
        ),
    )
    .err()
    .expect("material change must reject");
    match rejection {
        relay_engine::Rejection::StaleRevision {
            expected,
            current,
            changes,
            ..
        } => {
            assert_ne!(expected, current);
            assert!(
                changes.iter().any(|change| change.contains("art_in@1")),
                "changes must name the superseded input: {changes:?}"
            );
        }
        other => panic!("expected StaleRevision, got {other:?}"),
    }

    // Already-concluded attempts never accept a second receipt.
    let acceptance = relay_engine::accept_receipt(
        &graph,
        &completed_receipt(
            "att_reader",
            "work_reader",
            vec![output("art_out", "notes", 4)],
            vec![],
            vec![],
        ),
    )
    .ok()
    .expect("clean acceptance");
    apply_all(&mut graph, &acceptance.facts);
    let rejection = relay_engine::accept_receipt(
        &graph,
        &completed_receipt(
            "att_reader",
            "work_reader",
            vec![output("art_out", "notes", 4)],
            vec![],
            vec![],
        ),
    )
    .err()
    .expect("second receipt must be rejected");
    assert!(matches!(
        rejection,
        relay_engine::Rejection::AlreadyConcluded { .. }
    ));

    // A failed attempt is accounted (not completed): its work returns to
    // ready and the explanation names the failure.
    apply(
        &mut graph,
        &work_declared(model_work("work_retry", "research", "retry me")),
    );
    start(
        &mut graph,
        "att_retry",
        "work_retry",
        grant_read("grant_retry", &["."]),
    );
    let acceptance = relay_engine::accept_receipt(
        &graph,
        &failed_receipt("att_retry", "work_retry", "provider timed out", vec![]),
    )
    .ok()
    .expect("failure receipts are accepted accounts");
    apply_all(&mut graph, &acceptance.facts);
    let statuses = status_map(&graph);
    assert_eq!(statuses[&work("work_retry")], WorkStatus::Ready);
    let explained = relay_engine::explain(&graph, &work("work_retry"))
        .ok()
        .expect("explainable");
    assert!(
        explained.detail.contains("att_retry reported failure"),
        "ready explanation names the prior failure: {explained}"
    );
}

// ---------------------------------------------------------------------------
// Duplicate proposal rejection
// ---------------------------------------------------------------------------

#[test]
fn duplicate_proposals_are_rejected() {
    let facts = vec![
        project("prj_dup"),
        goal("goal_dup", "no clones"),
        work_declared(model_work("work_d1", "research", "original")),
        work_declared(model_work("work_d2", "research", "dependent")),
        requires("work_d2", "work_d1"),
    ];
    let graph = fold(&facts);

    let duplicate_work = relay_engine::validate_graph_change(
        &graph,
        &GraphChangeProposal::DeclareWork {
            work: model_work("work_d1", "research", "clone of the original"),
        },
    )
    .err()
    .expect("duplicate work must be rejected");
    assert!(matches!(
        duplicate_work,
        relay_engine::Rejected::Duplicate { .. }
    ));

    let duplicate_edge = relay_engine::validate_graph_change(
        &graph,
        &GraphChangeProposal::DeclareDependency {
            edge: DependencyEdge::new(work("work_d2"), work("work_d1"), DependencyKind::Requires),
        },
    )
    .err()
    .expect("duplicate edge must be rejected");
    assert!(matches!(
        duplicate_edge,
        relay_engine::Rejected::Duplicate { .. }
    ));

    // Unknown references are rejected with their own variant.
    let unknown = relay_engine::validate_graph_change(
        &graph,
        &GraphChangeProposal::DeclareDependency {
            edge: DependencyEdge::new(
                work("work_ghost"),
                work("work_d1"),
                DependencyKind::Requires,
            ),
        },
    )
    .err()
    .expect("unknown endpoint must be rejected");
    assert!(matches!(
        unknown,
        relay_engine::Rejected::UnknownReference { .. }
    ));
}

// ---------------------------------------------------------------------------
// Non-overlapping scheduling
// ---------------------------------------------------------------------------

fn writer_history() -> ProjectGraph {
    let mut facts = vec![
        project("prj_sched"),
        goal("goal_sched", "serialize the writes"),
    ];
    let mut w1 = workspace_work("work_w1", "implement", "touch src", "src");
    w1.read_scopes = vec![root_scope()];
    let w2 = workspace_work(
        "work_w2",
        "implement",
        "touch src/core (overlaps w1)",
        "src/core",
    );
    let w3 = workspace_work("work_w3", "implement", "touch docs", "docs");
    let reader = model_work("work_r1", "research", "read-only analysis");
    facts.push(work_declared(w1));
    facts.push(work_declared(w2));
    facts.push(work_declared(w3));
    facts.push(work_declared(reader));
    fold(&facts)
}

fn root_scope() -> relay_domain::ScopePath {
    relay_domain::ScopePath::parse(".")
        .ok()
        .expect("valid scope")
}

#[test]
fn parallel_batches_never_contain_writers_and_writers_never_overlap() {
    let graph = writer_history();

    // Cap 1 (first-release policy): one writer runs, overlapping and later
    // writers wait, read-only work parallelizes.
    let policy = SchedulingPolicy {
        autonomy: Autonomy::Safe,
        max_concurrent_writers: 1,
    };
    let recommendation = relay_engine::recommend(&graph, &policy);
    assert!(
        recommendation
            .parallel
            .iter()
            .all(|entry| entry.write_scopes.is_empty())
    );
    assert_eq!(recommendation.serialized.len(), 1);
    assert_eq!(recommendation.serialized[0].work, work("work_w1"));
    assert_eq!(
        recommendation.parallel.len(),
        1,
        "the read-only item joins the parallel batch"
    );
    assert_eq!(recommendation.deferred.len(), 2);

    // Cap 2: two disjoint writers may run, the overlapping one still waits.
    let policy = SchedulingPolicy {
        autonomy: Autonomy::Safe,
        max_concurrent_writers: 2,
    };
    let recommendation = relay_engine::recommend(&graph, &policy);
    assert_eq!(recommendation.serialized.len(), 2);
    let admitted: Vec<&relay_domain::WorkItemId> = recommendation
        .serialized
        .iter()
        .map(|entry| &entry.work)
        .collect();
    assert!(admitted.contains(&&work("work_w1")));
    assert!(admitted.contains(&&work("work_w3")));
    assert!(
        !admitted.contains(&&work("work_w2")),
        "src/core overlaps src"
    );
    // The invariant proper: no two serialized entries share a scope.
    for (i, a) in recommendation.serialized.iter().enumerate() {
        for b in recommendation.serialized.iter().skip(i + 1) {
            assert!(
                !a.write_scopes
                    .iter()
                    .any(|scope| { b.write_scopes.iter().any(|other| scope.overlaps(other)) }),
                "{a:?} overlaps {b:?}"
            );
        }
    }

    // Manual autonomy defers everything executor-shaped.
    let recommendation = relay_engine::recommend(
        &graph,
        &SchedulingPolicy {
            autonomy: Autonomy::Manual,
            max_concurrent_writers: 1,
        },
    );
    assert!(recommendation.parallel.is_empty() && recommendation.serialized.is_empty());
    assert_eq!(recommendation.deferred.len(), 4);
}

// ---------------------------------------------------------------------------
// Declared invalidation and evidence status (small extra properties)
// ---------------------------------------------------------------------------

#[test]
fn declared_invalidation_invalidates_exactly_its_target() {
    let mut facts = vec![project("prj_inv"), goal("goal_inv", "judge precisely")];
    facts.push(publish("art_j", 1, 1));
    facts.push(work_declared(model_work(
        "work_j1",
        "research",
        "independent",
    )));
    let ev = evidence(
        "ev_j",
        EvidenceTarget::ArtifactVersion(version_ref("art_j", 1)),
        EvidenceOutcome::Info,
        "observed",
    );
    facts.push(DomainFact::EvidenceRecorded { evidence: ev });
    let mut graph = fold(&facts);

    // A human judgment against the evidence invalidates exactly that evidence.
    apply(
        &mut graph,
        &DomainFact::InvalidationDeclared {
            target: InvalidationTarget::Evidence(evidence_id("ev_j")),
            cause: InvalidationCause::Declared {
                note: "the observation was taken against the wrong build".to_string(),
            },
            changed_input: None,
        },
    );
    let ev = graph
        .evidence_by_id(&evidence_id("ev_j"))
        .cloned()
        .expect("evidence exists");
    assert!(matches!(
        relay_engine::status::evidence_status(&graph, &ev),
        relay_engine::EvidenceStatus::Invalid { .. }
    ));
    // Nothing else moved.
    assert_eq!(status_map(&graph)[&work("work_j1")], WorkStatus::Ready);
    assert!(matches!(
        relay_engine::status::version_status(&graph, &version_ref("art_j", 1)),
        relay_engine::VersionStatus::Current
    ));
}

// ---------------------------------------------------------------------------
// Conversation interleaving
// ---------------------------------------------------------------------------

fn conversation_id(id: &str) -> relay_domain::ConversationId {
    id.parse().ok().expect("valid conversation id")
}

fn turn_id(id: &str) -> relay_domain::TurnId {
    id.parse().ok().expect("valid turn id")
}

fn message_id(id: &str) -> relay_domain::MessageId {
    id.parse().ok().expect("valid message id")
}

/// The turn facts of one conversation, in legal order: a completed turn, a
/// failed turn, a cancelled turn, and an interrupted (never-terminated)
/// turn. The `ConversationStarted` fact is *not* included — it belongs to
/// the fixed prefix both conversations share. `tag` names the turn/message
/// ids (`turn_<tag>-1`, `msg_<tag>-u1`, …).
fn conversation_turns(conversation: &relay_domain::ConversationId, tag: &str) -> Vec<DomainFact> {
    vec![
        DomainFact::UserMessageRecorded {
            conversation: conversation.clone(),
            turn: turn_id(&format!("turn_{tag}-1")),
            message: message_id(&format!("msg_{tag}-u1")),
            text: format!("{tag}: what is here?").to_string(),
            at_ms: 1_000,
        },
        DomainFact::AssistantMessageRecorded {
            conversation: conversation.clone(),
            turn: turn_id(&format!("turn_{tag}-1")),
            message: message_id(&format!("msg_{tag}-a1")),
            text: format!("{tag}: a work-graph harness").to_string(),
            at_ms: 1_100,
            related_attempts: Vec::new(),
            related_decisions: Vec::new(),
            related_work: vec![work("work_side")],
        },
        DomainFact::UserMessageRecorded {
            conversation: conversation.clone(),
            turn: turn_id(&format!("turn_{tag}-2")),
            message: message_id(&format!("msg_{tag}-u2")),
            text: format!("{tag}: try again").to_string(),
            at_ms: 1_200,
        },
        DomainFact::ConversationTurnFailed {
            conversation: conversation.clone(),
            turn: turn_id(&format!("turn_{tag}-2")),
            safe_message: "provider unavailable".to_string(),
            at_ms: 1_300,
        },
        DomainFact::UserMessageRecorded {
            conversation: conversation.clone(),
            turn: turn_id(&format!("turn_{tag}-3")),
            message: message_id(&format!("msg_{tag}-u3")),
            text: format!("{tag}: cancel this").to_string(),
            at_ms: 1_400,
        },
        DomainFact::ConversationTurnCancelled {
            conversation: conversation.clone(),
            turn: turn_id(&format!("turn_{tag}-3")),
            reason: "user interrupted".to_string(),
            at_ms: 1_500,
        },
        DomainFact::UserMessageRecorded {
            conversation: conversation.clone(),
            turn: turn_id(&format!("turn_{tag}-4")),
            message: message_id(&format!("msg_{tag}-u4")),
            text: format!("{tag}: left hanging").to_string(),
            at_ms: 1_600,
        },
    ]
}

#[test]
fn interleaved_independent_conversations_fold_identically() {
    // Facts of independent conversations commute: interleaving the turn
    // facts of two conversations (order inside each preserved) folds to
    // identical derived state — conversations, interrupted turns, bounded
    // transcripts, and the untouched work statuses.
    let alpha_id = conversation_id("cnv_alpha");
    let beta_id = conversation_id("cnv_beta");
    let prefix = vec![
        project("prj_chat"),
        goal("goal_chat", "unaffected by chat"),
        work_declared(model_work("work_side", "research", "unrelated")),
        DomainFact::ConversationStarted {
            conversation: alpha_id.clone(),
            started_at_ms: 900,
        },
        DomainFact::ConversationStarted {
            conversation: beta_id.clone(),
            started_at_ms: 901,
        },
    ];
    let alpha = conversation_turns(&alpha_id, "alpha");
    let beta = conversation_turns(&beta_id, "beta");

    let canonical = {
        let mut facts = prefix.clone();
        facts.extend(alpha.iter().cloned());
        facts.extend(beta.iter().cloned());
        fold(&facts)
    };

    let mut lcg = Lcg::new(11);
    for _ in 0..128 {
        let mut facts = prefix.clone();
        facts.extend(interleave(&alpha, &beta, &mut lcg));
        let graph = fold(&facts);
        assert_eq!(
            graph, canonical,
            "shuffled independent conversation facts must fold identically"
        );

        assert_eq!(graph.latest_conversation(), Some(beta_id.clone()));
        assert_eq!(
            graph.interrupted_turns(&alpha_id),
            vec![turn_id("turn_alpha-4")]
        );
        assert_eq!(
            graph.interrupted_turns(&beta_id),
            vec![turn_id("turn_beta-4")]
        );
        assert_eq!(graph.transcript_tail(&alpha_id, 3).len(), 3);
        assert_eq!(graph.transcript_tail(&beta_id, 10).len(), 5);
        // The work graph is untouched by conversation facts.
        assert_eq!(status_map(&graph)[&work("work_side")], WorkStatus::Ready);
    }
}
