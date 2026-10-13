//! Controlled import: turn an edited Markdown view back into validated
//! structured content and an explicit graph-change proposal.
//!
//! An edit never silently mutates graph meaning. Import parses the view,
//! validates that every referenced artifact version exists with the right
//! schema, builds the new structured document versions, and hands back the
//! `revise-artifact` work item whose accepted receipt publishes them — the
//! engine then invalidates exactly the dependents. The parsed format
//! mirrors [`crate::render::render_prd`] output exactly.

use relay_domain::{
    ArtifactId, ArtifactVersion, ArtifactVersionRef, GraphChangeProposal, InputRef, ProjectGraph,
    WorkItem, WorkItemId,
};

use crate::catalog;
use crate::schemas::{CriterionDoc, RequirementDoc, StructuredDoc, VerificationMethod};

/// Error raised when an edited view cannot be imported.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ImportError {
    /// The markdown structure does not match the view format.
    #[error("cannot parse view: {detail}")]
    Malformed {
        /// What was wrong.
        detail: String,
    },
    /// The view references an artifact version that does not exist.
    #[error("view references unknown artifact version {reference}")]
    UnknownReference {
        /// The missing version.
        reference: String,
    },
    /// A referenced version does not carry the expected schema.
    #[error("artifact {reference} carries schema {found:?}, expected {expected:?}")]
    WrongSchema {
        /// The referenced version.
        reference: String,
        /// What it carries.
        found: String,
        /// What the view needs.
        expected: &'static str,
    },
}

/// One parsed criterion from an edited PRD.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportedCriterion {
    /// The criterion slug.
    pub slug: String,
    /// The exact version this edit revises.
    pub reference: ArtifactVersionRef,
    /// The edited statement (empty means: unchanged).
    pub statement: String,
    /// The edited verification method.
    pub method: VerificationMethod,
}

/// One parsed requirement from an edited PRD.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportedRequirement {
    /// The requirement slug.
    pub slug: String,
    /// The exact version this edit revises.
    pub reference: ArtifactVersionRef,
    /// The edited statement (empty means: unchanged).
    pub statement: String,
    /// The edited rationale (empty means: unchanged).
    pub rationale: String,
    /// The criteria under this requirement.
    pub criteria: Vec<ImportedCriterion>,
}

/// A parsed PRD edit.
#[derive(Debug, Clone, PartialEq)]
pub struct PrdImport {
    /// The parsed requirements (with their criteria).
    pub requirements: Vec<ImportedRequirement>,
}

fn parse_reference(text: &str) -> Result<ArtifactVersionRef, ImportError> {
    let malformed = |note: String| ImportError::Malformed {
        detail: format!("bad artifact reference {text:?}: {note}"),
    };
    let (artifact, version) = text
        .split_once('@')
        .ok_or_else(|| malformed("expected art@N".to_string()))?;
    let artifact =
        ArtifactId::parse(artifact).map_err(|_| malformed("bad artifact id".to_string()))?;
    let version: u32 = version
        .parse()
        .map_err(|_| malformed("version must be a number".to_string()))?;
    let version =
        ArtifactVersion::new(version).map_err(|_| malformed("version must be >= 1".to_string()))?;
    Ok(ArtifactVersionRef::new(artifact, version))
}

#[derive(Default)]
struct PrdParser {
    requirements: Vec<ImportedRequirement>,
    requirement: Option<ImportedRequirement>,
    criterion: Option<ImportedCriterion>,
    body: Vec<String>,
}

impl PrdParser {
    /// Fold pending body lines into the innermost open section's
    /// statement.
    fn seal_body(&mut self) {
        if self.body.is_empty() {
            return;
        }
        let text = self.body.join("\n");
        self.body.clear();
        if let Some(criterion) = self.criterion.as_mut() {
            criterion.statement = text;
        } else if let Some(requirement) = self.requirement.as_mut() {
            requirement.statement = text;
        }
    }

    fn close_criterion(&mut self) {
        self.seal_body();
        if let (Some(criterion), Some(requirement)) =
            (self.criterion.take(), self.requirement.as_mut())
        {
            requirement.criteria.push(criterion);
        }
    }

    fn close_requirement(&mut self) {
        self.close_criterion();
        if let Some(requirement) = self.requirement.take() {
            self.requirements.push(requirement);
        }
    }

