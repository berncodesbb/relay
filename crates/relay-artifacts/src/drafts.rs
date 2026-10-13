//! Model-facing artifact draft schemas.
//!
//! Every artifact Relay persists starts life as a draft proposed by a model
//! and is therefore untrusted. This module is the single validation gate
//! before any artifact is rendered or written. It is a port of
//! `src/artifacts/schemas.ts` (Zod v4): same field names (camelCase in JSON),
//! same bounds, same optionality, and the same Zod-v4 issue text — `path:
//! message` with the path rendered as dotted segments (or `(root)`), so the
//! controller can feed identical diagnostics back to models.
//!
//! Validation semantics ported from Zod v4:
//!
//! * a value of the wrong type produces exactly one `Invalid input: expected
//!   <type>, received <type>` issue and aborts that field;
//! * a string of the right type accumulates its bound checks in definition
//!   order (`Too small` / `Too big`) and then its refine/regex check, so one
//!   field can emit several issues;
//! * an array of the wrong type aborts; an array that violates its size bound
//!   reports only the size issue (elements are not inspected);
//! * object fields are visited in schema definition order and unknown keys are
//!   stripped (never trusted);
//! * string lengths are counted in Unicode scalar values, matching Zod v4.

use relay_core::{ARTIFACT_KINDS, ArtifactKind};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::error::{ArtifactsError, InvalidArtifactDraftError, UnknownArtifactKindError};

/// Shared bounds documentation lives in the schema constants below; titles are
/// single-line so they stay renderable as headings.
const TITLE_MAX: usize = 200;
const TITLE_MIN: usize = 1;
const PROSE_MAX: usize = 1000;
const IDENTIFIER_MAX: usize = 100;

/// `formatZodIssues`: render `path: message` strings with a `(root)` fallback.
pub fn format_zod_issues(entries: &[(String, String)]) -> Vec<String> {
    entries
        .iter()
        .map(|(path, message)| {
            if path.is_empty() {
                format!("(root): {message}")
            } else {
                format!("{path}: {message}")
            }
        })
        .collect()
}

/// Accumulating issue list, mirroring the raw Zod issues before formatting.
#[derive(Debug, Default)]
struct DraftIssues {
    entries: Vec<(String, String)>,
}

impl DraftIssues {
    fn push(&mut self, path: &str, message: impl Into<String>) {
        self.entries.push((path.to_string(), message.into()));
    }

    fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn into_strings(self) -> Vec<String> {
        format_zod_issues(&self.entries)
    }
}

/// Zod's `received` descriptor for a JSON value (`None` = missing key).
fn received(value: Option<&Value>) -> &'static str {
    match value {
        None => "undefined",
        Some(Value::Null) => "null",
        Some(Value::Bool(_)) => "boolean",
        Some(Value::Number(_)) => "number",
        Some(Value::String(_)) => "string",
        Some(Value::Array(_)) => "array",
        Some(Value::Object(_)) => "object",
    }
}

fn type_issue(issues: &mut DraftIssues, path: &str, expected: &str, value: Option<&Value>) {
    issues.push(
        path,
        format!(
            "Invalid input: expected {expected}, received {}",
            received(value)
        ),
    );
}

// -- field validators --------------------------------------------------------

/// Validate a required string field with `min`/`max` bounds (bounds are
/// inclusive; `min`/`max` of 0 means "no check" for that side is expressed by
/// passing the schema's actual bound — Zod strings here always carry both
/// except the max-only fields handled by [`req_string_max_only`]).
fn req_string(
    map: &Map<String, Value>,
    key: &str,
    min: usize,
    max: usize,
    path: &str,
    issues: &mut DraftIssues,
) -> Option<String> {
    let Some(Value::String(text)) = map.get(key) else {
        type_issue(issues, path, "string", map.get(key));
        return None;
    };
    string_bounds(text, min, max, path, issues);
    Some(text.clone())
}

/// Validate a required string field that only carries an upper bound.
fn req_string_max_only(
    map: &Map<String, Value>,
    key: &str,
    max: usize,
    path: &str,
    issues: &mut DraftIssues,
) -> Option<String> {
    let Some(Value::String(text)) = map.get(key) else {
        type_issue(issues, path, "string", map.get(key));
        return None;
    };
    string_bounds(text, 0, max, path, issues);
    Some(text.clone())
}

/// Emit `Too small` / `Too big` issues for a string whose type already
/// matches. Both checks run (they cannot both fail), matching Zod stacking.
fn string_bounds(text: &str, min: usize, max: usize, path: &str, issues: &mut DraftIssues) {
    let len = text.chars().count();
    if min > 0 && len < min {
        issues.push(
            path,
            format!("Too small: expected string to have >={min} characters"),
        );
    }
    if max > 0 && len > max {
        issues.push(
            path,
            format!("Too big: expected string to have <={max} characters"),
        );
    }
}

/// The shared `titleSchema` check chain: bounds plus the single-line refine.
fn title_field(map: &Map<String, Value>, path: &str, issues: &mut DraftIssues) -> Option<String> {
    let Some(Value::String(text)) = map.get("title") else {
        type_issue(issues, path, "string", map.get("title"));
        return None;
    };
    string_bounds(text, TITLE_MIN, TITLE_MAX, path, issues);
    if text.contains('\r') || text.contains('\n') {
        issues.push(path, "title must be a single line");
    }
    Some(text.clone())
}

