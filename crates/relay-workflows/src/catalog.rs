//! The built-in software-delivery workflow as addressable data: work
//! definitions, a queryable catalog, and instantiation builders.
//!
//! Definitions are data plus builders — not kernel state. Several instances
//! of `Many` kinds (implementation tasks, verification checks, corrective
//! work) coexist; `Singleton` kinds exist at most once per project at a
//! time. The familiar delivery phases survive only as [`Phase`] labels for
//! grouped views.

use relay_domain::DeliveryAuthority;
use relay_domain::{
    ArtifactVersionRef, CapabilityCeiling, CompletionRule, Decision, DecisionId, DecisionOption,
    DecisionSubject, DependencyEdge, DependencyKind, EvidenceKind, ExecutorClass, ExpectedOutput,
    InputRef, Operation, ProjectGraph, ScopePath, WorkItem, WorkItemId, WorkKind,
};

use crate::schemas;

/// The recognizable delivery phase a work kind belongs to. A grouped-view
/// label only — never a mode, never an ordering the engine consults.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// Understanding the problem and the user.
    Discovery,
    /// Requirements and acceptance criteria.
    Specification,
    /// Technical approach and decisions.
    Design,
    /// Decomposition into bounded tasks.
    Planning,
    /// Repository work.
    Implementation,
    /// Evidence against criteria.
    Verification,
    /// Packaging and authorization.
    Delivery,
}

impl std::fmt::Display for Phase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Phase::Discovery => "Discovery",
            Phase::Specification => "Specification",
            Phase::Design => "Design",
            Phase::Planning => "Planning",
            Phase::Implementation => "Implementation",
            Phase::Verification => "Verification",
            Phase::Delivery => "Delivery",
        };
        f.write_str(name)
    }
}

/// How many instances of a kind may be live at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Multiplicity {
    /// One live instance at a time.
    Singleton,
    /// Any number of live instances.
    Many,
}

/// How a definition's instances pin their inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputPolicy {
    /// Pin exact current versions at declaration (the default; carries
    /// precise invalidation).
    ExactVersion,
    /// Float to the newest published version at evaluation time.
    LatestAtStart,
}

/// Whether an instance's completion additionally requires a human
/// approval decision on its produced artifact version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalRule {
    /// No extra approval gate.
    None,
    /// The produced artifact version must be approved through an
    /// `approve-artifact` decision before dependents are proposed.
    HumanDecision,
}

/// How completion maps onto the kernel's [`CompletionRule`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionTemplate {
    /// [`CompletionRule::Decision`] — human work.
    Decision,
    /// [`CompletionRule::Receipt`] — executor work.
    Receipt,
    /// [`CompletionRule::CriterionSatisfied`] — verification work.
    CriterionSatisfied,
}

/// How the capability ceiling is derived at instantiation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CeilingTemplate {
    /// Read-only: no writes, read operation only.
    ReadOnly,
    /// The ceiling covers exactly the write scopes declared on the
    /// instance (plus read); expanding beyond them is rejected policy.
    AsDeclaredScopes,
}

/// One built-in work definition.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkDefinition {
    /// The built-in kind label (a compile-time constant).
    pub label: &'static str,
    /// The parsed kind newtype instances carry.
    pub kind: WorkKind,
    /// One-line summary of what instances do.
    pub summary: String,
    /// Grouped-view phase.
    pub phase: Phase,
    /// Default executor class (instances may refine within write rules).
    pub executor: ExecutorClass,
    /// How many instances may be live.
    pub multiplicity: Multiplicity,
    /// Input pinning policy.
    pub input_policy: InputPolicy,
    /// Artifact schemas this kind consumes (expansion resolves them to
    /// pins).
    pub inputs: Vec<&'static str>,
    /// What instances produce.
    pub outputs: Vec<ExpectedOutput>,
    /// Evidence kinds instances are expected to attach.
    pub evidence_kinds: Vec<EvidenceKind>,
    /// How the capability ceiling is derived.
    pub ceiling: CeilingTemplate,
    /// Whether produced versions need a human approval gate.
    pub approval: ApprovalRule,
    /// Completion mapping.
    pub completion: CompletionTemplate,
    /// Whether the project goal is met without this kind completing.
    pub required: bool,
}

