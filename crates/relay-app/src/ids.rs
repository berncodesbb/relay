//! Application-owned id minting.
//!
//! The kernel never mints ids; every id enters through a fact. This module
//! is the application's deterministic minter for the ids it creates itself
//! (project, goal), derived from the workspace so re-running `init` in the
//! same directory produces the same identity, plus the evidence ids used by
//! published-view attachments.

use relay_domain::{EvidenceId, GoalId, ProjectId};

/// Maximum length of a domain-id slug (matching the kernel's id rule).
const MAX_SLUG: usize = 64;

/// Fold a filesystem-ish name into a domain-id slug: lowercase ASCII
/// alphanumerics with `-` separators, no leading/trailing/doubled dashes,
/// at most `MAX_SLUG` characters. Empty results fall back to `fallback`.
pub fn slugify_name(name: &str, fallback: &str) -> String {
    let mut slug = String::new();
    let mut pending_dash = false;
    for character in name.chars() {
        if character.is_ascii_alphanumeric() && character.is_ascii_lowercase() {
            slug.push(character);
            pending_dash = false;
        } else if character.is_ascii_alphanumeric() {
            // Uppercase folds to lowercase rather than to a separator.
            slug.push(character.to_ascii_lowercase());
            pending_dash = false;
        } else if !slug.is_empty() && !pending_dash {
            slug.push('-');
            pending_dash = true;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    slug.truncate(MAX_SLUG);
    while slug.ends_with('-') {
        slug.pop();
    }
    if slug.is_empty() {
        slug.clear();
        slug.push_str(fallback);
    }
    slug
}

/// The project identity for a workspace: `prj_<slug of the directory name>`.
pub fn project_id_for(workspace_name: &str) -> ProjectId {
    let slug = slugify_name(workspace_name, "project");
    ProjectId::parse(&format!("prj_{slug}")).unwrap_or_else(|error| fallback_project_id(&error))
}

// Unreachable while `slugify_name` emits valid slug characters; keeps
// minting total without inventing a fallback state.
#[allow(clippy::expect_used)]
fn fallback_project_id(error: &relay_domain::IdParseError) -> ProjectId {
    let _ = error;
    ProjectId::parse("prj_project").expect("literal project id always parses")
}

/// The goal identity for a workspace: `goal_<slug of the directory name>`.
pub fn goal_id_for(workspace_name: &str) -> GoalId {
    let slug = slugify_name(workspace_name, "goal");
    GoalId::parse(&format!("goal_{slug}")).unwrap_or_else(|error| fallback_goal_id(&error))
}

// Same infallible-literal invariant as `fallback_project_id`.
#[allow(clippy::expect_used)]
fn fallback_goal_id(error: &relay_domain::IdParseError) -> GoalId {
    let _ = error;
    GoalId::parse("goal_goal").expect("literal goal id always parses")
}

/// The evidence id for one published Markdown-view attachment:
/// `ev_<artifact slug>-v<version>-view`.
pub fn view_evidence_id(artifact: &str, version: u32) -> EvidenceId {
    let slug = slugify_name(artifact.trim_start_matches("art_"), "artifact");
    EvidenceId::parse(&format!("ev_{slug}-v{version}-view"))
        .unwrap_or_else(|error| fallback_evidence_id(&error))
}

// Same infallible-literal invariant as `fallback_project_id`.
#[allow(clippy::expect_used)]
fn fallback_evidence_id(error: &relay_domain::IdParseError) -> EvidenceId {
    let _ = error;
    EvidenceId::parse("ev_view").expect("literal evidence id always parses")
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn slugs_fold_to_domain_shape() {
        assert_eq!(slugify_name("Relay-Tmp-42", "project"), "relay-tmp-42");
        assert_eq!(slugify_name("--__--", "project"), "project");
        assert_eq!(slugify_name("", "project"), "project");
        assert_eq!(slugify_name("Ünïcode Dir", "project"), "n-code-dir");
        let long = "a".repeat(200);
        assert_eq!(slugify_name(&long, "project").len(), MAX_SLUG);
    }

    #[test]
    fn minted_ids_parse() {
        assert!(ProjectId::parse(project_id_for("My App").as_str()).is_ok());
        assert_eq!(project_id_for("My App").as_str(), "prj_my-app");
        assert_eq!(goal_id_for("My App").as_str(), "goal_my-app");
        assert_eq!(
            view_evidence_id("art_req-csv", 2).as_str(),
            "ev_req-csv-v2-view"
        );
    }
}