/// `identifierSchema`: uppercase letters, digits, dashes only. Single-pass
/// check chain (type, bounds, regex) so no bound issue is reported twice.
fn identifier_string(
    map: &Map<String, Value>,
    key: &str,
    path: &str,
    issues: &mut DraftIssues,
) -> Option<String> {
    let Some(Value::String(text)) = map.get(key) else {
        type_issue(issues, path, "string", map.get(key));
        return None;
    };
    identifier_field(text, path, issues);
    Some(text.clone())
}

/// `identifierSchema` bounds + regex for an element string.
fn identifier_field(text: &str, path: &str, issues: &mut DraftIssues) {
    string_bounds(text, 1, IDENTIFIER_MAX, path, issues);
    if !is_identifier(text) {
        issues.push(
            path,
            "identifier must contain only uppercase letters, digits, and dashes (e.g. \"REQ-1\")",
        );
    }
}

fn is_identifier(text: &str) -> bool {
    !text.is_empty()
        && text
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'-')
}

/// `identifierReferenceSchema` (dependencies between tasks).
fn dependency_field(text: &str, path: &str, issues: &mut DraftIssues) {
    string_bounds(text, 1, IDENTIFIER_MAX, path, issues);
    if !is_identifier(text) {
        issues.push(
            path,
            "dependency must be a task identifier (uppercase letters, digits, dashes)",
        );
    }
}

/// `workspaceRelativePathSchema`: not absolute (either separator), no drive
/// letters, no `~`, no `..` segments. Emits at most one issue.
fn workspace_relative_path_field(text: &str, path: &str, issues: &mut DraftIssues) {
    string_bounds(text, 1, 500, path, issues);
    if text.starts_with('/') || text.starts_with('\\') {
        issues.push(
            path,
            format!("affected file \"{text}\" must be workspace-relative, not absolute"),
        );
        return;
    }
    let mut chars = text.chars();
    if let (Some(first), Some(':')) = (chars.next(), chars.next())
        && first.is_ascii_alphabetic()
    {
        issues.push(
            path,
            format!("affected file \"{text}\" looks like an absolute path with a drive letter"),
        );
        return;
    }
    if text.starts_with('~') {
        issues.push(
            path,
            format!("affected file \"{text}\" must not reference the home directory"),
        );
        return;
    }
    if text.split(['/', '\\']).any(|segment| segment == "..") {
        issues.push(
            path,
            format!("affected file \"{text}\" must not contain \"..\" segments"),
        );
    }
}

/// A required string array with item bounds. Size failures abort element
/// validation, matching Zod array semantics.
#[allow(clippy::too_many_arguments)]
fn req_string_array(
    map: &Map<String, Value>,
    key: &str,
    min_items: usize,
    max_items: usize,
    item_min: usize,
    item_max: usize,
    path: &str,
    issues: &mut DraftIssues,
) -> Option<Vec<String>> {
    req_string_array_with(
        map, key, min_items, max_items, item_min, item_max, path, issues, None,
    )
}

/// Like [`req_string_array`], with an optional per-element refine (bounds then
/// refine, element by element — matching Zod's per-element check order).
#[allow(clippy::too_many_arguments)]
fn req_string_array_with(
    map: &Map<String, Value>,
    key: &str,
    min_items: usize,
    max_items: usize,
    item_min: usize,
    item_max: usize,
    path: &str,
    issues: &mut DraftIssues,
    refine: Option<fn(&str, &str, &mut DraftIssues)>,
) -> Option<Vec<String>> {
    let Some(Value::Array(items)) = map.get(key) else {
        type_issue(issues, path, "array", map.get(key));
        return None;
    };
    if min_items > 0 && items.len() < min_items {
        issues.push(
            path,
            format!("Too small: expected array to have >={min_items} items"),
        );
        return None;
    }
    if max_items > 0 && items.len() > max_items {
        issues.push(
            path,
            format!("Too big: expected array to have <={max_items} items"),
        );
        return None;
    }
    let mut out = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        let item_path = format!("{path}.{index}");
        let Value::String(text) = item else {
            type_issue(issues, &item_path, "string", Some(item));
            continue;
        };
        string_bounds(text, item_min, item_max, &item_path, issues);
        if let Some(refine) = refine {
            refine(text, &item_path, issues);
        }
        out.push(text.clone());
    }
    Some(out)
}

/// A required array of objects validated by `validate_item` (which pushes its
/// own nested issues). Size failures abort element validation.
fn req_object_array<T>(
    map: &Map<String, Value>,
    key: &str,
    min_items: usize,
    max_items: usize,
    path: &str,
    issues: &mut DraftIssues,
    validate_item: fn(&Map<String, Value>, &str, &mut DraftIssues) -> Option<T>,
) -> Option<Vec<T>> {
    let Some(Value::Array(items)) = map.get(key) else {
        type_issue(issues, path, "array", map.get(key));
        return None;
    };
    if min_items > 0 && items.len() < min_items {
        issues.push(
            path,
            format!("Too small: expected array to have >={min_items} items"),
        );
        return None;
    }
    if max_items > 0 && items.len() > max_items {
        issues.push(
            path,
            format!("Too big: expected array to have <={max_items} items"),
        );
        return None;
    }
    let mut out = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        let item_path = format!("{path}.{index}");
        let Value::Object(entry) = item else {
            type_issue(issues, &item_path, "object", Some(item));
            continue;
        };
        if let Some(parsed) = validate_item(entry, &item_path, issues) {
            out.push(parsed);
        }
    }
    Some(out)
}

