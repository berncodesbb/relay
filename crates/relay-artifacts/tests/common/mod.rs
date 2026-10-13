//! Shared valid draft samples for every artifact kind (port of
//! `test/unit/artifacts/samples.test.ts`), plus the demo drafts from the
//! deterministic fake provider (`src/runtime/demo-provider.ts`) used by the
//! golden fixture tests.

#![allow(dead_code)]

use relay_artifacts::drafts::*;

/// The demo goal shared by all fake-provider drafts.
pub const DEMO_GOAL: &str = "Demonstrate the Relay workflow end to end (fake provider)";

fn s(value: &str) -> String {
    value.to_string()
}

pub fn sample_opportunity_brief() -> OpportunityBriefDraft {
    OpportunityBriefDraft {
        title: s("Faster release feedback loop"),
        problem: s("Releases fail late because feedback only arrives after a full rollout."),
        target_users: vec![
            TargetUser {
                name: s("Platform engineers"),
                description: s("Own the deploy pipeline and its gates."),
            },
            TargetUser {
                name: s("Service owners"),
                description: s("Ship changes to production weekly."),
            },
        ],
        evidence: vec![
            Evidence {
                description: s("35% of rollbacks trace to failures first seen in production."),
                source: s("incident review 2026-09"),
            },
            Evidence {
                description: s("Median time from merge to production signal is 40 minutes."),
                source: String::new(),
            },
        ],
        desired_outcomes: vec![s("Halve the rollback rate within two quarters.")],
        assumptions: vec![s("Teams adopt the new gate without a migration period.")],
        constraints: vec![s("No new cloud services.")],
        open_questions: vec![s("Which repository should host the gate?")],
    }
}

pub fn sample_prd() -> PrdDraft {
    PrdDraft {
        title: s("Release gate PRD"),
        summary: s("Specify a pre-merge release gate that surfaces rollout risk before code lands."),
        requirements: vec![Requirement {
            id: s("REQ-1"),
            statement: s("The gate shall run the repository test suite on every proposed change."),
            rationale: s("Late failures are the dominant rollback cause."),
        }],
        exclusions: vec![s("Mobile client behavior.")],
        acceptance_criteria: vec![AcceptanceCriterion {
            id: s("AC-1"),
            given: s("a repository with a passing test suite"),
            when: s("a change is proposed"),
            then: s("the gate runs the suite and reports the result before merge"),
        }],
    }
}

pub fn sample_trd() -> TrdDraft {
    TrdDraft {
        title: s("Release gate TRD"),
        summary: s("Map the release gate onto the existing CLI and event log."),
        repository_findings: vec![RepositoryFinding {
            area: s("src/cli"),
            finding: s("Argument parsing already supports subcommands."),
        }],
        architecture: s(
            "A gate evaluator subscribes to the append-only event log and blocks ADVANCE outcomes until checks pass.",
        ),
        interfaces: vec![Interface {
            name: s("GateEvaluator"),
            description: s("Evaluates configured checks for a change."),
        }],
        migrations: vec![Migration {
            description: s("Add a checks table."),
            risk: s("Low; additive migration."),
        }],
        risks: vec![Risk {
            risk: s("Gate adds latency to every merge."),
            mitigation: s("Cache results per commit hash."),
        }],
        rollout_plan: s("Ship behind a flag, enable for one team, then expand."),
        rollback_plan: s("Disable the flag; no data migration is required."),
    }
}

pub fn sample_execution_plan() -> ExecutionPlanDraft {
    ExecutionPlanDraft {
        title: s("Release gate execution plan"),
        summary: s("Implement the gate evaluator and wire it into the transition policy."),
        tasks: vec![
            PlanTask {
                id: s("T-1"),
                title: s("Add gate evaluator"),
                description: s("Implement the evaluator with a Zod-validated configuration."),
                dependencies: vec![],
                affected_files: vec![
                    s("src/policies/gate.ts"),
                    s("test/unit/policies/gate.test.ts"),
                ],
                checks: vec![s("npm test -- --run test/unit/policies")],
                integration_points: vec![s("Transition policy CONTINUE/ADVANCE decisions.")],
            },
            PlanTask {
                id: s("T-2"),
                title: s("Wire the evaluator into the CLI"),
                description: s("Invoke the evaluator before advancing modes."),
                dependencies: vec![s("T-1")],
                affected_files: vec![s("src/cli/commands/run.ts")],
                checks: vec![s("npm run typecheck")],
                integration_points: vec![],
            },
        ],
    }
}

