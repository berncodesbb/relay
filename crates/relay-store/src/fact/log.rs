//! The segmented JSONL fact log (`docs/plans/fact-format-proposal.md`).
//!
//! [`FactLog`] owns JSONL only — no SQLite, no locking; the orchestrator
//! ([`crate::fact::FactStore`]) wraps it with the writer lock and the
//! projection. It reuses the same machinery as the legacy event log —
//! segment naming/discovery/tiling/rotation ([`crate::segments`]), the
//! manifest's atomic write, the conservative tail recovery, and the
//! crash-injection hooks ([`crate::fault`]) — but is parameterized over the
//! opaque [`FactRecord`] and never touches relay-core event types.
//!
//! The validated stream tail carries the last checksum so chained streams
//! can continue across processes.

use std::path::PathBuf;

use crate::clock::Clock;
use crate::error::Result;
use crate::error::StoreError;
use crate::fact::manifest::FactManifest;
use crate::fact::paths::FactPaths;
use crate::fact::record::FactEntry;
use crate::fact::record::FactRecord;
use crate::fact::record::Integrity;
use crate::fact::validator::FactValidator;
use crate::fault::FaultInjector;
use crate::fault::FaultPoint;
use crate::log::Durability;
use crate::log::Lines;
use crate::log::OpenMode;
use crate::log::RecoveredTail;
use crate::log::line_count;
use crate::log::split_complete;
use crate::log::truncate_file;
use crate::log::utf8_line;
use crate::log::write_append;
use crate::log::write_append_raw;
use crate::manifest::fsync_dir_best_effort;
use crate::manifest::write_and_sync;
use crate::segments::SegmentListing;
use crate::segments::SegmentName;
use crate::segments::should_rotate;

/// Tail of the fact log after validation.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FactTail {
    /// Highest sequence present (0 for an empty log).
    pub last_sequence: u64,
    /// Checksum of that record (`None` for an empty log). Used to continue
    /// a chained stream and to reconcile the projection.
    pub last_checksum: Option<String>,
    /// Records currently in the active segment.
    pub active_records: u64,
    /// Current byte size of the active segment file.
    pub active_bytes: u64,
}

/// Incremental scan state shared by `open`, `refresh`, and `read_all`.
#[derive(Default)]
struct ScanState {
    expected_sequence: u64,
    previous_checksum: Option<String>,
    active_records: u64,
    active_bytes: u64,
}

impl ScanState {
    fn new(start_sequence: u64, previous_checksum: Option<String>) -> ScanState {
        ScanState {
            expected_sequence: start_sequence,
            previous_checksum,
            ..ScanState::default()
        }
    }

    /// Validate one physical line and advance the scan state.
    #[allow(clippy::too_many_arguments)]
    fn accept(
        &mut self,
        text: &str,
        segment: &str,
        line: u64,
        chained: bool,
        validator: &dyn FactValidator,
        is_active: bool,
    ) -> Result<FactRecord> {
        let record = FactRecord::parse(text, segment, line)?;
        if record.sequence != self.expected_sequence {
            return Err(StoreError::FactRecord {
                segment: segment.to_string(),
                line,
                sequence: Some(record.sequence),
                reason: format!(
                    "sequence must be contiguous: expected {}, found {} (the log may be \
                     truncated or reordered)",
                    self.expected_sequence, record.sequence
                ),
            });
        }
        if chained {
            let expected = if record.sequence == 1 {
                None
            } else {
                self.previous_checksum.clone()
            };
            if record.previous != expected {
                return Err(StoreError::ChainBreak {
                    segment: segment.to_string(),
                    line,
                    sequence: record.sequence,
                    expected: expected.unwrap_or_else(|| "null".to_string()),
                    actual: record
                        .previous
                        .clone()
                        .unwrap_or_else(|| "null".to_string()),
                    hash_field: false,
                });
            }
        } else if record.previous.is_some() {
            return Err(StoreError::FactRecord {
                segment: segment.to_string(),
                line,
                sequence: Some(record.sequence),
                reason: "the record carries a \"previous\" chain link but this stream records \
                         integrity \"checksums\" (no chaining)"
                    .to_string(),
            });
        }
        if let Err(reason) = validator.validate(&record) {
            return Err(crate::fact::validator::located_rejection(
                segment,
                line,
                Some(record.sequence),
                reason,
            ));
        }
        self.expected_sequence += 1;
        self.previous_checksum = Some(record.checksum.clone());
        if is_active {
            self.active_records += 1;
        }
        Ok(record)
    }
}