fn req_bool(
    map: &Map<String, Value>,
    key: &str,
    path: &str,
    issues: &mut DraftIssues,
) -> Option<bool> {
    let value = map.get(key);
    match value {
        Some(Value::Bool(flag)) => Some(*flag),
        other => {
            type_issue(issues, path, "boolean", other);
            None
        }
    }
}

// -- draft types -------------------------------------------------------------

/// One target-user persona in an opportunity brief.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetUser {
    /// Persona name.
    pub name: String,
    /// Who this user is.
    pub description: String,
}

/// One piece of evidence cited in an opportunity brief.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Evidence {
    /// What the evidence says.
    pub description: String,
    /// Where it came from (may be empty).
    pub source: String,
}

/// Discovery output: problem, users, evidence, outcomes, assumptions,
/// constraints, and open questions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpportunityBriefDraft {
    /// Single-line title.
    pub title: String,
    /// The problem statement.
    pub problem: String,
    /// Target user personas (1..=12).
    pub target_users: Vec<TargetUser>,
    /// Supporting evidence (0..=20).
    pub evidence: Vec<Evidence>,
    /// Desired outcomes (1..=12).
    pub desired_outcomes: Vec<String>,
    /// Assumptions (0..=20).
    pub assumptions: Vec<String>,
    /// Constraints (0..=20).
    pub constraints: Vec<String>,
    /// Open questions (0..=20).
    pub open_questions: Vec<String>,
}

/// One testable requirement in a PRD.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Requirement {
    /// Requirement identifier (`REQ-1`).
    pub id: String,
    /// The requirement statement.
    pub statement: String,
    /// Why the requirement exists.
    pub rationale: String,
}

/// One Given/When/Then acceptance criterion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcceptanceCriterion {
    /// Criterion identifier (`AC-1`).
    pub id: String,
    /// Precondition.
    pub given: String,
    /// Trigger.
    pub when: String,
    /// Expected outcome.
    pub then: String,
}

/// Specification output: testable requirements with acceptance criteria.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrdDraft {
    /// Single-line title.
    pub title: String,
    /// Summary of the specification.
    pub summary: String,
    /// Requirements (1..=40).
    pub requirements: Vec<Requirement>,
    /// Explicitly out-of-scope items (0..=30).
    pub exclusions: Vec<String>,
    /// Acceptance criteria (1..=40).
    pub acceptance_criteria: Vec<AcceptanceCriterion>,
}

/// One repository finding recorded while designing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepositoryFinding {
    /// Area of the repository.
    pub area: String,
    /// What was found.
    pub finding: String,
}

/// One interface introduced or changed by the design.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Interface {
    /// Interface name.
    pub name: String,
    /// What it does.
    pub description: String,
}

/// One data or schema migration called out by the design.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Migration {
    /// What the migration does.
    pub description: String,
    /// Risk assessment.
    pub risk: String,
}

/// One risk with its mitigation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Risk {
    /// The risk.
    pub risk: String,
    /// How it is mitigated.
    pub mitigation: String,
}

/// Technical-design output: architecture, interfaces, migrations, risks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrdDraft {
    /// Single-line title.
    pub title: String,
    /// Summary of the design.
    pub summary: String,
    /// Repository findings (0..=30).
    pub repository_findings: Vec<RepositoryFinding>,
    /// The architecture description.
    pub architecture: String,
    /// Interfaces (0..=30).
    pub interfaces: Vec<Interface>,
    /// Migrations (0..=20).
    pub migrations: Vec<Migration>,
    /// Risks (0..=20).
    pub risks: Vec<Risk>,
    /// Rollout plan.
    pub rollout_plan: String,
    /// Rollback plan.
    pub rollback_plan: String,
}

/// One executable task in the plan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanTask {
    /// Task identifier (`T-1`).
    pub id: String,
    /// Short task title.
    pub title: String,
    /// What the task does.
    pub description: String,
    /// Task ids this task depends on (0..=10).
    pub dependencies: Vec<String>,
    /// Workspace-relative files the task touches (0..=50).
    pub affected_files: Vec<String>,
    /// Commands that verify the task (0..=10).
    pub checks: Vec<String>,
    /// Integration points (0..=10).
    pub integration_points: Vec<String>,
}

/// Planning output: the executable task graph.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionPlanDraft {
    /// Single-line title.
    pub title: String,
    /// Summary of the plan.
    pub summary: String,
    /// Tasks (1..=50).
    pub tasks: Vec<PlanTask>,
}

/// Verification status for one acceptance criterion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VerificationStatus {
    /// The criterion was evaluated and holds.
    Pass,
    /// The criterion was evaluated and does not hold.
    Fail,
    /// The criterion could not be evaluated.
    Unevaluated,
}