impl WorkDefinition {
    /// The kind label text (e.g. `verify-criteria`).
    pub fn kind_label(&self) -> &'static str {
        self.label
    }
}

/// The built-in catalog. Queried by the app layer and by
/// [`crate::expansion`]; nothing here mutates graphs.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkflowCatalog {
    definitions: Vec<WorkDefinition>,
}

impl WorkflowCatalog {
    /// The one built-in software-delivery workflow.
    pub fn builtin() -> Self {
        WorkflowCatalog {
            definitions: vec![
                definition_discover(),
                definition_clarify(),
                definition_requirements(),
                definition_research(),
                definition_design(),
                definition_plan(),
                definition_implement(),
                definition_integrate(),
                definition_verify(),
                definition_corrective(),
                definition_delivery_prep(),
                definition_delivery_authorization(),
                definition_approve_artifact(),
                definition_revise_artifact(),
            ],
        }
    }

    /// Look a definition up by kind label.
    pub fn get(&self, kind: &str) -> Option<&WorkDefinition> {
        self.definitions.iter().find(|def| def.label == kind)
    }

    /// All definitions in catalog order.
    pub fn iter(&self) -> impl Iterator<Item = &WorkDefinition> {
        self.definitions.iter()
    }
}

/// Parse a built-in kind label. The labels are compile-time constants; the
/// fallback keeps construction total, and the catalog test pins that it is
/// never taken.
#[allow(clippy::expect_used)] // invariant: built-in labels are valid; test catalog_labels_parse covers it
fn builtin_kind(label: &'static str) -> WorkKind {
    WorkKind::parse(label).expect("built-in kind label must parse")
}

fn output(label: &str, description: &str) -> ExpectedOutput {
    ExpectedOutput {
        label: label.to_string(),
        description: description.to_string(),
    }
}

fn definition_discover() -> WorkDefinition {
    WorkDefinition {
        label: crate::KIND_DISCOVER,
        kind: builtin_kind(crate::KIND_DISCOVER),
        summary: "Survey the problem space and draft an opportunity brief".to_string(),
        phase: Phase::Discovery,
        executor: ExecutorClass::Model,
        multiplicity: Multiplicity::Singleton,
        input_policy: InputPolicy::ExactVersion,
        inputs: Vec::new(),
        outputs: vec![output(
            "opportunity-brief",
            "structured brief with problem, audience, outcome, and open questions",
        )],
        evidence_kinds: Vec::new(),
        ceiling: CeilingTemplate::ReadOnly,
        approval: ApprovalRule::HumanDecision,
        completion: CompletionTemplate::Receipt,
        required: true,
    }
}

fn definition_clarify() -> WorkDefinition {
    WorkDefinition {
        label: crate::KIND_CLARIFY,
        kind: builtin_kind(crate::KIND_CLARIFY),
        summary: "Ask one human for the focused answer an ambiguity needs".to_string(),
        phase: Phase::Discovery,
        executor: ExecutorClass::Human,
        multiplicity: Multiplicity::Many,
        input_policy: InputPolicy::ExactVersion,
        inputs: Vec::new(),
        outputs: Vec::new(),
        evidence_kinds: Vec::new(),
        ceiling: CeilingTemplate::ReadOnly,
        approval: ApprovalRule::None,
        completion: CompletionTemplate::Decision,
        required: true,
    }
}

fn definition_requirements() -> WorkDefinition {
    WorkDefinition {
        label: crate::KIND_REQUIREMENTS,
        kind: builtin_kind(crate::KIND_REQUIREMENTS),
        summary: "Expand an approved brief into addressable requirements and acceptance criteria"
            .to_string(),
        phase: Phase::Specification,
        executor: ExecutorClass::Model,
        multiplicity: Multiplicity::Singleton,
        input_policy: InputPolicy::ExactVersion,
        inputs: vec![schemas::BRIEF_SCHEMA],
        outputs: vec![
            output("requirements", "one requirement artifact per requirement"),
            output(
                "criteria",
                "one criterion artifact per acceptance criterion",
            ),
        ],
        evidence_kinds: Vec::new(),
        ceiling: CeilingTemplate::ReadOnly,
        approval: ApprovalRule::HumanDecision,
        completion: CompletionTemplate::Receipt,
        required: true,
    }
}

