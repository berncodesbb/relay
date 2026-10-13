//! Golden tests: every view renders byte-stably, and the PRD round-trips
//! through the controlled import.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::ok_expect,
    clippy::err_expect
)] // test-only conveniences, house style

mod common;

use common::*;
use relay_domain::{DecisionId, EvidenceId, EvidenceOutcome};
use relay_workflows::{
    self as wf, BriefDoc, CriterionDoc, DeliveryDoc, DesignDecisionDoc, DesignDoc, PlanDoc,
    PlanTask, RequirementDoc, ResearchDoc, StructuredDoc, TaskKind, VerificationMethod,
};

fn docs() -> (
    RequirementDoc,
    [CriterionDoc; 2],
    DesignDoc,
    DesignDecisionDoc,
    PlanDoc,
    DeliveryDoc,
) {
    (
        RequirementDoc {
            slug: "csv-export".to_string(),
            statement: "Every team report exports as valid CSV.".to_string(),
            rationale: "Analysts live in spreadsheets.".to_string(),
            brief: vref("art_brief", 1),
        },
        [
            CriterionDoc {
                slug: "valid-rows".to_string(),
                requirement: vref("art_req-csv", 1),
                statement: "No malformed rows in any export.".to_string(),
                method: VerificationMethod::AutomatedTest,
            },
            CriterionDoc {
                slug: "headers-row".to_string(),
                requirement: vref("art_req-csv", 1),
                statement: "Exports carry a stable header row.".to_string(),
                method: VerificationMethod::Inspection,
            },
        ],
        DesignDoc {
            approach: "Stream rows through a validating writer".to_string(),
            components: vec!["src/csv/writer.rs".to_string()],
            risks: vec!["quoting edge cases".to_string()],
        },
        DesignDecisionDoc {
            slug: "csv-library".to_string(),
            question: "Which CSV writer?".to_string(),
            choice: "hand-rolled writer".to_string(),
            rationale: "control".to_string(),
            consequences: "we own the edge cases".to_string(),
        },
        PlanDoc {
            tasks: vec![PlanTask {
                slug: "csv-writer".to_string(),
                kind: TaskKind::Implement,
                objective: "Implement the CSV writer".to_string(),
                requirement_refs: vec![vref("art_req-csv", 1)],
                write_scopes: vec!["src".to_string()],
            }],
        },
        DeliveryDoc {
            summary: "CSV export".to_string(),
            includes: vec![vref("art_req-csv", 1)],
            evidence: vec![EvidenceId::parse("ev_pass0").ok().expect("id")],
            checks: vec!["evidence reviewed".to_string()],
        },
    )
}

#[test]
fn prd_renders_to_the_golden_bytes() {
    let (requirement, criteria, ..) = docs();
    let refs = (
        vref("art_brief", 1),
        vref("art_req-csv", 1),
        vref("art_cri-valid", 1),
        vref("art_cri-headers", 1),
    );
    let criterion_entries = [
        wf::CriterionEntry {
            reference: &refs.2,
            doc: &criteria[0],
            status: wf::CriterionStatus::Unevaluated,
            evidence: &[],
        },
        wf::CriterionEntry {
            reference: &refs.3,
            doc: &criteria[1],
            status: wf::CriterionStatus::Fail,
            evidence: &[(
                EvidenceId::parse("ev_1").ok().expect("id"),
                EvidenceOutcome::Fail,
            )],
        },
    ];
    let requirement_entries = [wf::RequirementEntry {
        reference: &refs.1,
        doc: &requirement,
        criteria: &criterion_entries,
    }];
    let golden = "\
# Product Requirements — export team reports as CSV

Brief: art_brief@1

## Requirement: csv-export — art_req-csv@1

Every team report exports as valid CSV.

Rationale: Analysts live in spreadsheets.

### Criterion: valid-rows — art_cri-valid@1 [unevaluated]

No malformed rows in any export.

Method: automated-test

### Criterion: headers-row — art_cri-headers@1 [fail]

Exports carry a stable header row.

Method: inspection

";
    let rendered = wf::render_prd(&wf::PrdView {
        goal: "export team reports as CSV",
        brief: Some(&refs.0),
        requirements: &requirement_entries,
    });
    assert_eq!(rendered, golden);
}