impl VerificationStatus {
    /// The wire spelling (also the table cell text in rendered reports).
    pub fn as_str(self) -> &'static str {
        match self {
            VerificationStatus::Pass => "pass",
            VerificationStatus::Fail => "fail",
            VerificationStatus::Unevaluated => "unevaluated",
        }
    }
}

/// One criterion evaluation in a verification report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationResult {
    /// Acceptance-criterion id (may be empty).
    pub criterion_id: String,
    /// The criterion text.
    pub criterion: String,
    /// Evaluation outcome.
    pub status: VerificationStatus,
    /// Supporting evidence.
    pub evidence: String,
}

/// Verification output: criterion-by-criterion evaluation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationReportDraft {
    /// Single-line title.
    pub title: String,
    /// Summary of the verification run.
    pub summary: String,
    /// Criterion results (1..=60).
    pub results: Vec<VerificationResult>,
    /// Whether every evaluated criterion passed.
    pub overall_pass: bool,
    /// Free-form notes (may be empty).
    pub notes: String,
}

/// One requirement-to-evidence trace entry in a delivery report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceEntry {
    /// Requirement id (may be empty).
    pub requirement_id: String,
    /// The requirement text.
    pub requirement: String,
    /// Design decisions behind the change (0..=10).
    pub design_decisions: Vec<String>,
    /// Changes made (0..=30).
    pub changes: Vec<String>,
    /// Verification evidence (0..=20).
    pub evidence: Vec<String>,
}

/// Delivery output: the requirement-to-evidence trace plus held actions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeliveryReportDraft {
    /// Single-line title.
    pub title: String,
    /// Summary of the delivery.
    pub summary: String,
    /// Requirement trace (1..=60).
    pub trace: Vec<TraceEntry>,
    /// Suggested next steps (0..=20).
    pub next_steps: Vec<String>,
    /// Actions intentionally NOT performed — commit, push, deploy, publish
    /// (1..=20).
    pub held_actions: Vec<String>,
}

/// One recorded decision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionEntry {
    /// The decision.
    pub decision: String,
    /// Why it was made.
    pub rationale: String,
    /// Alternatives considered (may be empty).
    pub alternatives: String,
    /// The workflow mode in which it was made.
    pub mode: String,
}

/// Implementation output: the record of decisions and evidence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionLogDraft {
    /// Single-line title.
    pub title: String,
    /// Decision entries (1..=100).
    pub entries: Vec<DecisionEntry>,
}

/// A validated draft payload for one of the seven artifact kinds.
#[derive(Debug, Clone, PartialEq)]
pub enum ArtifactDraft {
    /// Discovery output.
    OpportunityBrief(OpportunityBriefDraft),
    /// Implementation output.
    DecisionLog(DecisionLogDraft),
    /// Specification output.
    Prd(PrdDraft),
    /// Technical-design output.
    Trd(TrdDraft),
    /// Planning output.
    ExecutionPlan(ExecutionPlanDraft),
    /// Verification output.
    VerificationReport(VerificationReportDraft),
    /// Delivery output.
    DeliveryReport(DeliveryReportDraft),
}

impl ArtifactDraft {
    /// The artifact kind this draft belongs to.
    pub fn kind(&self) -> ArtifactKind {
        match self {
            ArtifactDraft::OpportunityBrief(_) => ArtifactKind::OpportunityBrief,
            ArtifactDraft::DecisionLog(_) => ArtifactKind::DecisionLog,
            ArtifactDraft::Prd(_) => ArtifactKind::Prd,
            ArtifactDraft::Trd(_) => ArtifactKind::Trd,
            ArtifactDraft::ExecutionPlan(_) => ArtifactKind::ExecutionPlan,
            ArtifactDraft::VerificationReport(_) => ArtifactKind::VerificationReport,
            ArtifactDraft::DeliveryReport(_) => ArtifactKind::DeliveryReport,
        }
    }

    /// The draft's single-line title (used for filenames and frontmatter).
    pub fn title(&self) -> &str {
        match self {
            ArtifactDraft::OpportunityBrief(draft) => &draft.title,
            ArtifactDraft::DecisionLog(draft) => &draft.title,
            ArtifactDraft::Prd(draft) => &draft.title,
            ArtifactDraft::Trd(draft) => &draft.title,
            ArtifactDraft::ExecutionPlan(draft) => &draft.title,
            ArtifactDraft::VerificationReport(draft) => &draft.title,
            ArtifactDraft::DeliveryReport(draft) => &draft.title,
        }
    }
}

// -- per-kind validation -----------------------------------------------------

fn validate_object_root<'a>(
    value: &'a Value,
    issues: &mut DraftIssues,
) -> Option<&'a Map<String, Value>> {
    match value {
        Value::Object(map) => Some(map),
        other => {
            type_issue(issues, "", "object", Some(other));
            None
        }
    }
}

fn validate_target_user(
    map: &Map<String, Value>,
    path: &str,
    issues: &mut DraftIssues,
) -> Option<TargetUser> {
    let name = req_string(map, "name", 1, 120, &format!("{path}.name"), issues);
    let description = req_string(
        map,
        "description",
        1,
        1000,
        &format!("{path}.description"),
        issues,
    );
    match (name, description) {
        (Some(name), Some(description)) => Some(TargetUser { name, description }),
        _ => None,
    }
}