fn definition_research() -> WorkDefinition {
    WorkDefinition {
        label: crate::KIND_RESEARCH,
        kind: builtin_kind(crate::KIND_RESEARCH),
        summary: "Inspect the repository and prior art; report findings without mutating anything"
            .to_string(),
        phase: Phase::Design,
        executor: ExecutorClass::Verifier,
        multiplicity: Multiplicity::Many,
        input_policy: InputPolicy::ExactVersion,
        inputs: vec![schemas::BRIEF_SCHEMA],
        outputs: vec![output(
            "research",
            "structured findings answering one question",
        )],
        evidence_kinds: vec![EvidenceKind::Inspection],
        ceiling: CeilingTemplate::ReadOnly,
        approval: ApprovalRule::None,
        completion: CompletionTemplate::Receipt,
        required: false,
    }
}

fn definition_design() -> WorkDefinition {
    WorkDefinition {
        label: crate::KIND_DESIGN,
        kind: builtin_kind(crate::KIND_DESIGN),
        summary: "Design the technical approach and record the decisions it rests on".to_string(),
        phase: Phase::Design,
        executor: ExecutorClass::Model,
        multiplicity: Multiplicity::Singleton,
        input_policy: InputPolicy::ExactVersion,
        inputs: vec![schemas::REQUIREMENT_SCHEMA, schemas::RESEARCH_SCHEMA],
        outputs: vec![
            output("design", "structured technical approach"),
            output(
                "design-decisions",
                "one recorded decision artifact per consequential choice",
            ),
        ],
        evidence_kinds: Vec::new(),
        ceiling: CeilingTemplate::ReadOnly,
        approval: ApprovalRule::HumanDecision,
        completion: CompletionTemplate::Receipt,
        required: true,
    }
}

fn definition_plan() -> WorkDefinition {
    WorkDefinition {
        label: crate::KIND_PLAN,
        kind: builtin_kind(crate::KIND_PLAN),
        summary: "Decompose the design into bounded, individually verifiable tasks".to_string(),
        phase: Phase::Planning,
        executor: ExecutorClass::Model,
        multiplicity: Multiplicity::Singleton,
        input_policy: InputPolicy::ExactVersion,
        inputs: vec![schemas::DESIGN_SCHEMA, schemas::REQUIREMENT_SCHEMA],
        outputs: vec![output("plan", "structured task decomposition")],
        evidence_kinds: Vec::new(),
        ceiling: CeilingTemplate::ReadOnly,
        approval: ApprovalRule::HumanDecision,
        completion: CompletionTemplate::Receipt,
        required: true,
    }
}

fn definition_implement() -> WorkDefinition {
    WorkDefinition {
        label: crate::KIND_IMPLEMENT,
        kind: builtin_kind(crate::KIND_IMPLEMENT),
        summary: "Implement one bounded task inside its declared write scopes".to_string(),
        phase: Phase::Implementation,
        executor: ExecutorClass::Workspace,
        multiplicity: Multiplicity::Many,
        input_policy: InputPolicy::ExactVersion,
        inputs: vec![schemas::PLAN_SCHEMA, schemas::REQUIREMENT_SCHEMA],
        outputs: vec![output("task-notes", "what changed and why")],
        evidence_kinds: Vec::new(),
        ceiling: CeilingTemplate::AsDeclaredScopes,
        approval: ApprovalRule::None,
        completion: CompletionTemplate::Receipt,
        required: false,
    }
}

fn definition_integrate() -> WorkDefinition {
    WorkDefinition {
        label: crate::KIND_INTEGRATE,
        kind: builtin_kind(crate::KIND_INTEGRATE),
        summary: "Integrate completed tasks into one coherent change set".to_string(),
        phase: Phase::Implementation,
        executor: ExecutorClass::Workspace,
        multiplicity: Multiplicity::Singleton,
        input_policy: InputPolicy::ExactVersion,
        inputs: vec![schemas::PLAN_SCHEMA],
        outputs: vec![output(
            "integration-notes",
            "the integrated change-set reference",
        )],
        evidence_kinds: Vec::new(),
        ceiling: CeilingTemplate::AsDeclaredScopes,
        approval: ApprovalRule::None,
        completion: CompletionTemplate::Receipt,
        required: true,
    }
}

