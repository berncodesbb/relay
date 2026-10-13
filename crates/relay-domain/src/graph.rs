//! The project graph: state folded from durable facts.

use std::collections::BTreeMap;

use crate::artifact::{ArtifactContent, ArtifactDigest, ArtifactVersionRef};
use crate::attempt::AttemptConclusion;
use crate::decision::{Decision, DecisionResolution};
use crate::dependency::{DependencyEdge, DependencyKind};
use crate::error::FactError;
use crate::evidence::Evidence;
use crate::facts::{DomainFact, InvalidationCause, InvalidationTarget};
use crate::goal::Goal;
use crate::grant::{CapabilityGrant, Operation};
use crate::ids::{
    ArtifactId, ArtifactVersion, AttemptId, DecisionId, EvidenceId, ProjectId, ProjectRevision,
    WorkItemId,
};
use crate::validation::{check_work_shape, find_cycle, grant_covers_scopes, scopes_within_ceiling};
use crate::work::WorkItem;

/// A work item plus its folded lifecycle facts. Operational state (ready,
/// blocked, running, invalid, …) is never stored here; `relay-engine`
/// derives it.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkNode {
    /// The immutable declaration.
    pub work: WorkItem,
    /// The replacement, when this work was superseded.
    pub superseded_by: Option<WorkItemId>,
    /// The cancellation reason, when canceled.
    pub canceled: Option<String>,
    /// Write scopes added after declaration (within the ceiling).
    pub added_write_scopes: Vec<crate::work::ScopePath>,
    /// Operation classes added after declaration (within the ceiling).
    pub added_operations: Vec<Operation>,
    /// Revision of the last fact that changed this node.
    pub last_changed: ProjectRevision,
}

impl WorkNode {
    /// Whether the work is in a declared terminal state (superseded or
    /// canceled). Completion is derived, not stored.
    pub fn is_declared_terminal(&self) -> bool {
        self.superseded_by.is_some() || self.canceled.is_some()
    }

    /// All write scopes: declared plus expanded.
    pub fn effective_write_scopes(&self) -> Vec<crate::work::ScopePath> {
        let mut scopes = self.work.write_scopes.clone();
        for added in &self.added_write_scopes {
            if !scopes.contains(added) {
                scopes.push(added.clone());
            }
        }
        scopes
    }

    /// All operation classes: ceiling-independent view of declared plus
    /// expanded operations.
    pub fn effective_operations(&self) -> Vec<Operation> {
        let mut operations = Vec::new();
        for operation in self
            .work
            .capability_ceiling
            .operations
            .iter()
            .chain(self.added_operations.iter())
        {
            if !operations.contains(operation) {
                operations.push(*operation);
            }
        }
        operations
    }
}

/// One asserted dependency edge and when it was asserted.
#[derive(Debug, Clone, PartialEq)]
pub struct DependencyNode {
    /// Revision of the fact that asserted this edge.
    pub asserted_revision: ProjectRevision,
}

/// A decision plus its folded resolution state.
#[derive(Debug, Clone, PartialEq)]
pub struct DecisionNode {
    /// The opened decision.
    pub decision: Decision,
    /// The resolution, when resolved.
    pub resolution: Option<DecisionResolution>,
    /// The reconsidering decision, when superseded.
    pub superseded_by: Option<DecisionId>,
    /// Revision of the last fact that changed this node.
    pub last_changed: ProjectRevision,
}

impl DecisionNode {
    /// Whether the decision currently blocks its subject: unresolved and
    /// not superseded.
    pub fn is_open(&self) -> bool {
        self.resolution.is_none() && self.superseded_by.is_none()
    }
}

/// One published artifact version.
#[derive(Debug, Clone, PartialEq)]
pub struct VersionRecord {
    /// A human-readable title for the version.
    pub title: Option<String>,
    /// The structured content.
    pub content: ArtifactContent,
    /// The digest commitment recorded with the content.
    pub digest: ArtifactDigest,
    /// The exact input versions this content was derived from.
    pub derived_from: Vec<ArtifactVersionRef>,
    /// Wall-clock milliseconds when the version was published.
    pub published_at_ms: i64,
    /// The revision at which the version was published.
    pub published_revision: ProjectRevision,
}

/// An artifact and its version history.
#[derive(Debug, Clone, PartialEq)]
pub struct ArtifactNode {
    /// Published versions, keyed by version number.
    pub versions: BTreeMap<ArtifactVersion, VersionRecord>,
    /// Revision of the last fact that changed this artifact.
    pub last_changed: ProjectRevision,
}

impl ArtifactNode {
    /// The newest published version.
    pub fn current_version(&self) -> ArtifactVersion {
        // Versions are inserted contiguously from FIRST by the fold, so the
        // last key always exists.
        self.versions
            .last_key_value()
            .map(|(version, _)| *version)
            .unwrap_or(ArtifactVersion::FIRST)
    }
}

/// An attempt and its folded terminal state.
#[derive(Debug, Clone, PartialEq)]
pub struct AttemptNode {
    /// The attempt identity.
    pub id: AttemptId,
    /// The work item the attempt runs.
    pub work: WorkItemId,
    /// The project revision the attempt was started against.
    pub expected_revision: ProjectRevision,
    /// The grant bound to the attempt.
    pub grant: CapabilityGrant,
    /// Wall-clock milliseconds when the attempt started.
    pub started_at_ms: i64,
    /// The terminal account, when concluded.
    pub conclusion: Option<AttemptConclusion>,
    /// Revision of the last fact that changed this node.
    pub last_changed: ProjectRevision,
}