fn validate_evidence(
    map: &Map<String, Value>,
    path: &str,
    issues: &mut DraftIssues,
) -> Option<Evidence> {
    let description = req_string(
        map,
        "description",
        1,
        1000,
        &format!("{path}.description"),
        issues,
    );
    let source = req_string_max_only(map, "source", 500, &format!("{path}.source"), issues);
    match (description, source) {
        (Some(description), Some(source)) => Some(Evidence {
            description,
            source,
        }),
        _ => None,
    }
}

fn validate_opportunity_brief(
    value: &Value,
    issues: &mut DraftIssues,
) -> Option<OpportunityBriefDraft> {
    let map = validate_object_root(value, issues)?;
    let title = title_field(map, "title", issues);
    let problem = req_string(map, "problem", 1, 4000, "problem", issues);
    let target_users = req_object_array(
        map,
        "targetUsers",
        1,
        12,
        "targetUsers",
        issues,
        validate_target_user,
    );
    let evidence = req_object_array(
        map,
        "evidence",
        0,
        20,
        "evidence",
        issues,
        validate_evidence,
    );
    let desired_outcomes = req_string_array(
        map,
        "desiredOutcomes",
        1,
        12,
        1,
        500,
        "desiredOutcomes",
        issues,
    );
    let assumptions = req_string_array(
        map,
        "assumptions",
        0,
        20,
        1,
        PROSE_MAX,
        "assumptions",
        issues,
    );
    let constraints = req_string_array(
        map,
        "constraints",
        0,
        20,
        1,
        PROSE_MAX,
        "constraints",
        issues,
    );
    let open_questions = req_string_array(
        map,
        "openQuestions",
        0,
        20,
        1,
        PROSE_MAX,
        "openQuestions",
        issues,
    );
    Some(OpportunityBriefDraft {
        title: title?,
        problem: problem?,
        target_users: target_users?,
        evidence: evidence?,
        desired_outcomes: desired_outcomes?,
        assumptions: assumptions?,
        constraints: constraints?,
        open_questions: open_questions?,
    })
}

fn validate_requirement(
    map: &Map<String, Value>,
    path: &str,
    issues: &mut DraftIssues,
) -> Option<Requirement> {
    let id = identifier_string(map, "id", &format!("{path}.id"), issues);
    let statement = req_string(
        map,
        "statement",
        1,
        1000,
        &format!("{path}.statement"),
        issues,
    );
    let rationale = req_string(
        map,
        "rationale",
        1,
        1000,
        &format!("{path}.rationale"),
        issues,
    );
    match (id, statement, rationale) {
        (Some(id), Some(statement), Some(rationale)) => Some(Requirement {
            id,
            statement,
            rationale,
        }),
        _ => None,
    }
}

fn validate_acceptance_criterion(
    map: &Map<String, Value>,
    path: &str,
    issues: &mut DraftIssues,
) -> Option<AcceptanceCriterion> {
    let id = identifier_string(map, "id", &format!("{path}.id"), issues);
    let given = req_string(map, "given", 1, 500, &format!("{path}.given"), issues);
    let when = req_string(map, "when", 1, 500, &format!("{path}.when"), issues);
    let then = req_string(map, "then", 1, 500, &format!("{path}.then"), issues);
    match (id, given, when, then) {
        (Some(id), Some(given), Some(when), Some(then)) => Some(AcceptanceCriterion {
            id,
            given,
            when,
            then,
        }),
        _ => None,
    }
}

fn validate_prd(value: &Value, issues: &mut DraftIssues) -> Option<PrdDraft> {
    let map = validate_object_root(value, issues)?;
    let title = title_field(map, "title", issues);
    let summary = req_string(map, "summary", 1, 4000, "summary", issues);
    let requirements = req_object_array(
        map,
        "requirements",
        1,
        40,
        "requirements",
        issues,
        validate_requirement,
    );
    let exclusions = req_string_array(map, "exclusions", 0, 30, 1, 500, "exclusions", issues);
    let acceptance_criteria = req_object_array(
        map,
        "acceptanceCriteria",
        1,
        40,
        "acceptanceCriteria",
        issues,
        validate_acceptance_criterion,
    );
    Some(PrdDraft {
        title: title?,
        summary: summary?,
        requirements: requirements?,
        exclusions: exclusions?,
        acceptance_criteria: acceptance_criteria?,
    })
}

fn validate_repository_finding(
    map: &Map<String, Value>,
    path: &str,
    issues: &mut DraftIssues,
) -> Option<RepositoryFinding> {
    let area = req_string(map, "area", 1, 200, &format!("{path}.area"), issues);
    let finding = req_string(map, "finding", 1, 1000, &format!("{path}.finding"), issues);
    match (area, finding) {
        (Some(area), Some(finding)) => Some(RepositoryFinding { area, finding }),
        _ => None,
    }
}

fn validate_interface(
    map: &Map<String, Value>,
    path: &str,
    issues: &mut DraftIssues,
) -> Option<Interface> {
    let name = req_string(map, "name", 1, 200, &format!("{path}.name"), issues);
    let description = req_string(
        map,
        "description",
        1,
        1000,
        &format!("{path}.description"),
        issues,
    );
    match (name, description) {
        (Some(name), Some(description)) => Some(Interface { name, description }),
        _ => None,
    }
}

