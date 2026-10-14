//! Internal bounded subprocess execution shared by the process and git tools.
//!
//! Ported from `src/tools/spawn.ts` (retired TypeScript tree, removed from
//! the repository 2026-10-14). Always spawns with an argument array
//! (never a shell string), detaches the child into its own supervised tree so
//! the whole tree can be terminated on timeout or cancellation, bounds
//! collected stdout/stderr memory, and restricts inherited environment
//! variables to an explicit allowlist.

use std::collections::BTreeMap;
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use relay_providers::contract::CancelToken;

use crate::redact::env_name_looks_secret;
use crate::supervisor::{GRACE_INTERVAL, ProcessSupervisor};

/// Environment variable names children always inherit.
pub const BASE_ENV_NAMES: [&str; 8] = [
    "PATH", "HOME", "LANG", "TERM", "TMPDIR", "CI", "TZ", "SHELL",
];

/// Environment variable name prefixes children always inherit (e.g. LC_ALL).
pub const BASE_ENV_PREFIXES: [&str; 1] = ["LC_"];

/// Default per-stream character cap for collected child output.
pub const DEFAULT_STREAM_CAP_CHARS: usize = 200_000;

/// A bounded spawn request. `env_source` defaults to the process environment;
/// tests inject entries so behavior stays deterministic.
pub struct BoundedSpawnRequest<'a> {
    /// Executable name or absolute path.
    pub command: String,
    /// Argument array passed verbatim.
    pub args: Vec<String>,
    /// Working directory.
    pub cwd: std::path::PathBuf,
    /// Timeout after which the tree is terminated.
    pub timeout_ms: u64,
    /// Cancellation signal.
    pub signal: CancelToken,
    /// Names of extra environment variables to inherit on top of the
    /// allowlist.
    pub extra_env: Vec<String>,
    /// Per-stream character cap for collected output.
    pub max_stream_chars: Option<usize>,
    /// Environment entries inherited from (`name`, `value`) pairs; `None`
    /// reads the process environment.
    pub env_source: Option<&'a [(String, String)]>,
}

impl<'a> BoundedSpawnRequest<'a> {
    /// A request with defaults for everything but the command line.
    pub fn new(command: &str, args: Vec<String>, cwd: std::path::PathBuf) -> Self {
        BoundedSpawnRequest {
            command: command.to_owned(),
            args,
            cwd,
            timeout_ms: 30_000,
            signal: CancelToken::new(),
            extra_env: Vec::new(),
            max_stream_chars: None,
            env_source: None,
        }
    }
}

/// The outcome of one bounded spawn. Never errors: failures are reported in
/// the fields (`spawn_error`, `timed_out`, `canceled`).
pub struct BoundedSpawnResult {
    /// Exit code, when the child exited on its own.
    pub exit_code: Option<i32>,
    /// Terminating signal name (Unix), when the child was signaled.
    pub term_signal: Option<String>,
    /// Collected stdout (head+tail, capped).
    pub stdout: String,
    /// Collected stderr (head+tail, capped).
    pub stderr: String,
    /// Wall-clock duration in milliseconds.
    pub duration_ms: u64,
    /// Whether the timeout fired.
    pub timed_out: bool,
    /// Whether the cancellation signal fired.
    pub canceled: bool,
    /// Why the command could not start, when spawning failed.
    pub spawn_error: Option<String>,
    /// Inherited variable names, for recording (names only, never values).
    pub inherited_env: Vec<String>,
    /// Extra requested names refused because they look secret-bearing.
    pub refused_env: Vec<String>,
}

/// A built child environment plus what was inherited/refused.
pub struct BoundedEnv {
    /// The environment map to pass to the child.
    pub env: BTreeMap<String, String>,
    /// Names actually inherited.
    pub inherited: Vec<String>,
    /// Extra requested names refused as secret-shaped.
    pub refused: Vec<String>,
}

/// Build a minimal child environment from the allowlist plus named extras.
/// Deterministic variant used by tests and callers that own the environment.
pub fn build_bounded_env_from(extra_names: &[String], source: &[(String, String)]) -> BoundedEnv {
    let extras: Vec<&str> = extra_names.iter().map(String::as_str).collect();
    let mut env = BTreeMap::new();
    let mut inherited = Vec::new();
    let mut refused = Vec::new();
    for (name, value) in source {
        if env_name_looks_secret(name) {
            if extras.contains(&name.as_str()) {
                refused.push(name.clone());
            }
            continue;
        }
        let allowed = BASE_ENV_NAMES.contains(&name.as_str())
            || BASE_ENV_PREFIXES
                .iter()
                .any(|prefix| name.starts_with(prefix))
            || extras.contains(&name.as_str());
        if !allowed {
            continue;
        }
        env.insert(name.clone(), value.clone());
        inherited.push(name.clone());
    }
    // Source-environment iteration order is platform-dependent; report the
    // names sorted so recorded output stays deterministic.
    inherited.sort();
    refused.sort();
    BoundedEnv {
        env,
        inherited,
        refused,
    }
}