/// The segmented fact log.
#[derive(Debug)]
pub struct FactLog {
    paths: FactPaths,
    manifest: FactManifest,
    listing: SegmentListing,
    tail: FactTail,
}

impl FactLog {
    /// Create a brand-new fact store: layout, empty active segment,
    /// manifest. Fails if a manifest already exists (open it instead).
    pub fn init(
        paths: &FactPaths,
        stream_id: &str,
        integrity: Integrity,
        clock: &dyn Clock,
    ) -> Result<FactLog> {
        if paths.manifest.exists() {
            return Err(StoreError::Invalid {
                reason: format!(
                    "a fact store already exists at {}. Open it instead of creating a new one.",
                    paths.root.display()
                ),
            });
        }
        paths.ensure_layout().map_err(|error| StoreError::Io {
            context: "creating the fact store layout".to_string(),
            path: paths.root.clone(),
            source: error,
        })?;
        let active = SegmentName::Open { first: 1 };
        let active_path = paths.facts_dir.join(active.file_name());
        std::fs::File::create(&active_path).map_err(|error| StoreError::Io {
            context: "creating the initial active segment".to_string(),
            path: active_path,
            source: error,
        })?;
        fsync_dir_best_effort(&paths.facts_dir);
        let manifest =
            FactManifest::new(stream_id, integrity, clock.now_iso(), active.file_name())?;
        manifest.write_atomic(&paths.manifest)?;
        Ok(FactLog {
            paths: paths.clone(),
            manifest,
            listing: SegmentListing {
                closed: vec![],
                active: Some(active),
                unknown_files: vec![],
            },
            tail: FactTail::default(),
        })
    }

    /// Open and fully validate the log. In [`OpenMode::Writer`] an
    /// incomplete final tail of the active segment is quarantined into
    /// `recovery/` and the file truncated; in [`OpenMode::Reader`] it is
    /// ignored (consistent-prefix read). The integrity level is read from
    /// the manifest, and every record passes the domain gate.
    pub fn open(
        paths: &FactPaths,
        mode: OpenMode,
        clock: &dyn Clock,
        validator: &dyn FactValidator,
    ) -> Result<(FactLog, Vec<RecoveredTail>)> {
        let manifest = FactManifest::read(&paths.manifest)?;
        let mut listing = crate::segments::discover(&paths.facts_dir)?;
        crate::segments::validate_tiling(&listing)?;
        let mut recoveries: Vec<RecoveredTail> = Vec::new();

        // Derive the active segment from tiling when it is absent (a
        // rotation crash before the fresh active file was created).
        if listing.active.is_none() && !listing.closed.is_empty() {
            let next = match listing.closed.last() {
                Some(SegmentName::Closed { last, .. }) => *last + 1,
                _ => 1,
            };
            listing.active = Some(SegmentName::Open { first: next });
        }

        let chained = manifest.integrity == Integrity::Chained;
        let mut scan = ScanState::new(1, None);
        for segment in listing.closed.iter().chain(listing.active.iter()) {
            let is_active = matches!(segment, SegmentName::Open { .. });
            let name = segment.file_name();
            let path = paths.facts_dir.join(&name);
            let bytes = read_segment(&path, mode, is_active, &paths.facts_dir)?;
            let (complete, incomplete) = split_complete(&bytes);
            if !is_active {
                if incomplete.is_some_and(|tail| !tail.is_empty()) {
                    return Err(StoreError::FactRecord {
                        segment: name,
                        line: line_count(complete) + 1,
                        sequence: None,
                        reason: "closed segment does not end with a newline (incomplete final \
                                 line); only the active segment may carry a crash tail"
                            .to_string(),
                    });
                }
                if let SegmentName::Closed { first, last } = segment {
                    let expected = last - first + 1;
                    let actual = line_count(complete);
                    if actual != expected {
                        return Err(StoreError::Segments {
                            reason: format!(
                                "closed segment {name} holds {actual} records but its name \
                                 promises {expected}"
                            ),
                        });
                    }
                }
            } else if let Some(tail_bytes) = incomplete.filter(|tail| !tail.is_empty())
                && mode == OpenMode::Writer
            {
                let quarantine = quarantine_tail(paths, &name, tail_bytes, clock)?;
                truncate_file(&path, complete.len() as u64)?;
                recoveries.push(RecoveredTail {
                    quarantine_path: quarantine,
                    bytes: tail_bytes.len() as u64,
                    segment: name.clone(),
                });
                // Reader mode: an append in progress; the tail is ignored.
            }
            let mut line_number: u64 = 0;
            for line in Lines::new(complete) {
                line_number += 1;
                let text = utf8_line(line, &name, line_number)?;
                scan.accept(&text, &name, line_number, chained, validator, is_active)?;
            }
            if is_active {
                scan.active_bytes = complete.len() as u64;
            }
        }

        let mut opened = FactLog {
            paths: paths.clone(),
            manifest,
            listing,
            tail: FactTail {
                last_sequence: scan.expected_sequence.saturating_sub(1),
                last_checksum: scan.previous_checksum,
                active_records: scan.active_records,
                active_bytes: scan.active_bytes,
            },
        };
        if mode == OpenMode::Writer {
            opened.repair_manifest()?;
        }
        Ok((opened, recoveries))
    }

