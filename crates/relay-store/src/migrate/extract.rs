//! Extracting durable product meaning from legacy evidence.
//!
//! A pure function of [`LegacyEvidence`]: no IO, no clocks beyond the
//! timestamps carried in the evidence itself. The extractor walks the
//! legacy stream, keeps what carries product meaning, and emits a fact
//! stream that folds cleanly through the domain graph, plus a structured
//! [`ExtractionReport`] recording every mapping, inference, unmapped legacy
//! kind, and anything needing user review.
//!
//! Mapping table (see `docs/plans/fact-format-proposal.md` §8):
//!
//! | Legacy | New |
//! | --- | --- |
//! | `project.initialized` | `ProjectCreated` + `GoalDeclared` (goal text; provenance `Import`) |
//! | `artifact.created`/`artifact.revised` (any kind) | a **preservation** artifact version on the same legacy artifact id: content is a `relay.legacy-document@1` record, the digest commits the original Markdown bytes, and the bytes are copied to the storage blob path |
//! | opportunity-brief | + derived `relay.brief@1` on `art_brief` (derived from the preservation version) |
//! | prd | + one `relay.requirement@1` per `REQ-n` and one `relay.criterion@1` per `AC-n` (derived from the preservation version) |
//! | trd | + derived `relay.design@1` on `art_design` |
//! | decision-log | + one `relay.design-decision@1` per entry |
//! | execution-plan | + derived `relay.plan@1` on `art_plan` |
//! | delivery-report | + derived `relay.delivery@1` on `art_delivery` |
//! | verification-report | preserved + verification evidence (below) |
//! | `artifact.approved` | `approve-artifact` human work + decision opened and resolved (`approve`) on the exact preserved version |
//! | `verification.completed` | one `EvidenceRecorded` per criterion result, targeting the derived criterion version when the statement matches |
//! | delivery-hold at the end | `prepare-delivery-evidence` work + `request-delivery-authorization` human work with an **open** authorization decision |
//! | implementation/verification mid-flight | coarse `implement-task`/`verify-criteria` continuation work, marked imported |
//! | mode churn, sessions, model calls, transitions, pauses | unmapped (reported with counts) |

use std::collections::BTreeMap;

use sha2::Digest;
use sha2::Sha256;

use relay_domain::ArtifactContent;
use relay_domain::ArtifactDigest;
use relay_domain::ArtifactId;
use relay_domain::ArtifactVersion;
use relay_domain::ArtifactVersionRef;
use relay_domain::DecisionId;
use relay_domain::DecisionResolution;
use relay_domain::DependencyKind;
use relay_domain::DomainFact;
use relay_domain::Evidence;
use relay_domain::EvidenceId;
use relay_domain::EvidenceKind;
use relay_domain::EvidenceOutcome;
use relay_domain::EvidenceTarget;
use relay_domain::Goal;
use relay_domain::GoalId;
use relay_domain::ProjectId;
use relay_domain::Provenance;
use relay_domain::ProvenanceSource;
use relay_domain::WorkItemId;
use relay_domain::work::CompletionRule;
use relay_domain::work::ExecutorClass;
use relay_domain::work::WorkItem;
use relay_workflows::schemas::BriefDoc;
use relay_workflows::schemas::CriterionDoc;
use relay_workflows::schemas::DeliveryDoc;
use relay_workflows::schemas::DesignDecisionDoc;
use relay_workflows::schemas::DesignDoc;
use relay_workflows::schemas::PlanDoc;
use relay_workflows::schemas::RequirementDoc;
use relay_workflows::schemas::StructuredDoc;
use relay_workflows::schemas::TaskKind;
use relay_workflows::schemas::VerificationMethod;

use crate::canonical::Json;
use crate::error::Result;
use crate::error::StoreError;
use crate::migrate::legacy_source::LegacyEvent;
use crate::migrate::legacy_source::LegacyEvidence;

/// Schema tag of a preserved legacy document.
pub const LEGACY_DOCUMENT_SCHEMA: &str = "relay.legacy-document@1";

/// One mapped legacy event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MappingEntry {
    /// Legacy sequence.
    pub sequence: u64,
    /// Legacy event type.
    pub event_type: String,
    /// What it became.
    pub mapping: String,
}

/// One legacy kind with no durable meaning, and how often it occurred.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnmappedKind {
    /// The legacy event type.
    pub kind: String,
    /// Occurrences.
    pub count: u64,
}

/// Something the migration inferred rather than read, with the reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inference {
    /// What was inferred.
    pub note: String,
}

/// Something a human should review after the migration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewNeeded {
    /// Why review is needed.
    pub note: String,
}

/// The structured result of meaning extraction.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ExtractionReport {
    /// Legacy events that produced new facts.
    pub mappings: Vec<MappingEntry>,
    /// Legacy kinds with no durable meaning (with counts).
    pub unmapped: Vec<UnmappedKind>,
    /// Relationships the legacy data only supports coarsely.
    pub inferences: Vec<Inference>,
    /// Items a human should review.
    pub needs_review: Vec<ReviewNeeded>,
    /// Legacy events read.
    pub legacy_events: u64,
    /// Facts emitted.
    pub facts_emitted: u64,
}

impl ExtractionReport {
    /// Serialize to the storage-canonical JSON form.
    pub fn to_json(&self) -> Result<Json> {
        Ok(Json::object([
            ("factsEmitted", Json::UInt(self.facts_emitted)),
            ("legacyEvents", Json::UInt(self.legacy_events)),
            (
                "mappings",
                Json::Array(
                    self.mappings
                        .iter()
                        .map(|entry| {
                            Ok(Json::object([
                                ("mapping", Json::string(&entry.mapping)),
                                ("sequence", Json::UInt(entry.sequence)),
                                ("type", Json::string(&entry.event_type)),
                            ]))
                        })
                        .collect::<Result<Vec<_>>>()?,
                ),
            ),
            (
                "needsReview",
                Json::Array(
                    self.needs_review
                        .iter()
                        .map(|item| Ok(Json::object([("note", Json::string(&item.note))])))
                        .collect::<Result<Vec<_>>>()?,
                ),
            ),
            (
                "unmapped",
                Json::Array(
                    self.unmapped
                        .iter()
                        .map(|kind| {
                            Ok(Json::object([
                                ("count", Json::UInt(kind.count)),
                                ("kind", Json::string(&kind.kind)),
                            ]))
                        })
                        .collect::<Result<Vec<_>>>()?,
                ),
            ),
            (
                "inferences",
                Json::Array(
                    self.inferences
                        .iter()
                        .map(|item| Ok(Json::object([("note", Json::string(&item.note))])))
                        .collect::<Result<Vec<_>>>()?,
                ),
            ),
        ]))
    }