    fn finish(mut self) -> Result<PrdImport, ImportError> {
        self.close_requirement();
        if self.requirements.is_empty() {
            return Err(ImportError::Malformed {
                detail: "no requirement sections found".to_string(),
            });
        }
        Ok(PrdImport {
            requirements: self.requirements,
        })
    }
}

/// Parse an edited PRD view (the format [`crate::render::render_prd`]
/// emits) into revised structured documents. Empty statement/rationale
/// fields mean "keep the current content"; references must name the
/// versions being revised.
pub fn import_prd(markdown: &str) -> Result<PrdImport, ImportError> {
    let mut parser = PrdParser::default();
    for line in markdown.lines() {
        if let Some(rest) = line.strip_prefix("## Requirement: ") {
            parser.close_requirement();
            let (slug, reference) =
                rest.split_once(" — ")
                    .ok_or_else(|| ImportError::Malformed {
                        detail: format!("requirement heading needs \"slug — ref\", got {rest:?}"),
                    })?;
            parser.requirement = Some(ImportedRequirement {
                slug: slug.trim().to_string(),
                reference: parse_reference(reference.trim())?,
                statement: String::new(),
                rationale: String::new(),
                criteria: Vec::new(),
            });
        } else if let Some(rest) = line.strip_prefix("### Criterion: ") {
            parser.close_criterion();
            let (slug, tail) = rest
                .split_once(" — ")
                .ok_or_else(|| ImportError::Malformed {
                    detail: format!(
                        "criterion heading needs \"slug — ref [status]\", got {rest:?}"
                    ),
                })?;
            let (reference, _status) =
                tail.split_once(" [")
                    .ok_or_else(|| ImportError::Malformed {
                        detail: format!("criterion heading needs a [status] tag, got {tail:?}"),
                    })?;
            parser.criterion = Some(ImportedCriterion {
                slug: slug.trim().to_string(),
                reference: parse_reference(reference.trim())?,
                statement: String::new(),
                method: VerificationMethod::AutomatedTest,
            });
        } else if let Some(rationale) = line.strip_prefix("Rationale: ") {
            parser.seal_body();
            if let Some(requirement) = parser.requirement.as_mut() {
                requirement.rationale = rationale.trim().to_string();
            }
        } else if let Some(method) = line.strip_prefix("Method: ") {
            parser.seal_body();
            let method = match method.trim() {
                "automated-test" => VerificationMethod::AutomatedTest,
                "manual-check" => VerificationMethod::ManualCheck,
                "inspection" => VerificationMethod::Inspection,
                other => {
                    return Err(ImportError::Malformed {
                        detail: format!("unknown verification method {other:?}"),
                    });
                }
            };
            if let Some(criterion) = parser.criterion.as_mut() {
                criterion.method = method;
            }
        } else if !line.trim().is_empty()
            && !line.starts_with('#')
            && !line.starts_with("Goal:")
            && !line.starts_with("Brief:")
        {
            parser.body.push(line.trim().to_string());
        } else {
            parser.seal_body();
        }
    }
    parser.finish()
}

/// One revised document: requirement or criterion content.
#[derive(Debug, Clone, PartialEq)]
pub enum PrdDocument {
    /// A revised requirement.
    Requirement(RequirementDoc),
    /// A revised criterion.
    Criterion(CriterionDoc),
}

/// The output of a validated PRD import: the new document versions to
/// publish and the work item that will publish them.
pub struct PrdRevision {
    /// One entry per artifact being revised: its id and the next
    /// structured content.
    pub documents: Vec<(ArtifactId, PrdDocument)>,
    /// The `revise-artifact` work item pinned to the versions being
    /// revised; its accepted receipt publishes the new versions.
    pub work: WorkItem,
}

fn wrong_schema(
    reference: &ArtifactVersionRef,
    content: &relay_domain::ArtifactContent,
    expected: &'static str,
) -> ImportError {
    ImportError::WrongSchema {
        reference: reference.to_string(),
        found: crate::schemas::content_schema(content)
            .unwrap_or("<none>")
            .to_string(),
        expected,
    }
}