    /// Re-derive the tail after acquiring the writer lock (another writer
    /// may have appended or rotated meanwhile). New closed segments and the
    /// active segment are fully revalidated; an incomplete crash tail is
    /// recovered exactly like `open` does.
    pub fn refresh(&mut self, clock: &dyn Clock, validator: &dyn FactValidator) -> Result<()> {
        let listing = crate::segments::discover(&self.paths.facts_dir)?;
        crate::segments::validate_tiling(&listing)?;
        let known_closed = self.listing.closed.len();
        if listing.closed.len() < known_closed {
            return Err(StoreError::Segments {
                reason: "closed segments disappeared while the lock was released; the store \
                         directory changed unexpectedly"
                    .to_string(),
            });
        }
        let mut listing = listing;
        if listing.active.is_none() {
            let next = match listing.closed.last() {
                Some(SegmentName::Closed { last, .. }) => *last + 1,
                _ => 1,
            };
            listing.active = Some(SegmentName::Open { first: next });
        }
        let chained = self.manifest.integrity == Integrity::Chained;
        let known_last = self.tail.last_sequence;
        let mut scan = ScanState::new(self.tail.last_sequence + 1, self.tail.last_checksum.clone());
        for segment in listing
            .closed
            .iter()
            .skip(known_closed)
            .chain(listing.active.iter())
        {
            let is_active = matches!(segment, SegmentName::Open { .. });
            let name = segment.file_name();
            let path = self.paths.facts_dir.join(&name);
            let bytes = read_segment(&path, OpenMode::Writer, is_active, &self.paths.facts_dir)?;
            let (complete, incomplete) = split_complete(&bytes);
            if !is_active && incomplete.is_some_and(|tail| !tail.is_empty()) {
                return Err(StoreError::FactRecord {
                    segment: name,
                    line: line_count(complete) + 1,
                    sequence: None,
                    reason: "closed segment does not end with a newline".to_string(),
                });
            }
            if let Some(tail_bytes) = incomplete.filter(|tail| !tail.is_empty())
                && is_active
            {
                quarantine_tail(&self.paths, &name, tail_bytes, clock)?;
                truncate_file(&path, complete.len() as u64)?;
            }
            // Records at or below `known_last` were validated by this process
            // (or by an immutable close); the active segment only grows.
            let skip_lines = if is_active {
                let active_first = segment.first();
                known_last.saturating_sub(active_first.saturating_sub(1))
            } else {
                0
            };
            let mut line_number = skip_lines;
            for line in Lines::new(complete).skip(skip_lines as usize) {
                line_number += 1;
                let text = utf8_line(line, &name, line_number)?;
                scan.accept(&text, &name, line_number, chained, validator, is_active)?;
            }
            if is_active {
                scan.active_records += skip_lines;
                scan.active_bytes = complete.len() as u64;
            }
        }
        self.listing = listing;
        self.tail = FactTail {
            last_sequence: scan.expected_sequence.saturating_sub(1),
            last_checksum: scan.previous_checksum,
            active_records: scan.active_records,
            active_bytes: scan.active_bytes,
        };
        self.repair_manifest()
    }

    /// Align the manifest with the discovered active segment (writer mode).
    fn repair_manifest(&mut self) -> Result<()> {
        let active_name = self
            .listing
            .active
            .as_ref()
            .map(|segment| segment.file_name())
            .unwrap_or_default();
        if !active_name.is_empty() && self.manifest.active_segment != active_name {
            self.manifest.active_segment = active_name;
            self.manifest.write_atomic(&self.paths.manifest)?;
        }
        Ok(())
    }