impl AttemptNode {
    /// Whether the attempt started and has not concluded.
    pub fn is_active(&self) -> bool {
        self.conclusion.is_none()
    }
}

/// One declared invalidation.
#[derive(Debug, Clone, PartialEq)]
pub struct DeclaredInvalidation {
    /// Why the target is invalid.
    pub cause: InvalidationCause,
    /// The input version change that motivated the invalidation.
    pub changed_input: Option<ArtifactVersionRef>,
    /// Revision of the declaring fact.
    pub revision: ProjectRevision,
}

/// The work graph of one project, folded from its durable facts.
///
/// Folding validates every fact (duplicates, references, cycles,
/// capabilities, sequences) and stamps each node with the revision of its
/// last change so receipt acceptance can detect material changes. Facts
/// that do not reference each other are order-independent: shuffling them
/// yields a graph with identical derived state.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProjectGraph {
    project: Option<ProjectId>,
    goal: Option<Goal>,
    works: BTreeMap<WorkItemId, WorkNode>,
    dependencies: BTreeMap<DependencyEdge, DependencyNode>,
    decisions: BTreeMap<DecisionId, DecisionNode>,
    artifacts: BTreeMap<ArtifactId, ArtifactNode>,
    evidence: BTreeMap<EvidenceId, Evidence>,
    attempts: BTreeMap<AttemptId, AttemptNode>,
    invalidations: BTreeMap<InvalidationTarget, Vec<DeclaredInvalidation>>,
    revision: ProjectRevision,
}

impl ProjectGraph {
    /// Fold a full fact history. The first fact must be `ProjectCreated`;
    /// every subsequent fact is validated as it is applied.
    pub fn from_facts<'a, I>(facts: I) -> Result<Self, FactError>
    where
        I: IntoIterator<Item = &'a DomainFact>,
    {
        let mut graph = ProjectGraph::default();
        for fact in facts {
            graph.apply(fact)?;
        }
        Ok(graph)
    }

    /// Validate and apply one fact, advancing the revision.
    pub fn apply(&mut self, fact: &DomainFact) -> Result<(), FactError> {
        if !matches!(fact, DomainFact::ProjectCreated { .. }) && self.project.is_none() {
            return Err(FactError::NotInitialized { fact: fact.kind() });
        }
        match fact {
            DomainFact::ProjectCreated { project } => {
                self.apply_project_created(fact.kind(), project)?
            }
            DomainFact::GoalDeclared { goal } => self.apply_goal_declared(fact.kind(), goal)?,
            DomainFact::WorkDeclared { work } => self.apply_work_declared(fact.kind(), work)?,
            DomainFact::WorkSuperseded {
                work,
                superseded_by,
                reason: _,
            } => self.apply_work_superseded(fact.kind(), work, superseded_by)?,
            DomainFact::WorkCanceled { work, reason } => {
                self.apply_work_canceled(fact.kind(), work, reason)?
            }
            DomainFact::WorkScopeExpanded {
                work,
                added_write_scopes,
                added_operations,
                reason: _,
            } => self.apply_work_scope_expanded(
                fact.kind(),
                work,
                added_write_scopes,
                added_operations,
            )?,
            DomainFact::DependencyAsserted { from, to, kind } => {
                self.apply_dependency_asserted(fact.kind(), from, to, *kind)?
            }
            DomainFact::DependencyRetracted {
                from,
                to,
                kind,
                reason: _,
            } => self.apply_dependency_retracted(fact.kind(), from, to, *kind)?,
            DomainFact::DecisionOpened { decision } => {
                self.apply_decision_opened(fact.kind(), decision)?
            }
            DomainFact::DecisionResolved {
                decision,
                resolution,
            } => self.apply_decision_resolved(fact.kind(), decision, resolution)?,
            DomainFact::DecisionSuperseded {
                decision,
                superseded_by,
            } => self.apply_decision_superseded(fact.kind(), decision, superseded_by)?,
            DomainFact::ArtifactVersionPublished {
                artifact,
                version,
                title,
                content,
                digest,
                derived_from,
                published_at_ms,
            } => self.apply_artifact_published(
                fact.kind(),
                artifact,
                *version,
                title.clone(),
                content.clone(),
                digest.clone(),
                derived_from,
                *published_at_ms,
            )?,
            DomainFact::EvidenceRecorded { evidence } => {
                self.apply_evidence_recorded(fact.kind(), evidence)?
            }
            DomainFact::AttemptStarted {
                attempt,
                work,
                expected_revision,
                grant,
                started_at_ms,
            } => self.apply_attempt_started(
                fact.kind(),
                attempt,
                work,
                *expected_revision,
                grant,
                *started_at_ms,
            )?,
            DomainFact::AttemptConcluded {
                attempt,
                conclusion,
            } => self.apply_attempt_concluded(fact.kind(), attempt, conclusion)?,
            DomainFact::InvalidationDeclared {
                target,
                cause,
                changed_input,
            } => {
                self.apply_invalidation_declared(fact.kind(), target, cause, changed_input.clone())?
            }
        }
        self.revision = ProjectRevision::from_count(self.revision.get() + 1);
        Ok(())
    }

    // -- fold steps ---------------------------------------------------------

    fn apply_project_created(
        &mut self,
        kind: &'static str,
        project: &ProjectId,
    ) -> Result<(), FactError> {
        if let Some(existing) = &self.project {
            return Err(FactError::AlreadyInitialized {
                fact: kind,
                existing: existing.clone(),
            });
        }
        self.project = Some(project.clone());
        Ok(())
    }