/// Validate an import against the graph and build the controlled revision.
pub fn prd_revision(
    graph: &ProjectGraph,
    import: &PrdImport,
    work_id: WorkItemId,
) -> Result<PrdRevision, ImportError> {
    let mut documents: Vec<(ArtifactId, PrdDocument)> = Vec::new();
    let mut inputs: Vec<InputRef> = Vec::new();
    for requirement in &import.requirements {
        let Some(record) = graph.version_record(&requirement.reference) else {
            return Err(ImportError::UnknownReference {
                reference: requirement.reference.to_string(),
            });
        };
        let existing = RequirementDoc::from_content(&record.content).map_err(|_| {
            wrong_schema(
                &requirement.reference,
                &record.content,
                crate::schemas::REQUIREMENT_SCHEMA,
            )
        })?;
        documents.push((
            requirement.reference.artifact.clone(),
            PrdDocument::Requirement(RequirementDoc {
                slug: requirement.slug.clone(),
                statement: if requirement.statement.is_empty() {
                    existing.statement.clone()
                } else {
                    requirement.statement.clone()
                },
                rationale: if requirement.rationale.is_empty() {
                    existing.rationale.clone()
                } else {
                    requirement.rationale.clone()
                },
                brief: existing.brief.clone(),
            }),
        ));
        inputs.push(InputRef::Exact(requirement.reference.clone()));
        for criterion in &requirement.criteria {
            let Some(record) = graph.version_record(&criterion.reference) else {
                return Err(ImportError::UnknownReference {
                    reference: criterion.reference.to_string(),
                });
            };
            let existing = CriterionDoc::from_content(&record.content).map_err(|_| {
                wrong_schema(
                    &criterion.reference,
                    &record.content,
                    crate::schemas::CRITERION_SCHEMA,
                )
            })?;
            documents.push((
                criterion.reference.artifact.clone(),
                PrdDocument::Criterion(CriterionDoc {
                    slug: criterion.slug.clone(),
                    statement: if criterion.statement.is_empty() {
                        existing.statement.clone()
                    } else {
                        criterion.statement.clone()
                    },
                    requirement: existing.requirement.clone(),
                    method: criterion.method,
                }),
            ));
            inputs.push(InputRef::Exact(criterion.reference.clone()));
        }
    }
    let definitions = catalog::WorkflowCatalog::builtin();
    let definition = definitions
        .get(crate::KIND_REVISE)
        .ok_or_else(|| ImportError::Malformed {
            detail: "built-in catalog is missing revise-artifact".to_string(),
        })?;
    let work = catalog::instantiate_executor(
        definition,
        work_id,
        "Apply the edited product requirements view as new artifact versions",
        inputs,
        Vec::new(),
        Vec::new(),
    )
    .map_err(|error| ImportError::Malformed {
        detail: error.to_string(),
    })?;
    Ok(PrdRevision { documents, work })
}

/// Convenience: the declaration proposal for a revision.
pub fn revision_proposal(revision: &PrdRevision) -> GraphChangeProposal {
    GraphChangeProposal::DeclareWork {
        work: revision.work.clone(),
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

    #[test]
    fn prd_format_round_trips_through_import() {
        let markdown = "\
# Product Requirements — ship CSV export

Goal: ship CSV export

Brief: art_brief@1

## Requirement: csv-export — art_req-csv@1

Every report exports as valid CSV.

Rationale: users live in spreadsheets.

### Criterion: valid-rows — art_cri-valid@1 [pass]

No malformed rows in any export.

Method: automated-test
";
        let import = import_prd(markdown).ok().expect("parses");
        assert_eq!(import.requirements.len(), 1);
        let requirement = &import.requirements[0];
        assert_eq!(requirement.slug, "csv-export");
        assert_eq!(requirement.reference.to_string(), "art_req-csv@1");
        assert_eq!(requirement.statement, "Every report exports as valid CSV.");
        assert_eq!(requirement.rationale, "users live in spreadsheets.");
        assert_eq!(requirement.criteria.len(), 1);
        let criterion = &requirement.criteria[0];
        assert_eq!(criterion.slug, "valid-rows");
        assert_eq!(criterion.statement, "No malformed rows in any export.");
        assert_eq!(criterion.method, VerificationMethod::AutomatedTest);
    }

    #[test]
    fn malformed_views_fail_closed() {
        assert!(import_prd("# nothing here\n").is_err());
        assert!(import_prd("## Requirement: x — not-a-ref\n").is_err());
        assert!(import_prd("## Requirement: x\n").is_err());
        assert!(import_prd("## Requirement: x — art_a@1\n\nbody\n\nMethod: nonsense\n").is_err());
    }
}