pub fn sample_verification_report() -> VerificationReportDraft {
    VerificationReportDraft {
        title: s("Release gate verification report"),
        summary: s("Every approved acceptance criterion was evaluated against the diff."),
        results: vec![
            VerificationResult {
                criterion_id: s("AC-1"),
                criterion: s("Gate runs the suite before merge"),
                status: VerificationStatus::Pass,
                evidence: s("test/unit/policies/gate.test.ts covers the blocking path."),
            },
            VerificationResult {
                criterion_id: s("AC-2"),
                criterion: s("Latency stays under 5s"),
                status: VerificationStatus::Unevaluated,
                evidence: s("No latency budget was instrumented in this change."),
            },
        ],
        overall_pass: false,
        notes: s("One criterion remains unevaluated; latency instrumentation is follow-up work."),
    }
}

pub fn sample_delivery_report() -> DeliveryReportDraft {
    DeliveryReportDraft {
        title: s("Release gate delivery report"),
        summary: s("Trace from requirement to change to verification evidence."),
        trace: vec![TraceEntry {
            requirement_id: s("REQ-1"),
            requirement: s("The gate shall run the test suite on every proposed change."),
            design_decisions: vec![s("Evaluator is deterministic and config-driven.")],
            changes: vec![s("Added src/policies/gate.ts.")],
            evidence: vec![s("Verification report AC-1: pass.")],
        }],
        next_steps: vec![s("User reviews this report.")],
        held_actions: vec![s("git commit"), s("git push"), s("pull request creation")],
    }
}

pub fn sample_decision_log() -> DecisionLogDraft {
    DecisionLogDraft {
        title: s("Release gate decision log"),
        entries: vec![DecisionEntry {
            decision: s("Use node:sqlite for project state"),
            rationale: s("Avoids a native dependency and ships with Node 22."),
            alternatives: s("better-sqlite3 was rejected for its native build step."),
            mode: s("implementation"),
        }],
    }
}

pub fn sample_draft(kind: relay_core::ArtifactKind) -> ArtifactDraft {
    use relay_core::ArtifactKind;
    match kind {
        ArtifactKind::OpportunityBrief => ArtifactDraft::OpportunityBrief(sample_opportunity_brief()),
        ArtifactKind::Prd => ArtifactDraft::Prd(sample_prd()),
        ArtifactKind::Trd => ArtifactDraft::Trd(sample_trd()),
        ArtifactKind::ExecutionPlan => ArtifactDraft::ExecutionPlan(sample_execution_plan()),
        ArtifactKind::VerificationReport => {
            ArtifactDraft::VerificationReport(sample_verification_report())
        }
        ArtifactKind::DeliveryReport => ArtifactDraft::DeliveryReport(sample_delivery_report()),
        ArtifactKind::DecisionLog => ArtifactDraft::DecisionLog(sample_decision_log()),
    }
}

pub fn sample_json(kind: relay_core::ArtifactKind) -> serde_json::Value {
    serde_json::to_value(sample_draft(kind)).expect("samples serialize")
}

// -- demo provider drafts (src/runtime/demo-provider.ts) ----------------------

pub fn demo_goal() -> String {
    s(DEMO_GOAL)
}