    /// Read every validated record in sequence order, re-verifying each
    /// line. A concurrent rotation may remove a listed file between
    /// discovery and read; the read re-discovers and retries once.
    pub fn read_all(&self, validator: &dyn FactValidator) -> Result<Vec<FactEntry>> {
        let listing = crate::segments::discover(&self.paths.facts_dir)?;
        match self.read_pass(&listing, validator) {
            Err(StoreError::Io {
                context, source, ..
            }) if context == "reading a segment"
                && source.kind() == std::io::ErrorKind::NotFound =>
            {
                let retry = crate::segments::discover(&self.paths.facts_dir)?;
                self.read_pass(&retry, validator)
            }
            other => other,
        }
    }

    fn read_pass(
        &self,
        listing: &SegmentListing,
        validator: &dyn FactValidator,
    ) -> Result<Vec<FactEntry>> {
        let chained = self.manifest.integrity == Integrity::Chained;
        let mut scan = ScanState::new(1, None);
        let mut entries = Vec::new();
        for segment in listing.closed.iter().chain(listing.active.iter()) {
            let is_active = matches!(segment, SegmentName::Open { .. });
            let name = segment.file_name();
            let path = self.paths.facts_dir.join(&name);
            let bytes = match std::fs::read(&path) {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound && is_active => {
                    continue;
                }
                Err(error) => {
                    return Err(StoreError::Io {
                        context: "reading a segment".to_string(),
                        path,
                        source: error,
                    });
                }
            };
            let (complete, _incomplete) = split_complete(&bytes);
            let mut line_number = 0;
            for line in Lines::new(complete) {
                line_number += 1;
                let text = utf8_line(line, &name, line_number)?;
                let record =
                    scan.accept(&text, &name, line_number, chained, validator, is_active)?;
                entries.push(FactEntry {
                    record,
                    segment: name.clone(),
                    line: line_number,
                });
            }
        }
        Ok(entries)
    }

    /// The validated tail.
    pub fn tail(&self) -> &FactTail {
        &self.tail
    }

    /// The store manifest.
    pub fn manifest(&self) -> &FactManifest {
        &self.manifest
    }

    /// The integrity level this stream records.
    pub fn integrity(&self) -> Integrity {
        self.manifest.integrity
    }

    /// Unknown (non-segment) files seen during the last discovery.
    pub fn unknown_files(&self) -> &[String] {
        &self.listing.unknown_files
    }