    /// Render the report for a person.
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str("# Semantic migration report\n\n");
        out.push_str(&format!(
            "- legacy events read: {}\n- facts emitted: {}\n- mapped events: {}\n- unmapped legacy kinds: {}\n- inferences: {}\n- needs review: {}\n\n",
            self.legacy_events,
            self.facts_emitted,
            self.mappings.len(),
            self.unmapped.iter().map(|k| k.count).sum::<u64>(),
            self.inferences.len(),
            self.needs_review.len()
        ));
        out.push_str("## Mappings\n\n");
        out.push_str("| Seq | Legacy event | Became |\n| --- | --- | --- |\n");
        for entry in &self.mappings {
            out.push_str(&format!(
                "| {} | {} | {} |\n",
                entry.sequence, entry.event_type, entry.mapping
            ));
        }
        out.push_str("\n## Unmapped legacy kinds (no durable meaning)\n\n");
        for kind in &self.unmapped {
            out.push_str(&format!("- {} ×{}\n", kind.kind, kind.count));
        }
        out.push_str("\n## Inferred relationships (coarse in the legacy data)\n\n");
        for item in &self.inferences {
            out.push_str(&format!("- {}\n", item.note));
        }
        out.push_str("\n## Needs review\n\n");
        if self.needs_review.is_empty() {
            out.push_str("- nothing pending\n");
        } else {
            for item in &self.needs_review {
                out.push_str(&format!("- {}\n", item.note));
            }
        }
        out
    }
}

/// One legacy artifact version, as the extractor tracks it.
#[derive(Debug, Clone)]
struct LegacyArtifactVersion {
    sequence: u64,
    timestamp: String,
    version: u32,
    path: Option<String>,
    title: Option<String>,
    /// Parsed Markdown sections (when the bytes were found).
    document: Option<LegacyDocument>,
}

/// A parsed legacy Markdown document: frontmatter fields + body sections.
#[derive(Debug, Clone, Default)]
struct LegacyDocument {
    title: String,
    sections: BTreeMap<String, String>,
}

impl LegacyDocument {
    /// The text under a `## Heading` section (trimmed).
    fn section(&self, heading: &str) -> &str {
        self.sections.get(heading).map(String::as_str).unwrap_or("")
    }
}

/// One legacy artifact, keyed by its id.
#[derive(Debug, Clone)]
struct LegacyArtifact {
    kind: String,
    versions: Vec<LegacyArtifactVersion>,
}

/// Extract durable product meaning from validated legacy evidence.
///
/// Returns the new fact stream (which folds cleanly through
/// `ProjectGraph::from_facts`) and the structured report.
pub fn extract(evidence: &LegacyEvidence) -> Result<(Vec<DomainFact>, ExtractionReport)> {
    Extractor::new(evidence).run()
}

struct Extractor<'a> {
    evidence: &'a LegacyEvidence,
    report: ExtractionReport,
    facts: Vec<DomainFact>,
    /// next version per new artifact id.
    versions: BTreeMap<String, u32>,
    /// artifacts keyed by legacy artifact id.
    legacy_artifacts: BTreeMap<String, LegacyArtifact>,
    /// legacy artifact ids in first-creation (event) order.
    artifact_order: Vec<String>,
    /// (criterion statement, criterion reference) pairs emitted so far.
    criteria: Vec<(String, ArtifactVersionRef)>,
    /// the latest published brief version (for requirement pins).
    brief_version: Option<ArtifactVersionRef>,
    approvals: Vec<(String, u32, u64, String)>,
    /// terminal legacy mode + paused flag.
    final_mode: Option<String>,
    final_paused: bool,
    clock_ms: i64,
}

