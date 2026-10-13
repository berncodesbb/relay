//! Structured, addressable artifact content schemas.
//!
//! Requirements, acceptance criteria, technical decisions, plan tasks,
//! research findings, briefs, and delivery reports serialize into
//! [`ArtifactContent`] payloads carrying an explicit `"schema"` tag, so
//! every artifact version is self-describing and validated at the boundary.
//! The Markdown views in [`crate::render`] are deterministic projections of
//! these structures — never the reverse, except through the controlled
//! import in [`crate::import`].

use relay_domain::{ArtifactContent, ArtifactVersionRef, EvidenceId};

/// Schema tag of the opportunity brief.
pub const BRIEF_SCHEMA: &str = "relay.brief@1";
/// Schema tag of a requirement.
pub const REQUIREMENT_SCHEMA: &str = "relay.requirement@1";
/// Schema tag of an acceptance criterion.
pub const CRITERION_SCHEMA: &str = "relay.criterion@1";
/// Schema tag of the technical design.
pub const DESIGN_SCHEMA: &str = "relay.design@1";
/// Schema tag of a recorded technical decision.
pub const DESIGN_DECISION_SCHEMA: &str = "relay.design-decision@1";
/// Schema tag of the execution plan.
pub const PLAN_SCHEMA: &str = "relay.plan@1";
/// Schema tag of research findings.
pub const RESEARCH_SCHEMA: &str = "relay.research@1";
/// Schema tag of the delivery report.
pub const DELIVERY_SCHEMA: &str = "relay.delivery@1";

/// Error raised when artifact content does not match a schema.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SchemaError {
    /// The content is not a JSON object with a schema tag.
    #[error("content is not a schema-tagged object")]
    NotTagged,
    /// The schema tag names a different schema than expected.
    #[error("content schema is {found:?}, expected {expected:?}")]
    WrongSchema {
        /// The tag found in the content.
        found: String,
        /// The tag this reader expected.
        expected: &'static str,
    },
    /// The tagged content does not decode into the schema's shape.
    #[error("content does not decode as {schema:?}: {detail}")]
    Malformed {
        /// The schema tag that was expected.
        schema: &'static str,
        /// What was wrong.
        detail: String,
    },
}

/// A structured artifact document: serializable into (and decodable from)
/// schema-tagged [`ArtifactContent`].
pub trait StructuredDoc: serde::Serialize + serde::de::DeserializeOwned + Sized {
    /// This document's schema tag.
    const SCHEMA: &'static str;

    /// Serialize into schema-tagged artifact content.
    fn to_content(&self) -> ArtifactContent {
        let mut value = serde_json::to_value(self).unwrap_or(serde_json::Value::Null);
        if let serde_json::Value::Object(map) = &mut value {
            map.insert(
                "schema".to_string(),
                serde_json::Value::String(Self::SCHEMA.to_string()),
            );
        }
        ArtifactContent(value)
    }

    /// Decode from schema-tagged artifact content, failing closed on a
    /// foreign tag or malformed shape.
    fn from_content(content: &ArtifactContent) -> Result<Self, SchemaError> {
        let malformed = |detail: String| SchemaError::Malformed {
            schema: Self::SCHEMA,
            detail,
        };
        let serde_json::Value::Object(map) = &content.0 else {
            return Err(SchemaError::NotTagged);
        };
        let Some(serde_json::Value::String(found)) = map.get("schema") else {
            return Err(SchemaError::NotTagged);
        };
        if found != Self::SCHEMA {
            return Err(SchemaError::WrongSchema {
                found: found.clone(),
                expected: Self::SCHEMA,
            });
        }
        serde_json::from_value(content.0.clone()).map_err(|error| malformed(error.to_string()))
    }
}

macro_rules! structured_doc {
    ($ty:ty, $schema:expr) => {
        impl StructuredDoc for $ty {
            const SCHEMA: &'static str = $schema;
        }
    };
}

/// The schema tag carried by a content payload, when present.
pub fn content_schema(content: &ArtifactContent) -> Option<&str> {
    content.0.get("schema").and_then(|value| value.as_str())
}

/// The opportunity brief: what discovery produces and a human approves.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BriefDoc {
    /// The problem, in one or two sentences.
    pub problem: String,
    /// Who the outcome is for.
    pub audience: String,
    /// The outcome wanted.
    pub outcome: String,
    /// Focused questions still needing a human answer.
    pub open_questions: Vec<String>,
}

structured_doc!(BriefDoc, BRIEF_SCHEMA);

/// One requirement, addressable as its own versioned artifact.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RequirementDoc {
    /// A short stable slug (e.g. `csv-export`).
    pub slug: String,
    /// The requirement statement.
    pub statement: String,
    /// Why it matters.
    pub rationale: String,
    /// The brief version this requirement was derived from.
    pub brief: ArtifactVersionRef,
}

