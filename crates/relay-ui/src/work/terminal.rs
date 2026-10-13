//! Terminal restoration.
//!
//! One [`TerminalGuard`] per interactive session: it runs the caller's
//! restore function exactly once — on normal drop, on an explicit
//! [`restore_now`](TerminalGuard::restore_now) after an error, and from a
//! chained panic hook — so a panic or early return never leaves the
//! terminal in raw mode.
//!
//! The guard is backend-agnostic: the caller supplies the restore function
//! (crossterm's `disable_raw_mode` + `LeaveAlternateScreen` in the CLI, a
//! counting closure in tests). Non-interactive code paths never construct
//! one, so they cannot touch the terminal at all.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// The restore function the guard runs once.
pub type RestoreFn = Arc<dyn Fn() + Send + Sync>;

struct SharedRestore {
    restore: RestoreFn,
    done: AtomicBool,
}

impl SharedRestore {
    fn run_once(&self) {
        if !self.done.swap(true, Ordering::SeqCst) {
            (self.restore)();
        }
    }
}

/// Restores the terminal exactly once, on drop, on demand, or on panic.
pub struct TerminalGuard {
    shared: Arc<SharedRestore>,
}

impl TerminalGuard {
    /// Install the guard: registers the panic hook and returns the guard.
    /// Dropping the guard (or calling [`restore_now`](Self::restore_now))
    /// runs `restore` once; a panic in between runs it through the hook.
    pub fn new(restore: RestoreFn) -> Self {
        let shared = Arc::new(SharedRestore {
            restore,
            done: AtomicBool::new(false),
        });
        install_panic_hook(Arc::clone(&shared));
        TerminalGuard { shared }
    }

    /// Run the restore now (idempotent).
    pub fn restore_now(&self) {
        self.shared.run_once();
    }

    /// Whether the restore already ran.
    pub fn restored(&self) -> bool {
        self.shared.done.load(Ordering::SeqCst)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        self.shared.run_once();
    }
}

/// Chain a panic hook that restores before the previous hook runs. Hooks
/// are process-global; each guard chains onto whatever was installed, and
/// the restore is idempotent so nested guards are harmless.
fn install_panic_hook(shared: Arc<SharedRestore>) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        shared.run_once();
        previous(info);
    }));
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)] // test-only, house style
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn restores_exactly_once_on_drop() {
        let count = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&count);
        {
            let _guard = TerminalGuard::new(Arc::new(move || {
                counter.fetch_add(1, Ordering::SeqCst);
            }));
            assert!(!_guard.restored());
        }
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn explicit_restore_is_idempotent() {
        let count = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&count);
        let guard = TerminalGuard::new(Arc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        }));
        guard.restore_now();
        guard.restore_now();
        assert!(guard.restored());
        drop(guard);
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn restores_on_panic() {
        let count = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&count);
        // Silence the panic message during the probe.
        std::panic::set_hook(Box::new(|_| {}));
        let result = {
            let _guard = TerminalGuard::new(Arc::new(move || {
                counter.fetch_add(1, Ordering::SeqCst);
            }));
            std::panic::catch_unwind(|| {
                panic!("probe");
            })
        };
        drop(result);
        assert_eq!(count.load(Ordering::SeqCst), 1);
        // Restore a quiet default so later failures still print.
        std::panic::set_hook(Box::new(|_| {}));
    }
}