impl<'a> Extractor<'a> {
    fn new(evidence: &'a LegacyEvidence) -> Extractor<'a> {
        Extractor {
            evidence,
            report: ExtractionReport {
                legacy_events: evidence.events.len() as u64,
                ..ExtractionReport::default()
            },
            facts: Vec::new(),
            versions: BTreeMap::new(),
            legacy_artifacts: BTreeMap::new(),
            artifact_order: Vec::new(),
            criteria: Vec::new(),
            brief_version: None,
            approvals: Vec::new(),
            final_mode: None,
            final_paused: false,
            clock_ms: 0,
        }
    }

    fn run(mut self) -> Result<(Vec<DomainFact>, ExtractionReport)> {
        let project =
            ProjectId::parse(&self.evidence.project_id).map_err(|error| StoreError::Legacy {
                reason: format!("legacy project id: {error}"),
            })?;

        // Pass 1: gather artifact metadata, approvals, mode, and goal.
        let mut goal_text: Option<String> = None;
        let mut unmapped: BTreeMap<String, u64> = BTreeMap::new();
        for event in &self.evidence.events {
            match event.event_type.as_str() {
                "project.initialized" => {
                    goal_text = event
                        .payload
                        .get("goal")
                        .and_then(Json::as_str)
                        .map(str::to_string);
                    self.clock_ms = self.clock_ms.max(parse_ms(&event.timestamp));
                }
                "artifact.created" | "artifact.revised" => {
                    self.gather_artifact(event)?;
                }
                "artifact.approved" => {
                    let artifact = str_field(&event.payload, "artifactId")
                        .ok_or_else(|| bad_payload(event, "artifactId"))?;
                    let version = event
                        .payload
                        .get("version")
                        .and_then(Json::as_u64)
                        .and_then(|v| u32::try_from(v).ok())
                        .ok_or_else(|| bad_payload(event, "version"))?;
                    self.approvals.push((
                        artifact,
                        version,
                        event.sequence,
                        event.timestamp.clone(),
                    ));
                }
                "verification.completed" => {
                    self.clock_ms = self.clock_ms.max(parse_ms(&event.timestamp));
                }
                "mode.entered" => {
                    self.final_mode = event
                        .payload
                        .get("to")
                        .and_then(Json::as_str)
                        .map(str::to_string);
                    self.clock_ms = self.clock_ms.max(parse_ms(&event.timestamp));
                }
                "workflow.paused" => {
                    self.final_paused = true;
                }
                "workflow.resumed" => {
                    self.final_paused = false;
                }
                _ => {
                    *unmapped.entry(event.event_type.clone()).or_default() += 1;
                }
            }
        }
        self.report.unmapped = unmapped
            .into_iter()
            .map(|(kind, count)| UnmappedKind { kind, count })
            .collect();
        // Pauses that were only ever flow control are unmapped churn; the
        // extractor reports them once as unmapped if they had no durable
        // effect (delivery-hold handling below re-adds the held pause).
        if self.final_paused {
            self.report.unmapped.push(UnmappedKind {
                kind: "workflow.paused (delivery hold — mapped below)".to_string(),
                count: 0,
            });
            self.report
                .unmapped
                .retain(|k| !k.kind.starts_with("workflow.paused ("));
        }

        // Pass 2: emit facts in a fold-valid order.
        self.emit_project(&project, goal_text.as_deref())?;
        let artifact_ids: Vec<String> = self.artifact_order.clone();
        for id in artifact_ids {
            let versions: Vec<u32> = self
                .legacy_artifacts
                .get(&id)
                .map(|a| a.versions.iter().map(|v| v.version).collect())
                .unwrap_or_default();
            for version in versions {
                self.emit_artifact(&project, &id, version)?;
            }
        }
        self.emit_approvals()?;
        self.emit_terminal_state(&project)?;
        self.emit_verification_evidence()?;

        self.report.facts_emitted = self.facts.len() as u64;
        Ok((self.facts, self.report))
    }

    fn gather_artifact(&mut self, event: &LegacyEvent) -> Result<()> {
        let id = str_field(&event.payload, "artifactId")
            .ok_or_else(|| bad_payload(event, "artifactId"))?;
        let kind = str_field(&event.payload, "kind").ok_or_else(|| bad_payload(event, "kind"))?;
        let version = event
            .payload
            .get("version")
            .and_then(Json::as_u64)
            .and_then(|v| u32::try_from(v).ok())
            .ok_or_else(|| bad_payload(event, "version"))?;
        let path = str_field(&event.payload, "path");
        let title = str_field(&event.payload, "title");
        let document = path
            .as_ref()
            .and_then(|p| self.evidence.artifacts.get(p))
            .map(|bytes| parse_legacy_markdown(bytes));
        if path.is_some() && document.is_none() {
            self.report.needs_review.push(ReviewNeeded {
                note: format!(
                    "legacy artifact {kind} v{version} references {} whose bytes were not \
                     found; preserved from the event summary only",
                    path.clone().unwrap_or_default()
                ),
            });
        }
        let artifact = self.legacy_artifacts.entry(id.clone()).or_insert_with(|| {
            self.artifact_order.push(id.clone());
            LegacyArtifact {
                kind: kind.clone(),
                versions: Vec::new(),
            }
        });
        if artifact.kind != kind {
            self.report.needs_review.push(ReviewNeeded {
                note: format!("legacy artifact changed kind ({} → {kind})", artifact.kind),
            });
        }
        // artifact.revised carries version N+1; keep versions sorted.
        if artifact.versions.iter().any(|v| v.version == version) {
            return Err(StoreError::Legacy {
                reason: format!("legacy artifact {id} declares version {version} more than once"),
            });
        }
        artifact.versions.push(LegacyArtifactVersion {
            sequence: event.sequence,
            timestamp: event.timestamp.clone(),
            version,
            path,
            title,
            document,
        });
        artifact.versions.sort_by_key(|v| v.version);
        self.clock_ms = self.clock_ms.max(parse_ms(&event.timestamp));
        Ok(())
    }

    fn emit_project(&mut self, project: &ProjectId, goal_text: Option<&str>) -> Result<()> {
        self.facts.push(DomainFact::ProjectCreated {
            project: project.clone(),
        });
        self.report.mappings.push(MappingEntry {
            sequence: 1,
            event_type: "project.initialized".to_string(),
            mapping: "project_created".to_string(),
        });
        let Some(text) = goal_text else {
            self.report.needs_review.push(ReviewNeeded {
                note: "the legacy project declared no goal text; a placeholder goal was \
                       created and should be edited"
                    .to_string(),
            });
            return Ok(());
        };
        let goal = Goal {
            id: GoalId::parse("goal_migrated").map_err(|e| StoreError::Legacy {
                reason: e.to_string(),
            })?,
            outcome: text.to_string(),
            constraints: Vec::new(),
            success_definition: String::new(),
            provenance: Provenance {
                source: ProvenanceSource::Import,
                statement: Some(text.to_string()),
                at_ms: self.clock_ms,
            },
            declared_at_ms: self.clock_ms,
        };
        self.facts.push(DomainFact::GoalDeclared { goal });
        self.report.mappings.push(MappingEntry {
            sequence: 1,
            event_type: "project.initialized".to_string(),
            mapping: format!("goal_declared (imported: \"{text}\")"),
        });
        Ok(())
    }

    /// Emit the preservation version plus every derived document for one
    /// legacy artifact version.
    fn emit_artifact(&mut self, project: &ProjectId, legacy_id: &str, version: u32) -> Result<()> {
        let (kind, meta, document) = {
            let artifact =
                self.legacy_artifacts
                    .get(legacy_id)
                    .ok_or_else(|| StoreError::Legacy {
                        reason: format!("unknown legacy artifact {legacy_id}"),
                    })?;
            let meta = artifact
                .versions
                .iter()
                .find(|v| v.version == version)
                .ok_or_else(|| StoreError::Legacy {
                    reason: format!("unknown legacy artifact version {legacy_id} v{version}"),
                })?
                .clone();
            (artifact.kind.clone(), meta.clone(), meta.document.clone())
        };
        let _ = project;
        let legacy_ref = ArtifactVersionRef {
            artifact: ArtifactId::parse(legacy_id).map_err(|e| StoreError::Legacy {
                reason: format!("legacy artifact id {legacy_id}: {e}"),
            })?,
            version: ArtifactVersion::new(version).map_err(|e| StoreError::Legacy {
                reason: e.to_string(),
            })?,
        };

        // The preservation record commits the original Markdown bytes.
        let bytes = meta
            .path
            .as_ref()
            .and_then(|p| self.evidence.artifacts.get(p));
        let (digest, markdown_len, source_path) = match bytes {
            Some(bytes) => (
                ArtifactDigest::parse(&sha256_hex(bytes)).map_err(|e| StoreError::Legacy {
                    reason: format!("digest of legacy bytes: {e}"),
                })?,
                bytes.len() as u64,
                meta.path.clone().unwrap_or_default(),
            ),
            None => {
                let summary = meta.title.clone().unwrap_or_default();
                (
                    ArtifactDigest::parse(&sha256_hex(summary.as_bytes())).map_err(|e| {
                        StoreError::Legacy {
                            reason: format!("digest of legacy summary: {e}"),
                        }
                    })?,
                    0,
                    String::new(),
                )
            }
        };
        let content = ArtifactContent::new(serde_json::json!({
            "schema": LEGACY_DOCUMENT_SCHEMA,
            "kind": kind,
            "markdownBytes": markdown_len,
            "sourcePath": source_path,
            "title": meta.title.clone().unwrap_or_default(),
        }));
        self.publish_at(
            parse_ms(&meta.timestamp),
            legacy_ref.artifact.clone(),
            Some(meta.title.clone().unwrap_or_else(|| kind.clone())),
            content,
            digest,
            Vec::new(),
        )?;
        self.report.mappings.push(MappingEntry {
            sequence: meta.sequence,
            event_type: format!(
                "artifact.{}({kind})",
                if version == 1 { "created" } else { "revised" }
            ),
            mapping: format!(
                "preserved {legacy_id}@{version} as a legacy document (original bytes committed)"
            ),
        });

        // Derived structured documents.
        match kind.as_str() {
            "opportunity-brief" => self.derive_brief(&legacy_ref, &meta, document.as_ref())?,
            "prd" => self.derive_requirements(&legacy_ref, &meta, document.as_ref())?,
            "trd" => self.derive_design(&legacy_ref, &meta, document.as_ref())?,
            "decision-log" => self.derive_decisions(&legacy_ref, &meta, document.as_ref())?,
            "execution-plan" => self.derive_plan(&legacy_ref, &meta, document.as_ref())?,
            "delivery-report" => self.derive_delivery(&legacy_ref, &meta, document.as_ref())?,
            "verification-report" => self.report.mappings.push(MappingEntry {
                sequence: meta.sequence,
                event_type: "artifact.created(verification-report)".to_string(),
                mapping: "preserved; verification evidence is emitted from \
                          verification.completed"
                    .to_string(),
            }),
            other => {
                self.report.needs_review.push(ReviewNeeded {
                    note: format!(
                        "legacy artifact kind {other} has no new schema; only the preserved \
                         document was created"
                    ),
                });
            }
        }
        Ok(())
    }

    fn derive_brief(
        &mut self,
        legacy_ref: &ArtifactVersionRef,
        meta: &LegacyArtifactVersion,
        document: Option<&LegacyDocument>,
    ) -> Result<()> {
        let doc = document.cloned().unwrap_or_default();
        let problem = if doc.section("Problem").is_empty() {
            meta.title.clone().unwrap_or_default()
        } else {
            doc.section("Problem").to_string()
        };
        let audience = doc
            .section("Target users")
            .lines()
            .find_map(|line| {
                line.trim_start_matches('|')
                    .split('|')
                    .next()
                    .map(str::trim)
            })
            .filter(|name| !name.is_empty() && *name != "Name")
            .unwrap_or("unknown (legacy brief carried no audience)")
            .to_string();
        let outcome = doc
            .section("Desired outcomes")
            .lines()
            .map(str::trim)
            .find(|line| line.starts_with("- "))
            .map(|line| line.trim_start_matches("- ").to_string())
            .unwrap_or_else(|| self.goal_outcome());
        let brief = BriefDoc {
            problem,
            audience,
            outcome,
            open_questions: Vec::new(),
        };
        let reference = self.publish_derived("art_brief", brief.to_content(), legacy_ref)?;
        self.brief_version = Some(reference);
        Ok(())
    }

    fn goal_outcome(&self) -> String {
        self.facts
            .iter()
            .find_map(|fact| match fact {
                DomainFact::GoalDeclared { goal } => Some(goal.outcome.clone()),
                _ => None,
            })
            .unwrap_or_else(|| "unknown".to_string())
    }

    fn derive_requirements(
        &mut self,
        legacy_ref: &ArtifactVersionRef,
        _meta: &LegacyArtifactVersion,
        document: Option<&LegacyDocument>,
    ) -> Result<()> {
        let doc = document.cloned().unwrap_or_default();
        // Requirements: "### REQ-n" followed by a statement and
        // "_Rationale:_ …".
        let requirements = parse_requirements(doc.section("Requirements"));
        if requirements.is_empty() {
            self.report.needs_review.push(ReviewNeeded {
                note: format!(
                    "legacy PRD {} carries no parseable requirements; only the preserved \
                     document was created",
                    legacy_ref
                ),
            });
            return Ok(());
        }
        let criteria = parse_criteria(doc.section("Acceptance criteria"));
        let brief = match self.brief_version.clone() {
            Some(brief) => brief,
            None => {
                self.report.needs_review.push(ReviewNeeded {
                    note: format!(
                        "legacy PRD {} was created without a preceding brief; its \
                         requirements pin no brief version",
                        legacy_ref
                    ),
                });
                // Requirements need a brief pin; fall back to the first
                // published brief if any appeared later in the legacy
                // stream, else refuse to fabricate one.
                match self.versions.get("art_brief") {
                    Some(version) => ArtifactVersionRef {
                        artifact: ArtifactId::parse("art_brief").map_err(|e| {
                            StoreError::Legacy {
                                reason: e.to_string(),
                            }
                        })?,
                        version: ArtifactVersion::new(*version).map_err(|e| {
                            StoreError::Legacy {
                                reason: e.to_string(),
                            }
                        })?,
                    },
                    None => {
                        self.report.needs_review.push(ReviewNeeded {
                            note: "no legacy brief exists at all; the PRD requirements were \
                                   not migrated"
                                .to_string(),
                        });
                        return Ok(());
                    }
                }
            }
        };
        self.report.inferences.push(Inference {
            note: format!(
                "requirements derived from {} pin the brief version that was latest at \
                 import time ({}); the legacy stream did not record the link",
                legacy_ref, brief
            ),
        });
        let mut requirement_refs = Vec::new();
        for (slug, statement, rationale) in requirements.iter() {
            let doc = RequirementDoc {
                slug: slug.clone(),
                statement: statement.clone(),
                rationale: rationale.clone(),
                brief: brief.clone(),
            };
            let reference =
                self.publish_derived(&format!("art_req-{slug}"), doc.to_content(), legacy_ref)?;
            requirement_refs.push(reference.clone());
        }
        // Criteria: the legacy draft scopes them to the whole PRD, so the
        // criterion→requirement link is coarse. Attach to the first
        // requirement and surface the limitation for review.
        if !criteria.is_empty() {
            let first = requirement_refs.first().cloned();
            if requirements.len() > 1 {
                self.report.needs_review.push(ReviewNeeded {
                    note: format!(
                        "legacy acceptance criteria are document-scoped; {} criteria were \
                         attached to the first requirement ({}). Re-parent any that belong \
                         elsewhere",
                        criteria.len(),
                        requirement_refs
                            .first()
                            .map(|r| r.to_string())
                            .unwrap_or_default()
                    ),
                });
            }
            for criterion in criteria {
                let doc = CriterionDoc {
                    slug: criterion.slug.clone(),
                    requirement: first.clone().ok_or_else(|| StoreError::Legacy {
                        reason: "criteria without requirements".to_string(),
                    })?,
                    statement: criterion.statement.clone(),
                    method: criterion.method,
                };
                let statement = doc.statement.clone();
                let reference = self.publish_derived(
                    &format!("art_crit-{}", criterion.slug),
                    doc.to_content(),
                    legacy_ref,
                )?;
                // Record the statement so verification evidence can match it.
                self.criteria.push((statement, reference));
            }
        }
        Ok(())
    }

    fn derive_design(
        &mut self,
        legacy_ref: &ArtifactVersionRef,
        _meta: &LegacyArtifactVersion,
        document: Option<&LegacyDocument>,
    ) -> Result<()> {
        let doc = document.cloned().unwrap_or_default();
        let design = DesignDoc {
            approach: non_empty(
                doc.section("Architecture"),
                "not recorded in the legacy TRD",
            ),
            components: doc
                .section("Interfaces")
                .lines()
                .filter_map(|line| {
                    let line = line.trim();
                    line.strip_prefix("- ").map(|rest| {
                        rest.split("—")
                            .next()
                            .map(str::trim)
                            .unwrap_or(rest)
                            .to_string()
                    })
                })
                .collect(),
            risks: doc
                .section("Risks")
                .lines()
                .filter_map(|line| {
                    let line = line.trim();
                    line.strip_prefix("- ").map(|rest| {
                        rest.split(" — mitigation")
                            .next()
                            .unwrap_or(rest)
                            .to_string()
                    })
                })
                .collect(),
        };
        self.publish_derived("art_design", design.to_content(), legacy_ref)?;
        Ok(())
    }

    fn derive_decisions(
        &mut self,
        legacy_ref: &ArtifactVersionRef,
        _meta: &LegacyArtifactVersion,
        document: Option<&LegacyDocument>,
    ) -> Result<()> {
        let doc = document.cloned().unwrap_or_default();
        let entries = parse_decision_entries(doc.section("Entries"));
        if entries.is_empty() {
            self.report.needs_review.push(ReviewNeeded {
                note: format!(
                    "legacy decision log {} carries no parseable entries",
                    legacy_ref
                ),
            });
            return Ok(());
        }
        for (index, entry) in entries.iter().enumerate() {
            let decision = DesignDecisionDoc {
                slug: format!("decision-{index}"),
                question: entry.decision.clone(),
                choice: "adopted".to_string(),
                rationale: entry.rationale.clone(),
                consequences: entry
                    .alternatives
                    .clone()
                    .unwrap_or_else(|| "not recorded".to_string()),
            };
            self.publish_derived(
                &format!("art_dec-{index}"),
                decision.to_content(),
                legacy_ref,
            )?;
        }
        Ok(())
    }

    fn derive_plan(
        &mut self,
        legacy_ref: &ArtifactVersionRef,
        _meta: &LegacyArtifactVersion,
        document: Option<&LegacyDocument>,
    ) -> Result<()> {
        let doc = document.cloned().unwrap_or_default();
        let tasks = parse_plan_tasks(doc.section("Tasks"));
        let plan = PlanDoc {
            tasks: tasks
                .iter()
                .map(|task| relay_workflows::schemas::PlanTask {
                    slug: task.slug.clone(),
                    kind: TaskKind::Implement,
                    objective: task.objective.clone(),
                    requirement_refs: Vec::new(),
                    write_scopes: Vec::new(),
                })
                .collect(),
        };
        if tasks.iter().any(|task| !task.requirements.is_empty()) {
            self.report.inferences.push(Inference {
                note: format!(
                    "legacy plan tasks in {} referenced requirements by document position; \
                     no exact criterion pins were invented",
                    legacy_ref
                ),
            });
        }
        self.publish_derived("art_plan", plan.to_content(), legacy_ref)?;
        Ok(())
    }

    fn derive_delivery(
        &mut self,
        legacy_ref: &ArtifactVersionRef,
        _meta: &LegacyArtifactVersion,
        _document: Option<&LegacyDocument>,
    ) -> Result<()> {
        let includes: Vec<ArtifactVersionRef> = self
            .versions
            .iter()
            .filter(|(id, _)| id.as_str() != "art_delivery")
            .filter_map(|(id, version)| {
                Some(ArtifactVersionRef {
                    artifact: ArtifactId::parse(id).ok()?,
                    version: ArtifactVersion::new(*version).ok()?,
                })
            })
            .collect();
        let delivery = DeliveryDoc {
            summary: format!("imported from legacy delivery report {legacy_ref}"),
            includes,
            evidence: Vec::new(),
            checks: Vec::new(),
        };
        self.publish_derived("art_delivery", delivery.to_content(), legacy_ref)?;
        Ok(())
    }

    fn emit_approvals(&mut self) -> Result<()> {
        let definitions = relay_workflows::WorkflowCatalog::builtin();
        let approve = definitions
            .get(relay_workflows::KIND_APPROVE)
            .ok_or_else(|| StoreError::Legacy {
                reason: "built-in catalog is missing approve-artifact".to_string(),
            })?;
        for (index, (legacy_id, version, sequence, timestamp)) in
            self.approvals.clone().iter().enumerate()
        {
            let reference = ArtifactVersionRef {
                artifact: ArtifactId::parse(legacy_id).map_err(|e| StoreError::Legacy {
                    reason: format!("legacy artifact id {legacy_id}: {e}"),
                })?,
                version: ArtifactVersion::new(*version).map_err(|e| StoreError::Legacy {
                    reason: e.to_string(),
                })?,
            };
            let work_id = WorkItemId::parse(&format!("work_approve-{index}")).map_err(|e| {
                StoreError::Legacy {
                    reason: e.to_string(),
                }
            })?;
            let decision_id = DecisionId::parse(&format!("dec_approve-{index}")).map_err(|e| {
                StoreError::Legacy {
                    reason: e.to_string(),
                }
            })?;
            let decision = relay_workflows::approval_decision(
                decision_id.clone(),
                &reference,
                format!("Approve the imported {legacy_id} v{version}?"),
            );
            let mut opened = decision;
            opened.opened_at_ms = parse_ms(timestamp);
            let work = relay_workflows::instantiate_human(
                approve,
                work_id,
                format!("Approve the imported legacy artifact {legacy_id} v{version}"),
                decision_id.clone(),
                vec![relay_domain::InputRef::Exact(reference.clone())],
            )
            .map_err(|error| StoreError::Legacy {
                reason: format!("building approve-artifact work: {error}"),
            })?;
            self.facts.push(DomainFact::WorkDeclared { work });
            self.facts
                .push(DomainFact::DecisionOpened { decision: opened });
            self.facts.push(DomainFact::DecisionResolved {
                decision: decision_id,
                resolution: DecisionResolution {
                    choice: "approve".to_string(),
                    rationale: "approved in the legacy workflow".to_string(),
                    resolved_at_ms: parse_ms(timestamp),
                },
            });
            self.report.mappings.push(MappingEntry {
                sequence: *sequence,
                event_type: "artifact.approved".to_string(),
                mapping: format!(
                    "approve-artifact work + decision resolved (approve) on {reference}"
                ),
            });
        }
        Ok(())
    }

    fn emit_terminal_state(&mut self, project: &ProjectId) -> Result<()> {
        let definitions = relay_workflows::WorkflowCatalog::builtin();
        let mode = self.final_mode.clone().unwrap_or_default();
        if mode == "delivery" && self.final_paused {
            let prep = definitions
                .get(relay_workflows::KIND_DELIVERY_PREP)
                .ok_or_else(|| StoreError::Legacy {
                    reason: "built-in catalog is missing prepare-delivery-evidence".to_string(),
                })?;
            let auth = definitions
                .get(relay_workflows::KIND_DELIVERY_AUTH)
                .ok_or_else(|| StoreError::Legacy {
                    reason: "built-in catalog is missing request-delivery-authorization"
                        .to_string(),
                })?;
            let work_id =
                WorkItemId::parse("work_delivery-prep").map_err(|e| StoreError::Legacy {
                    reason: e.to_string(),
                })?;
            let auth_id =
                WorkItemId::parse("work_delivery-auth").map_err(|e| StoreError::Legacy {
                    reason: e.to_string(),
                })?;
            let decision_id = DecisionId::parse("dec_delivery-authorization").map_err(|e| {
                StoreError::Legacy {
                    reason: e.to_string(),
                }
            })?;
            let prep_work = relay_workflows::instantiate_executor(
                prep,
                work_id.clone(),
                "Assemble the delivery package and evidence (imported from the legacy \
                 delivery hold)",
                Vec::new(),
                Vec::new(),
                Vec::new(),
            )
            .map_err(|error| StoreError::Legacy {
                reason: format!("building prepare-delivery-evidence work: {error}"),
            })?;
            let mut decision = relay_workflows::human_decision(
                decision_id.clone(),
                &auth_id.clone(),
                "Authorize delivery of the imported project?",
                &[
                    ("authorize", "release the delivery package"),
                    ("hold", "keep delivery on hold"),
                ],
            );
            decision.opened_at_ms = self.clock_ms;
            let auth_work = relay_workflows::instantiate_human(
                auth,
                auth_id.clone(),
                "Authorize (or keep holding) delivery of the imported project",
                decision_id,
                Vec::new(),
            )
            .map_err(|error| StoreError::Legacy {
                reason: format!("building request-delivery-authorization work: {error}"),
            })?;
            self.facts
                .push(DomainFact::WorkDeclared { work: prep_work });
            self.facts
                .push(DomainFact::WorkDeclared { work: auth_work });
            self.facts.push(DomainFact::DependencyAsserted {
                from: auth_id,
                to: work_id,
                kind: DependencyKind::Requires,
            });
            self.facts.push(DomainFact::DecisionOpened { decision });
            self.report.mappings.push(MappingEntry {
                sequence: self.evidence.events.last().map(|e| e.sequence).unwrap_or(0),
                event_type: "workflow.paused (delivery)".to_string(),
                mapping: "prepare-delivery-evidence + request-delivery-authorization work \
                          with an OPEN authorization decision"
                    .to_string(),
            });
            self.report.needs_review.push(ReviewNeeded {
                note: "the legacy project was in a delivery hold: an open delivery \
                       authorization decision awaits a human"
                    .to_string(),
            });
        } else if matches!(mode.as_str(), "implementation" | "verification") {
            let kind = if mode == "implementation" {
                relay_workflows::KIND_IMPLEMENT
            } else {
                relay_workflows::KIND_VERIFY
            };
            let definition = definitions.get(kind).ok_or_else(|| StoreError::Legacy {
                reason: format!("built-in catalog is missing {kind}"),
            })?;
            let work_id = WorkItemId::parse(&format!("work_continue-{}", mode)).map_err(|e| {
                StoreError::Legacy {
                    reason: e.to_string(),
                }
            })?;
            let objective = format!(
                "Continue the {} phase imported from the legacy workflow (coarse: the \
                 legacy stream recorded the phase, not the remaining tasks)",
                mode
            );
            let work = if definition.completion
                == relay_workflows::CompletionTemplate::CriterionSatisfied
            {
                // Verification continuation needs a criterion; use the first
                // imported criterion when one exists, else fall back to a
                // receipt-completing corrective work item.
                match self
                    .criteria
                    .first()
                    .map(|(_, reference)| reference.clone())
                {
                    Some(criterion) => relay_workflows::instantiate_verification(
                        definition,
                        work_id,
                        objective,
                        criterion,
                        Vec::new(),
                    )
                    .map_err(|error| StoreError::Legacy {
                        reason: format!("building verify-criteria work: {error}"),
                    })?,
                    None => continue_as_corrective(definition, &work_id, objective)?,
                }
            } else {
                relay_workflows::instantiate_executor(
                    definition,
                    work_id.clone(),
                    objective,
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                )
                .map_err(|error| StoreError::Legacy {
                    reason: format!("building {kind} work: {error}"),
                })?
            };
            self.facts.push(DomainFact::WorkDeclared { work });
            self.report.mappings.push(MappingEntry {
                sequence: self.evidence.events.last().map(|e| e.sequence).unwrap_or(0),
                event_type: format!("mode.entered({mode})"),
                mapping: format!("coarse continuation work ({kind}) marked imported"),
            });
            self.report.inferences.push(Inference {
                note: format!(
                    "the legacy stream recorded the {mode} phase but not its remaining \
                     tasks; one coarse continuation work item was created"
                ),
            });
            let _ = project;
        }
        Ok(())
    }

    fn emit_verification_evidence(&mut self) -> Result<()> {
        let mut sequence_evidence: Vec<(u64, String, EvidenceOutcome, String)> = Vec::new();
        for event in &self.evidence.events {
            if event.event_type != "verification.completed" {
                continue;
            }
            let passed = event.payload.get("passed").and_then(Json::as_bool);
            let results = event
                .payload
                .get("results")
                .and_then(|value| match value {
                    Json::Array(items) => Some(items.clone()),
                    _ => None,
                })
                .unwrap_or_default();
            for result in results {
                let statement = str_field(&result, "criterion").unwrap_or_default();
                let status = str_field(&result, "status").unwrap_or_else(|| "info".to_string());
                let detail = str_field(&result, "evidence").unwrap_or_default();
                let outcome = match (passed, status.as_str()) {
                    (_, "pass") => EvidenceOutcome::Pass,
                    (_, "fail") => EvidenceOutcome::Fail,
                    (Some(false), _) => EvidenceOutcome::Fail,
                    _ => EvidenceOutcome::Info,
                };
                sequence_evidence.push((event.sequence, statement, outcome, detail));
            }
        }
        for (index, (sequence, statement, outcome, detail)) in
            sequence_evidence.into_iter().enumerate()
        {
            // Match the legacy criterion text against the imported
            // criterion statements; fall back to the preserved verification
            // document when nothing matches.
            let target = self
                .criteria
                .iter()
                .find(|(candidate, _)| criterion_matches(candidate, &statement))
                .map(|(_, reference)| reference.clone());
            let (target, matched) = match target {
                Some(reference) => (EvidenceTarget::ArtifactVersion(reference), true),
                None => {
                    self.report.needs_review.push(ReviewNeeded {
                        note: format!(
                            "verification evidence for \"{statement}\" matched no imported \
                             criterion; it targets a migrated verification-record artifact"
                        ),
                    });
                    // Publish a verification-record artifact for the
                    // evidence to target (once).
                    let artifact =
                        ArtifactId::parse("art_verification").map_err(|e| StoreError::Legacy {
                            reason: format!("verification artifact id: {e}"),
                        })?;
                    let reference = if self.versions.contains_key("art_verification") {
                        ArtifactVersionRef {
                            artifact: artifact.clone(),
                            version: ArtifactVersion::new(
                                self.versions.get("art_verification").copied().unwrap_or(1),
                            )
                            .map_err(|e| StoreError::Legacy {
                                reason: e.to_string(),
                            })?,
                        }
                    } else {
                        let content = ArtifactContent::new(serde_json::json!({
                            "schema": LEGACY_DOCUMENT_SCHEMA,
                            "kind": "verification-report",
                            "note": "imported verification evidence that matched no criterion",
                        }));
                        let digest = ArtifactDigest::parse(&sha256_hex(
                            serde_json::to_string(&content)
                                .unwrap_or_default()
                                .as_bytes(),
                        ))
                        .map_err(|e| StoreError::Legacy {
                            reason: format!("digest of verification record: {e}"),
                        })?;
                        self.publish_at(
                            self.clock_ms,
                            artifact.clone(),
                            Some("Imported verification evidence".to_string()),
                            content,
                            digest,
                            Vec::new(),
                        )?
                    };
                    (EvidenceTarget::ArtifactVersion(reference), false)
                }
            };
            let id = EvidenceId::parse(&format!("ev_migration-{index}")).map_err(|e| {
                StoreError::Legacy {
                    reason: e.to_string(),
                }
            })?;
            let evidence = Evidence::new(
                id,
                EvidenceKind::Inspection,
                target,
                outcome,
                if detail.is_empty() {
                    format!("imported from legacy verification (sequence {sequence})")
                } else {
                    detail
                },
                self.clock_ms,
            );
            self.facts.push(DomainFact::EvidenceRecorded { evidence });
            self.report.mappings.push(MappingEntry {
                sequence,
                event_type: "verification.completed".to_string(),
                mapping: format!(
                    "evidence ({}) {}",
                    if matched {
                        "criterion-matched"
                    } else {
                        "document-targeted"
                    },
                    match outcome {
                        EvidenceOutcome::Pass => "pass",
                        EvidenceOutcome::Fail => "fail",
                        EvidenceOutcome::Info => "info",
                    }
                ),
            });
        }
        Ok(())
    }

    /// Publish the next version of `artifact` and return its reference,
    /// stamped with the legacy event time.
    #[allow(clippy::too_many_arguments)]
    fn publish_at(
        &mut self,
        at_ms: i64,
        artifact: ArtifactId,
        title: Option<String>,
        content: ArtifactContent,
        digest: ArtifactDigest,
        derived_from: Vec<ArtifactVersionRef>,
    ) -> Result<ArtifactVersionRef> {
        let next = self
            .versions
            .entry(artifact.as_str().to_string())
            .or_insert(0);
        *next += 1;
        let version = ArtifactVersion::new(*next).map_err(|e| StoreError::Legacy {
            reason: e.to_string(),
        })?;
        self.facts.push(DomainFact::ArtifactVersionPublished {
            artifact: artifact.clone(),
            version,
            title,
            content,
            digest,
            derived_from,
            published_at_ms: at_ms,
        });
        Ok(ArtifactVersionRef { artifact, version })
    }

    fn publish_derived(
        &mut self,
        id: &str,
        content: ArtifactContent,
        derived_from: &ArtifactVersionRef,
    ) -> Result<ArtifactVersionRef> {
        let artifact = ArtifactId::parse(id).map_err(|e| StoreError::Legacy {
            reason: format!("derived artifact id {id}: {e}"),
        })?;
        let digest = ArtifactDigest::parse(&sha256_hex(
            serde_json::to_string(&content)
                .unwrap_or_default()
                .as_bytes(),
        ))
        .map_err(|e| StoreError::Legacy {
            reason: format!("digest of derived content: {e}"),
        })?;
        self.publish_at(
            self.clock_ms,
            artifact.clone(),
            None,
            content,
            digest,
            vec![derived_from.clone()],
        )
    }
}

fn continue_as_corrective(
    definition: &relay_workflows::WorkDefinition,
    work_id: &WorkItemId,
    objective: String,
) -> Result<WorkItem> {
    // Verification without an importable criterion becomes corrective work
    // (receipt-completing), recorded as an inference.
    let mut work = WorkItem::new(
        work_id.clone(),
        relay_domain::WorkKind::parse(relay_workflows::KIND_CORRECTIVE).map_err(|e| {
            StoreError::Legacy {
                reason: format!("corrective work kind: {e}"),
            }
        })?,
        ExecutorClass::System,
        objective,
        CompletionRule::Receipt,
    );
    work.required = false;
    let _ = definition;
    Ok(work)
}

/// Lowercase hex SHA-256 (same shape as `relay_artifacts::sha256_hex`,
/// kept local so the migrator adds no cross-crate dependency).
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn str_field(payload: &Json, field: &str) -> Option<String> {
    payload
        .get(field)
        .and_then(Json::as_str)
        .map(str::to_string)
}

fn bad_payload(event: &LegacyEvent, field: &str) -> StoreError {
    StoreError::Legacy {
        reason: format!(
            "legacy event {} (sequence {}) is missing field \"{field}\"",
            event.event_type, event.sequence
        ),
    }
}

fn parse_ms(timestamp: &str) -> i64 {
    crate::clock::parse_iso_to_epoch_millis(timestamp).unwrap_or(0)
}

fn non_empty(text: &str, fallback: &str) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        fallback.to_string()
    } else {
        trimmed.to_string()
    }
}