/// Build a minimal child environment from the allowlist plus named extras,
/// reading the process environment.
pub fn build_bounded_env(extra_names: &[String]) -> BoundedEnv {
    let source: Vec<(String, String)> = std::env::vars().collect();
    build_bounded_env_from(extra_names, &source)
}

/// Bounded byte collector that keeps the head and the tail of a stream.
#[derive(Default)]
pub struct HeadTailBuffer {
    head: Vec<u8>,
    tail: Vec<u8>,
}

impl HeadTailBuffer {
    /// Head and tail byte limits.
    pub const HEAD_LIMIT: usize = 1_000_000;
    /// Tail byte limit.
    pub const TAIL_LIMIT: usize = 1_000_000;

    /// An empty collector.
    pub fn new() -> Self {
        HeadTailBuffer::default()
    }

    /// Add one chunk.
    pub fn push(&mut self, chunk: &[u8]) {
        let mut data = chunk;
        if self.head.len() < Self::HEAD_LIMIT {
            let take = (Self::HEAD_LIMIT - self.head.len()).min(data.len());
            self.head.extend_from_slice(&data[..take]);
            data = &data[take..];
        }
        if data.is_empty() {
            return;
        }
        self.tail.extend_from_slice(data);
        if self.tail.len() > Self::TAIL_LIMIT {
            let overflow = self.tail.len() - Self::TAIL_LIMIT;
            self.tail.drain(..overflow);
        }
    }

    /// Decode the collected head+tail as UTF-8 (lossy).
    pub fn to_string_utf8(&self) -> String {
        let mut joined = self.head.clone();
        joined.extend_from_slice(&self.tail);
        String::from_utf8_lossy(&joined).into_owned()
    }
}

/// Cap decoded stream text, keeping head and tail around a truncation marker.
pub fn cap_stream_text(text: &str, max_chars: usize) -> String {
    if max_chars < 64 {
        let mut bounded: String = String::new();
        for character in text.chars().take(max_chars) {
            bounded.push(character);
        }
        return bounded;
    }
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    let budget = max_chars - 32;
    let head_chars = budget * 3 / 4;
    let tail_chars = budget - head_chars;
    let dropped = text.chars().count() - head_chars - tail_chars;
    let head: String = text.chars().take(head_chars).collect();
    let tail: String = text
        .chars()
        .skip(text.chars().count() - tail_chars)
        .collect();
    format!("{head}\n…[{dropped} chars truncated]…\n{tail}")
}

#[cfg(unix)]
type PlatformSupervisor = crate::supervisor::UnixProcessGroup;
#[cfg(windows)]
type PlatformSupervisor = crate::supervisor::WindowsJobObject;

/// Read one piped child stream into a shared bounded buffer until EOF.
/// Returns `None` when the stream was not piped.
fn spawn_stream_reader<R: Read + Send + 'static>(
    handle: Option<R>,
    buffer: &Arc<Mutex<HeadTailBuffer>>,
) -> Option<std::thread::JoinHandle<()>> {
    let mut handle = handle?;
    let buffer = Arc::clone(buffer);
    Some(std::thread::spawn(move || {
        let mut chunk = [0u8; 8192];
        loop {
            match handle.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    let mut guard = buffer.lock().unwrap_or_else(|poison| poison.into_inner());
                    guard.push(&chunk[..read]);
                }
            }
        }
    }))
}