fn validate_migration(
    map: &Map<String, Value>,
    path: &str,
    issues: &mut DraftIssues,
) -> Option<Migration> {
    let description = req_string(
        map,
        "description",
        1,
        1000,
        &format!("{path}.description"),
        issues,
    );
    let risk = req_string(map, "risk", 1, 500, &format!("{path}.risk"), issues);
    match (description, risk) {
        (Some(description), Some(risk)) => Some(Migration { description, risk }),
        _ => None,
    }
}

fn validate_risk(map: &Map<String, Value>, path: &str, issues: &mut DraftIssues) -> Option<Risk> {
    let risk = req_string(map, "risk", 1, 500, &format!("{path}.risk"), issues);
    let mitigation = req_string(
        map,
        "mitigation",
        1,
        1000,
        &format!("{path}.mitigation"),
        issues,
    );
    match (risk, mitigation) {
        (Some(risk), Some(mitigation)) => Some(Risk { risk, mitigation }),
        _ => None,
    }
}

fn validate_trd(value: &Value, issues: &mut DraftIssues) -> Option<TrdDraft> {
    let map = validate_object_root(value, issues)?;
    let title = title_field(map, "title", issues);
    let summary = req_string(map, "summary", 1, 4000, "summary", issues);
    let repository_findings = req_object_array(
        map,
        "repositoryFindings",
        0,
        30,
        "repositoryFindings",
        issues,
        validate_repository_finding,
    );
    let architecture = req_string(map, "architecture", 1, 8000, "architecture", issues);
    let interfaces = req_object_array(
        map,
        "interfaces",
        0,
        30,
        "interfaces",
        issues,
        validate_interface,
    );
    let migrations = req_object_array(
        map,
        "migrations",
        0,
        20,
        "migrations",
        issues,
        validate_migration,
    );
    let risks = req_object_array(map, "risks", 0, 20, "risks", issues, validate_risk);
    let rollout_plan = req_string(map, "rolloutPlan", 1, 4000, "rolloutPlan", issues);
    let rollback_plan = req_string(map, "rollbackPlan", 1, 2000, "rollbackPlan", issues);
    Some(TrdDraft {
        title: title?,
        summary: summary?,
        repository_findings: repository_findings?,
        architecture: architecture?,
        interfaces: interfaces?,
        migrations: migrations?,
        risks: risks?,
        rollout_plan: rollout_plan?,
        rollback_plan: rollback_plan?,
    })
}

fn validate_plan_task(
    map: &Map<String, Value>,
    path: &str,
    issues: &mut DraftIssues,
) -> Option<PlanTask> {
    let id = identifier_string(map, "id", &format!("{path}.id"), issues);
    let title = req_string(map, "title", 1, 200, &format!("{path}.title"), issues);
    let description = req_string(
        map,
        "description",
        1,
        2000,
        &format!("{path}.description"),
        issues,
    );
    let dependencies = req_string_array_with(
        map,
        "dependencies",
        0,
        10,
        1,
        IDENTIFIER_MAX,
        &format!("{path}.dependencies"),
        issues,
        Some(dependency_field),
    );
    let affected_files = req_string_array_with(
        map,
        "affectedFiles",
        0,
        50,
        1,
        500,
        &format!("{path}.affectedFiles"),
        issues,
        Some(workspace_relative_path_field),
    );
    let checks = req_string_array(
        map,
        "checks",
        0,
        10,
        1,
        300,
        &format!("{path}.checks"),
        issues,
    );
    let integration_points = req_string_array(
        map,
        "integrationPoints",
        0,
        10,
        1,
        300,
        &format!("{path}.integrationPoints"),
        issues,
    );
    Some(PlanTask {
        id: id?,
        title: title?,
        description: description?,
        dependencies: dependencies?,
        affected_files: affected_files?,
        checks: checks?,
        integration_points: integration_points?,
    })
}

fn validate_execution_plan(value: &Value, issues: &mut DraftIssues) -> Option<ExecutionPlanDraft> {
    let map = validate_object_root(value, issues)?;
    let title = title_field(map, "title", issues);
    let summary = req_string(map, "summary", 1, 4000, "summary", issues);
    let tasks = req_object_array(map, "tasks", 1, 50, "tasks", issues, validate_plan_task);
    Some(ExecutionPlanDraft {
        title: title?,
        summary: summary?,
        tasks: tasks?,
    })
}

const STATUS_OPTIONS: &str = "\"pass\"|\"fail\"|\"unevaluated\"";

fn validate_verification_result(
    map: &Map<String, Value>,
    path: &str,
    issues: &mut DraftIssues,
) -> Option<VerificationResult> {
    let criterion_id = req_string_max_only(
        map,
        "criterionId",
        100,
        &format!("{path}.criterionId"),
        issues,
    );
    let criterion = req_string(
        map,
        "criterion",
        1,
        1000,
        &format!("{path}.criterion"),
        issues,
    );
    let status = match map.get("status") {
        Some(Value::String(text)) => match text.as_str() {
            "pass" => Some(VerificationStatus::Pass),
            "fail" => Some(VerificationStatus::Fail),
            "unevaluated" => Some(VerificationStatus::Unevaluated),
            _ => {
                issues.push(
                    &format!("{path}.status"),
                    format!("Invalid option: expected one of {STATUS_OPTIONS}"),
                );
                None
            }
        },
        _ => {
            issues.push(
                &format!("{path}.status"),
                format!("Invalid option: expected one of {STATUS_OPTIONS}"),
            );
            None
        }
    };
    let evidence = req_string(
        map,
        "evidence",
        1,
        2000,
        &format!("{path}.evidence"),
        issues,
    );
    Some(VerificationResult {
        criterion_id: criterion_id?,
        criterion: criterion?,
        status: status?,
        evidence: evidence?,
    })
}