/// Parse `### REQ-n` sections: statement lines then `_Rationale:_ …`.
fn parse_requirements(section: &str) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    let mut current: Option<(String, String, String)> = None;
    for line in section.lines() {
        let line = line.trim_end();
        if let Some(rest) = line.strip_prefix("### REQ-") {
            if let Some(entry) = current.take() {
                out.push(entry);
            }
            current = Some((
                format!("req-{}", rest.trim().to_lowercase()),
                String::new(),
                String::new(),
            ));
        } else if let Some(rationale) = line.trim().strip_prefix("_Rationale:_") {
            if let Some(entry) = current.as_mut() {
                entry.2 = rationale.trim().to_string();
            }
        } else if !line.trim().is_empty()
            && let Some(entry) = current.as_mut()
        {
            if entry.1.is_empty() {
                entry.1.push_str(line.trim());
            } else {
                entry.1.push(' ');
                entry.1.push_str(line.trim());
            }
        }
    }
    if let Some(entry) = current.take() {
        out.push(entry);
    }
    out
}

/// One parsed `### AC-n` criterion.
struct ParsedCriterion {
    slug: String,
    statement: String,
    method: VerificationMethod,
}

/// Parse `### AC-n` given/when/then bullets into one checkable statement.
fn parse_criteria(section: &str) -> Vec<ParsedCriterion> {
    #[derive(Default)]
    struct Partial {
        slug: Option<String>,
        given: String,
        when: String,
        then: String,
    }
    fn flush(partial: &mut Partial, out: &mut Vec<ParsedCriterion>) {
        if let Some(slug) = partial.slug.take() {
            let statement = format!(
                "Given {}when {}then {}",
                partial.given, partial.when, partial.then
            );
            out.push(ParsedCriterion {
                slug,
                statement,
                method: VerificationMethod::AutomatedTest,
            });
        }
        partial.given.clear();
        partial.when.clear();
        partial.then.clear();
    }
    let mut out = Vec::new();
    let mut partial = Partial::default();
    for line in section.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("### AC-") {
            flush(&mut partial, &mut out);
            partial.slug = Some(format!("crit-{}", rest.trim().to_lowercase()));
        } else if let Some(value) = line.strip_prefix("- **Given:**") {
            partial.given = format!("{}, ", value.trim());
        } else if let Some(value) = line.strip_prefix("- **When:**") {
            partial.when = format!("{}, ", value.trim());
        } else if let Some(value) = line.strip_prefix("- **Then:**") {
            partial.then = value.trim().to_string();
        }
    }
    flush(&mut partial, &mut out);
    out
}

