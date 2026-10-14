//! The invocation seam between the binary and dispatch.
//!
//! Pure parsing ([`crate::args`]) never reads the terminal; this module is
//! the one place that does, turning "are stdin and stdout TTYs?" into a
//! [`TerminalCapability`] value dispatch can be tested against without a
//! live terminal.

use crossterm::tty::IsTty;

use crate::dispatch::CliDeps;
use crate::dispatch::run_with_capability;
use crate::output::OutputWriter;

/// What the process's terminal can do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalCapability {
    /// Whether stdin is a TTY.
    pub stdin_tty: bool,
    /// Whether stdout is a TTY.
    pub stdout_tty: bool,
}

impl TerminalCapability {
    /// Chat is interactive only when both stdin and stdout are TTYs: input
    /// must be readable and rendered output would otherwise be lost to the
    /// redirect.
    pub fn interactive(&self) -> bool {
        self.stdin_tty && self.stdout_tty
    }

    /// A non-interactive capability (captured buffers, redirected output).
    pub fn non_interactive() -> Self {
        TerminalCapability {
            stdin_tty: false,
            stdout_tty: false,
        }
    }
}

/// Detect the process's terminal capability — the only place TTY-ness is
/// read.
pub fn detect_terminal_capability() -> TerminalCapability {
    TerminalCapability {
        stdin_tty: std::io::stdin().is_tty(),
        stdout_tty: std::io::stdout().is_tty(),
    }
}

/// The binary entry point: parse, dispatch under the real terminal
/// capability, and return the process exit code.
///
/// `--json` is per-command; the writer is built here before dispatch, and
/// `--help`/`-h` always get a plain writer so help output is never
/// suppressed.
pub fn run(argv: &[String], capability: TerminalCapability) -> i32 {
    let json = argv
        .iter()
        .any(|argument| argument == "--json" || argument.starts_with("--json="));
    let mut writer = OutputWriter::std(json);
    if argv
        .iter()
        .any(|argument| argument == "--help" || argument == "-h")
    {
        let mut plain = OutputWriter::std(false);
        run_with_capability(argv, &mut plain, CliDeps::production(None), capability)
    } else {
        run_with_capability(argv, &mut writer, CliDeps::production(None), capability)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn interactive_requires_both_streams_to_be_ttys() {
        assert!(
            TerminalCapability {
                stdin_tty: true,
                stdout_tty: true
            }
            .interactive()
        );
        for (stdin_tty, stdout_tty) in [(true, false), (false, true), (false, false)] {
            assert!(
                !TerminalCapability {
                    stdin_tty,
                    stdout_tty
                }
                .interactive(),
                "stdin={stdin_tty} stdout={stdout_tty}"
            );
        }
        assert!(!TerminalCapability::non_interactive().interactive());
    }
}