fn validate_verification_report(
    value: &Value,
    issues: &mut DraftIssues,
) -> Option<VerificationReportDraft> {
    let map = validate_object_root(value, issues)?;
    let title = title_field(map, "title", issues);
    let summary = req_string(map, "summary", 1, 4000, "summary", issues);
    let results = req_object_array(
        map,
        "results",
        1,
        60,
        "results",
        issues,
        validate_verification_result,
    );
    let overall_pass = req_bool(map, "overallPass", "overallPass", issues);
    let notes = req_string_max_only(map, "notes", 4000, "notes", issues);
    Some(VerificationReportDraft {
        title: title?,
        summary: summary?,
        results: results?,
        overall_pass: overall_pass?,
        notes: notes?,
    })
}

fn validate_trace_entry(
    map: &Map<String, Value>,
    path: &str,
    issues: &mut DraftIssues,
) -> Option<TraceEntry> {
    let requirement_id = req_string_max_only(
        map,
        "requirementId",
        100,
        &format!("{path}.requirementId"),
        issues,
    );
    let requirement = req_string(
        map,
        "requirement",
        1,
        1000,
        &format!("{path}.requirement"),
        issues,
    );
    let design_decisions = req_string_array(
        map,
        "designDecisions",
        0,
        10,
        1,
        1000,
        &format!("{path}.designDecisions"),
        issues,
    );
    let changes = req_string_array(
        map,
        "changes",
        0,
        30,
        1,
        1000,
        &format!("{path}.changes"),
        issues,
    );
    let evidence = req_string_array(
        map,
        "evidence",
        0,
        20,
        1,
        1000,
        &format!("{path}.evidence"),
        issues,
    );
    Some(TraceEntry {
        requirement_id: requirement_id?,
        requirement: requirement?,
        design_decisions: design_decisions?,
        changes: changes?,
        evidence: evidence?,
    })
}

fn validate_delivery_report(
    value: &Value,
    issues: &mut DraftIssues,
) -> Option<DeliveryReportDraft> {
    let map = validate_object_root(value, issues)?;
    let title = title_field(map, "title", issues);
    let summary = req_string(map, "summary", 1, 4000, "summary", issues);
    let trace = req_object_array(map, "trace", 1, 60, "trace", issues, validate_trace_entry);
    let next_steps = req_string_array(map, "nextSteps", 0, 20, 1, 500, "nextSteps", issues);
    let held_actions = req_string_array(map, "heldActions", 1, 20, 1, 300, "heldActions", issues);
    Some(DeliveryReportDraft {
        title: title?,
        summary: summary?,
        trace: trace?,
        next_steps: next_steps?,
        held_actions: held_actions?,
    })
}

fn validate_decision_entry(
    map: &Map<String, Value>,
    path: &str,
    issues: &mut DraftIssues,
) -> Option<DecisionEntry> {
    let decision = req_string(map, "decision", 1, 500, &format!("{path}.decision"), issues);
    let rationale = req_string(
        map,
        "rationale",
        1,
        2000,
        &format!("{path}.rationale"),
        issues,
    );
    let alternatives = req_string_max_only(
        map,
        "alternatives",
        1000,
        &format!("{path}.alternatives"),
        issues,
    );
    let mode = req_string(map, "mode", 1, 40, &format!("{path}.mode"), issues);
    Some(DecisionEntry {
        decision: decision?,
        rationale: rationale?,
        alternatives: alternatives?,
        mode: mode?,
    })
}

fn validate_decision_log(value: &Value, issues: &mut DraftIssues) -> Option<DecisionLogDraft> {
    let map = validate_object_root(value, issues)?;
    let title = title_field(map, "title", issues);
    let entries = req_object_array(
        map,
        "entries",
        1,
        100,
        "entries",
        issues,
        validate_decision_entry,
    );
    Some(DecisionLogDraft {
        title: title?,
        entries: entries?,
    })
}

fn validate_draft(kind: ArtifactKind, value: &Value) -> DraftIssues {
    let mut issues = DraftIssues::default();
    match kind {
        ArtifactKind::OpportunityBrief => {
            validate_opportunity_brief(value, &mut issues);
        }
        ArtifactKind::Prd => {
            validate_prd(value, &mut issues);
        }
        ArtifactKind::Trd => {
            validate_trd(value, &mut issues);
        }
        ArtifactKind::ExecutionPlan => {
            validate_execution_plan(value, &mut issues);
        }
        ArtifactKind::VerificationReport => {
            validate_verification_report(value, &mut issues);
        }
        ArtifactKind::DeliveryReport => {
            validate_delivery_report(value, &mut issues);
        }
        ArtifactKind::DecisionLog => {
            validate_decision_log(value, &mut issues);
        }
    }
    issues
}