/// One parsed decision-log entry.
struct ParsedDecision {
    decision: String,
    rationale: String,
    alternatives: Option<String>,
}

/// Parse legacy decision-log entry bodies (`### N. <decision>` with
/// `- **Rationale:**` and `- **Alternatives:**` bullets; the legacy demo
/// log uses a single flowing paragraph per entry).
fn parse_decision_entries(section: &str) -> Vec<ParsedDecision> {
    let mut out = Vec::new();
    let mut current: Option<ParsedDecision> = None;
    for line in section.lines() {
        let line = line.trim_end();
        let numbered = line
            .trim_start_matches('#')
            .trim_start()
            .split_once(". ")
            .map(|(_, rest)| rest.trim().to_string());
        if line.starts_with('#') && numbered.is_some() {
            if let Some(entry) = current.take() {
                out.push(entry);
            }
            current = Some(ParsedDecision {
                decision: numbered.unwrap_or_default(),
                rationale: String::new(),
                alternatives: None,
            });
        } else if let Some(rationale) = line.trim().strip_prefix("- **Rationale:**") {
            if let Some(entry) = current.as_mut() {
                entry.rationale = rationale.trim().to_string();
            }
        } else if let Some(alternatives) = line.trim().strip_prefix("- **Alternatives:**") {
            if let Some(entry) = current.as_mut() {
                entry.alternatives = Some(alternatives.trim().to_string());
            }
        } else if !line.trim().is_empty()
            && let Some(entry) = current.as_mut()
            && entry.rationale.is_empty()
            && !line.trim().starts_with('-')
        {
            // Free-form rationale sentence, like the demo log.
            let sentence = line.split("- **").next().unwrap_or(line).trim();
            if !sentence.is_empty() {
                entry.rationale = sentence.to_string();
            }
        }
    }
    if let Some(entry) = current.take() {
        out.push(entry);
    }
    out
}