    /// Append one batch as a single buffered write, then rotate if the
    /// active segment crossed a threshold. The caller holds the writer lock
    /// and has refreshed the tail; the records are already sealed, chained,
    /// and validated. Returns whether a rotation happened.
    pub fn append(
        &mut self,
        records: &[FactRecord],
        durability: Durability,
        fault: &dyn FaultInjector,
    ) -> Result<bool> {
        if records.is_empty() {
            return Ok(false);
        }
        if let Some(first) = records.first()
            && first.sequence != self.tail.last_sequence + 1
        {
            return Err(StoreError::Append {
                reason: format!(
                    "the batch starts at sequence {} but the log tail is at sequence {}",
                    first.sequence, self.tail.last_sequence
                ),
            });
        }
        let active_path = self.paths.facts_dir.join(&self.manifest.active_segment);
        let mut buffer = Vec::new();
        for record in records {
            buffer.extend_from_slice(&record.to_line()?);
        }
        if fault.should_crash(FaultPoint::AfterPartialWrite) {
            let partial = (buffer.len() / 2).clamp(1, buffer.len());
            write_append(&active_path, &buffer[..partial], Durability::Relaxed)?;
            return Err(StoreError::SimulatedCrash {
                point: FaultPoint::AfterPartialWrite.name().to_string(),
            });
        }
        if fault.should_crash(FaultPoint::AfterWriteNoSync) {
            write_append_raw(&active_path, &buffer)?;
            return Err(StoreError::SimulatedCrash {
                point: FaultPoint::AfterWriteNoSync.name().to_string(),
            });
        }
        write_append(&active_path, &buffer, durability)?;
        if let Some(last) = records.last() {
            self.tail.last_sequence = last.sequence;
            self.tail.last_checksum = Some(last.checksum.clone());
        }
        self.tail.active_records += records.len() as u64;
        self.tail.active_bytes += buffer.len() as u64;
        if fault.should_crash(FaultPoint::AfterDurableAppend) {
            return Err(StoreError::SimulatedCrash {
                point: FaultPoint::AfterDurableAppend.name().to_string(),
            });
        }
        if should_rotate(self.tail.active_records, self.tail.active_bytes) {
            self.rotate()?;
            if fault.should_crash(FaultPoint::AfterRotation) {
                return Err(StoreError::SimulatedCrash {
                    point: FaultPoint::AfterRotation.name().to_string(),
                });
            }
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Rotate: close the active segment under its final name, create the
    /// fresh active segment, rewrite the manifest atomically.
    fn rotate(&mut self) -> Result<()> {
        let first = match self.listing.active.as_ref() {
            Some(SegmentName::Open { first }) => *first,
            _ => self.tail.last_sequence + 1,
        };
        let last = self.tail.last_sequence;
        let active = self
            .listing
            .active
            .clone()
            .unwrap_or(SegmentName::Open { first });
        let active_path = self.paths.facts_dir.join(active.file_name());
        let closed = SegmentName::Closed { first, last };
        let closed_path = self.paths.facts_dir.join(closed.file_name());
        std::fs::rename(&active_path, &closed_path).map_err(|error| StoreError::Io {
            context: "rotating the active segment".to_string(),
            path: active_path,
            source: error,
        })?;
        fsync_dir_best_effort(&self.paths.facts_dir);
        let fresh = SegmentName::Open { first: last + 1 };
        let fresh_path = self.paths.facts_dir.join(fresh.file_name());
        std::fs::File::create(&fresh_path).map_err(|error| StoreError::Io {
            context: "creating the fresh active segment".to_string(),
            path: fresh_path,
            source: error,
        })?;
        fsync_dir_best_effort(&self.paths.facts_dir);
        self.manifest.active_segment = fresh.file_name();
        self.manifest.write_atomic(&self.paths.manifest)?;
        self.listing.closed.push(closed);
        self.listing.active = Some(fresh);
        self.tail.active_records = 0;
        self.tail.active_bytes = 0;
        Ok(())
    }
}

/// Read a segment file, recreating a missing active segment in writer mode.
fn read_segment(
    path: &std::path::Path,
    mode: OpenMode,
    is_active: bool,
    facts_dir: &std::path::Path,
) -> Result<Vec<u8>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && is_active => {
            if mode == OpenMode::Writer {
                std::fs::File::create(path).map_err(|error| StoreError::Io {
                    context: "recreating the missing active segment".to_string(),
                    path: path.to_path_buf(),
                    source: error,
                })?;
                fsync_dir_best_effort(facts_dir);
            }
            Ok(Vec::new())
        }
        Err(error) => Err(StoreError::Io {
            context: "reading a segment".to_string(),
            path: path.to_path_buf(),
            source: error,
        }),
    }
}

