//! Deterministic system jobs.
//!
//! The system executor class covers rendering, projection, and invalidation
//! work. Jobs read the graph through the read-only
//! [`GraphView`](relay_executor::spec::GraphView) on the run context,
//! assemble structured documents, and hand them back as receipt data —
//! outputs (with canonical digests), proposals, and evidence. The engine
//! still assigns versions and validates everything; a job never mutates the
//! graph or delivers anything.

use relay_domain::{
    ArtifactContent, ArtifactDigest, ArtifactId, AttemptId, Decision, DecisionId, DecisionOption,
    DecisionSubject, Evidence, EvidenceId, EvidenceKind, EvidenceOutcome, EvidenceTarget, GoalId,
    OutputProposal, ProjectGraph,
};
use relay_executor::spec::{AttemptSpec, RunContext};
use relay_executor::{SystemJob, SystemOutcome};
use relay_workflows::{
    CRITERION_SCHEMA, DELIVERY_SCHEMA, DESIGN_SCHEMA, DeliveryDoc, PLAN_SCHEMA, REQUIREMENT_SCHEMA,
    StructuredDoc, artifacts_with_schema,
};

/// The artifact id the delivery report publishes under.
pub const DELIVERY_REPORT_ARTIFACT: &str = "art_delivery-report";

/// Assemble the delivery package: one structured delivery report over the
/// satisfied criteria and their standing evidence, proposed for publication
/// by the system executor. Delivery itself stays behind an explicit human
/// authorization decision — nothing here grants delivery authority.
pub struct DeliveryPrepJob;

impl SystemJob for DeliveryPrepJob {
    fn run(&self, spec: &AttemptSpec, context: &RunContext) -> SystemOutcome {
        let graph = (context.graph)();
        let criteria = artifacts_with_schema(&graph, CRITERION_SCHEMA);
        if criteria.is_empty() {
            return nothing("no acceptance criteria exist yet; nothing to assemble");
        }
        let unsatisfied: Vec<String> = criteria
            .iter()
            .filter(|reference| !relay_engine::status::criterion_satisfied(&graph, reference))
            .map(|reference| reference.to_string())
            .collect();
        if !unsatisfied.is_empty() {
            return nothing(&format!(
                "criteria not yet satisfied: {}",
                unsatisfied.join(", ")
            ));
        }

        // The package: every criterion, requirement, design, and plan
        // version the satisfied state rests on, plus the standing evidence.
        let mut includes = criteria.clone();
        includes.extend(artifacts_with_schema(&graph, REQUIREMENT_SCHEMA));
        includes.extend(artifacts_with_schema(&graph, DESIGN_SCHEMA));
        includes.extend(artifacts_with_schema(&graph, PLAN_SCHEMA));
        includes.sort_by_key(|reference| reference.to_string());
        includes.dedup_by(|a, b| a == b);
        let mut evidence: Vec<EvidenceId> = criteria
            .iter()
            .flat_map(|reference| {
                graph.evidence_for(&EvidenceTarget::ArtifactVersion(reference.clone()))
            })
            .filter(|item| {
                item.outcome == EvidenceOutcome::Pass
                    && matches!(
                        relay_engine::status::evidence_status(&graph, item),
                        relay_engine::status::EvidenceStatus::Valid
                    )
            })
            .map(|item| item.id.clone())
            .collect();
        evidence.sort_by(|a, b| a.as_str().cmp(b.as_str()));

        let doc = DeliveryDoc {
            summary: format!(
                "All {} acceptance criterion(s) satisfied at revision {}.",
                criteria.len(),
                graph.revision().get()
            ),
            includes: includes.clone(),
            evidence: evidence.clone(),
            checks: vec![
                "every acceptance criterion has standing passing evidence".to_string(),
                "delivery requires an explicit human authorization decision".to_string(),
            ],
        };
        let content = doc.to_content();
        let digest = canonical_digest_of(&content);
        let report = OutputProposal {
            artifact: report_artifact_id(),
            title: Some("Delivery report".to_string()),
            content,
            digest,
        };
        let mut package = Evidence::new(
            delivery_evidence_id(&spec.attempt),
            EvidenceKind::Inspection,
            EvidenceTarget::WorkItem(spec.work.clone()),
            EvidenceOutcome::Info,
            format!(
                "assembled the delivery package over {} criterion(s), {} included version(s), {} \
                 evidence record(s)",
                criteria.len(),
                includes.len(),
                evidence.len()
            ),
            context.now_ms,
        );
        package.payload = serde_json::json!({
            "includes": includes
                .iter()
                .map(|reference| reference.to_string())
                .collect::<Vec<_>>(),
        });
        SystemOutcome {
            outputs: vec![report],
            proposals: Vec::new(),
            evidence: vec![package],
            note: None,
        }
    }
}