    fn apply_goal_declared(&mut self, kind: &'static str, goal: &Goal) -> Result<(), FactError> {
        if self.goal.is_some() {
            return Err(FactError::Duplicate {
                fact: kind,
                detail: format!(
                    "goal {} is already declared; a project has exactly one goal",
                    goal.id
                ),
            });
        }
        self.goal = Some(goal.clone());
        Ok(())
    }

    /// Validate one edge against the graph plus an optional in-flight
    /// declaration. Returns the edges to run cycle detection over.
    fn check_edge(
        &self,
        kind: &'static str,
        edge: &DependencyEdge,
        pending: Option<&WorkItem>,
    ) -> Result<(), FactError> {
        if edge.from == edge.to {
            return Err(FactError::InvalidState {
                fact: kind,
                detail: format!("work {} cannot depend on itself", edge.from),
            });
        }
        let known =
            |id: &WorkItemId| self.works.contains_key(id) || pending.is_some_and(|w| w.id == *id);
        if !known(&edge.from) {
            return Err(FactError::UnknownReference {
                fact: kind,
                detail: format!(
                    "dependent {edge_from} does not exist",
                    edge_from = edge.from
                ),
            });
        }
        if !known(&edge.to) {
            return Err(FactError::UnknownReference {
                fact: kind,
                detail: format!("prerequisite {to} does not exist", to = edge.to),
            });
        }
        if self.dependencies.contains_key(edge) {
            return Err(FactError::Duplicate {
                fact: kind,
                detail: format!("dependency {edge} is already asserted"),
            });
        }
        if edge.kind.is_ordering() {
            let mut edges: Vec<DependencyEdge> = self
                .dependencies
                .keys()
                .filter(|existing| existing.kind.is_ordering())
                .cloned()
                .collect();
            edges.push(edge.clone());
            if let Some(cycle) = find_cycle(&edges) {
                return Err(FactError::DependencyCycle {
                    fact: kind,
                    path: cycle,
                });
            }
        }
        Ok(())
    }

    fn apply_work_declared(
        &mut self,
        kind: &'static str,
        work: &WorkItem,
    ) -> Result<(), FactError> {
        if self.works.contains_key(&work.id) {
            return Err(FactError::Duplicate {
                fact: kind,
                detail: format!("work {} is already declared", work.id),
            });
        }
        if let Err(error) = check_work_shape(work) {
            return Err(FactError::InvalidState {
                fact: kind,
                detail: error.to_string(),
            });
        }
        for edge in &work.dependencies {
            if edge.from != work.id {
                return Err(FactError::InvalidState {
                    fact: kind,
                    detail: format!(
                        "initial dependency {edge} must have {work_id} as its dependent",
                        work_id = work.id
                    ),
                });
            }
            // The edge list itself must not repeat.
            if work.dependencies.iter().filter(|e| **e == *edge).count() > 1 {
                return Err(FactError::Duplicate {
                    fact: kind,
                    detail: format!("initial dependencies list {edge} more than once"),
                });
            }
            self.check_edge(kind, edge, Some(work))?;
        }
        self.works.insert(
            work.id.clone(),
            WorkNode {
                work: work.clone(),
                superseded_by: None,
                canceled: None,
                added_write_scopes: Vec::new(),
                added_operations: Vec::new(),
                last_changed: self.next_revision(),
            },
        );
        for edge in &work.dependencies {
            self.dependencies.insert(
                edge.clone(),
                DependencyNode {
                    asserted_revision: self.next_revision(),
                },
            );
        }
        Ok(())
    }

    fn apply_work_superseded(
        &mut self,
        kind: &'static str,
        work: &WorkItemId,
        superseded_by: &WorkItemId,
    ) -> Result<(), FactError> {
        let node = self.work_node(kind, work)?;
        if work == superseded_by {
            return Err(FactError::InvalidState {
                fact: kind,
                detail: format!("work {work} cannot supersede itself"),
            });
        }
        if node.is_declared_terminal() {
            return Err(FactError::InvalidState {
                fact: kind,
                detail: format!("work {work} is already superseded or canceled"),
            });
        }
        if !self.works.contains_key(superseded_by) {
            return Err(FactError::UnknownReference {
                fact: kind,
                detail: format!("replacement {superseded_by} does not exist"),
            });
        }
        let stamp = self.next_revision();
        let node = self
            .works
            .get_mut(work)
            .ok_or(FactError::UnknownReference {
                fact: kind,
                detail: work.to_string(),
            })?;
        node.superseded_by = Some(superseded_by.clone());
        node.last_changed = stamp;
        Ok(())
    }

    fn apply_work_canceled(
        &mut self,
        kind: &'static str,
        work: &WorkItemId,
        reason: &str,
    ) -> Result<(), FactError> {
        let node = self.work_node(kind, work)?;
        if node.is_declared_terminal() {
            return Err(FactError::InvalidState {
                fact: kind,
                detail: format!("work {work} is already superseded or canceled"),
            });
        }
        let stamp = self.next_revision();
        let node = self
            .works
            .get_mut(work)
            .ok_or(FactError::UnknownReference {
                fact: kind,
                detail: work.to_string(),
            })?;
        node.canceled = Some(reason.to_string());
        node.last_changed = stamp;
        Ok(())
    }

