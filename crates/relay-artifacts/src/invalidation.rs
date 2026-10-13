//! Deterministic downstream artifact invalidation.
//!
//! Port of `src/artifacts/invalidation.ts`. When an approved artifact is
//! revised, every existing artifact that is transitively downstream of it in
//! the core dependency graph is stale until it is regenerated and
//! re-approved. The plan computed here is purely a function of the core graph
//! and the kinds that currently exist, so the reducer and policies replay it
//! identically. The dependency graph itself lives in `relay-core` and is
//! reused, not duplicated.

pub use relay_core::kind_to_mode;

use relay_core::{ARTIFACT_KINDS, ArtifactKind, downstream_artifact_kinds};

use crate::error::UnknownArtifactKindError;

/// Input to [`plan_invalidation_from_names`]: the revised kind plus the kinds
/// that currently exist in project state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanInvalidationInput {
    /// The kind whose artifact was just revised.
    pub revised_kind: String,
    /// Kinds that currently exist in project state (unknown values are
    /// ignored, matching the TypeScript behavior).
    pub existing_kinds: Vec<String>,
}

/// Existing kinds that are transitively downstream of `revised_kind`,
/// returned in canonical `ARTIFACT_KINDS` order so invalidation application
/// is stable. Kinds with no transitive dependency on the revised kind (and
/// the revised kind itself) are never included.
pub fn plan_invalidation(
    revised_kind: ArtifactKind,
    existing_kinds: &[ArtifactKind],
) -> Vec<ArtifactKind> {
    let downstream = downstream_artifact_kinds(revised_kind);
    ARTIFACT_KINDS
        .into_iter()
        .filter(|kind| existing_kinds.contains(kind) && downstream.contains(kind))
        .collect()
}

/// String-based variant mirroring the TypeScript entry point: it throws
/// (fails) for an unknown revised kind and ignores unknown entries in
/// `existing_kinds`.
pub fn plan_invalidation_from_names(
    input: &PlanInvalidationInput,
) -> Result<Vec<ArtifactKind>, UnknownArtifactKindError> {
    let revised = relay_core::ArtifactKind::parse(&input.revised_kind).ok_or_else(|| {
        UnknownArtifactKindError {
            kind: input.revised_kind.clone(),
            detail: format!(
                "expected one of: {}",
                ARTIFACT_KINDS
                    .iter()
                    .map(|kind| kind.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    })?;
    let existing: Vec<ArtifactKind> = input
        .existing_kinds
        .iter()
        .filter_map(|name| ArtifactKind::parse(name))
        .collect();
    Ok(plan_invalidation(revised, &existing))
}