structured_doc!(RequirementDoc, REQUIREMENT_SCHEMA);

/// How a criterion is verified.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum VerificationMethod {
    /// An automated test or command.
    AutomatedTest,
    /// A manual check by a person.
    ManualCheck,
    /// Reading the repository or artifact.
    Inspection,
}

/// One acceptance criterion, addressable as its own versioned artifact and
/// the target of verification evidence.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CriterionDoc {
    /// A short stable slug (e.g. `valid-rows`).
    pub slug: String,
    /// The requirement version this criterion belongs to.
    pub requirement: ArtifactVersionRef,
    /// The checkable statement.
    pub statement: String,
    /// How it is verified.
    pub method: VerificationMethod,
}

structured_doc!(CriterionDoc, CRITERION_SCHEMA);

/// The technical design.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DesignDoc {
    /// The chosen approach, in prose.
    pub approach: String,
    /// The components or areas it touches.
    pub components: Vec<String>,
    /// Risks worth remembering.
    pub risks: Vec<String>,
}

structured_doc!(DesignDoc, DESIGN_SCHEMA);

/// One recorded technical decision. These are design-time records, distinct
/// from the graph's human-gate [`relay_domain::Decision`] entities: a
/// recorded choice, not a blocking question.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DesignDecisionDoc {
    /// A short stable slug.
    pub slug: String,
    /// The question the decision answers.
    pub question: String,
    /// The choice made.
    pub choice: String,
    /// Why.
    pub rationale: String,
    /// What this choice commits us to.
    pub consequences: String,
}

structured_doc!(DesignDecisionDoc, DESIGN_DECISION_SCHEMA);

/// What kind of plan task it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TaskKind {
    /// Repository implementation work.
    Implement,
    /// Integration of completed tasks.
    Integrate,
    /// Corrective work responding to failing evidence.
    Correct,
}

/// One bounded task in the execution plan.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PlanTask {
    /// A short stable slug unique within the plan.
    pub slug: String,
    /// The task's kind.
    pub kind: TaskKind,
    /// The bounded objective.
    pub objective: String,
    /// The requirement versions this task helps satisfy (drives
    /// verification edges).
    pub requirement_refs: Vec<ArtifactVersionRef>,
    /// Workspace areas the task will write.
    pub write_scopes: Vec<String>,
}

/// The execution plan: a decomposition of the design into tasks.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PlanDoc {
    /// The tasks, in plan order.
    pub tasks: Vec<PlanTask>,
}

structured_doc!(PlanDoc, PLAN_SCHEMA);

/// Research findings from read-only inspection.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ResearchDoc {
    /// The question researched.
    pub question: String,
    /// The findings, in order.
    pub findings: Vec<String>,
    /// What was read.
    pub references: Vec<String>,
}

structured_doc!(ResearchDoc, RESEARCH_SCHEMA);

/// The delivery package description: what ships and what proves it.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DeliveryDoc {
    /// One-line summary of what is being delivered.
    pub summary: String,
    /// The artifact versions included in the package.
    pub includes: Vec<ArtifactVersionRef>,
    /// The evidence proving the criteria.
    pub evidence: Vec<EvidenceId>,
    /// Final pre-delivery checks performed.
    pub checks: Vec<String>,
}

structured_doc!(DeliveryDoc, DELIVERY_SCHEMA);

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::ok_expect,
    clippy::err_expect
)] // test-only conveniences, house style
mod tests {
    use super::*;

    fn brief() -> BriefDoc {
        BriefDoc {
            problem: "triage is slow".to_string(),
            audience: "support agents".to_string(),
            outcome: "one-screen triage".to_string(),
            open_questions: vec!["which channel?".to_string()],
        }
    }

    #[test]
    fn docs_round_trip_through_tagged_content() {
        let doc = brief();
        let content = doc.to_content();
        assert_eq!(content_schema(&content), Some(BRIEF_SCHEMA));
        let back = BriefDoc::from_content(&content).ok().expect("decodes");
        assert_eq!(back, doc);
    }

    #[test]
    fn wrong_schema_and_untagged_content_fail_closed() {
        let content = brief().to_content();
        let error = RequirementDoc::from_content(&content)
            .err()
            .expect("rejects foreign schema");
        assert!(matches!(error, SchemaError::WrongSchema { .. }));
        let error =
            BriefDoc::from_content(&ArtifactContent::new(serde_json::json!({"problem": "x"})))
                .err()
                .expect("rejects untagged content");
        assert!(matches!(error, SchemaError::NotTagged));
        let error = BriefDoc::from_content(&ArtifactContent::new(
            serde_json::json!({"schema": BRIEF_SCHEMA}),
        ))
        .err()
        .expect("rejects missing fields");
        assert!(matches!(error, SchemaError::Malformed { .. }));
    }
}