    fn apply_work_scope_expanded(
        &mut self,
        kind: &'static str,
        work: &WorkItemId,
        added_write_scopes: &[crate::work::ScopePath],
        added_operations: &[Operation],
    ) -> Result<(), FactError> {
        let node = self.work_node(kind, work)?;
        if node.is_declared_terminal() {
            return Err(FactError::InvalidState {
                fact: kind,
                detail: format!("work {work} is already superseded or canceled"),
            });
        }
        if !added_write_scopes.is_empty() && !node.work.executor.may_write() {
            return Err(FactError::InvalidState {
                fact: kind,
                detail: format!(
                    "work {work} has executor class {executor}, which cannot write",
                    executor = node.work.executor
                ),
            });
        }
        let mut combined = node.effective_write_scopes();
        for scope in added_write_scopes {
            if !combined.contains(scope) {
                combined.push(scope.clone());
            }
        }
        if !scopes_within_ceiling(&combined, &node.work.capability_ceiling.write_scopes) {
            return Err(FactError::CapabilityExceeded {
                fact: kind,
                detail: format!(
                    "expanded write scopes exceed the capability ceiling of work {work} ({ceiling:?})",
                    ceiling = node.work.capability_ceiling.write_scopes
                ),
            });
        }
        let mut operations = node.work.capability_ceiling.operations.clone();
        for operation in added_operations {
            if !operations.contains(operation) {
                operations.push(*operation);
            }
        }
        let stamp = self.next_revision();
        let node = self
            .works
            .get_mut(work)
            .ok_or(FactError::UnknownReference {
                fact: kind,
                detail: work.to_string(),
            })?;
        for scope in added_write_scopes {
            if !node.added_write_scopes.contains(scope) {
                node.added_write_scopes.push(scope.clone());
            }
        }
        for operation in added_operations {
            if !node.added_operations.contains(operation) {
                node.added_operations.push(*operation);
            }
        }
        node.last_changed = stamp;
        Ok(())
    }

    fn apply_dependency_asserted(
        &mut self,
        kind: &'static str,
        from: &WorkItemId,
        to: &WorkItemId,
        edge_kind: DependencyKind,
    ) -> Result<(), FactError> {
        let edge = DependencyEdge::new(from.clone(), to.clone(), edge_kind);
        self.check_edge(kind, &edge, None)?;
        self.dependencies.insert(
            edge,
            DependencyNode {
                asserted_revision: self.next_revision(),
            },
        );
        Ok(())
    }

    fn apply_dependency_retracted(
        &mut self,
        kind: &'static str,
        from: &WorkItemId,
        to: &WorkItemId,
        edge_kind: DependencyKind,
    ) -> Result<(), FactError> {
        let edge = DependencyEdge::new(from.clone(), to.clone(), edge_kind);
        if self.dependencies.remove(&edge).is_none() {
            return Err(FactError::UnknownReference {
                fact: kind,
                detail: format!("dependency {edge} is not asserted"),
            });
        }
        Ok(())
    }

    fn apply_decision_opened(
        &mut self,
        kind: &'static str,
        decision: &Decision,
    ) -> Result<(), FactError> {
        if self.decisions.contains_key(&decision.id) {
            return Err(FactError::Duplicate {
                fact: kind,
                detail: format!("decision {} is already opened", decision.id),
            });
        }
        match &decision.subject {
            crate::decision::DecisionSubject::Goal(goal) => {
                if self
                    .goal
                    .as_ref()
                    .is_none_or(|declared| declared.id != *goal)
                {
                    return Err(FactError::UnknownReference {
                        fact: kind,
                        detail: format!("decision subject goal {goal} does not exist"),
                    });
                }
            }
            crate::decision::DecisionSubject::WorkItem(work) => {
                if !self.works.contains_key(work) {
                    return Err(FactError::UnknownReference {
                        fact: kind,
                        detail: format!("decision subject work {work} does not exist"),
                    });
                }
            }
            crate::decision::DecisionSubject::ArtifactVersion(reference) => {
                if self.version_record(reference).is_none() {
                    return Err(FactError::UnknownReference {
                        fact: kind,
                        detail: format!("decision subject {reference} does not exist"),
                    });
                }
            }
        }
        for reference in &decision.evidence {
            if self.version_record(reference).is_none() {
                return Err(FactError::UnknownReference {
                    fact: kind,
                    detail: format!("decision evidence {reference} does not exist"),
                });
            }
        }
        self.decisions.insert(
            decision.id.clone(),
            DecisionNode {
                decision: decision.clone(),
                resolution: None,
                superseded_by: None,
                last_changed: self.next_revision(),
            },
        );
        Ok(())
    }

    fn apply_decision_resolved(
        &mut self,
        kind: &'static str,
        decision: &DecisionId,
        resolution: &DecisionResolution,
    ) -> Result<(), FactError> {
        let node = self.decision_node(kind, decision)?;
        if node.superseded_by.is_some() {
            return Err(FactError::InvalidState {
                fact: kind,
                detail: format!("decision {decision} was superseded and cannot be resolved"),
            });
        }
        if node.resolution.is_some() {
            return Err(FactError::InvalidState {
                fact: kind,
                detail: format!("decision {decision} is already resolved"),
            });
        }
        if !node.decision.accepts_choice(&resolution.choice) {
            let options: Vec<String> = node
                .decision
                .options
                .iter()
                .map(|o| o.label.clone())
                .collect();
            return Err(FactError::InvalidState {
                fact: kind,
                detail: format!(
                    "choice \"{}\" is not one of the declared options {options:?}",
                    resolution.choice
                ),
            });
        }
        let stamp = self.next_revision();
        let node = self
            .decisions
            .get_mut(decision)
            .ok_or(FactError::UnknownReference {
                fact: kind,
                detail: decision.to_string(),
            })?;
        node.resolution = Some(resolution.clone());
        node.last_changed = stamp;
        Ok(())
    }