pub fn demo_opportunity_brief() -> serde_json::Value {
    serde_json::json!({
        "title": format!("Demo opportunity brief — {DEMO_GOAL}"),
        "problem": format!(
            "The stated goal is: \"{DEMO_GOAL}\". This demo brief exists to show the discovery stage producing a schema-valid opportunity brief."
        ),
        "targetUsers": [{
            "name": "Relay user",
            "description": "A developer evaluating the workflow pipeline with the deterministic fake provider."
        }],
        "evidence": [{
            "description": "Demo run configured with the fake provider; no repository evidence is claimed.",
            "source": "demo"
        }],
        "desiredOutcomes": [
            "A complete set of workflow artifacts from discovery through delivery.",
            "A pause before delivery requiring explicit user authorization."
        ],
        "assumptions": ["The demo runs against a disposable demo workspace."],
        "constraints": ["No commits, pushes, or deployments may be performed."],
        "openQuestions": []
    })
}

pub fn demo_prd() -> serde_json::Value {
    serde_json::json!({
        "title": format!("Demo PRD — {DEMO_GOAL}"),
        "summary": format!("Demo requirements derived from the goal \"{DEMO_GOAL}\" for pipeline demonstration purposes."),
        "requirements": [
            {
                "id": "REQ-1",
                "statement": "The workflow shall produce one artifact per mode, versioned as Markdown.",
                "rationale": "Demonstrates artifact creation and approval checkpoints."
            },
            {
                "id": "REQ-2",
                "statement": "The workflow shall pause before any delivery action.",
                "rationale": "Demonstrates the delivery hold policy."
            }
        ],
        "exclusions": ["Real model reasoning.", "Changes to user files."],
        "acceptanceCriteria": [
            {
                "id": "AC-1",
                "given": "the demo workflow has run through verification",
                "when": "artifact storage is inspected",
                "then": "a Markdown artifact exists for every workflow mode"
            },
            {
                "id": "AC-2",
                "given": "the delivery report exists",
                "when": "the workflow attempts to finish",
                "then": "it stops and requires explicit user authorization for delivery actions"
            }
        ]
    })
}

pub fn demo_trd() -> serde_json::Value {
    serde_json::json!({
        "title": format!("Demo TRD — {DEMO_GOAL}"),
        "summary": format!("Demo technical design for \"{DEMO_GOAL}\": the pipeline itself is the architecture under demonstration."),
        "repositoryFindings": [{
            "area": "demo",
            "finding": "Repository facts are not inspected by the demo script; this finding is a placeholder."
        }],
        "architecture": "Demo architecture: the workflow controller drives seven modes over an append-only event log; providers are replaceable workers.",
        "interfaces": [{
            "name": "WorkflowController",
            "description": "Drives mode steps and applies transition policy (demo description)."
        }],
        "migrations": [],
        "risks": [{
            "risk": "Demo content may be mistaken for real analysis.",
            "mitigation": "Every demo artifact title and summary is labeled \"demo\"."
        }],
        "rolloutPlan": "Demo rollout: run the workflow with the fake provider in a disposable workspace.",
        "rollbackPlan": "Delete the demo workspace; nothing outside it is touched."
    })
}

pub fn demo_execution_plan() -> serde_json::Value {
    serde_json::json!({
        "title": format!("Demo execution plan — {DEMO_GOAL}"),
        "summary": format!("Demo task graph for \"{DEMO_GOAL}\": one task per downstream mode, no user-file changes."),
        "tasks": [{
            "id": "TASK-1",
            "title": "Produce demo artifacts",
            "description": "Each mode emits its schema-valid demo draft for approval.",
            "dependencies": [],
            "affectedFiles": [],
            "checks": ["demo check: artifact files exist under .relay/artifacts"],
            "integrationPoints": []
        }]
    })
}

pub fn demo_decision_log() -> serde_json::Value {
    serde_json::json!({
        "title": format!("Demo decision log — {DEMO_GOAL}"),
        "entries": [{
            "decision": "No changes required (demo)",
            "rationale": format!(
                "The demo provider does not modify user files; the goal \"{DEMO_GOAL}\" is satisfied by the pipeline itself. No commands were run."
            ),
            "alternatives": "",
            "mode": "implementation"
        }]
    })
}

