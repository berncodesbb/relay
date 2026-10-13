//! Writer lock (`.relay/locks/writer.lock`, `docs/event-format.md` §8).
//!
//! Exactly one process may append at a time. The lock file is created
//! exclusively (`O_CREAT | O_EXCL` semantics via `create_new`, which maps to
//! `CreateNew` on Windows) and holds canonical JSON `{pid, acquiredAt}`.
//! Acquisition retries with a short sleep until a bounded timeout (default
//! 5000 ms) and then fails with a readable diagnostic including the holder
//! pid and lock age. A lock whose owning process no longer exists (PID
//! liveness probe: `kill(pid, 0)` on Unix, process table on Windows) is
//! quarantined into `recovery/` and re-acquired. Release is best-effort;
//! the stale probe is the real guard against crashed holders.

use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;
use std::time::Instant;

use crate::canonical::Json;
use crate::clock::Clock;
use crate::clock::parse_iso_to_epoch_millis;
use crate::error::Result;
use crate::error::StoreError;

/// Default bounded acquisition timeout (5000 ms, matching the legacy SQLite
/// busy timeout).
pub const DEFAULT_LOCK_TIMEOUT: Duration = Duration::from_millis(5000);

/// Pause between acquisition attempts.
const RETRY_PAUSE: Duration = Duration::from_millis(25);

/// The lock file contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockInfo {
    /// OS process id of the holder.
    pub pid: u32,
    /// When the lock was acquired (Relay timestamp).
    pub acquired_at: String,
}

impl LockInfo {
    /// Canonical JSON body of the lock file.
    pub fn to_json(&self) -> Json {
        Json::object([
            ("pid", Json::UInt(self.pid as u64)),
            ("acquiredAt", Json::Str(self.acquired_at.clone())),
        ])
    }

    /// Parse lock file contents.
    pub fn parse(text: &str) -> Option<LockInfo> {
        let value = Json::parse(text).ok()?;
        let pid = value.get("pid")?.as_u64()?;
        if pid > u32::MAX as u64 {
            return None;
        }
        let acquired_at = value.get("acquiredAt")?.as_str()?.to_string();
        Some(LockInfo {
            pid: pid as u32,
            acquired_at,
        })
    }
}

/// An exclusively-held lock. Released (file removed) on drop; release is
/// best-effort by design.
#[derive(Debug)]
pub struct WriterGuard {
    path: PathBuf,
}

impl WriterGuard {
    /// The lock file path.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for WriterGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Whether a writer lock file currently exists (held or stale).
pub fn is_present(lock_path: &Path) -> bool {
    lock_path.exists()
}

/// Read the holder information from a lock file, when readable and
/// well-formed.
pub fn read_holder(lock_path: &Path) -> Option<LockInfo> {
    let text = std::fs::read_to_string(lock_path).ok()?;
    LockInfo::parse(&text)
}

/// Acquire the lock exclusively, retrying until `timeout` elapses.
///
/// Stale locks (holder process gone) are moved to
/// `recovery/stale-writer-lock-<timestamp>.jsonl` and re-acquired. A lock
/// held by a live process until the timeout produces an error naming the
/// holder pid and lock age.
pub fn acquire(
    lock_path: &Path,
    recovery_dir: &Path,
    timeout: Duration,
    clock: &dyn Clock,
) -> Result<WriterGuard> {
    let started = Instant::now();
    loop {
        match try_create(lock_path, clock) {
            Ok(guard) => return Ok(guard),
            Err(reason) => {
                if let Some(info) = read_holder(lock_path)
                    && !process_alive(info.pid)
                {
                    // Stale holder: quarantine and retry immediately.
                    quarantine_stale(lock_path, recovery_dir, clock)?;
                    continue;
                }
                if started.elapsed() >= timeout {
                    return Err(lock_timeout_error(lock_path, reason, clock));
                }
                std::thread::sleep(RETRY_PAUSE);
            }
        }
    }
}

fn try_create(lock_path: &Path, clock: &dyn Clock) -> Result<WriterGuard, String> {
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create locks directory: {error}"))?;
    }
    let info = LockInfo {
        pid: std::process::id(),
        acquired_at: clock.now_iso(),
    };
    let body = info.to_json().to_canonical_bytes();
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    let mut file = options
        .open(lock_path)
        .map_err(|error| format!("lock file exists or cannot be created: {error}"))?;
    use std::io::Write;
    if let Err(error) = file.write_all(&body) {
        let _ = std::fs::remove_file(lock_path);
        return Err(format!("cannot write lock file: {error}"));
    }
    if let Err(error) = file.sync_all() {
        let _ = std::fs::remove_file(lock_path);
        return Err(format!("cannot fsync lock file: {error}"));
    }
    Ok(WriterGuard {
        path: lock_path.to_path_buf(),
    })
}