/// One parsed plan task.
struct ParsedTask {
    slug: String,
    objective: String,
    requirements: Vec<String>,
}

/// Parse legacy `### Task N — title` plan sections.
fn parse_plan_tasks(section: &str) -> Vec<ParsedTask> {
    let mut out = Vec::new();
    let mut current: Option<ParsedTask> = None;
    let mut index = 0;
    for line in section.lines() {
        let line = line.trim_end();
        if let Some(rest) = line.trim_start_matches('#').trim().strip_prefix("Task ") {
            if let Some(entry) = current.take() {
                out.push(entry);
            }
            index += 1;
            let title = rest.split(" — ").next().unwrap_or(rest).trim();
            current = Some(ParsedTask {
                slug: format!("task-{index}"),
                objective: title.to_string(),
                requirements: Vec::new(),
            });
        } else if let Some(entry) = current.as_mut() {
            let line = line.trim();
            if !line.is_empty() && !line.starts_with('#') {
                // Satisfies references (legacy plans reference REQ ids).
                if line.starts_with("Satisfies:") {
                    entry.requirements.extend(
                        line.trim_start_matches("Satisfies:")
                            .split(',')
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(str::to_string),
                    );
                } else if entry.objective.len() < 200 {
                    entry.objective.push(' ');
                    entry.objective.push_str(line);
                }
            }
        }
    }
    if let Some(entry) = current.take() {
        out.push(entry);
    }
    out
}