/// Extract a typed draft from an already-validated JSON value.
///
/// # Panics
/// Never panics for values that passed [`validate_draft`]; callers must not
/// use it on unvalidated input (serde errors are surfaced as a draft error
/// instead of panicking).
fn extract_draft(kind: ArtifactKind, value: &Value) -> Result<ArtifactDraft, ArtifactsError> {
    let into_error = |detail: String| {
        ArtifactsError::InvalidArtifactDraft(InvalidArtifactDraftError {
            kind: kind.as_str().to_string(),
            issues: vec![detail],
        })
    };
    match kind {
        ArtifactKind::OpportunityBrief => {
            serde_json::from_value::<OpportunityBriefDraft>(value.clone())
                .map(ArtifactDraft::OpportunityBrief)
                .map_err(|error| into_error(error.to_string()))
        }
        ArtifactKind::DecisionLog => serde_json::from_value::<DecisionLogDraft>(value.clone())
            .map(ArtifactDraft::DecisionLog)
            .map_err(|error| into_error(error.to_string())),
        ArtifactKind::Prd => serde_json::from_value::<PrdDraft>(value.clone())
            .map(ArtifactDraft::Prd)
            .map_err(|error| into_error(error.to_string())),
        ArtifactKind::Trd => serde_json::from_value::<TrdDraft>(value.clone())
            .map(ArtifactDraft::Trd)
            .map_err(|error| into_error(error.to_string())),
        ArtifactKind::ExecutionPlan => serde_json::from_value::<ExecutionPlanDraft>(value.clone())
            .map(ArtifactDraft::ExecutionPlan)
            .map_err(|error| into_error(error.to_string())),
        ArtifactKind::VerificationReport => {
            serde_json::from_value::<VerificationReportDraft>(value.clone())
                .map(ArtifactDraft::VerificationReport)
                .map_err(|error| into_error(error.to_string()))
        }
        ArtifactKind::DeliveryReport => {
            serde_json::from_value::<DeliveryReportDraft>(value.clone())
                .map(ArtifactDraft::DeliveryReport)
                .map_err(|error| into_error(error.to_string()))
        }
    }
}

/// The result of validating an untrusted draft payload (`ParseArtifactDraftResult`).
#[derive(Debug, Clone, PartialEq)]
pub enum ParseArtifactDraftResult {
    /// The draft is valid.
    Ok(ArtifactDraft),
    /// The draft is invalid; issues are human-readable `path: message` strings.
    Err {
        /// The kind that was validated.
        kind: ArtifactKind,
        /// Zod-style issue strings in schema order.
        issues: Vec<String>,
    },
}

/// The kind-list detail used by `UnknownArtifactKindError`, matching the
/// TypeScript `Object.keys(ARTIFACT_DRAFT_SCHEMAS)` ordering.
pub fn expected_kinds_detail() -> String {
    format!(
        "expected one of: {}",
        ARTIFACT_KINDS
            .iter()
            .map(|kind| kind.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    )
}

/// Resolve the draft schema kind for `kind`, mirroring `artifactDraftSchema`'s
/// throw for unknown kinds.
pub fn artifact_draft_kind(kind: &str) -> Result<ArtifactKind, UnknownArtifactKindError> {
    ArtifactKind::parse(kind).ok_or_else(|| UnknownArtifactKindError {
        kind: kind.to_string(),
        detail: expected_kinds_detail(),
    })
}

/// Validate an untrusted draft payload against the schema for `kind` without
/// failing. Failures surface as human-readable issue strings.
pub fn parse_artifact_draft(kind: ArtifactKind, value: &Value) -> ParseArtifactDraftResult {
    let issues = validate_draft(kind, value);
    if !issues.is_empty() {
        return ParseArtifactDraftResult::Err {
            kind,
            issues: issues.into_strings(),
        };
    }
    match extract_draft(kind, value) {
        Ok(draft) => ParseArtifactDraftResult::Ok(draft),
        Err(error) => match error {
            ArtifactsError::InvalidArtifactDraft(inner) => ParseArtifactDraftResult::Err {
                kind,
                issues: inner.issues,
            },
            // extract_draft only produces InvalidArtifactDraft
            other => ParseArtifactDraftResult::Err {
                kind,
                issues: vec![other.to_string()],
            },
        },
    }
}

/// String-kind variant of [`parse_artifact_draft`] that fails for unknown
/// kinds with the TypeScript error.
pub fn parse_artifact_draft_by_name(
    kind: &str,
    value: &Value,
) -> Result<ParseArtifactDraftResult, UnknownArtifactKindError> {
    let kind = artifact_draft_kind(kind)?;
    Ok(parse_artifact_draft(kind, value))
}

/// Validate a draft and fail with [`InvalidArtifactDraftError`] on failure.
pub fn require_artifact_draft(
    kind: ArtifactKind,
    value: &Value,
) -> Result<ArtifactDraft, InvalidArtifactDraftError> {
    match parse_artifact_draft(kind, value) {
        ParseArtifactDraftResult::Ok(draft) => Ok(draft),
        ParseArtifactDraftResult::Err { kind, issues } => Err(InvalidArtifactDraftError {
            kind: kind.as_str().to_string(),
            issues,
        }),
    }
}