/// Copy the incomplete tail into `recovery/rejected-fact-tail-<stamp>Z.jsonl`.
fn quarantine_tail(
    paths: &FactPaths,
    segment: &str,
    tail: &[u8],
    clock: &dyn Clock,
) -> Result<PathBuf> {
    std::fs::create_dir_all(&paths.recovery_dir).map_err(|error| StoreError::Io {
        context: "creating the recovery directory".to_string(),
        path: paths.recovery_dir.clone(),
        source: error,
    })?;
    let stamp: String = clock
        .now_iso()
        .chars()
        .filter(|c| *c != '-' && *c != ':' && *c != '.')
        .collect();
    let base = format!("rejected-fact-tail-{stamp}Z");
    let mut candidate = paths.recovery_dir.join(format!("{base}.jsonl"));
    let mut attempt = 1;
    while candidate.exists() && attempt < 100 {
        candidate = paths.recovery_dir.join(format!("{base}-{attempt}.jsonl"));
        attempt += 1;
    }
    let mut contents = format!("-- rejected fact tail from {segment}\n").into_bytes();
    contents.extend_from_slice(tail);
    contents.push(b'\n');
    write_and_sync(&candidate, &contents)?;
    Ok(candidate)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canonical::Json;
    use crate::clock::FixedClock;
    use crate::fact::validator::AcceptAllFacts;
    use crate::fault::NoFaults;

    const STREAM: &str = "prj_00000000-0000-4000-8000-000000000001";

    fn clock() -> FixedClock {
        FixedClock::new(1_791_878_400_000)
    }

    fn input(n: u64) -> crate::fact::record::FactInput {
        crate::fact::record::FactInput::new(
            "goal.declared",
            1,
            Json::object([("n", Json::UInt(n))]),
        )
    }

    fn seal_from(
        chain_start: u64,
        previous: Option<String>,
        count: u64,
    ) -> Result<Vec<FactRecord>> {
        seal_batch(chain_start, previous, count, false)
    }

    fn chained_seal_from(
        chain_start: u64,
        previous: Option<String>,
        count: u64,
    ) -> Result<Vec<FactRecord>> {
        seal_batch(chain_start, previous, count, true)
    }

    /// Seal a batch, chaining within it only when the stream is chained
    /// (checksum-only streams carry `previous: null` on every record).
    fn seal_batch(
        chain_start: u64,
        previous: Option<String>,
        count: u64,
        chained: bool,
    ) -> Result<Vec<FactRecord>> {
        let mut previous = previous;
        let mut batch = Vec::new();
        for offset in 0..count {
            let sequence = chain_start + offset;
            let link = if chained { previous.clone() } else { None };
            let record =
                FactRecord::seal(&input(sequence), sequence, "2026-10-13T08:00:00.000Z", link)?;
            if chained {
                previous = Some(record.checksum.clone());
            }
            batch.push(record);
        }
        Ok(batch)
    }

    #[test]
    fn init_append_read_roundtrip() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let paths = FactPaths::new(dir.path());
        let mut log = FactLog::init(&paths, STREAM, Integrity::Checksums, &clock())?;
        let batch = seal_from(1, None, 3)?;
        assert!(!log.append(&batch, Durability::Durable, &NoFaults)?);
        let entries = log.read_all(&AcceptAllFacts)?;
        assert_eq!(entries.len(), 3);
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.record.sequence)
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert!(entries.iter().all(|entry| entry.record.previous.is_none()));
        assert_eq!(log.tail().last_sequence, 3);
        assert_eq!(
            log.tail().last_checksum.as_deref(),
            batch.last().map(|r| r.checksum.as_str())
        );
        // The physical layout lives beside the legacy store root.
        assert!(paths.facts_dir.join("0000000000000001-open.jsonl").exists());
        let (reopened, recoveries) =
            FactLog::open(&paths, OpenMode::Writer, &clock(), &AcceptAllFacts)?;
        assert!(recoveries.is_empty());
        assert_eq!(reopened.tail().last_sequence, 3);
        Ok(())
    }

    #[test]
    fn chained_stream_links_across_batches() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let paths = FactPaths::new(dir.path());
        let mut log = FactLog::init(&paths, STREAM, Integrity::Chained, &clock())?;
        let first = chained_seal_from(1, None, 2)?;
        log.append(&first, Durability::Durable, &NoFaults)?;
        let tail_checksum = log.tail().last_checksum.clone();
        let second = chained_seal_from(3, tail_checksum, 2)?;
        log.append(&second, Durability::Durable, &NoFaults)?;
        let entries = log.read_all(&AcceptAllFacts)?;
        assert_eq!(entries.len(), 4);
        assert_eq!(entries[0].record.previous, None);
        for window in entries.windows(2) {
            assert_eq!(
                window[1].record.previous.as_deref(),
                Some(window[0].record.checksum.as_str()),
                "chain must link consecutive records"
            );
        }
        // Tampering with the middle record is caught by its own checksum
        // before the chain is even consulted.
        let active = paths.facts_dir.join("0000000000000001-open.jsonl");
        let text = std::fs::read_to_string(&active)?;
        let doctored = text.replace("\"n\":2", "\"n\":9");
        std::fs::write(&active, doctored)?;
        let error = FactLog::open(&paths, OpenMode::Writer, &clock(), &AcceptAllFacts)
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(error.contains("checksum mismatch"), "{error}");
        Ok(())
    }

    #[test]
    fn chain_break_detected_when_a_record_is_removed() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let paths = FactPaths::new(dir.path());
        {
            let mut log = FactLog::init(&paths, STREAM, Integrity::Chained, &clock())?;
            let batch = chained_seal_from(1, None, 3)?;
            log.append(&batch, Durability::Durable, &NoFaults)?;
        }
        // Drop the middle line and re-link the last record onto the first.
        let active = paths.facts_dir.join("0000000000000001-open.jsonl");
        let lines: Vec<String> = std::fs::read_to_string(&active)?
            .trim_end()
            .split('\n')
            .map(str::to_string)
            .collect();
        let first = crate::fact::canonical::parse(&lines[0])?;
        let mut last = crate::fact::canonical::parse(&lines[2])?;
        if let Json::Object(map) = &mut last {
            map.insert(
                "previous".to_string(),
                first.get("checksum").cloned().unwrap_or(Json::Null),
            );
        }
        // Recompute the checksum over the tampered envelope.
        let mut hash_input = last.clone();
        if let Json::Object(map) = &mut hash_input {
            map.remove("checksum");
        }
        let new_checksum = {
            let record_for_hash = FactRecord {
                sequence: 3,
                fact_type: "goal.declared".to_string(),
                fact_version: 1,
                payload: last.get("payload").cloned().unwrap_or(Json::Null),
                recorded_at: last
                    .get("recordedAt")
                    .and_then(Json::as_str)
                    .unwrap_or_default()
                    .to_string(),
                previous: last
                    .get("previous")
                    .and_then(Json::as_str)
                    .map(str::to_string),
                checksum: String::new(),
            };
            record_for_hash.compute_checksum()?
        };
        if let Json::Object(map) = &mut last {
            map.insert("checksum".to_string(), Json::Str(new_checksum));
        }
        let rewritten = format!(
            "{}\n{}\n",
            lines[0],
            crate::fact::canonical::serialize(&last)?
        );
        std::fs::write(&active, rewritten)?;
        let error = FactLog::open(&paths, OpenMode::Writer, &clock(), &AcceptAllFacts)
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(error.contains("sequence must be contiguous"), "{error}");
        Ok(())
    }

    #[test]
    fn reader_ignores_crash_tail_writer_recovers() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let paths = FactPaths::new(dir.path());
        {
            let mut log = FactLog::init(&paths, STREAM, Integrity::Checksums, &clock())?;
            let batch = seal_from(1, None, 1)?;
            log.append(&batch, Durability::Durable, &NoFaults)?;
        }
        let active = paths.facts_dir.join("0000000000000001-open.jsonl");
        std::fs::write(
            &active,
            format!(
                "{}{{\"sequence\":2,\"trunc",
                std::fs::read_to_string(&active)?
            ),
        )?;
        let (reader, recoveries) =
            FactLog::open(&paths, OpenMode::Reader, &clock(), &AcceptAllFacts)?;
        assert!(recoveries.is_empty());
        assert_eq!(reader.read_all(&AcceptAllFacts)?.len(), 1);
        let (writer, recoveries) =
            FactLog::open(&paths, OpenMode::Writer, &clock(), &AcceptAllFacts)?;
        assert_eq!(recoveries.len(), 1);
        let recovery = recoveries.first().ok_or("missing recovery")?;
        assert!(recovery.quarantine_path.exists());
        assert!(
            recovery
                .quarantine_path
                .to_string_lossy()
                .contains("fact-tail")
        );
        assert_eq!(writer.read_all(&AcceptAllFacts)?.len(), 1);
        assert!(std::fs::read(&active)?.ends_with(b"\n"));
        Ok(())
    }

    #[test]
    fn sequence_gap_fails_with_location() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let paths = FactPaths::new(dir.path());
        {
            let mut log = FactLog::init(&paths, STREAM, Integrity::Checksums, &clock())?;
            let batch = seal_from(1, None, 2)?;
            log.append(&batch, Durability::Durable, &NoFaults)?;
        }
        let active = paths.facts_dir.join("0000000000000001-open.jsonl");
        let text = std::fs::read_to_string(&active)?;
        let mut lines: Vec<&str> = text.trim_end().split('\n').collect();
        lines.pop();
        // Append a record claiming sequence 4 (with a self-consistent
        // checksum, built through the public seal API).
        let doctored = seal_from(4, None, 1)?.pop().ok_or("empty")?;
        let mut rewritten = lines.join("\n");
        rewritten.push('\n');
        rewritten.push_str(&String::from_utf8(doctored.to_line()?)?);
        std::fs::write(&active, rewritten)?;
        let error = FactLog::open(&paths, OpenMode::Writer, &clock(), &AcceptAllFacts)
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(error.contains("sequence must be contiguous"), "{error}");
        assert!(error.contains("line 2"), "{error}");
        Ok(())
    }

    #[test]
    fn rotation_after_threshold_and_tiling() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let paths = FactPaths::new(dir.path());
        let mut log = FactLog::init(&paths, STREAM, Integrity::Checksums, &clock())?;
        let mut next: u64 = 1;
        let mut rotated = false;
        let mut batches = 0;
        while !rotated {
            batches += 1;
            if batches > 20 {
                return Err("rotation never triggered".into());
            }
            let batch = seal_from(next, None, 200)?;
            next += 200;
            rotated = log.append(&batch, Durability::Relaxed, &NoFaults)?;
        }
        assert_eq!(next, 1001);
        assert_eq!(log.manifest().active_segment, "00000000000003e9-open.jsonl");
        assert!(
            paths
                .facts_dir
                .join("0000000000000001-00000000000003e8.jsonl")
                .exists()
        );
        let (reopened, _) = FactLog::open(&paths, OpenMode::Writer, &clock(), &AcceptAllFacts)?;
        assert_eq!(reopened.tail().last_sequence, 1000);
        assert_eq!(reopened.read_all(&AcceptAllFacts)?.len(), 1000);
        Ok(())
    }

    #[test]
    fn refresh_picks_up_external_append() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let paths = FactPaths::new(dir.path());
        let mut log = FactLog::init(&paths, STREAM, Integrity::Checksums, &clock())?;
        let batch = seal_from(1, None, 2)?;
        log.append(&batch, Durability::Durable, &NoFaults)?;

        let (mut other, _) = FactLog::open(&paths, OpenMode::Writer, &clock(), &AcceptAllFacts)?;
        let more = seal_from(3, None, 2)?;
        other.append(&more, Durability::Durable, &NoFaults)?;

        log.refresh(&clock(), &AcceptAllFacts)?;
        assert_eq!(log.tail().last_sequence, 4);
        assert_eq!(
            log.tail().last_checksum.as_deref(),
            more.last().map(|r| r.checksum.as_str())
        );
        Ok(())
    }

    #[test]
    fn domain_gate_fails_closed_on_unknown_type() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let paths = FactPaths::new(dir.path());
        {
            let mut log = FactLog::init(&paths, STREAM, Integrity::Checksums, &clock())?;
            let batch = seal_from(1, None, 1)?;
            log.append(&batch, Durability::Durable, &NoFaults)?;
        }
        let strict =
            crate::fact::validator::KnownFactVersions::new([("other.type".to_string(), 1)]);
        let error = FactLog::open(&paths, OpenMode::Writer, &clock(), &strict)
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(error.contains("unknown fact type"), "{error}");
        Ok(())
    }

    #[test]
    fn manifest_newer_version_fails_closed() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let paths = FactPaths::new(dir.path());
        {
            let _log = FactLog::init(&paths, STREAM, Integrity::Checksums, &clock())?;
        }
        let mut manifest = FactManifest::read(&paths.manifest)?;
        manifest.store_version = 2;
        manifest.write_atomic(&paths.manifest)?;
        let error = FactLog::open(&paths, OpenMode::Writer, &clock(), &AcceptAllFacts)
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(error.contains("Upgrade Relay"), "{error}");
        Ok(())
    }

    #[test]
    fn append_rejects_a_batch_that_does_not_continue_the_tail()
    -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let paths = FactPaths::new(dir.path());
        let mut log = FactLog::init(&paths, STREAM, Integrity::Checksums, &clock())?;
        let batch = seal_from(1, None, 2)?;
        log.append(&batch, Durability::Durable, &NoFaults)?;
        let stale = seal_from(2, None, 1)?;
        let error = log
            .append(&stale, Durability::Durable, &NoFaults)
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(error.contains("the batch starts at sequence"), "{error}");
        Ok(())
    }

    #[test]
    fn chained_link_on_a_checksum_stream_is_rejected() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let paths = FactPaths::new(dir.path());
        {
            let mut log = FactLog::init(&paths, STREAM, Integrity::Checksums, &clock())?;
            let batch = seal_from(1, None, 1)?;
            log.append(&batch, Durability::Durable, &NoFaults)?;
        }
        // A record with a `previous` link on a checksum-only stream must not
        // validate on reopen.
        let active = paths.facts_dir.join("0000000000000001-open.jsonl");
        let text = std::fs::read_to_string(&active)?;
        let linked = chained_seal_from(2, Some("b".repeat(64)), 1)?
            .pop()
            .ok_or("empty")?;
        std::fs::write(
            &active,
            format!("{}{}", text, String::from_utf8(linked.to_line()?)?),
        )?;
        let error = FactLog::open(&paths, OpenMode::Writer, &clock(), &AcceptAllFacts)
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(error.contains("integrity \"checksums\""), "{error}");
        Ok(())
    }
}