fn definition_verify() -> WorkDefinition {
    WorkDefinition {
        label: crate::KIND_VERIFY,
        kind: builtin_kind(crate::KIND_VERIFY),
        summary: "Collect evidence against one acceptance criterion's exact version".to_string(),
        phase: Phase::Verification,
        executor: ExecutorClass::Verifier,
        multiplicity: Multiplicity::Many,
        input_policy: InputPolicy::ExactVersion,
        inputs: vec![schemas::CRITERION_SCHEMA],
        outputs: Vec::new(),
        evidence_kinds: vec![EvidenceKind::TestRun, EvidenceKind::CommandOutput],
        ceiling: CeilingTemplate::ReadOnly,
        approval: ApprovalRule::None,
        completion: CompletionTemplate::CriterionSatisfied,
        required: true,
    }
}

fn definition_corrective() -> WorkDefinition {
    WorkDefinition {
        label: crate::KIND_CORRECTIVE,
        kind: builtin_kind(crate::KIND_CORRECTIVE),
        summary: "Fix what failing evidence showed is broken, then let verification re-run"
            .to_string(),
        phase: Phase::Implementation,
        executor: ExecutorClass::Workspace,
        multiplicity: Multiplicity::Many,
        input_policy: InputPolicy::ExactVersion,
        inputs: vec![schemas::CRITERION_SCHEMA],
        outputs: vec![output("correction-notes", "what was fixed")],
        evidence_kinds: Vec::new(),
        ceiling: CeilingTemplate::AsDeclaredScopes,
        approval: ApprovalRule::None,
        completion: CompletionTemplate::Receipt,
        required: false,
    }
}

fn definition_delivery_prep() -> WorkDefinition {
    WorkDefinition {
        label: crate::KIND_DELIVERY_PREP,
        kind: builtin_kind(crate::KIND_DELIVERY_PREP),
        summary: "Assemble the delivery package and its evidence for human review".to_string(),
        phase: Phase::Delivery,
        executor: ExecutorClass::System,
        multiplicity: Multiplicity::Singleton,
        input_policy: InputPolicy::ExactVersion,
        inputs: vec![schemas::CRITERION_SCHEMA],
        outputs: vec![output(
            "delivery-report",
            "structured delivery package reference",
        )],
        evidence_kinds: Vec::new(),
        ceiling: CeilingTemplate::ReadOnly,
        approval: ApprovalRule::None,
        completion: CompletionTemplate::Receipt,
        required: true,
    }
}

fn definition_delivery_authorization() -> WorkDefinition {
    WorkDefinition {
        label: crate::KIND_DELIVERY_AUTH,
        kind: builtin_kind(crate::KIND_DELIVERY_AUTH),
        summary: "Ask one human to authorize (or hold) delivery of the package".to_string(),
        phase: Phase::Delivery,
        executor: ExecutorClass::Human,
        multiplicity: Multiplicity::Singleton,
        input_policy: InputPolicy::ExactVersion,
        inputs: vec![schemas::DELIVERY_SCHEMA],
        outputs: Vec::new(),
        evidence_kinds: Vec::new(),
        ceiling: CeilingTemplate::ReadOnly,
        approval: ApprovalRule::None,
        completion: CompletionTemplate::Decision,
        required: true,
    }
}

fn definition_approve_artifact() -> WorkDefinition {
    WorkDefinition {
        label: crate::KIND_APPROVE,
        kind: builtin_kind(crate::KIND_APPROVE),
        summary: "Approve (or send back) one exact artifact version".to_string(),
        // Approval work belongs to the phase of whatever it approves; the
        // label is what matters to policy.
        phase: Phase::Discovery,
        executor: ExecutorClass::Human,
        multiplicity: Multiplicity::Many,
        input_policy: InputPolicy::ExactVersion,
        inputs: Vec::new(),
        outputs: Vec::new(),
        evidence_kinds: Vec::new(),
        ceiling: CeilingTemplate::ReadOnly,
        approval: ApprovalRule::None,
        completion: CompletionTemplate::Decision,
        required: false,
    }
}

