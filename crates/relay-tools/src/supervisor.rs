//! Process-tree supervision behind one platform-split contract.
//!
//! Unix: the child is spawned into its own process group (`setpgid` via the
//! `CommandExt::process_group` extension) so termination signals reach the
//! whole tree (`kill(-pgid, ...)` through the safe `nix` wrapper).
//!
//! Windows: the child is assigned to a Job Object with kill-on-job-close (via
//! the safe `win32job` wrapper), so the tree dies when the job handle closes —
//! even if Relay itself dies. Graceful termination posts a close request to
//! the tree (`taskkill /T` without `/F`); forced termination closes the job.
//!
//! In both cases termination is graceful first, then forced after a bounded
//! interval ([`GRACE_INTERVAL`], matching the reference escalation timer).
//! The trait keeps the platform details behind one contract; `unsafe` is
//! forbidden by workspace lints and none is used. The Windows implementation
//! is compiled only on Windows and exercised by the platform CI matrix.

use std::process::Child;
use std::process::Command;
use std::time::Duration;

/// How long a graceful termination signal is allowed to take effect before
/// escalation to forced termination.
pub const GRACE_INTERVAL: Duration = Duration::from_millis(2_000);

/// Platform process-tree supervisor for one spawned child.
pub trait ProcessSupervisor: Send {
    /// Prepare a command for tree-scoped supervision before spawning.
    fn configure(command: &mut Command);

    /// Wrap a freshly spawned child (borrowed: the caller keeps ownership so
    /// it can still reap the exit status).
    fn adopt(child: &mut Child) -> Self
    where
        Self: Sized;

    /// Ask the whole tree to shut down (SIGTERM / close request).
    fn terminate_graceful(&mut self);

    /// Kill the whole tree unconditionally.
    fn terminate_forced(&mut self);
}

/// Unix supervisor: process group signaled with SIGTERM then SIGKILL.
#[cfg(unix)]
pub struct UnixProcessGroup {
    pid: i32,
}

#[cfg(unix)]
impl ProcessSupervisor for UnixProcessGroup {
    fn configure(command: &mut Command) {
        use std::os::unix::process::CommandExt;
        // Own process group (setpgid(0, 0)) so signals to -pgid reach every
        // descendant, including shells the child spawns.
        command.process_group(0);
    }

    fn adopt(child: &mut Child) -> Self {
        UnixProcessGroup {
            pid: child.id() as i32,
        }
    }

    fn terminate_graceful(&mut self) {
        kill_process_group(self.pid, nix::sys::signal::Signal::SIGTERM);
    }

    fn terminate_forced(&mut self) {
        kill_process_group(self.pid, nix::sys::signal::Signal::SIGKILL);
    }
}

/// Signal the child's whole process group, falling back to the child itself.
#[cfg(unix)]
fn kill_process_group(pid: i32, signal: nix::sys::signal::Signal) {
    use nix::sys::signal::{kill, killpg};
    use nix::unistd::Pid;
    let pgid = Pid::from_raw(pid);
    if killpg(pgid, signal).is_err() {
        // Group already gone: try the child directly (also a no-op if exited).
        let _ = kill(pgid, signal);
    }
}

/// Windows supervisor: Job Object with kill-on-job-close.
#[cfg(windows)]
pub struct WindowsJobObject {
    pid: u32,
    job: Option<win32job::Job>,
}

#[cfg(windows)]
impl WindowsJobObject {
    fn close_job(&mut self) {
        // Dropping the job closes its handle; JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
        // then terminates every process in the job, including descendants.
        self.job = None;
    }
}

#[cfg(windows)]
impl ProcessSupervisor for WindowsJobObject {
    fn configure(_command: &mut Command) {
        // Nothing to prepare: the job is assigned right after spawn, and
        // descendants inherit job membership automatically.
    }

    fn adopt(child: &mut Child) -> Self {
        use std::os::windows::io::AsRawHandle;
        let job = (|| -> Option<win32job::Job> {
            let mut info = win32job::ExtendedLimitInfo::new();
            info.limit_kill_on_job_close();
            let job = win32job::Job::create_with_limit_info(&info).ok()?;
            job.assign_process(child.as_raw_handle() as isize).ok()?;
            Some(job)
        })();
        WindowsJobObject {
            pid: child.id(),
            job,
        }
    }

    fn terminate_graceful(&mut self) {
        // Post close requests to the tree without forcing. Console children
        // without a message loop survive this; the grace interval escalates.
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &self.pid.to_string(), "/T"])
            .status();
    }

    fn terminate_forced(&mut self) {
        self.close_job();
    }
}

#[cfg(all(test, unix))]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use std::process::Stdio;

    #[test]
    fn process_group_termination_kills_child_and_grandchild() {
        let marker = tempfile::Builder::new()
            .prefix("relay-supervisor-")
            .tempdir()
            .expect("temp dir");
        let grandchild_log = marker.path().join("grandchild.log");
        let mut command = Command::new("/bin/sh");
        command
            .args([
                "-c",
                &format!(
                    "/bin/sh -c 'while true; do echo alive >> {:?}; sleep 0.05; done' &\necho started\nwait",
                    grandchild_log
                ),
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        UnixProcessGroup::configure(&mut command);
        let mut child = command.spawn().expect("spawn sh");
        let mut supervisor = UnixProcessGroup::adopt(&mut child);
        std::thread::sleep(Duration::from_millis(300));
        supervisor.terminate_graceful();
        std::thread::sleep(Duration::from_millis(200));
        let size_before = std::fs::metadata(&grandchild_log)
            .map(|info| info.len())
            .unwrap_or(0);
        std::thread::sleep(Duration::from_millis(400));
        let size_after = std::fs::metadata(&grandchild_log)
            .map(|info| info.len())
            .unwrap_or(0);
        assert!(
            size_before > 0,
            "grandchild should have been running before the kill"
        );
        assert_eq!(
            size_before, size_after,
            "grandchild must die with the process group"
        );
        supervisor.terminate_forced();
        let _ = child.wait();
    }
}
