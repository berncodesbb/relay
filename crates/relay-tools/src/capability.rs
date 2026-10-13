//! Tool capability classification.
//!
//! Side-effect classes a tool can carry. Grants and scheduling policy permit
//! classes; per-call enforcement happens in [`crate::grant_gate`]. This enum
//! is owned by `relay-tools` (moved from the frozen `relay-core` reference
//! during Phase 5) so the tool boundary carries no old-domain dependency;
//! [`relay_domain::Operation`] is the domain-side spelling the grant gate
//! maps onto.

/// Side-effect classes a tool can carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum ToolCapability {
    /// Read-only access to workspace files.
    Read,
    /// Create or modify workspace files.
    Write,
    /// Run bounded child processes.
    Execute,
}

impl ToolCapability {
    /// The wire spelling of this capability.
    pub fn as_str(self) -> &'static str {
        match self {
            ToolCapability::Read => "read",
            ToolCapability::Write => "write",
            ToolCapability::Execute => "execute",
        }
    }

    /// Parse the wire spelling of a capability.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "read" => Some(ToolCapability::Read),
            "write" => Some(ToolCapability::Write),
            "execute" => Some(ToolCapability::Execute),
            _ => None,
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn wire_spellings_round_trip() {
        for capability in [
            ToolCapability::Read,
            ToolCapability::Write,
            ToolCapability::Execute,
        ] {
            assert_eq!(ToolCapability::parse(capability.as_str()), Some(capability));
        }
        assert_eq!(ToolCapability::parse("nope"), None);
    }
}