fn definition_revise_artifact() -> WorkDefinition {
    WorkDefinition {
        label: crate::KIND_REVISE,
        kind: builtin_kind(crate::KIND_REVISE),
        summary: "Turn an edited Markdown view into a validated new artifact version".to_string(),
        phase: Phase::Specification,
        executor: ExecutorClass::Model,
        multiplicity: Multiplicity::Many,
        input_policy: InputPolicy::ExactVersion,
        inputs: Vec::new(),
        outputs: vec![output(
            "revised-version",
            "the structured content parsed from the edited view",
        )],
        evidence_kinds: Vec::new(),
        ceiling: CeilingTemplate::ReadOnly,
        approval: ApprovalRule::None,
        completion: CompletionTemplate::Receipt,
        required: false,
    }
}

/// Error raised when a definition is instantiated with the wrong builder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum InstantiateError {
    /// The definition completes by decision and needs
    /// [`instantiate_human`].
    #[error("kind {kind} completes by human decision; use instantiate_human")]
    NeedsDecision {
        /// The kind label.
        kind: &'static str,
    },
    /// The definition completes by receipt; use [`instantiate_executor`].
    #[error("kind {kind} completes by receipt; use instantiate_executor")]
    NeedsReceipt {
        /// The kind label.
        kind: &'static str,
    },
    /// The definition completes by criterion; use
    /// [`instantiate_verification`].
    #[error("kind {kind} completes by criterion; use instantiate_verification")]
    NeedsCriterion {
        /// The kind label.
        kind: &'static str,
    },
}

fn base_item(definition: &WorkDefinition, id: WorkItemId, objective: &str) -> WorkItem {
    let mut work = WorkItem::new(
        id,
        definition.kind.clone(),
        definition.executor,
        objective,
        CompletionRule::Receipt,
    );
    work.expected_outputs = definition.outputs.clone();
    work.required = definition.required;
    work
}

fn apply_ceiling(definition: &WorkDefinition, work: &mut WorkItem) {
    work.capability_ceiling = match definition.ceiling {
        CeilingTemplate::ReadOnly => CapabilityCeiling::default(),
        CeilingTemplate::AsDeclaredScopes => CapabilityCeiling {
            write_scopes: work.write_scopes.clone(),
            operations: vec![Operation::Read, Operation::Write, Operation::Execute],
            delivery: DeliveryAuthority::None,
        },
    };
}

/// Instantiate an executor (receipt-completing) work item.
pub fn instantiate_executor(
    definition: &WorkDefinition,
    id: WorkItemId,
    objective: impl Into<String>,
    inputs: Vec<InputRef>,
    write_scopes: Vec<ScopePath>,
    dependencies: Vec<DependencyEdge>,
) -> Result<WorkItem, InstantiateError> {
    if definition.completion != CompletionTemplate::Receipt {
        return Err(match definition.completion {
            CompletionTemplate::Decision => InstantiateError::NeedsDecision {
                kind: definition.label,
            },
            CompletionTemplate::CriterionSatisfied => InstantiateError::NeedsCriterion {
                kind: definition.label,
            },
            CompletionTemplate::Receipt => unreachable!("checked above"),
        });
    }
    let mut work = base_item(definition, id, &objective.into());
    work.inputs = inputs;
    work.write_scopes = write_scopes;
    work.dependencies = dependencies;
    apply_ceiling(definition, &mut work);
    Ok(work)
}

/// Instantiate a human (decision-completing) work item. The decision fact
/// is opened immediately after the work is declared.
pub fn instantiate_human(
    definition: &WorkDefinition,
    id: WorkItemId,
    objective: impl Into<String>,
    decision: DecisionId,
    inputs: Vec<InputRef>,
) -> Result<WorkItem, InstantiateError> {
    if definition.completion != CompletionTemplate::Decision {
        return Err(InstantiateError::NeedsReceipt {
            kind: definition.label,
        });
    }
    let mut work = base_item(definition, id, &objective.into());
    work.completion = CompletionRule::Decision { decision };
    work.inputs = inputs;
    apply_ceiling(definition, &mut work);
    Ok(work)
}