/// The authorization decision the workflow opens once a delivery report is
/// published (rule 10 of `relay_workflows::propose_next`): exposed here so
/// callers that need the graph shape reuse the same wording. Delivery
/// authority never leaves the human.
pub fn delivery_authorization_decision(graph: &ProjectGraph) -> Option<Decision> {
    let deliveries = artifacts_with_schema(graph, DELIVERY_SCHEMA);
    let reference = deliveries.first()?.clone();
    let goal = graph.goal().map(|goal| goal.id.clone())?;
    let count = graph.decisions().count() + 1;
    Some(Decision::new(
        delivery_decision_id(count),
        DecisionSubject::Goal(goal),
        format!("Authorize delivery of the package at {reference}?"),
        vec![
            DecisionOption {
                label: "authorize".to_string(),
                description: "deliver the prepared package".to_string(),
            },
            DecisionOption {
                label: "hold".to_string(),
                description: "keep the package undelivered".to_string(),
            },
        ],
        0,
    ))
}

/// Whether every criterion of the graph is satisfied — the job's gate,
/// exposed for views.
pub fn delivery_ready(graph: &ProjectGraph) -> bool {
    let criteria = artifacts_with_schema(graph, CRITERION_SCHEMA);
    !criteria.is_empty()
        && criteria
            .iter()
            .all(|reference| relay_engine::status::criterion_satisfied(graph, reference))
}

// -- total-construction helpers ---------------------------------------------
//
// Every literal below is a compile-time-fixed id shape; the fallbacks keep
// construction total without panicking inside the application service.

#[allow(clippy::expect_used)] // scoped: static literal, compile-time fixed
fn report_artifact_id() -> ArtifactId {
    ArtifactId::parse(DELIVERY_REPORT_ARTIFACT).expect("literal parses")
}

#[allow(clippy::expect_used)] // scoped: static literal shape
fn delivery_decision_id(count: usize) -> DecisionId {
    match DecisionId::parse(&format!("dec_request-delivery-authorization-{count}")) {
        Ok(id) => id,
        Err(_) => DecisionId::parse("dec_delivery").expect("literal parses"),
    }
}

#[allow(clippy::expect_used)] // scoped: static literal shape
fn delivery_evidence_id(attempt: &AttemptId) -> EvidenceId {
    let slug = attempt.as_str().strip_prefix("att_").unwrap_or("attempt");
    match EvidenceId::parse(&format!("ev_{slug}-delivery")) {
        Ok(id) => id,
        Err(_) => EvidenceId::parse("ev_delivery").expect("literal parses"),
    }
}

#[allow(clippy::expect_used)] // scoped: canonical digests are valid hex
fn canonical_digest_of(content: &ArtifactContent) -> ArtifactDigest {
    relay_artifacts::content_digest(&content.0)
        .ok()
        .and_then(|hex| ArtifactDigest::parse(&hex).ok())
        .expect("canonical digest parses")
}

fn nothing(note: &str) -> SystemOutcome {
    SystemOutcome {
        outputs: Vec::new(),
        proposals: Vec::new(),
        evidence: Vec::new(),
        note: Some(note.to_string()),
    }
}

// Unused-today imports kept out: GoalId is referenced through the goal
// accessor's type; this alias documents the dependency without a lint.
#[allow(dead_code)]
type _GoalId = GoalId;
