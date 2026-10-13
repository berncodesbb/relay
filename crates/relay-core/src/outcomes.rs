//! Workflow outcome contracts. After every controller step the deterministic
//! transition policy produces exactly one outcome; models may propose
//! transitions, only policy code emits outcomes. Ported from
//! `src/core/outcomes.ts`.

use crate::modes::WorkflowMode;

/// The four outcome kinds, serialized with their SCREAMING-case wire spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum OutcomeKind {
    /// Keep working in the current mode.
    #[serde(rename = "CONTINUE")]
    Continue,
    /// Move to the next mode.
    #[serde(rename = "ADVANCE")]
    Advance,
    /// A human decision is required before proceeding.
    #[serde(rename = "NEEDS_USER")]
    NeedsUser,
    /// Route back to an earlier stage to address failing work.
    #[serde(rename = "REVISE")]
    Revise,
}

impl OutcomeKind {
    /// The wire spelling of this outcome kind.
    pub fn as_str(self) -> &'static str {
        match self {
            OutcomeKind::Continue => "CONTINUE",
            OutcomeKind::Advance => "ADVANCE",
            OutcomeKind::NeedsUser => "NEEDS_USER",
            OutcomeKind::Revise => "REVISE",
        }
    }

    /// Parse the wire spelling of an outcome kind.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "CONTINUE" => Some(OutcomeKind::Continue),
            "ADVANCE" => Some(OutcomeKind::Advance),
            "NEEDS_USER" => Some(OutcomeKind::NeedsUser),
            "REVISE" => Some(OutcomeKind::Revise),
            _ => None,
        }
    }

    /// The enum options as a Zod-style quoted list for validation messages.
    pub fn zod_options() -> String {
        crate::modes::zod_option_list(["CONTINUE", "ADVANCE", "NEEDS_USER", "REVISE"].into_iter())
    }
}

/// Exactly one controller outcome per step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControllerOutcome {
    /// Keep working in the current mode.
    Continue {
        /// Why no transition is needed.
        reason: String,
    },
    /// Move to `to`.
    Advance {
        /// The mode to enter.
        to: WorkflowMode,
        /// Why the transition is allowed.
        reason: String,
    },
    /// A human decision is required.
    NeedsUser {
        /// The focused question to surface.
        question: String,
        /// The options offered to the user.
        options: Vec<String>,
        /// Why the user is needed.
        reason: String,
    },
    /// Route back to `stage` to address failing work.
    Revise {
        /// The earliest stage affected by the failing work.
        stage: WorkflowMode,
        /// Why the work routes back.
        reason: String,
    },
}

impl ControllerOutcome {
    /// The kind of this outcome.
    pub fn kind(&self) -> OutcomeKind {
        match self {
            ControllerOutcome::Continue { .. } => OutcomeKind::Continue,
            ControllerOutcome::Advance { .. } => OutcomeKind::Advance,
            ControllerOutcome::NeedsUser { .. } => OutcomeKind::NeedsUser,
            ControllerOutcome::Revise { .. } => OutcomeKind::Revise,
        }
    }

    /// The outcome's reason string (every variant carries one).
    pub fn reason(&self) -> &str {
        match self {
            ControllerOutcome::Continue { reason }
            | ControllerOutcome::Advance { reason, .. }
            | ControllerOutcome::NeedsUser { reason, .. }
            | ControllerOutcome::Revise { reason, .. } => reason,
        }
    }
}

/// The kind of a controller outcome (mirrors the TypeScript helper).
pub fn outcome_kind(outcome: &ControllerOutcome) -> OutcomeKind {
    outcome.kind()
}