/// Instantiate a verification work item bound to one exact criterion
/// version.
pub fn instantiate_verification(
    definition: &WorkDefinition,
    id: WorkItemId,
    objective: impl Into<String>,
    criterion: ArtifactVersionRef,
    dependencies: Vec<DependencyEdge>,
) -> Result<WorkItem, InstantiateError> {
    if definition.completion != CompletionTemplate::CriterionSatisfied {
        return Err(match definition.completion {
            CompletionTemplate::Decision => InstantiateError::NeedsDecision {
                kind: definition.label,
            },
            CompletionTemplate::Receipt => InstantiateError::NeedsReceipt {
                kind: definition.label,
            },
            CompletionTemplate::CriterionSatisfied => unreachable!("checked above"),
        });
    }
    let mut work = base_item(definition, id, &objective.into());
    work.completion = CompletionRule::CriterionSatisfied {
        criterion: criterion.clone(),
    };
    work.inputs = vec![InputRef::Exact(criterion)];
    work.dependencies = dependencies;
    apply_ceiling(definition, &mut work);
    Ok(work)
}

/// Build the decision that completes a human work item.
pub fn human_decision(
    id: DecisionId,
    work: &WorkItemId,
    question: impl Into<String>,
    options: &[(&str, &str)],
) -> Decision {
    let mut decision = Decision::new(
        id,
        DecisionSubject::WorkItem(work.clone()),
        question,
        options
            .iter()
            .map(|(label, description)| DecisionOption {
                label: label.to_string(),
                description: description.to_string(),
            })
            .collect(),
        0,
    );
    decision.authority = relay_domain::Authority::Human;
    decision
}

/// Build an approval decision on one exact artifact version.
pub fn approval_decision(
    id: DecisionId,
    version: &ArtifactVersionRef,
    question: impl Into<String>,
) -> Decision {
    Decision::new(
        id,
        DecisionSubject::ArtifactVersion(version.clone()),
        question,
        vec![
            DecisionOption {
                label: "approve".to_string(),
                description: "accept this version as the basis for dependent work".to_string(),
            },
            DecisionOption {
                label: "revise".to_string(),
                description: "send it back for another pass".to_string(),
            },
        ],
        0,
    )
}

/// Whether a kind's live instances exist (non-terminal works of that kind).
pub fn has_live_instance(graph: &ProjectGraph, kind: &str) -> bool {
    graph
        .works()
        .any(|(_, node)| node.work.kind.as_str() == kind && !node.is_declared_terminal())
}

/// A Requires edge from `from` to `to`.
pub fn requires_edge(from: &WorkItemId, to: &WorkItemId) -> DependencyEdge {
    DependencyEdge::new(from.clone(), to.clone(), DependencyKind::Requires)
}

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

    #[test]
    fn catalog_labels_parse_and_lookup_works() {
        let catalog = WorkflowCatalog::builtin();
        assert_eq!(catalog.iter().count(), 14);
        for definition in catalog.iter() {
            assert!(!definition.kind_label().is_empty());
            assert_eq!(
                catalog.get(definition.kind_label()).map(|d| d.kind_label()),
                Some(definition.kind_label())
            );
        }
        assert!(catalog.get("mode-advance").is_none());
    }

    #[test]
    fn instantiation_rejects_wrong_completion_shapes() {
        let catalog = WorkflowCatalog::builtin();
        let clarify = catalog.get(crate::KIND_CLARIFY).unwrap();
        let error = instantiate_executor(
            clarify,
            "work_c1".parse().ok().unwrap(),
            "objective",
            vec![],
            vec![],
            vec![],
        )
        .err()
        .expect("builder mismatch rejected");
        assert!(matches!(error, InstantiateError::NeedsDecision { .. }));

        let verify = catalog.get(crate::KIND_VERIFY).unwrap();
        let error = instantiate_executor(
            verify,
            "work_v1".parse().ok().unwrap(),
            "objective",
            vec![],
            vec![],
            vec![],
        )
        .err()
        .expect("builder mismatch rejected");
        assert!(matches!(error, InstantiateError::NeedsCriterion { .. }));
    }

    #[test]
    fn executor_instances_carry_scopes_and_ceilings() {
        let catalog = WorkflowCatalog::builtin();
        let implement = catalog.get(crate::KIND_IMPLEMENT).unwrap();
        let work = instantiate_executor(
            implement,
            "work_t1".parse().ok().unwrap(),
            "implement the CSV writer",
            vec![],
            vec![ScopePath::parse("src").ok().unwrap()],
            vec![],
        )
        .ok()
        .expect("instantiates");
        assert_eq!(work.write_scopes.len(), 1);
        assert_eq!(work.capability_ceiling.write_scopes, work.write_scopes);
        assert!(work.required == implement.required);
    }
}