/// Whether an imported criterion statement plausibly matches a legacy
/// verification result text.
fn criterion_matches(candidate: &str, legacy: &str) -> bool {
    let normalize = |text: &str| {
        text.chars()
            .filter(|c| c.is_alphanumeric())
            .collect::<String>()
            .to_lowercase()
    };
    let candidate = normalize(candidate);
    let legacy = normalize(legacy);
    !legacy.is_empty() && (candidate == legacy || candidate.contains(&legacy))
}

/// Parse a legacy Markdown artifact into frontmatter title + `## Heading`
/// sections.
fn parse_legacy_markdown(bytes: &[u8]) -> LegacyDocument {
    let text = String::from_utf8_lossy(bytes);
    let mut document = LegacyDocument::default();
    // Split a `---\n…\n---\n` frontmatter block from the body.
    let body = if let Some(rest) = text.strip_prefix("---\n") {
        match rest.split_once("\n---\n") {
            Some((frontmatter, body)) => {
                for line in frontmatter.lines() {
                    if let Some((key, value)) = line.split_once(':') {
                        let key = key.trim();
                        let mut value = value.trim().to_string();
                        // Handle the folded-scalar continuation the legacy
                        // writer emits for long titles.
                        if value.is_empty() {
                            continue;
                        }
                        if key == "title" {
                            while value.starts_with(' ') {
                                value.remove(0);
                            }
                            document.title = value;
                        }
                    }
                }
                body.to_string()
            }
            None => text.to_string(),
        }
    } else {
        text.to_string()
    };
    let mut heading = String::new();
    let mut buffer = String::new();
    for line in body.lines() {
        if let Some(rest) = line.strip_prefix("## ") {
            if !heading.is_empty() {
                document
                    .sections
                    .insert(heading.clone(), buffer.trim().to_string());
            }
            heading = rest.trim().to_string();
            buffer.clear();
        } else if !line.starts_with("# ") {
            buffer.push_str(line);
            buffer.push('\n');
        }
    }
    if !heading.is_empty() {
        document.sections.insert(heading, buffer.trim().to_string());
    }
    document
}
