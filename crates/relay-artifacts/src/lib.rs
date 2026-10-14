//! Artifact drafts, deterministic Markdown rendering, versioned immutable
//! artifact storage, and invalidation planning for Relay.
//!
//! This crate is the Rust port of the retired TypeScript artifact layer
//! (`src/artifacts/*`, removed from the tree 2026-10-14): draft
//! schemas and their Zod-v4 issue text, rendered Markdown bytes, YAML
//! frontmatter bytes, filename slugs, versioned no-overwrite storage
//! semantics, and the downstream invalidation plan all mirror that retired
//! implementation exactly. The compatibility oracle is the frozen golden
//! fixtures under `tests/fixtures/cases/e2e-demo-run/artifacts`, which pin
//! byte-level parity.
//!
//! Module map:
//!
//! - [`drafts`]: typed draft schemas for all seven artifact kinds plus the
//!   validation gate (`parse_artifact_draft`) with Zod-compatible issues.
//! - [`render`]: `render_artifact_markdown` — deterministic Markdown bodies.
//! - [`frontmatter`]: YAML frontmatter writer (byte-exact port of the `yaml`
//!   package's scalar styles and flow folding) and reader.
//! - [`store`]: `ArtifactMarkdownStore` — versioned immutable files with
//!   prepare-and-publish atomicity, digests, and orphan inspection.
//! - [`slug`]: kebab-case filename slugs (NFKD-aware, byte-compatible).
//! - [`invalidation`]: downstream invalidation planning over the core graph.

pub mod canonical;
pub mod drafts;
pub mod error;
pub mod frontmatter;
pub mod invalidation;
pub mod render;
pub mod slug;
pub mod store;

pub use canonical::{CanonicalError, canonical_json, canonical_json_bytes, content_digest};
pub use drafts::{
    AcceptanceCriterion, ArtifactDraft, DecisionEntry, DecisionLogDraft, DeliveryReportDraft,
    Evidence, ExecutionPlanDraft, Interface, Migration, OpportunityBriefDraft,
    ParseArtifactDraftResult, PlanTask, PrdDraft, RepositoryFinding, Requirement, Risk, TargetUser,
    TraceEntry, TrdDraft, VerificationReportDraft, VerificationResult, VerificationStatus,
    format_zod_issues, parse_artifact_draft, require_artifact_draft,
};
pub use error::{
    ArtifactPathOutsideStoreError, ArtifactVersionExistsError, ArtifactsError,
    InvalidArtifactDraftError, InvalidArtifactFileError, InvalidArtifactOptionsError,
    InvalidArtifactsDirError, UnknownArtifactKindError,
};
pub use frontmatter::{Frontmatter, ParsedArtifactFile, render_artifact_file, split_frontmatter};
pub use invalidation::{PlanInvalidationInput, plan_invalidation, plan_invalidation_from_names};
pub use render::{render_artifact_markdown, render_draft};
pub use slug::slugify;
pub use store::{
    ArtifactFile, ArtifactMarkdownStore, OrphanArtifact, PublishedFile, SaveArtifactOptions,
    SavedArtifact, StoredArtifact, find_orphan_artifacts, publish_file, sha256_hex,
    store_relative_path,
};
