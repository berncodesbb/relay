//! Central mapping from command failures to CLI exit codes and user-facing
//! text, ported from `src/cli/errors.ts` (retired TypeScript tree, removed
//! from the repository 2026-10-14). Errors carry the TypeScript
//! implementation's error *names* (rather than Rust types) so classification
//! stays byte-compatible with the frozen `exit-codes.json` fixture: 0
//! success, 1 runtime failure, 2 usage or configuration error.

/// Provider failure classification (`ProviderFailure.kind`), which selects
/// the hint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderFailureKind {
    /// Authentication failed.
    Auth,
    /// Any other provider failure.
    Other,
}

/// A command failure, classified by the TypeScript error name it
/// corresponds to.
#[derive(Debug, Clone, PartialEq)]
pub enum CliError {
    /// `ConfigValidationError` (exit 2).
    ConfigValidation {
        /// The formatted validation report.
        message: String,
    },
    /// `UsageError` (exit 2).
    Usage {
        /// The usage error.
        message: String,
    },
    /// `UnknownModelRoleError` (exit 2).
    UnknownModelRole {
        /// The offending role.
        role: String,
        /// Roles available in the effective catalog, in order.
        available: Vec<String>,
    },
    /// `ProjectNotFoundError` (exit 1).
    ProjectNotFound {
        /// The failure detail.
        message: String,
    },
    /// `NotInitializedError` (exit 1).
    NotInitialized {
        /// What needed an initialized project.
        detail: String,
    },
    /// `ProjectAlreadyInitializedError` (exit 1).
    ProjectAlreadyInitialized {
        /// The existing project id.
        project_id: String,
    },
    /// `ProviderFailure` (exit 1).
    ProviderFailure {
        /// Failure classification.
        kind: ProviderFailureKind,
        /// The failure message.
        message: String,
    },
    /// `RouteUnavailableError` (exit 1).
    RouteUnavailable {
        /// The failure message.
        message: String,
    },
    /// `PolicyRejectionError` (exit 1).
    PolicyRejection {
        /// The rejection reason.
        message: String,
    },
    /// Anything else: printed as `relay: <message>`, exit 1.
    Internal {
        /// The failure message.
        message: String,
    },
}

impl CliError {
    /// The TypeScript error class name this error reports as.
    pub fn error_name(&self) -> &'static str {
        match self {
            CliError::ConfigValidation { .. } => "ConfigValidationError",
            CliError::Usage { .. } => "UsageError",
            CliError::UnknownModelRole { .. } => "UnknownModelRoleError",
            CliError::ProjectNotFound { .. } => "ProjectNotFoundError",
            CliError::NotInitialized { .. } => "NotInitializedError",
            CliError::ProjectAlreadyInitialized { .. } => "ProjectAlreadyInitializedError",
            CliError::ProviderFailure { .. } => "ProviderFailure",
            CliError::RouteUnavailable { .. } => "RouteUnavailableError",
            CliError::PolicyRejection { .. } => "PolicyRejectionError",
            CliError::Internal { .. } => "unclassifiedError",
        }
    }

    /// The printable message.
    pub fn message(&self) -> String {
        match self {
            CliError::ConfigValidation { message }
            | CliError::Usage { message }
            | CliError::ProjectNotFound { message }
            | CliError::NotInitialized { detail: message }
            | CliError::ProviderFailure { message, .. }
            | CliError::RouteUnavailable { message }
            | CliError::PolicyRejection { message }
            | CliError::Internal { message } => message.clone(),
            CliError::ProjectAlreadyInitialized { project_id } => format!(
                "This workspace already has a Relay project ({project_id}); continue it or use a fresh workspace."
            ),
            CliError::UnknownModelRole { role, available } => {
                let listed = if available.is_empty() {
                    "(none defined)".to_string()
                } else {
                    available.join(", ")
                };
                format!("unknown model role \"{role}\"; available roles: {listed}")
            }
        }
    }
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for CliError {}

/// A mapped error report: printable message, exit code, optional hint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliErrorReport {
    /// The printable message.
    pub message: String,
    /// 0 success, 1 runtime failure, 2 usage/configuration error.
    pub code: i32,
    /// An actionable hint, when one applies.
    pub hint: Option<String>,
}

