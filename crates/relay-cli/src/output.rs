//! Plain-text output for non-interactive Relay commands, ported from
//! `src/cli/output.ts` (retired TypeScript tree, removed 2026-10-14). All
//! CLI output flows through an [`OutputWriter`] so
//! commands never print directly and output can be captured in tests. This
//! is the one place in the Relay crates that writes to stdout/stderr; the
//! relay-ui renderers return strings and the writer serializes them.
//!
//! Plain text is suppressed in `--json` mode so machine output stays
//! parseable; errors always print (to stderr); help/version print through
//! [`OutputWriter::always`], which `--json` never suppresses (and the
//! binary passes a plain writer for `--help`/`-h`).

use std::io::Write;

/// Where a writer sends its output. `std` uses the process streams; tests
/// inject buffers.
pub enum OutputTarget {
    /// The process's real stdout/stderr.
    Std,
    /// Captured sinks (in-process tests and the compatibility harness).
    Buffers(Box<dyn Write>, Box<dyn Write>),
}

/// The CLI output writer.
pub struct OutputWriter {
    json: bool,
    target: OutputTarget,
}

impl OutputWriter {
    /// A writer over the process streams.
    pub fn std(json: bool) -> OutputWriter {
        OutputWriter {
            json,
            target: OutputTarget::Std,
        }
    }

    /// A writer over captured sinks.
    pub fn captured(json: bool, out: Box<dyn Write>, err: Box<dyn Write>) -> OutputWriter {
        OutputWriter {
            json,
            target: OutputTarget::Buffers(out, err),
        }
    }

    /// Write a plain-text line to stdout (suppressed in `--json` mode).
    pub fn text(&mut self, line: &str) {
        if self.json {
            return;
        }
        self.write_out(&format!("{line}\n"));
    }

    /// Write a plain-text line to stdout regardless of `--json` (help,
    /// version, and trailing notes that must always be visible).
    pub fn always(&mut self, line: &str) {
        self.write_out(&format!("{line}\n"));
    }

    /// Write pretty-printed JSON to stdout (never suppressed).
    pub fn json(&mut self, value: &str) {
        self.write_out(&format!("{value}\n"));
    }

    /// Write an error line to stderr.
    pub fn error(&mut self, line: &str) {
        self.write_err(&format!("{line}\n"));
    }

    fn write_out(&mut self, payload: &str) {
        match &mut self.target {
            OutputTarget::Std => {
                let stdout = std::io::stdout();
                let mut lock = stdout.lock();
                // A failed write (closed pipe) is not worth crashing over;
                // the reference falls back to a buffered write and moves on.
                let _ = lock.write_all(payload.as_bytes());
                let _ = lock.flush();
            }
            OutputTarget::Buffers(out, _) => {
                let _ = out.write_all(payload.as_bytes());
                let _ = out.flush();
            }
        }
    }

    fn write_err(&mut self, payload: &str) {
        match &mut self.target {
            OutputTarget::Std => {
                let stderr = std::io::stderr();
                let mut lock = stderr.lock();
                let _ = lock.write_all(payload.as_bytes());
                let _ = lock.flush();
            }
            OutputTarget::Buffers(_, err) => {
                let _ = err.write_all(payload.as_bytes());
                let _ = err.flush();
            }
        }
    }
}