pub fn demo_verification_report() -> serde_json::Value {
    serde_json::json!({
        "title": format!("Demo verification report — {DEMO_GOAL}"),
        "summary": "Demo verification: criteria are satisfied by construction of the scripted pipeline.",
        "results": [
            {
                "criterionId": "AC-1",
                "criterion": "a Markdown artifact exists for every workflow mode",
                "status": "pass",
                "evidence": "demo run — criteria satisfied by construction"
            },
            {
                "criterionId": "AC-2",
                "criterion": "the workflow pauses before delivery actions",
                "status": "pass",
                "evidence": "demo run — criteria satisfied by construction"
            }
        ],
        "overallPass": true,
        "notes": "Demo verification; no repository commands were executed."
    })
}

pub fn demo_delivery_report() -> serde_json::Value {
    serde_json::json!({
        "title": format!("Demo delivery report — {DEMO_GOAL}"),
        "summary": format!("Demo delivery trace for \"{DEMO_GOAL}\" linking requirements to evidence; delivery actions are held."),
        "trace": [
            {
                "requirementId": "REQ-1",
                "requirement": "The workflow shall produce one artifact per mode, versioned as Markdown.",
                "designDecisions": ["Artifacts are rendered deterministically and stored under .relay/artifacts (demo)."],
                "changes": [],
                "evidence": ["demo run — artifacts exist for every mode"]
            },
            {
                "requirementId": "REQ-2",
                "requirement": "The workflow shall pause before any delivery action.",
                "designDecisions": ["Delivery always ends in a policy pause requiring user authorization (demo)."],
                "changes": [],
                "evidence": ["demo run — workflow paused with the delivery hold"]
            }
        ],
        "nextSteps": ["Review the demo artifacts.", "Delete the demo workspace when finished."],
        "heldActions": ["git commit", "git push", "pull request creation", "deployment", "publishing"]
    })
}

pub fn demo_draft(kind: relay_core::ArtifactKind) -> serde_json::Value {
    use relay_core::ArtifactKind;
    match kind {
        ArtifactKind::OpportunityBrief => demo_opportunity_brief(),
        ArtifactKind::Prd => demo_prd(),
        ArtifactKind::Trd => demo_trd(),
        ArtifactKind::ExecutionPlan => demo_execution_plan(),
        ArtifactKind::VerificationReport => demo_verification_report(),
        ArtifactKind::DeliveryReport => demo_delivery_report(),
        ArtifactKind::DecisionLog => demo_decision_log(),
    }
}

/// The fixture id and timestamp for one demo artifact (assigned by the
/// fixture exporter in exported-kind order).
pub fn demo_fixture_meta(kind: relay_core::ArtifactKind) -> (&'static str, &'static str) {
    use relay_core::ArtifactKind;
    match kind {
        ArtifactKind::DecisionLog => (
            "art_00000000-0000-4000-8000-000000000001",
            "2026-10-13T02:46:40.000Z",
        ),
        ArtifactKind::DeliveryReport => (
            "art_00000000-0000-4000-8000-000000000002",
            "2026-10-13T02:46:41.000Z",
        ),
        ArtifactKind::ExecutionPlan => (
            "art_00000000-0000-4000-8000-000000000003",
            "2026-10-13T02:46:42.000Z",
        ),
        ArtifactKind::OpportunityBrief => (
            "art_00000000-0000-4000-8000-000000000004",
            "2026-10-13T02:46:43.000Z",
        ),
        ArtifactKind::Prd => (
            "art_00000000-0000-4000-8000-000000000005",
            "2026-10-13T02:46:44.000Z",
        ),
        ArtifactKind::Trd => (
            "art_00000000-0000-4000-8000-000000000006",
            "2026-10-13T02:46:45.000Z",
        ),
        ArtifactKind::VerificationReport => (
            "art_00000000-0000-4000-8000-000000000007",
            "2026-10-13T02:46:46.000Z",
        ),
    }
}