/// Map any failure onto a printable message, an exit code, and an optional
/// actionable hint. Ported from `describeCliError`.
pub fn describe_cli_error(error: &CliError) -> CliErrorReport {
    let message = error.message();
    match error {
        CliError::ConfigValidation { .. } => CliErrorReport {
            message,
            code: 2,
            hint: Some(
                "Run `relay config path` to see which configuration files are loaded.".to_string(),
            ),
        },
        CliError::Usage { .. } => CliErrorReport {
            message,
            code: 2,
            hint: Some("Run relay --help for usage.".to_string()),
        },
        CliError::UnknownModelRole { .. } => CliErrorReport {
            message,
            code: 2,
            hint: Some("Run `relay config show` to list configured model roles.".to_string()),
        },
        CliError::ProjectNotFound { .. } => CliErrorReport {
            message,
            code: 1,
            hint: Some("Run relay init first.".to_string()),
        },
        CliError::NotInitialized { .. } => CliErrorReport {
            message,
            code: 1,
            hint: Some("Run relay init first (or relay run \"<goal>\").".to_string()),
        },
        CliError::ProjectAlreadyInitialized { .. } => CliErrorReport {
            message,
            code: 1,
            hint: None,
        },
        CliError::ProviderFailure { kind, .. } => CliErrorReport {
            message,
            code: 1,
            hint: Some(
                if *kind == ProviderFailureKind::Auth {
                    "Check the provider API key environment variable (ANTHROPIC_API_KEY)."
                } else {
                    "Workflow state is resumable; fix the issue and rerun `relay run`."
                }
                .to_string(),
            ),
        },
        CliError::RouteUnavailable { .. } => CliErrorReport {
            message,
            code: 1,
            hint: Some("Set ANTHROPIC_API_KEY or pass --provider fake.".to_string()),
        },
        CliError::PolicyRejection { .. } => CliErrorReport {
            message,
            code: 1,
            hint: None,
        },
        CliError::Internal { .. } => CliErrorReport {
            message: format!("relay: {message}"),
            code: 1,
            hint: None,
        },
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn maps_error_names_to_codes_and_hints_like_the_fixture() {
        // The exit-codes.json errorClasses table.
        let cases: Vec<(CliError, i32)> = vec![
            (
                CliError::ConfigValidation {
                    message: "bad config".to_string(),
                },
                2,
            ),
            (
                CliError::NotInitialized {
                    detail: "x".to_string(),
                },
                1,
            ),
            (
                CliError::PolicyRejection {
                    message: "no".to_string(),
                },
                1,
            ),
            (
                CliError::ProjectAlreadyInitialized {
                    project_id: "prj_1".to_string(),
                },
                1,
            ),
            (
                CliError::ProjectNotFound {
                    message: "gone".to_string(),
                },
                1,
            ),
            (
                CliError::ProviderFailure {
                    kind: ProviderFailureKind::Other,
                    message: "boom".to_string(),
                },
                1,
            ),
            (
                CliError::RouteUnavailable {
                    message: "no route".to_string(),
                },
                1,
            ),
            (
                CliError::UnknownModelRole {
                    role: "ghost".to_string(),
                    available: vec!["fast".to_string()],
                },
                2,
            ),
            (
                CliError::Usage {
                    message: "what".to_string(),
                },
                2,
            ),
            (
                CliError::Internal {
                    message: "mystery".to_string(),
                },
                1,
            ),
        ];
        for (error, code) in cases {
            let report = describe_cli_error(&error);
            assert_eq!(report.code, code, "{}", error.error_name());
        }
    }

    #[test]
    fn auth_provider_failures_get_the_api_key_hint() {
        let report = describe_cli_error(&CliError::ProviderFailure {
            kind: ProviderFailureKind::Auth,
            message: "401".to_string(),
        });
        assert_eq!(
            report.hint.as_deref(),
            Some("Check the provider API key environment variable (ANTHROPIC_API_KEY).")
        );
    }

    #[test]
    fn internal_errors_are_prefixed_with_relay() {
        let report = describe_cli_error(&CliError::Internal {
            message: "mystery".to_string(),
        });
        assert_eq!(report.message, "relay: mystery");
        assert_eq!(report.code, 1);
    }
}
