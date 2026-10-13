//! Fault-injection hooks for crash-recovery tests (phase 2 item 9).
//!
//! Storage operations consult a [`FaultInjector`] at defined points. When a
//! injector reports a crash at a point, the store performs the effect that
//! the point's name describes (e.g. `AfterPartialWrite` writes only half
//! the append buffer) and then returns
//! [`StoreError::SimulatedCrash`](crate::StoreError::SimulatedCrash) —
//! modelling process death without killing anything. Tests reopen the
//! store and assert the recovery contract: no event reported durable is
//! lost, and SQLite never advances beyond validated JSONL.

use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

/// Points along the append/reconcile path where a crash can be simulated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FaultPoint {
    /// Right after the writer lock is acquired; nothing else happened.
    LockAcquired,
    /// After the whole batch validated, before any bytes are written.
    BatchValidated,
    /// Immediately before the append buffer is written.
    BeforeWrite,
    /// Half the append buffer was written; no flush, no fsync.
    AfterPartialWrite,
    /// The full buffer was written; no flush, no fsync.
    AfterWriteNoSync,
    /// The append was flushed and fsynced (durable), before rotation and
    /// projection. An event durable here must survive every crash after it.
    AfterDurableAppend,
    /// Rotation (rename + fresh active segment + manifest) completed.
    AfterRotation,
    /// One chunk of a suffix replay / rebuild committed inside SQLite;
    /// more chunks (and possibly the final checkpoint) are still missing.
    ProjectionChunkCommitted,
    /// The projection transaction (events + derived rows + checkpoint)
    /// committed.
    ProjectionCommitted,
}

impl FaultPoint {
    /// Stable identifier used in [`crate::StoreError::SimulatedCrash`].
    pub fn name(&self) -> &'static str {
        match self {
            FaultPoint::LockAcquired => "LockAcquired",
            FaultPoint::BatchValidated => "BatchValidated",
            FaultPoint::BeforeWrite => "BeforeWrite",
            FaultPoint::AfterPartialWrite => "AfterPartialWrite",
            FaultPoint::AfterWriteNoSync => "AfterWriteNoSync",
            FaultPoint::AfterDurableAppend => "AfterDurableAppend",
            FaultPoint::AfterRotation => "AfterRotation",
            FaultPoint::ProjectionChunkCommitted => "ProjectionChunkCommitted",
            FaultPoint::ProjectionCommitted => "ProjectionCommitted",
        }
    }
}

/// Consulted by the store at each fault point.
pub trait FaultInjector: Send + Sync {
    /// Whether the store should simulate a crash at `point` (after applying
    /// the effect the point describes).
    fn should_crash(&self, point: FaultPoint) -> bool;
}

/// The default injector: never crashes.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoFaults;

impl FaultInjector for NoFaults {
    fn should_crash(&self, _point: FaultPoint) -> bool {
        false
    }
}

/// One-shot injector for tests: crashes once at the armed point.
#[derive(Debug)]
pub struct CrashAt {
    point: FaultPoint,
    fired: AtomicBool,
}

impl CrashAt {
    /// Arm a crash at `point`.
    pub fn new(point: FaultPoint) -> CrashAt {
        CrashAt {
            point,
            fired: AtomicBool::new(false),
        }
    }
}

impl FaultInjector for CrashAt {
    fn should_crash(&self, point: FaultPoint) -> bool {
        point == self.point && !self.fired.swap(true, Ordering::SeqCst)
    }
}