fn lock_timeout_error(lock_path: &Path, reason: String, clock: &dyn Clock) -> StoreError {
    let detail = match read_holder(lock_path) {
        Some(info) => {
            let age = parse_iso_to_epoch_millis(&info.acquired_at)
                .zip(parse_iso_to_epoch_millis(&clock.now_iso()))
                .map(|(start, now)| (now - start).max(0))
                .map(|ms| format!(" (held for about {ms} ms)"))
                .unwrap_or_default();
            format!(
                "another Relay writer (pid {}) holds it, acquired at {}{}. Wait for it to \
                 finish; if that process is gone, remove the lock file {} and retry.",
                info.pid,
                info.acquired_at,
                age,
                lock_path.display()
            )
        }
        None => format!(
            "{reason}. If no other Relay process is running, remove the lock file {} and \
             retry.",
            lock_path.display()
        ),
    };
    StoreError::Lock {
        path: lock_path.to_path_buf(),
        detail,
    }
}

/// Move a stale lock into `recovery/` (§8).
fn quarantine_stale(lock_path: &Path, recovery_dir: &Path, clock: &dyn Clock) -> Result<()> {
    let _ = std::fs::create_dir_all(recovery_dir);
    let stamp = clock.now_iso();
    let compact = stamp
        .chars()
        .filter(|c| *c != '-' && *c != ':' && *c != '.')
        .collect::<String>();
    let quarantine = recovery_dir.join(format!("stale-writer-lock-{compact}.json"));
    std::fs::rename(lock_path, quarantine).map_err(|error| StoreError::Lock {
        path: lock_path.to_path_buf(),
        detail: format!(
            "the lock looks stale but could not be quarantined into recovery/: {error}"
        ),
    })
}

/// PID liveness probe (§8): `kill(pid, 0)` on Unix (EPERM counts as alive),
/// the process table on Windows.
pub fn process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        use nix::sys::signal::kill;
        use nix::unistd::Pid;
        let nix_pid = Pid::from_raw(pid as i32);
        match kill(nix_pid, None) {
            Ok(()) => true,
            Err(nix::errno::Errno::EPERM) => true, // exists, not ours to signal
            Err(_) => false,
        }
    }
    #[cfg(windows)]
    {
        use sysinfo::{Pid, ProcessesToUpdate, System};
        let mut system = System::new();
        let target = Pid::from_u32(pid);
        system.refresh_processes(ProcessesToUpdate::Some(&[target]), true);
        system.process(target).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FixedClock;
    use crate::manifest::write_and_sync;

    #[test]
    fn acquires_and_releases() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let recovery = dir.path().join("recovery");
        let lock_path = dir.path().join("locks").join("writer.lock");
        let clock = FixedClock::new(1_791_878_400_000);
        assert!(!is_present(&lock_path));
        {
            let _guard = acquire(&lock_path, &recovery, Duration::from_millis(200), &clock)?;
            assert!(is_present(&lock_path));
            // A second acquisition while the first is held by our own (live)
            // pid must time out.
            let second = acquire(&lock_path, &recovery, Duration::from_millis(100), &clock);
            assert!(second.is_err());
        }
        assert!(!is_present(&lock_path));
        Ok(())
    }

    #[test]
    fn timeout_reports_holder_pid_and_age() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let recovery = dir.path().join("recovery");
        let lock_path = dir.path().join("writer.lock");
        // Hold the lock with our own (live) pid so the stale probe keeps it.
        let info = LockInfo {
            pid: std::process::id(),
            acquired_at: "2026-10-13T08:00:00.000Z".to_string(),
        };
        write_and_sync(&lock_path, &info.to_json().to_canonical_bytes())?;
        let clock = FixedClock::new(1_791_878_402_000);
        let error = acquire(&lock_path, &recovery, Duration::from_millis(60), &clock)
            .err()
            .ok_or("expected timeout")?;
        let message = error.to_string();
        assert!(
            message.contains(&format!("pid {}", std::process::id())),
            "{message}"
        );
        assert!(message.contains("held for about 2000 ms"), "{message}");
        Ok(())
    }

    #[test]
    fn stale_lock_is_quarantined_and_reacquired() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let recovery = dir.path().join("recovery");
        let lock_path = dir.path().join("writer.lock");
        // Pick a pid that is not alive; skip on machines that happen to run
        // such a high pid.
        let candidate: u32 = 4_000_000;
        if process_alive(candidate) {
            return Ok(());
        }
        let info = LockInfo {
            pid: candidate,
            acquired_at: "2026-10-13T08:00:00.000Z".to_string(),
        };
        write_and_sync(&lock_path, &info.to_json().to_canonical_bytes())?;
        let clock = FixedClock::new(1_791_878_400_000);
        let guard = acquire(&lock_path, &recovery, Duration::from_millis(500), &clock)?;
        assert!(is_present(&lock_path));
        drop(guard);
        let quarantined: Vec<String> = std::fs::read_dir(&recovery)?
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(quarantined.len(), 1);
        assert!(
            quarantined
                .first()
                .is_some_and(|n| n.starts_with("stale-writer-lock-"))
        );
        Ok(())
    }

    #[test]
    fn lock_info_roundtrip() {
        let info = LockInfo {
            pid: 12345,
            acquired_at: "2026-10-13T08:00:00.000Z".to_string(),
        };
        let text = info.to_json().to_canonical_string();
        assert_eq!(
            text,
            r#"{"acquiredAt":"2026-10-13T08:00:00.000Z","pid":12345}"#
        );
        assert_eq!(LockInfo::parse(&text), Some(info));
        assert_eq!(LockInfo::parse("not json"), None);
    }

    #[test]
    fn own_process_is_alive() {
        assert!(process_alive(std::process::id()));
    }
}
