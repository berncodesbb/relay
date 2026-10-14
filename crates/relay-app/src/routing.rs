//! Catalog-phase to routing-purpose mapping.
//!
//! Built-in work kinds carry a [`Phase`] label in
//! [`relay_workflows::WorkflowCatalog`]; runtime routing resolves each
//! executable work item to a configuration routing key through that label.
//! This is composition metadata consulted while constructing an attempt
//! grant — it is not stored as graph state, imposes no phase ordering, and
//! changes nothing about engine readiness, scheduling, or acceptance. The
//! mapping deliberately lives here, at the application boundary, never in
//! `relay-domain` or `relay-engine`.

use relay_domain::CompletionRule;
use relay_workflows::Phase;
use relay_workflows::WorkflowCatalog;

/// The routing key for discovery work (catalog phase Discovery).
pub const PURPOSE_DISCOVERY: &str = "discovery";
/// The routing key for specification work (catalog phase Specification).
pub const PURPOSE_SPECIFICATION: &str = "specification";
/// The routing key for design work (catalog phase Design).
pub const PURPOSE_TECHNICAL_DESIGN: &str = "technical-design";
/// The routing key for planning work (catalog phase Planning).
pub const PURPOSE_PLANNING: &str = "planning";
/// The routing key for implementation work (catalog phase Implementation).
pub const PURPOSE_IMPLEMENTATION: &str = "implementation";
/// The routing key for verification work (catalog phase Verification).
pub const PURPOSE_VERIFICATION: &str = "verification";
/// The routing key for delivery work (catalog phase Delivery).
pub const PURPOSE_DELIVERY: &str = "delivery";

/// The routing purpose a built-in work kind resolves to, through its catalog
/// phase. `None` for kinds absent from the built-in catalog (custom kinds)
/// and for anything that is not a work kind at all; callers decide how to
/// fail on unmapped executable work — it is never silently routed through
/// another purpose.
pub fn routing_purpose(kind: &str) -> Option<&'static str> {
    let catalog = WorkflowCatalog::builtin();
    catalog
        .get(kind)
        .map(|definition| phase_purpose(definition.phase))
}

/// The configuration routing key of one catalog phase (the plan's §5 table).
fn phase_purpose(phase: Phase) -> &'static str {
    match phase {
        Phase::Discovery => PURPOSE_DISCOVERY,
        Phase::Specification => PURPOSE_SPECIFICATION,
        Phase::Design => PURPOSE_TECHNICAL_DESIGN,
        Phase::Planning => PURPOSE_PLANNING,
        Phase::Implementation => PURPOSE_IMPLEMENTATION,
        Phase::Verification => PURPOSE_VERIFICATION,
        Phase::Delivery => PURPOSE_DELIVERY,
    }
}

/// Whether a work item's completion rule makes it *executor-executable* —
/// completed by an attempt receipt or criterion evidence — as opposed to
/// human work, which completes through a decision and never receives a
/// model-route grant.
pub fn is_executable_work(completion: &CompletionRule) -> bool {
    matches!(
        completion,
        CompletionRule::Receipt | CompletionRule::CriterionSatisfied { .. }
    )
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)] // test-only, house style
mod tests {
    use super::*;

    /// Every built-in catalog kind maps to the documented purpose — the
    /// mapping is total over the catalog by construction.
    #[test]
    fn every_catalog_kind_maps_to_its_phase_purpose() {
        let catalog = WorkflowCatalog::builtin();
        for definition in catalog.iter() {
            let purpose = routing_purpose(definition.label)
                .unwrap_or_else(|| panic!("{} must map", definition.label));
            let expected = phase_purpose(definition.phase);
            assert_eq!(purpose, expected, "{}", definition.label);
        }
    }

    /// The plan's §5 table, pinned kind by kind for the kinds that carry
    /// work through the pipeline (not the human decision kinds).
    #[test]
    fn the_documented_purpose_table_holds() {
        let cases = [
            ("discover-opportunity", PURPOSE_DISCOVERY),
            ("define-requirements", PURPOSE_SPECIFICATION),
            ("inspect-repository", PURPOSE_TECHNICAL_DESIGN),
            ("design-approach", PURPOSE_TECHNICAL_DESIGN),
            ("decompose-plan", PURPOSE_PLANNING),
            ("implement-task", PURPOSE_IMPLEMENTATION),
            ("integrate-changes", PURPOSE_IMPLEMENTATION),
            ("create-corrective-work", PURPOSE_IMPLEMENTATION),
            ("verify-criteria", PURPOSE_VERIFICATION),
            ("prepare-delivery-evidence", PURPOSE_DELIVERY),
        ];
        for (kind, purpose) in cases {
            assert_eq!(routing_purpose(kind), Some(purpose), "{kind}");
        }
    }

    /// Human work kinds still map (their phase exists); what keeps them out
    /// of model routing is their completion rule, not the catalog.
    #[test]
    fn human_work_kinds_map_but_are_not_executable() {
        for kind in [
            "clarify-unknowns",
            "request-delivery-authorization",
            "approve-artifact",
        ] {
            assert!(WorkflowCatalog::builtin().get(kind).is_some(), "{kind}");
            assert!(routing_purpose(kind).is_some(), "{kind}");
        }
        assert!(!is_executable_work(&CompletionRule::Decision {
            decision: "dec_1".parse().unwrap()
        }));
    }

    /// Unknown (custom) kinds never silently inherit a purpose.
    #[test]
    fn custom_kinds_have_no_purpose() {
        assert_eq!(routing_purpose("my-custom-experiment"), None);
        assert_eq!(routing_purpose(""), None);
    }

    /// Only receipt- and criterion-ruled work is executable.
    #[test]
    fn only_receipt_and_criterion_work_is_executable() {
        assert!(is_executable_work(&CompletionRule::Receipt));
        assert!(is_executable_work(&CompletionRule::CriterionSatisfied {
            criterion: relay_domain::ArtifactVersionRef::new(
                "art_cri-x".parse().unwrap(),
                relay_domain::ArtifactVersion::FIRST,
            )
        }));
        assert!(!is_executable_work(&CompletionRule::Decision {
            decision: "dec_1".parse().unwrap()
        }));
    }
}