/// Spawn a command with bounded output, timeout, abort, and process-tree
/// termination. Never panics: failures are reported in the result
/// (`spawn_error` / `timed_out` / `canceled`).
pub fn spawn_bounded(request: BoundedSpawnRequest<'_>) -> BoundedSpawnResult {
    let started_at = Instant::now();
    let max_stream_chars = request.max_stream_chars.unwrap_or(DEFAULT_STREAM_CAP_CHARS);
    let source: Vec<(String, String)> = match request.env_source {
        Some(source) => source.to_vec(),
        None => std::env::vars().collect(),
    };
    let bounded = build_bounded_env_from(&request.extra_env, &source);

    let mut command = Command::new(&request.command);
    command
        .args(&request.args)
        .current_dir(&request.cwd)
        .env_clear()
        .envs(&bounded.env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    PlatformSupervisor::configure(&mut command);

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return BoundedSpawnResult {
                exit_code: None,
                term_signal: None,
                stdout: String::new(),
                stderr: String::new(),
                duration_ms: started_at.elapsed().as_millis() as u64,
                timed_out: false,
                canceled: false,
                spawn_error: Some(error.to_string()),
                inherited_env: bounded.inherited,
                refused_env: bounded.refused,
            };
        }
    };

    let stdout_handle = child.stdout.take();
    let stderr_handle = child.stderr.take();
    let stdout_buffer = Arc::new(Mutex::new(HeadTailBuffer::new()));
    let stderr_buffer = Arc::new(Mutex::new(HeadTailBuffer::new()));
    let mut reader_handles = Vec::new();
    reader_handles.extend(spawn_stream_reader(stdout_handle, &stdout_buffer));
    reader_handles.extend(spawn_stream_reader(stderr_handle, &stderr_buffer));

    let mut supervisor = PlatformSupervisor::adopt(&mut child);
    let mut timed_out = false;
    let mut canceled = false;

    let deadline = Instant::now() + Duration::from_millis(request.timeout_ms.max(1));
    let mut escalated = false;
    let mut escalation_deadline: Option<Instant> = None;
    loop {
        if let Some(escalate_at) = escalation_deadline {
            if !escalated && Instant::now() >= escalate_at {
                escalated = true;
                supervisor.terminate_forced();
            }
        } else {
            let now_canceled = request.signal.is_cancelled();
            let now_timed_out = Instant::now() >= deadline;
            if now_canceled || now_timed_out {
                canceled = now_canceled;
                timed_out = now_timed_out && !now_canceled;
                supervisor.terminate_graceful();
                escalation_deadline = Some(Instant::now() + GRACE_INTERVAL);
            }
        }
        match child.try_wait() {
            Ok(Some(_status)) => break,
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Err(_) => break,
        }
    }

    for handle in reader_handles {
        let _ = handle.join();
    }

    let (exit_code, term_signal) = match child.wait() {
        Ok(status) => {
            #[cfg(unix)]
            let term_signal = {
                use std::os::unix::process::ExitStatusExt;
                status.signal().map(signal_signal_name)
            };
            #[cfg(not(unix))]
            let term_signal = None;
            (status.code(), term_signal)
        }
        Err(_) => (None, None),
    };

    let stdout = cap_stream_text(
        &stdout_buffer
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .to_string_utf8(),
        max_stream_chars,
    );
    let stderr = cap_stream_text(
        &stderr_buffer
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .to_string_utf8(),
        max_stream_chars,
    );

    BoundedSpawnResult {
        exit_code,
        term_signal,
        stdout,
        stderr,
        duration_ms: started_at.elapsed().as_millis() as u64,
        timed_out,
        canceled,
        spawn_error: None,
        inherited_env: bounded.inherited,
        refused_env: bounded.refused,
    }
}