    fn apply_decision_superseded(
        &mut self,
        kind: &'static str,
        decision: &DecisionId,
        superseded_by: &DecisionId,
    ) -> Result<(), FactError> {
        let node = self.decision_node(kind, decision)?;
        if decision == superseded_by {
            return Err(FactError::InvalidState {
                fact: kind,
                detail: format!("decision {decision} cannot supersede itself"),
            });
        }
        if node.superseded_by.is_some() {
            return Err(FactError::InvalidState {
                fact: kind,
                detail: format!("decision {decision} is already superseded"),
            });
        }
        if !self.decisions.contains_key(superseded_by) {
            return Err(FactError::UnknownReference {
                fact: kind,
                detail: format!("superseding decision {superseded_by} does not exist"),
            });
        }
        let stamp = self.next_revision();
        let node = self
            .decisions
            .get_mut(decision)
            .ok_or(FactError::UnknownReference {
                fact: kind,
                detail: decision.to_string(),
            })?;
        node.superseded_by = Some(superseded_by.clone());
        node.last_changed = stamp;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_artifact_published(
        &mut self,
        kind: &'static str,
        artifact: &ArtifactId,
        version: ArtifactVersion,
        title: Option<String>,
        content: ArtifactContent,
        digest: ArtifactDigest,
        derived_from: &[ArtifactVersionRef],
        published_at_ms: i64,
    ) -> Result<(), FactError> {
        if let Some(node) = self.artifacts.get(artifact) {
            let expected = ArtifactVersion::new(node.current_version().get() + 1)
                .unwrap_or(ArtifactVersion::FIRST);
            if version != expected {
                return Err(FactError::SequenceViolation {
                    fact: kind,
                    detail: format!(
                        "artifact {artifact} is at version {current}; the next publication must be version {expected}, got {version}",
                        current = node.current_version()
                    ),
                });
            }
        } else if version != ArtifactVersion::FIRST {
            return Err(FactError::SequenceViolation {
                fact: kind,
                detail: format!(
                    "artifact {artifact} is new; its first publication must be version 1, got {version}"
                ),
            });
        }
        for reference in derived_from {
            if self.version_record(reference).is_none() {
                return Err(FactError::UnknownReference {
                    fact: kind,
                    detail: format!("provenance reference {reference} does not exist"),
                });
            }
        }
        let stamp = self.next_revision();
        let record = VersionRecord {
            title,
            content,
            digest,
            derived_from: derived_from.to_vec(),
            published_at_ms,
            published_revision: stamp,
        };
        match self.artifacts.get_mut(artifact) {
            Some(node) => {
                node.versions.insert(version, record);
                node.last_changed = stamp;
            }
            None => {
                let mut versions = BTreeMap::new();
                versions.insert(version, record);
                self.artifacts.insert(
                    artifact.clone(),
                    ArtifactNode {
                        versions,
                        last_changed: stamp,
                    },
                );
            }
        }
        Ok(())
    }

    fn apply_evidence_recorded(
        &mut self,
        kind: &'static str,
        evidence: &Evidence,
    ) -> Result<(), FactError> {
        if self.evidence.contains_key(&evidence.id) {
            return Err(FactError::Duplicate {
                fact: kind,
                detail: format!("evidence {} is already recorded", evidence.id),
            });
        }
        match &evidence.target {
            crate::evidence::EvidenceTarget::WorkItem(work) => {
                if !self.works.contains_key(work) {
                    return Err(FactError::UnknownReference {
                        fact: kind,
                        detail: format!("evidence target work {work} does not exist"),
                    });
                }
            }
            crate::evidence::EvidenceTarget::ArtifactVersion(reference) => {
                if self.version_record(reference).is_none() {
                    return Err(FactError::UnknownReference {
                        fact: kind,
                        detail: format!("evidence target {reference} does not exist"),
                    });
                }
            }
            crate::evidence::EvidenceTarget::Decision(decision) => {
                if !self.decisions.contains_key(decision) {
                    return Err(FactError::UnknownReference {
                        fact: kind,
                        detail: format!("evidence target decision {decision} does not exist"),
                    });
                }
            }
        }
        self.evidence.insert(evidence.id.clone(), evidence.clone());
        Ok(())
    }

    fn apply_attempt_started(
        &mut self,
        kind: &'static str,
        attempt: &AttemptId,
        work: &WorkItemId,
        expected_revision: ProjectRevision,
        grant: &CapabilityGrant,
        started_at_ms: i64,
    ) -> Result<(), FactError> {
        if self.attempts.contains_key(attempt) {
            return Err(FactError::Duplicate {
                fact: kind,
                detail: format!("attempt {attempt} already exists"),
            });
        }
        let node = self.work_node(kind, work)?;
        if node.is_declared_terminal() {
            return Err(FactError::InvalidState {
                fact: kind,
                detail: format!("work {work} is superseded or canceled and cannot be attempted"),
            });
        }
        if expected_revision != self.revision {
            return Err(FactError::SequenceViolation {
                fact: kind,
                detail: format!(
                    "attempt {attempt} expects revision {expected_revision} but the project is at revision {current}",
                    current = self.revision
                ),
            });
        }
        let read_scopes = node.work.read_scopes.clone();
        let write_scopes = node.effective_write_scopes();
        if let Err(gap) = grant_covers_scopes(grant, &read_scopes, &write_scopes) {
            return Err(FactError::CapabilityExceeded {
                fact: kind,
                detail: format!(
                    "grant {grant_id} does not cover work {work}: {gap}",
                    grant_id = grant.id
                ),
            });
        }
        self.attempts.insert(
            attempt.clone(),
            AttemptNode {
                id: attempt.clone(),
                work: work.clone(),
                expected_revision,
                grant: grant.clone(),
                started_at_ms,
                conclusion: None,
                last_changed: self.next_revision(),
            },
        );
        Ok(())
    }

    fn apply_attempt_concluded(
        &mut self,
        kind: &'static str,
        attempt: &AttemptId,
        conclusion: &AttemptConclusion,
    ) -> Result<(), FactError> {
        let node = self.attempt_node(kind, attempt)?;
        if node.conclusion.is_some() {
            return Err(FactError::InvalidState {
                fact: kind,
                detail: format!("attempt {attempt} is already concluded"),
            });
        }
        if let AttemptConclusion::Accepted { receipt } = conclusion {
            if &receipt.attempt != attempt {
                return Err(FactError::InvalidState {
                    fact: kind,
                    detail: format!(
                        "receipt belongs to attempt {other}, not {attempt}",
                        other = receipt.attempt
                    ),
                });
            }
            if receipt.work != node.work {
                return Err(FactError::InvalidState {
                    fact: kind,
                    detail: format!(
                        "receipt names work {receipt_work} but attempt {attempt} runs {attempt_work}",
                        receipt_work = receipt.work,
                        attempt_work = node.work
                    ),
                });
            }
            if receipt.completed_at_ms < node.started_at_ms {
                return Err(FactError::InvalidState {
                    fact: kind,
                    detail: format!(
                        "receipt concludes attempt {attempt} before it started ({completed} < {started})",
                        completed = receipt.completed_at_ms,
                        started = node.started_at_ms
                    ),
                });
            }
            if receipt.usage.duration_ms < 0 {
                return Err(FactError::InvalidState {
                    fact: kind,
                    detail: format!(
                        "receipt reports negative duration {}",
                        receipt.usage.duration_ms
                    ),
                });
            }
        }
        let stamp = self.next_revision();
        let node = self
            .attempts
            .get_mut(attempt)
            .ok_or(FactError::UnknownReference {
                fact: kind,
                detail: attempt.to_string(),
            })?;
        node.conclusion = Some(conclusion.clone());
        node.last_changed = stamp;
        Ok(())
    }

    fn apply_invalidation_declared(
        &mut self,
        kind: &'static str,
        target: &InvalidationTarget,
        cause: &InvalidationCause,
        changed_input: Option<ArtifactVersionRef>,
    ) -> Result<(), FactError> {
        let known = match target {
            InvalidationTarget::Work(work) => self.works.contains_key(work),
            InvalidationTarget::ArtifactVersion(reference) => {
                self.version_record(reference).is_some()
            }
            InvalidationTarget::Evidence(evidence) => self.evidence.contains_key(evidence),
        };
        if !known {
            return Err(FactError::UnknownReference {
                fact: kind,
                detail: format!("invalidation target does not exist: {target:?}"),
            });
        }
        let stamp = self.next_revision();
        self.invalidations
            .entry(target.clone())
            .or_default()
            .push(DeclaredInvalidation {
                cause: cause.clone(),
                changed_input,
                revision: stamp,
            });
        Ok(())
    }

    // -- internal helpers ---------------------------------------------------

    /// The revision this fact will land at once applied.
    fn next_revision(&self) -> ProjectRevision {
        ProjectRevision::from_count(self.revision.get() + 1)
    }

    fn work_node(&self, kind: &'static str, work: &WorkItemId) -> Result<&WorkNode, FactError> {
        self.works
            .get(work)
            .ok_or_else(|| FactError::UnknownReference {
                fact: kind,
                detail: format!("work {work} does not exist"),
            })
    }

    fn decision_node(
        &self,
        kind: &'static str,
        decision: &DecisionId,
    ) -> Result<&DecisionNode, FactError> {
        self.decisions
            .get(decision)
            .ok_or_else(|| FactError::UnknownReference {
                fact: kind,
                detail: format!("decision {decision} does not exist"),
            })
    }

    fn attempt_node(
        &self,
        kind: &'static str,
        attempt: &AttemptId,
    ) -> Result<&AttemptNode, FactError> {
        self.attempts
            .get(attempt)
            .ok_or_else(|| FactError::UnknownReference {
                fact: kind,
                detail: format!("attempt {attempt} does not exist"),
            })
    }

    // -- read accessors -----------------------------------------------------

    /// The project identity, when at least `ProjectCreated` was folded.
    pub fn project_id(&self) -> Option<ProjectId> {
        self.project.clone()
    }

    /// The project goal, when declared.
    pub fn goal(&self) -> Option<&Goal> {
        self.goal.as_ref()
    }

    /// The number of facts applied so far.
    pub fn revision(&self) -> ProjectRevision {
        self.revision
    }

    /// All work nodes in deterministic (id) order.
    pub fn works(&self) -> impl Iterator<Item = (&WorkItemId, &WorkNode)> {
        self.works.iter()
    }

    /// Look up one work node.
    pub fn work(&self, id: &WorkItemId) -> Option<&WorkNode> {
        self.works.get(id)
    }

    /// All dependency edges in deterministic order.
    pub fn dependencies(&self) -> impl Iterator<Item = (&DependencyEdge, &DependencyNode)> {
        self.dependencies.iter()
    }

    /// Edges where `id` is the dependent.
    pub fn dependencies_of(&self, id: &WorkItemId) -> impl Iterator<Item = &DependencyEdge> {
        self.dependencies
            .keys()
            .filter(move |edge| &edge.from == id)
    }

    /// Edges where `id` is the prerequisite.
    pub fn dependents_of(&self, id: &WorkItemId) -> impl Iterator<Item = &DependencyEdge> {
        self.dependencies.keys().filter(move |edge| &edge.to == id)
    }

    /// Look up one dependency edge.
    pub fn dependency(&self, edge: &DependencyEdge) -> Option<&DependencyNode> {
        self.dependencies.get(edge)
    }

    /// All decisions in deterministic order.
    pub fn decisions(&self) -> impl Iterator<Item = (&DecisionId, &DecisionNode)> {
        self.decisions.iter()
    }

    /// Look up one decision.
    pub fn decision(&self, id: &DecisionId) -> Option<&DecisionNode> {
        self.decisions.get(id)
    }

    /// All artifacts in deterministic order.
    pub fn artifacts(&self) -> impl Iterator<Item = (&ArtifactId, &ArtifactNode)> {
        self.artifacts.iter()
    }

    /// Look up one artifact.
    pub fn artifact(&self, id: &ArtifactId) -> Option<&ArtifactNode> {
        self.artifacts.get(id)
    }

    /// The newest published version of an artifact.
    pub fn current_version(&self, id: &ArtifactId) -> Option<ArtifactVersion> {
        self.artifacts.get(id).map(ArtifactNode::current_version)
    }

    /// Look up one exact published version.
    pub fn version_record(&self, reference: &ArtifactVersionRef) -> Option<&VersionRecord> {
        self.artifacts
            .get(&reference.artifact)
            .and_then(|node| node.versions.get(&reference.version))
    }

    /// All evidence in deterministic order.
    pub fn evidence(&self) -> impl Iterator<Item = (&EvidenceId, &Evidence)> {
        self.evidence.iter()
    }

    /// Evidence attached to one exact target.
    pub fn evidence_for(&self, target: &crate::evidence::EvidenceTarget) -> Vec<&Evidence> {
        self.evidence
            .values()
            .filter(|e| &e.target == target)
            .collect()
    }

    /// Look up one piece of evidence.
    pub fn evidence_by_id(&self, id: &EvidenceId) -> Option<&Evidence> {
        self.evidence.get(id)
    }

    /// All attempts in deterministic order.
    pub fn attempts(&self) -> impl Iterator<Item = (&AttemptId, &AttemptNode)> {
        self.attempts.iter()
    }

    /// Attempts that ran one work item.
    pub fn attempts_of(&self, id: &WorkItemId) -> impl Iterator<Item = &AttemptNode> {
        self.attempts.values().filter(move |node| &node.work == id)
    }

    /// Look up one attempt.
    pub fn attempt(&self, id: &AttemptId) -> Option<&AttemptNode> {
        self.attempts.get(id)
    }

    /// All declared invalidations, keyed by target.
    pub fn declared_invalidations(
        &self,
    ) -> &BTreeMap<InvalidationTarget, Vec<DeclaredInvalidation>> {
        &self.invalidations
    }

    /// Declared invalidations for one target.
    pub fn invalidations_for(&self, target: &InvalidationTarget) -> &[DeclaredInvalidation] {
        self.invalidations
            .get(target)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }
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
    use crate::artifact::{ArtifactContent, ArtifactDigest};
    use crate::dependency::DependencyKind;
    use crate::evidence::{Evidence, EvidenceKind, EvidenceOutcome, EvidenceTarget};
    use crate::grant::CapabilityGrant;
    use crate::work::{CompletionRule, ExecutorClass, WorkItem, WorkKind};

    fn ids<T: std::str::FromStr<Err: std::fmt::Debug>>(value: &str) -> T {
        value.parse::<T>().ok().expect("valid id literal in test")
    }

    fn base_facts() -> Vec<DomainFact> {
        vec![DomainFact::ProjectCreated {
            project: ids("prj_demo"),
        }]
    }

    fn model_work(id: &str) -> WorkItem {
        WorkItem::new(
            ids(id),
            WorkKind::parse("research").ok().unwrap(),
            ExecutorClass::Model,
            "investigate the repository",
            CompletionRule::Receipt,
        )
    }

    fn digest(n: u8) -> ArtifactDigest {
        ArtifactDigest::parse(&format!("{:064x}", n)).ok().unwrap()
    }

    fn publish(artifact: &str, version: u32, revision_note: u8) -> DomainFact {
        DomainFact::ArtifactVersionPublished {
            artifact: ids(artifact),
            version: ArtifactVersion::new(version).ok().unwrap(),
            title: None,
            content: ArtifactContent::new(serde_json::json!({"note": revision_note})),
            digest: digest(revision_note),
            derived_from: Vec::new(),
            published_at_ms: 1_000,
        }
    }

    #[test]
    fn folds_a_small_history_and_counts_revisions() {
        let mut facts = base_facts();
        facts.push(DomainFact::WorkDeclared {
            work: model_work("work_research"),
        });
        let graph = ProjectGraph::from_facts(&facts)
            .ok()
            .expect("history folds");
        assert_eq!(graph.revision(), ProjectRevision::from_count(2));
        assert_eq!(graph.project_id(), Some(ids("prj_demo")));
        assert!(graph.work(&ids("work_research")).is_some());
    }

    #[test]
    fn rejects_facts_before_project_creation() {
        let fact = DomainFact::WorkDeclared {
            work: model_work("work_early"),
        };
        let error = ProjectGraph::from_facts(&[fact]).err().expect("must fail");
        assert!(matches!(error, FactError::NotInitialized { .. }));
    }

    #[test]
    fn rejects_duplicate_work_and_dependency() {
        let mut facts = base_facts();
        facts.push(DomainFact::WorkDeclared {
            work: model_work("work_a"),
        });
        facts.push(DomainFact::WorkDeclared {
            work: model_work("work_a"),
        });
        let error = ProjectGraph::from_facts(&facts)
            .err()
            .expect("duplicate must fail");
        assert!(matches!(
            error,
            FactError::Duplicate {
                fact: "work_declared",
                ..
            }
        ));

        let mut facts = base_facts();
        facts.push(DomainFact::WorkDeclared {
            work: model_work("work_a"),
        });
        facts.push(DomainFact::WorkDeclared {
            work: model_work("work_b"),
        });
        facts.push(DomainFact::DependencyAsserted {
            from: ids("work_b"),
            to: ids("work_a"),
            kind: DependencyKind::Requires,
        });
        facts.push(DomainFact::DependencyAsserted {
            from: ids("work_b"),
            to: ids("work_a"),
            kind: DependencyKind::Requires,
        });
        let error = ProjectGraph::from_facts(&facts)
            .err()
            .expect("duplicate edge must fail");
        assert!(matches!(
            error,
            FactError::Duplicate {
                fact: "dependency_asserted",
                ..
            }
        ));
    }

    #[test]
    fn rejects_dependency_cycles_at_fold_time() {
        let mut facts = base_facts();
        for id in ["work_a", "work_b"] {
            facts.push(DomainFact::WorkDeclared {
                work: model_work(id),
            });
        }
        facts.push(DomainFact::DependencyAsserted {
            from: ids("work_a"),
            to: ids("work_b"),
            kind: DependencyKind::Requires,
        });
        facts.push(DomainFact::DependencyAsserted {
            from: ids("work_b"),
            to: ids("work_a"),
            kind: DependencyKind::Requires,
        });
        let error = ProjectGraph::from_facts(&facts)
            .err()
            .expect("cycle must fail");
        assert!(matches!(
            error,
            FactError::DependencyCycle {
                fact: "dependency_asserted",
                ..
            }
        ));
    }

    #[test]
    fn enforces_version_monotonicity_and_provenance() {
        let mut facts = base_facts();
        facts.push(publish("art_brief", 2, 1));
        let error = ProjectGraph::from_facts(&facts)
            .err()
            .expect("first version must be 1");
        assert!(matches!(error, FactError::SequenceViolation { .. }));

        let mut facts = base_facts();
        facts.push(publish("art_brief", 1, 1));
        facts.push(publish("art_brief", 3, 2));
        let error = ProjectGraph::from_facts(&facts)
            .err()
            .expect("version skip must fail");
        assert!(matches!(error, FactError::SequenceViolation { .. }));

        let mut facts = base_facts();
        let mut orphan = publish("art_brief", 1, 1);
        if let DomainFact::ArtifactVersionPublished { derived_from, .. } = &mut orphan {
            derived_from.push(ArtifactVersionRef::new(
                ids("art_unknown"),
                ArtifactVersion::FIRST,
            ));
        }
        facts.push(orphan);
        let error = ProjectGraph::from_facts(&facts)
            .err()
            .expect("orphan provenance must fail");
        assert!(matches!(error, FactError::UnknownReference { .. }));
    }

    #[test]
    fn rejects_evidence_with_unknown_target() {
        let mut facts = base_facts();
        facts.push(DomainFact::EvidenceRecorded {
            evidence: Evidence::new(
                ids("ev_1"),
                EvidenceKind::Inspection,
                EvidenceTarget::WorkItem(ids("work_missing")),
                EvidenceOutcome::Info,
                "looked around",
                1_000,
            ),
        });
        let error = ProjectGraph::from_facts(&facts)
            .err()
            .expect("unknown target must fail");
        assert!(matches!(
            error,
            FactError::UnknownReference {
                fact: "evidence_recorded",
                ..
            }
        ));
    }

    #[test]
    fn enforces_attempt_start_revision_and_grant_coverage() {
        fn workspace_work(id: &str) -> WorkItem {
            let mut work = model_work(id);
            work.executor = ExecutorClass::Workspace;
            let src = crate::work::ScopePath::parse("src").ok().unwrap();
            work.write_scopes = vec![src.clone()];
            work.capability_ceiling.write_scopes = vec![src];
            work
        }
        // Wrong expected revision.
        let mut facts = base_facts();
        facts.push(DomainFact::WorkDeclared {
            work: workspace_work("work_impl"),
        });
        facts.push(DomainFact::AttemptStarted {
            attempt: ids("att_1"),
            work: ids("work_impl"),
            expected_revision: ProjectRevision::from_count(99),
            grant: CapabilityGrant::for_scopes(
                ids("grant_1"),
                vec![],
                vec![crate::work::ScopePath::parse("src").ok().unwrap()],
            ),
            started_at_ms: 1_000,
        });
        let error = ProjectGraph::from_facts(&facts)
            .err()
            .expect("wrong revision must fail");
        assert!(matches!(error, FactError::SequenceViolation { .. }));

        // Right revision, insufficient grant.
        let mut facts = base_facts();
        facts.push(DomainFact::WorkDeclared {
            work: workspace_work("work_impl"),
        });
        facts.push(DomainFact::AttemptStarted {
            attempt: ids("att_1"),
            work: ids("work_impl"),
            expected_revision: ProjectRevision::from_count(2),
            grant: CapabilityGrant::read_only(ids("grant_1"), vec![]),
            started_at_ms: 1_000,
        });
        let error = ProjectGraph::from_facts(&facts)
            .err()
            .expect("grant gap must fail");
        assert!(matches!(error, FactError::CapabilityExceeded { .. }));
    }
}