#[test]
fn every_view_renders_byte_stably() {
    let (requirement, criteria, design, decision, plan, delivery) = docs();
    let refs = (
        vref("art_brief", 1),
        vref("art_req-csv", 1),
        vref("art_cri-valid", 1),
        vref("art_cri-headers", 1),
    );

    let brief = BriefDoc {
        problem: "p".to_string(),
        audience: "a".to_string(),
        outcome: "o".to_string(),
        open_questions: vec!["q".to_string()],
    };
    let brief_view = wf::BriefView {
        goal: "g",
        reference: &refs.0,
        doc: &brief,
    };
    assert_eq!(wf::render_brief(&brief_view), wf::render_brief(&brief_view));

    let criterion_entries = [
        wf::CriterionEntry {
            reference: &refs.2,
            doc: &criteria[0],
            status: wf::CriterionStatus::Pass,
            evidence: &[],
        },
        wf::CriterionEntry {
            reference: &refs.3,
            doc: &criteria[1],
            status: wf::CriterionStatus::Unevaluated,
            evidence: &[],
        },
    ];
    let requirement_entries = [wf::RequirementEntry {
        reference: &refs.1,
        doc: &requirement,
        criteria: &criterion_entries,
    }];
    let prd_view = wf::PrdView {
        goal: "g",
        brief: Some(&refs.0),
        requirements: &requirement_entries,
    };
    assert_eq!(wf::render_prd(&prd_view), wf::render_prd(&prd_view));

    let design_ref = vref("art_design", 1);
    let decision_ref = vref("art_dec-csv", 1);
    let trd_view = wf::TrdView {
        reference: &design_ref,
        doc: &design,
        decisions: &[(&decision_ref, &decision)],
    };
    assert_eq!(wf::render_trd(&trd_view), wf::render_trd(&trd_view));

    let plan_view = wf::PlanView {
        reference: &design_ref,
        doc: &plan,
    };
    assert_eq!(wf::render_plan(&plan_view), wf::render_plan(&plan_view));

    let log_entry = wf::DecisionLogEntry {
        id: &DecisionId::parse("dec_q1").ok().expect("id"),
        question: "approve?",
        options: &["approve".to_string(), "revise".to_string()],
        resolution: Some("approve"),
        superseded_by: None,
    };
    let log_view = wf::DecisionLogView {
        entries: &[log_entry],
    };
    assert_eq!(
        wf::render_decision_log(&log_view),
        wf::render_decision_log(&log_view)
    );

    let report_view = wf::VerificationReportView {
        criteria: &criterion_entries,
        overall: "incomplete",
    };
    assert_eq!(
        wf::render_verification_report(&report_view),
        wf::render_verification_report(&report_view)
    );

    let delivery_view = wf::DeliveryReportView {
        reference: &vref("art_delivery", 1),
        doc: &delivery,
    };
    assert_eq!(
        wf::render_delivery_report(&delivery_view),
        wf::render_delivery_report(&delivery_view)
    );
}

#[test]
fn prd_renders_then_imports_back_to_the_same_documents() {
    let (requirement, criteria, ..) = docs();
    let refs = (
        vref("art_brief", 1),
        vref("art_req-csv", 1),
        vref("art_cri-valid", 1),
        vref("art_cri-headers", 1),
    );
    let criterion_entries = [
        wf::CriterionEntry {
            reference: &refs.2,
            doc: &criteria[0],
            status: wf::CriterionStatus::Unevaluated,
            evidence: &[],
        },
        wf::CriterionEntry {
            reference: &refs.3,
            doc: &criteria[1],
            status: wf::CriterionStatus::Unevaluated,
            evidence: &[],
        },
    ];
    let requirement_entries = [wf::RequirementEntry {
        reference: &refs.1,
        doc: &requirement,
        criteria: &criterion_entries,
    }];
    let markdown = wf::render_prd(&wf::PrdView {
        goal: "g",
        brief: Some(&refs.0),
        requirements: &requirement_entries,
    });

    let import = wf::import_prd(&markdown).ok().expect("round-trips");
    assert_eq!(import.requirements.len(), 1);
    let parsed = &import.requirements[0];
    assert_eq!(parsed.slug, requirement.slug);
    assert_eq!(parsed.reference, vref("art_req-csv", 1));
    assert_eq!(parsed.statement, requirement.statement);
    assert_eq!(parsed.rationale, requirement.rationale);
    assert_eq!(parsed.criteria.len(), 2);
    assert_eq!(parsed.criteria[0].slug, criteria[0].slug);
    assert_eq!(parsed.criteria[0].reference, vref("art_cri-valid", 1));
    assert_eq!(parsed.criteria[0].statement, criteria[0].statement);
    assert_eq!(parsed.criteria[0].method, criteria[0].method);
    assert_eq!(parsed.criteria[1].method, criteria[1].method);
}

#[test]
fn provider_flavors_change_bytes_not_documents() {
    let research = ResearchDoc {
        question: "q".to_string(),
        findings: vec!["f".to_string()],
        references: vec!["r".to_string()],
    };
    let alpha = FakeProvider::alpha().content(&research);
    let beta = FakeProvider::beta().content(&research);
    assert_ne!(
        serde_json::to_string(&alpha).ok().unwrap(),
        serde_json::to_string(&beta).ok().unwrap()
    );
    assert_eq!(
        ResearchDoc::from_content(&alpha).ok().expect("decodes"),
        ResearchDoc::from_content(&beta).ok().expect("decodes")
    );
}