#[cfg(unix)]
fn signal_signal_name(signal: i32) -> String {
    match signal {
        1 => "SIGHUP".to_owned(),
        2 => "SIGINT".to_owned(),
        3 => "SIGQUIT".to_owned(),
        9 => "SIGKILL".to_owned(),
        13 => "SIGPIPE".to_owned(),
        15 => "SIGTERM".to_owned(),
        other => format!("SIG{other}"),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn env_source(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect()
    }

    fn tmp() -> PathBuf {
        tempfile::Builder::new()
            .prefix("relay-spawn-")
            .tempdir()
            .expect("temp dir")
            .keep()
    }

    #[test]
    fn builds_a_bounded_environment_from_an_explicit_source() {
        let source = env_source(&[
            ("PATH", "/usr/bin"),
            ("HOME", "/home/tester"),
            ("LANG", "C"),
            ("LC_ALL", "en_US.UTF-8"),
            ("RELAY_UNLISTED_VAR", "leaky"),
            ("RELAY_TEST_API_KEY", "secret"),
            ("SECRET", "secret"),
        ]);
        let extra = vec![
            "RELAY_UNLISTED_VAR".to_owned(),
            "RELAY_TEST_API_KEY".to_owned(),
        ];
        let bounded = build_bounded_env_from(&extra, &source);
        assert_eq!(
            bounded.inherited,
            vec![
                "HOME".to_owned(),
                "LANG".to_owned(),
                "LC_ALL".to_owned(),
                "PATH".to_owned(),
                "RELAY_UNLISTED_VAR".to_owned()
            ]
        );
        assert_eq!(bounded.refused, vec!["RELAY_TEST_API_KEY".to_owned()]);
        assert!(!bounded.env.contains_key("RELAY_TEST_API_KEY"));
        assert!(!bounded.env.contains_key("SECRET"));
    }

    #[cfg(unix)]
    #[test]
    fn runs_a_command_and_collects_output() {
        let cwd = tmp();
        let result = spawn_bounded(BoundedSpawnRequest::new(
            "/bin/echo",
            vec!["hello".to_owned(), "relay".to_owned()],
            cwd,
        ));
        assert_eq!(result.exit_code, Some(0));
        assert!(result.stdout.contains("hello relay"), "{:?}", result.stdout);
        assert_eq!(result.stderr, "");
        assert!(!result.timed_out);
        assert!(!result.canceled);
        assert_eq!(result.spawn_error, None);
    }

    #[cfg(unix)]
    #[test]
    fn reports_spawn_errors_without_panicking() {
        let cwd = tmp();
        let result = spawn_bounded(BoundedSpawnRequest::new(
            "definitely-not-a-real-binary-xyz",
            vec!["--flag".to_owned()],
            cwd,
        ));
        assert!(result.spawn_error.is_some());
        assert_eq!(result.exit_code, None);
    }

    #[cfg(unix)]
    #[test]
    fn kills_the_tree_on_timeout() {
        let cwd = tmp();
        let mut request =
            BoundedSpawnRequest::new("/bin/sh", vec!["-c".to_owned(), "sleep 60".to_owned()], cwd);
        request.timeout_ms = 200;
        let result = spawn_bounded(request);
        assert!(result.timed_out);
        assert!(!result.canceled);
        assert!(result.exit_code.is_none() || result.exit_code != Some(0));
        assert!(result.term_signal.is_some(), "{:?}", result.term_signal);
    }

    #[cfg(unix)]
    #[test]
    fn kills_the_tree_on_cancellation() {
        let cwd = tmp();
        let token = CancelToken::new();
        let watcher = token.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            watcher.cancel("test");
        });
        let mut request =
            BoundedSpawnRequest::new("/bin/sh", vec!["-c".to_owned(), "sleep 60".to_owned()], cwd);
        request.signal = token;
        let started = Instant::now();
        let result = spawn_bounded(request);
        assert!(result.canceled);
        assert!(!result.timed_out);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[cfg(unix)]
    #[test]
    fn caps_oversized_output_with_a_marker() {
        let cwd = tmp();
        let mut request = BoundedSpawnRequest::new(
            "/bin/sh",
            vec!["-c".to_owned(), "yes zzz | head -c 300000".to_owned()],
            cwd,
        );
        request.max_stream_chars = Some(2_000);
        let result = spawn_bounded(request);
        assert!(
            result.stdout.contains("chars truncated"),
            "{}",
            result.stdout
        );
        assert!(result.stdout.chars().count() <= 2_100);
    }

    #[cfg(unix)]
    #[test]
    fn passes_an_environment_allowlist_to_children() {
        let cwd = tmp();
        let source = env_source(&[
            ("PATH", "/usr/bin:/bin"),
            ("HOME", "/home/tester"),
            ("RELAY_UNLISTED_VAR", "leaky-value-123"),
        ]);
        let mut request = BoundedSpawnRequest::new(
            "/bin/sh",
            vec![
                "-c".to_owned(),
                "echo \"unlisted=$RELAY_UNLISTED_VAR home=$HOME\"".to_owned(),
            ],
            cwd,
        );
        request.env_source = Some(&source);
        let result = spawn_bounded(request);
        assert!(result.stdout.contains("unlisted="), "{}", result.stdout);
        assert!(
            result.stdout.contains("home=/home/tester"),
            "{}",
            result.stdout
        );
    }

    #[test]
    fn cap_stream_text_keeps_head_and_tail() {
        let text = "a".repeat(1_000) + &"b".repeat(1_000);
        let capped = cap_stream_text(&text, 200);
        assert!(capped.contains("chars truncated"), "{capped}");
        assert!(capped.starts_with('a'), "{capped}");
        assert!(capped.ends_with('b'), "{capped}");
        // Short text passes through; tiny budgets truncate without a marker.
        assert_eq!(cap_stream_text("hello", 200), "hello");
        assert_eq!(cap_stream_text("hello", 3), "hel");
    }

    #[test]
    fn head_tail_buffer_keeps_head_and_tail_bytes() {
        let mut buffer = HeadTailBuffer::new();
        buffer.push(b"head");
        buffer.push(b"tail");
        assert_eq!(buffer.to_string_utf8(), "headtail");
    }
}
