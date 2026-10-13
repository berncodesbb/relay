//! The segmented, hash-chained JSONL event log (`docs/event-format.md`
//! §4–§7).
//!
//! The [`EventLog`] owns JSONL only — no SQLite, no locking. The store
//! orchestrator ([`crate::RelayStore`]) wraps it with the writer lock and
//! the SQLite projection. Responsibilities here: open with full validation
//! and conservative tail recovery, append with a single buffered write,
//! rotate segments, and read records back with chain verification.

use std::collections::HashSet;
use std::io::Write;
use std::path::PathBuf;

use crate::clock::Clock;
use crate::envelope::EventEnvelope;
use crate::envelope::RecordLocation;
use crate::error::Result;
use crate::error::StoreError;
use crate::fault::FaultInjector;
use crate::fault::FaultPoint;
use crate::manifest::Manifest;
use crate::manifest::fsync_dir_best_effort;
use crate::paths::RelayPaths;
use crate::segments::SegmentListing;
use crate::segments::SegmentName;
use crate::segments::should_rotate;

/// Durability of an append (§6 step 7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Durability {
    /// Flush and fsync the segment file. The only mode used for contractual
    /// state in v1.
    Durable,
    /// Skip fsync (test mode).
    Relaxed,
}

/// Tail of the log after validation.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TailState {
    /// Highest sequence present (0 for an empty log).
    pub last_sequence: u64,
    /// Hash of that event (`None` for an empty log).
    pub last_hash: Option<String>,
    /// Events currently in the active segment.
    pub active_events: u64,
    /// Current byte size of the active segment file.
    pub active_bytes: u64,
}

/// An event plus where it came from.
#[derive(Debug, Clone)]
pub struct EventRecord {
    /// The validated envelope.
    pub envelope: EventEnvelope,
    /// Segment file name.
    pub segment: String,
    /// 1-based line number within the segment.
    pub line: u64,
}

/// A tail-recovery action performed during open (§7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveredTail {
    /// Where the incomplete bytes were quarantined.
    pub quarantine_path: PathBuf,
    /// How many bytes were incomplete.
    pub bytes: u64,
    /// The segment that was truncated.
    pub segment: String,
}

/// How [`EventLog::open`] treats an incomplete active-segment tail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenMode {
    /// Writer mode (caller holds the writer lock): quarantine an incomplete
    /// final tail into `recovery/`, truncate the file, repair the manifest
    /// after a rotation crash, and create a missing active segment.
    Writer,
    /// Reader mode: never write. An incomplete final tail of the active
    /// segment is an in-progress append and is ignored (consistent-prefix
    /// read); the manifest is advisory.
    Reader,
}

/// The segmented event log.
#[derive(Debug)]
pub struct EventLog {
    paths: RelayPaths,
    manifest: Manifest,
    listing: SegmentListing,
    tail: TailState,
}

/// Incremental validation state shared by `open`, `refresh`, and `read_all`.
#[derive(Default)]
struct ChainState {
    expected_sequence: u64,
    previous_hash: Option<String>,
    seen_ids: HashSet<String>,
    active_events: u64,
    active_bytes: u64,
}

impl ChainState {
    fn new(start_sequence: u64, previous_hash: Option<String>) -> ChainState {
        ChainState {
            expected_sequence: start_sequence,
            previous_hash,
            ..ChainState::default()
        }
    }

    /// Validate one physical line and advance the chain state.
    fn accept(
        &mut self,
        text: &str,
        segment: &str,
        line: u64,
        project_id: &str,
        is_active: bool,
    ) -> Result<EventEnvelope> {
        let location = RecordLocation { segment, line };
        let envelope = EventEnvelope::parse(text, location)?;
        if envelope.project_id != project_id {
            return Err(StoreError::Record {
                segment: segment.to_string(),
                line,
                sequence: Some(envelope.sequence),
                reason: format!(
                    "event belongs to project {} but this log belongs to {project_id}",
                    envelope.project_id
                ),
            });
        }
        if envelope.sequence != self.expected_sequence {
            return Err(StoreError::Record {
                segment: segment.to_string(),
                line,
                sequence: Some(envelope.sequence),
                reason: format!(
                    "sequence must be contiguous: expected {}, found {} (the log may be \
                     truncated or reordered)",
                    self.expected_sequence, envelope.sequence
                ),
            });
        }
        if envelope.previous_hash != self.previous_hash {
            return Err(StoreError::ChainBreak {
                segment: segment.to_string(),
                line,
                sequence: envelope.sequence,
                expected: self
                    .previous_hash
                    .clone()
                    .unwrap_or_else(|| "null".to_string()),
                actual: envelope
                    .previous_hash
                    .clone()
                    .unwrap_or_else(|| "null".to_string()),
                hash_field: false,
            });
        }
        if !self.seen_ids.insert(envelope.id.clone()) {
            return Err(StoreError::Record {
                segment: segment.to_string(),
                line,
                sequence: Some(envelope.sequence),
                reason: format!("duplicate event id {}", envelope.id),
            });
        }
        self.expected_sequence += 1;
        self.previous_hash = Some(envelope.hash.clone());
        if is_active {
            self.active_events += 1;
        }
        Ok(envelope)
    }
}

impl EventLog {
    /// Create a brand-new store: layout, empty active segment, manifest.
    /// Fails if a manifest already exists (open it instead).
    pub fn init(paths: &RelayPaths, project_id: &str, clock: &dyn Clock) -> Result<EventLog> {
        if paths.manifest.exists() {
            return Err(StoreError::Invalid {
                reason: format!(
                    "a Relay store already exists at {}. Open it instead of creating a new \
                     one, or migrate the existing project.",
                    paths.root.display()
                ),
            });
        }
        crate::envelope::validate_prefixed_uuid(project_id, "prj").map_err(|reason| {
            StoreError::Invalid {
                reason: format!("project id: {reason}"),
            }
        })?;
        paths.ensure_layout().map_err(|error| StoreError::Io {
            context: "creating .relay layout".to_string(),
            path: paths.root.clone(),
            source: error,
        })?;
        let active = SegmentName::Open { first: 1 };
        let active_path = paths.events_dir.join(active.file_name());
        std::fs::File::create(&active_path).map_err(|error| StoreError::Io {
            context: "creating the initial active segment".to_string(),
            path: active_path,
            source: error,
        })?;
        fsync_dir_best_effort(&paths.events_dir);
        let manifest = Manifest::new(project_id, clock.now_iso(), active.file_name());
        manifest.write_atomic(&paths.manifest)?;
        Ok(EventLog {
            paths: paths.clone(),
            manifest,
            listing: SegmentListing {
                closed: vec![],
                active: Some(active),
                unknown_files: vec![],
            },
            tail: TailState::default(),
        })
    }

    /// Open and fully validate the log (§7). In [`OpenMode::Writer`] an
    /// incomplete final tail of the active segment is quarantined and the
    /// file truncated; in [`OpenMode::Reader`] it is ignored.
    pub fn open(
        paths: &RelayPaths,
        mode: OpenMode,
        clock: &dyn Clock,
    ) -> Result<(EventLog, Vec<RecoveredTail>)> {
        let manifest = Manifest::read(&paths.manifest)?;
        let mut listing = crate::segments::discover(&paths.events_dir)?;
        crate::segments::validate_tiling(&listing)?;
        let mut recoveries: Vec<RecoveredTail> = Vec::new();

        // Derive the active segment from tiling when it is absent (rotation
        // crash before the fresh active file was created).
        if listing.active.is_none() && !listing.closed.is_empty() {
            let next = match listing.closed.last() {
                Some(SegmentName::Closed { last, .. }) => *last + 1,
                _ => 1,
            };
            listing.active = Some(SegmentName::Open { first: next });
        }

        let project_id = manifest.project_id.clone();
        let mut chain = ChainState::new(1, None);
        for segment in listing.closed.iter().chain(listing.active.iter()) {
            let is_active = matches!(segment, SegmentName::Open { .. });
            let name = segment.file_name();
            let path = paths.events_dir.join(&name);
            let bytes = match std::fs::read(&path) {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound && is_active => {
                    if mode == OpenMode::Writer {
                        std::fs::File::create(&path).map_err(|error| StoreError::Io {
                            context: "recreating the missing active segment".to_string(),
                            path: path.clone(),
                            source: error,
                        })?;
                        fsync_dir_best_effort(&paths.events_dir);
                    }
                    Vec::new()
                }
                Err(error) => {
                    return Err(StoreError::Io {
                        context: "reading a segment".to_string(),
                        path,
                        source: error,
                    });
                }
            };
            let (complete, incomplete) = split_complete(&bytes);
            if !is_active {
                if incomplete.is_some_and(|tail| !tail.is_empty()) {
                    return Err(StoreError::Record {
                        segment: name,
                        line: line_count(complete) + 1,
                        sequence: None,
                        reason: "closed segment does not end with a newline (incomplete \
                                 final line); only the active segment may carry a crash tail"
                            .to_string(),
                    });
                }
                if let SegmentName::Closed { first, last } = segment {
                    let expected = last - first + 1;
                    let actual = line_count(complete);
                    if actual != expected {
                        return Err(StoreError::Segments {
                            reason: format!(
                                "closed segment {name} holds {actual} events but its name \
                                 promises {expected}"
                            ),
                        });
                    }
                }
            } else if let Some(tail_bytes) = incomplete.filter(|tail| !tail.is_empty()) {
                match mode {
                    OpenMode::Writer => {
                        let quarantine = quarantine_tail(paths, &name, tail_bytes, clock)?;
                        truncate_file(&path, complete.len() as u64)?;
                        recoveries.push(RecoveredTail {
                            quarantine_path: quarantine,
                            bytes: tail_bytes.len() as u64,
                            segment: name.clone(),
                        });
                    }
                    OpenMode::Reader => {
                        // In-progress append: ignore the tail for this read.
                    }
                }
            }
            let mut line_number: u64 = 0;
            for line in Lines::new(complete) {
                line_number += 1;
                let text = utf8_line(line, &name, line_number)?;
                chain.accept(&text, &name, line_number, &project_id, is_active)?;
            }
            if is_active {
                chain.active_bytes = complete.len() as u64;
            }
        }

        let mut opened = EventLog {
            paths: paths.clone(),
            manifest,
            listing,
            tail: TailState {
                last_sequence: chain.expected_sequence.saturating_sub(1),
                last_hash: chain.previous_hash,
                active_events: chain.active_events,
                active_bytes: chain.active_bytes,
            },
        };
        if mode == OpenMode::Writer {
            opened.repair_manifest()?;
        }
        Ok((opened, recoveries))
    }

    /// Re-derive the tail after acquiring the writer lock: another writer
    /// may have appended or rotated since this log was opened. Segments
    /// validated at open are trusted (closed segments are immutable); new
    /// closed segments and the active segment are fully revalidated, and an
    /// incomplete crash tail is recovered exactly like `open` does.
    pub fn refresh(&mut self, clock: &dyn Clock) -> Result<()> {
        let listing = crate::segments::discover(&self.paths.events_dir)?;
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
        let project_id = self.manifest.project_id.clone();
        let known_last = self.tail.last_sequence;
        let mut chain = ChainState::new(self.tail.last_sequence + 1, self.tail.last_hash.clone());
        for segment in listing
            .closed
            .iter()
            .skip(known_closed)
            .chain(listing.active.iter())
        {
            let is_active = matches!(segment, SegmentName::Open { .. });
            let name = segment.file_name();
            let path = self.paths.events_dir.join(&name);
            let bytes = match std::fs::read(&path) {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound && is_active => {
                    std::fs::File::create(&path).map_err(|error| StoreError::Io {
                        context: "recreating the missing active segment".to_string(),
                        path: path.clone(),
                        source: error,
                    })?;
                    fsync_dir_best_effort(&self.paths.events_dir);
                    Vec::new()
                }
                Err(error) => {
                    return Err(StoreError::Io {
                        context: "re-reading a segment after acquiring the lock".to_string(),
                        path,
                        source: error,
                    });
                }
            };
            let (complete, incomplete) = split_complete(&bytes);
            if !is_active && incomplete.is_some_and(|tail| !tail.is_empty()) {
                return Err(StoreError::Record {
                    segment: name,
                    line: line_count(complete) + 1,
                    sequence: None,
                    reason: "closed segment does not end with a newline".to_string(),
                });
            }
            if let Some(tail_bytes) = incomplete.filter(|tail| !tail.is_empty())
                && is_active
            {
                let quarantine = quarantine_tail(&self.paths, &name, tail_bytes, clock)?;
                truncate_file(&path, complete.len() as u64)?;
                let _ = quarantine;
            }
            // Events already validated by this process (or a previous,
            // immutability-guaranteed close) at or below `known_last` are
            // skipped; the active segment can only have grown by appends.
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
                chain.accept(&text, &name, line_number, &project_id, is_active)?;
            }
            if is_active {
                // Count the skipped, already-known active events too.
                chain.active_events += skip_lines;
                chain.active_bytes = complete.len() as u64;
            }
        }
        self.listing = listing;
        self.tail = TailState {
            last_sequence: chain.expected_sequence.saturating_sub(1),
            last_hash: chain.previous_hash,
            active_events: chain.active_events,
            active_bytes: chain.active_bytes,
        };
        self.repair_manifest()
    }

    /// Align the manifest with the discovered active segment (writer mode;
    /// §5 requires the manifest to name the active segment).
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

    /// Read every validated record in sequence order, chain-verifying each
    /// line. A concurrent rotation may remove a listed file between
    /// discovery and read; the read re-discovers and retries once before
    /// failing.
    pub fn read_all(&self) -> Result<Vec<EventRecord>> {
        let listing = crate::segments::discover(&self.paths.events_dir)?;
        match self.read_pass(&listing) {
            Err(StoreError::Io {
                context, source, ..
            }) if context == "reading a segment"
                && source.kind() == std::io::ErrorKind::NotFound =>
            {
                let retry = crate::segments::discover(&self.paths.events_dir)?;
                self.read_pass(&retry)
            }
            other => other,
        }
    }

    fn read_pass(&self, listing: &SegmentListing) -> Result<Vec<EventRecord>> {
        let project_id = self.manifest.project_id.clone();
        let mut chain = ChainState::new(1, None);
        let mut records = Vec::new();
        for segment in listing.closed.iter().chain(listing.active.iter()) {
            let is_active = matches!(segment, SegmentName::Open { .. });
            let name = segment.file_name();
            let path = self.paths.events_dir.join(&name);
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
                let envelope = chain.accept(&text, &name, line_number, &project_id, is_active)?;
                records.push(EventRecord {
                    envelope,
                    segment: name.clone(),
                    line: line_number,
                });
            }
        }
        Ok(records)
    }

    /// The validated tail.
    pub fn tail(&self) -> &TailState {
        &self.tail
    }

    /// The store manifest.
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// Unknown (non-segment) files seen during the last discovery.
    pub fn unknown_files(&self) -> &[String] {
        &self.listing.unknown_files
    }

    /// Append one batch as a single buffered write (§6 steps 5–7), then
    /// rotate if the active segment crossed a threshold (§4).
    ///
    /// The caller holds the writer lock and has refreshed the tail; the
    /// envelopes are already fully validated and chained onto the tail.
    /// Returns whether a rotation happened.
    pub fn append(
        &mut self,
        envelopes: &[EventEnvelope],
        durability: Durability,
        fault: &dyn FaultInjector,
    ) -> Result<bool> {
        if envelopes.is_empty() {
            return Ok(false);
        }
        let active_path = self.paths.events_dir.join(&self.manifest.active_segment);
        let mut buffer = Vec::new();
        for envelope in envelopes {
            buffer.extend_from_slice(&envelope.to_line());
        }
        if fault.should_crash(FaultPoint::AfterPartialWrite) {
            let partial = (buffer.len() / 2).clamp(1, buffer.len());
            write_append(&active_path, &buffer[..partial], Durability::Relaxed)?;
            return Err(StoreError::SimulatedCrash {
                point: "AfterPartialWrite".to_string(),
            });
        }
        if fault.should_crash(FaultPoint::AfterWriteNoSync) {
            write_append_raw(&active_path, &buffer)?;
            return Err(StoreError::SimulatedCrash {
                point: "AfterWriteNoSync".to_string(),
            });
        }
        write_append(&active_path, &buffer, durability)?;
        if let Some(last) = envelopes.last() {
            self.tail.last_sequence = last.sequence;
            self.tail.last_hash = Some(last.hash.clone());
        }
        self.tail.active_events += envelopes.len() as u64;
        self.tail.active_bytes += buffer.len() as u64;
        if fault.should_crash(FaultPoint::AfterDurableAppend) {
            return Err(StoreError::SimulatedCrash {
                point: "AfterDurableAppend".to_string(),
            });
        }
        if should_rotate(self.tail.active_events, self.tail.active_bytes) {
            self.rotate()?;
            if fault.should_crash(FaultPoint::AfterRotation) {
                return Err(StoreError::SimulatedCrash {
                    point: "AfterRotation".to_string(),
                });
            }
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Rotate: close the active segment under its final name, create the
    /// fresh active segment, rewrite the manifest atomically (§4, §5).
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
        let active_path = self.paths.events_dir.join(active.file_name());
        let closed = SegmentName::Closed { first, last };
        let closed_path = self.paths.events_dir.join(closed.file_name());
        std::fs::rename(&active_path, &closed_path).map_err(|error| StoreError::Io {
            context: "rotating the active segment".to_string(),
            path: active_path,
            source: error,
        })?;
        fsync_dir_best_effort(&self.paths.events_dir);
        let fresh = SegmentName::Open { first: last + 1 };
        let fresh_path = self.paths.events_dir.join(fresh.file_name());
        std::fs::File::create(&fresh_path).map_err(|error| StoreError::Io {
            context: "creating the fresh active segment".to_string(),
            path: fresh_path,
            source: error,
        })?;
        fsync_dir_best_effort(&self.paths.events_dir);
        self.manifest.active_segment = fresh.file_name();
        self.manifest.write_atomic(&self.paths.manifest)?;
        self.listing.closed.push(closed);
        self.listing.active = Some(fresh);
        self.tail.active_events = 0;
        self.tail.active_bytes = 0;
        Ok(())
    }
}

/// Iterate complete `\n`-terminated lines of `bytes`.
struct Lines<'a> {
    bytes: &'a [u8],
}

impl<'a> Lines<'a> {
    fn new(bytes: &'a [u8]) -> Lines<'a> {
        Lines { bytes }
    }
}

impl<'a> Iterator for Lines<'a> {
    type Item = &'a [u8];
    fn next(&mut self) -> Option<&'a [u8]> {
        let split = self.bytes.iter().position(|&b| b == b'\n')?;
        let (line, rest) = self.bytes.split_at(split);
        self.bytes = &rest[1..];
        Some(line)
    }
}

/// Decode one physical line as UTF-8 with a located error otherwise.
fn utf8_line(line: &[u8], segment: &str, line_number: u64) -> Result<String> {
    std::str::from_utf8(line)
        .map(str::to_string)
        .map_err(|_| StoreError::Record {
            segment: segment.to_string(),
            line: line_number,
            sequence: None,
            reason: "line is not valid UTF-8".to_string(),
        })
}

/// Split file bytes into the complete-line prefix and the incomplete tail
/// after the final `\n` (if any).
fn split_complete(bytes: &[u8]) -> (&[u8], Option<&[u8]>) {
    match bytes.iter().rposition(|&b| b == b'\n') {
        Some(index) => (&bytes[..index + 1], Some(&bytes[index + 1..])),
        None => (&[], Some(bytes)),
    }
}

/// Number of complete lines.
fn line_count(complete: &[u8]) -> u64 {
    complete.iter().filter(|&&b| b == b'\n').count() as u64
}

/// Write-append with (durable) fsync.
fn write_append(path: &std::path::Path, bytes: &[u8], durability: Durability) -> Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.append(true).create(true);
    let mut file = options.open(path).map_err(|error| StoreError::Io {
        context: "opening the active segment for append".to_string(),
        path: path.to_path_buf(),
        source: error,
    })?;
    file.write_all(bytes).map_err(|error| StoreError::Io {
        context: "appending to the active segment".to_string(),
        path: path.to_path_buf(),
        source: error,
    })?;
    if durability == Durability::Durable {
        file.sync_all().map_err(|error| StoreError::Io {
            context: "fsyncing the active segment".to_string(),
            path: path.to_path_buf(),
            source: error,
        })?;
    }
    Ok(())
}

/// Write-append without flush or fsync (fault injection).
fn write_append_raw(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.append(true).create(true);
    let mut file = options.open(path).map_err(|error| StoreError::Io {
        context: "opening the active segment for append".to_string(),
        path: path.to_path_buf(),
        source: error,
    })?;
    file.write(bytes).map_err(|error| StoreError::Io {
        context: "appending to the active segment".to_string(),
        path: path.to_path_buf(),
        source: error,
    })?;
    Ok(())
}

/// Truncate `path` to `len` bytes and fsync (tail recovery).
fn truncate_file(path: &std::path::Path, len: u64) -> Result<()> {
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .map_err(|error| StoreError::Io {
            context: "opening the active segment for truncation".to_string(),
            path: path.to_path_buf(),
            source: error,
        })?;
    file.set_len(len).map_err(|error| StoreError::Io {
        context: "truncating the incomplete tail".to_string(),
        path: path.to_path_buf(),
        source: error,
    })?;
    file.sync_all().map_err(|error| StoreError::Io {
        context: "fsyncing after truncation".to_string(),
        path: path.to_path_buf(),
        source: error,
    })?;
    Ok(())
}

/// Copy the incomplete tail into `recovery/rejected-tail-<stamp>Z.jsonl`
/// (§4 example naming, §7).
fn quarantine_tail(
    paths: &RelayPaths,
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
    let base = format!("rejected-tail-{stamp}Z");
    let mut candidate = paths.recovery_dir.join(format!("{base}.jsonl"));
    let mut attempt = 1;
    while candidate.exists() && attempt < 100 {
        candidate = paths.recovery_dir.join(format!("{base}-{attempt}.jsonl"));
        attempt += 1;
    }
    // The raw incomplete bytes are preserved verbatim under a header line
    // that attributes them to their source segment.
    let mut contents = format!("-- rejected tail from {segment}\n").into_bytes();
    contents.extend_from_slice(tail);
    contents.push(b'\n');
    crate::manifest::write_and_sync(&candidate, &contents)?;
    Ok(candidate)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canonical::Json;
    use crate::clock::FixedClock;
    use crate::fault::NoFaults;

    const PROJECT: &str = "prj_00000000-0000-4000-8000-000000000001";

    fn envelope_for(sequence: u64, previous: Option<String>, id_n: u64) -> Result<EventEnvelope> {
        EventEnvelope::build(crate::envelope::EnvelopeDraft {
            project_id: PROJECT.to_string(),
            sequence,
            session_id: None,
            event_type: "run.interrupted".to_string(),
            timestamp: format!("2026-10-13T08:00:{:02}.000Z", sequence % 60),
            correlation_id: format!("cor_00000000-0000-4000-8000-{id_n:012x}"),
            causation_id: None,
            payload: Json::object([("phase", Json::string("t")), ("hint", Json::string("h"))]),
            previous_hash: previous,
            id: format!("evt_00000000-0000-4000-8000-{id_n:012x}"),
        })
    }

    fn batch_from(
        chain_start: u64,
        previous: Option<String>,
        count: u64,
    ) -> Result<Vec<EventEnvelope>> {
        let mut previous = previous;
        let mut batch = Vec::new();
        for offset in 0..count {
            let sequence = chain_start + offset;
            let envelope = envelope_for(sequence, previous.clone(), sequence)?;
            previous = Some(envelope.hash.clone());
            batch.push(envelope);
        }
        Ok(batch)
    }

    #[test]
    fn init_append_read_roundtrip() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let paths = RelayPaths::new(dir.path());
        let clock = FixedClock::new(1_791_878_400_000);
        let mut log = EventLog::init(&paths, PROJECT, &clock)?;
        let batch = batch_from(1, None, 3)?;
        assert!(!log.append(&batch, Durability::Durable, &NoFaults)?);
        let records = log.read_all()?;
        assert_eq!(records.len(), 3);
        assert_eq!(log.tail().last_sequence, 3);
        assert_eq!(
            log.tail().last_hash.as_deref(),
            batch.last().map(|e| e.hash.as_str())
        );
        assert_eq!(log.manifest().active_segment, "0000000000000001-open.jsonl");
        // Reopen clean: no recoveries, same tail.
        let (reopened, recoveries) = EventLog::open(&paths, OpenMode::Writer, &clock)?;
        assert!(recoveries.is_empty());
        assert_eq!(reopened.tail().last_sequence, 3);
        Ok(())
    }

    #[test]
    fn reader_ignores_crash_tail_writer_recovers() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let paths = RelayPaths::new(dir.path());
        let clock = FixedClock::new(1_791_878_400_000);
        {
            let mut log = EventLog::init(&paths, PROJECT, &clock)?;
            let batch = batch_from(1, None, 1)?;
            log.append(&batch, Durability::Durable, &NoFaults)?;
        }
        // Simulate a crash mid-write: append half of a new line.
        let active = paths.events_dir.join("0000000000000001-open.jsonl");
        let mut file = std::fs::OpenOptions::new().append(true).open(&active)?;
        file.write_all(b"{\"envelopeVersion\":1,\"seq")?;
        drop(file);

        let (reader, recoveries) = EventLog::open(&paths, OpenMode::Reader, &clock)?;
        assert!(recoveries.is_empty());
        assert_eq!(reader.read_all()?.len(), 1);

        let (writer, recoveries) = EventLog::open(&paths, OpenMode::Writer, &clock)?;
        assert_eq!(recoveries.len(), 1);
        let recovery = recoveries.first().ok_or("missing recovery")?;
        assert!(recovery.quarantine_path.exists());
        assert_eq!(recovery.segment, "0000000000000001-open.jsonl");
        assert_eq!(writer.read_all()?.len(), 1);
        let content = std::fs::read(&active)?;
        assert!(content.ends_with(b"\n"));
        Ok(())
    }

    #[test]
    fn closed_segment_incomplete_tail_fails_open() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let paths = RelayPaths::new(dir.path());
        let clock = FixedClock::new(1_791_878_400_000);
        {
            let mut log = EventLog::init(&paths, PROJECT, &clock)?;
            let batch = batch_from(1, None, 1)?;
            log.append(&batch, Durability::Durable, &NoFaults)?;
            std::fs::rename(
                paths.events_dir.join("0000000000000001-open.jsonl"),
                paths
                    .events_dir
                    .join("0000000000000001-0000000000000001.jsonl"),
            )?;
            std::fs::File::create(paths.events_dir.join("0000000000000002-open.jsonl"))?;
        }
        let closed = paths
            .events_dir
            .join("0000000000000001-0000000000000001.jsonl");
        let mut file = std::fs::OpenOptions::new().append(true).open(&closed)?;
        file.write_all(b"partial")?;
        drop(file);
        let error = EventLog::open(&paths, OpenMode::Writer, &clock)
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(
            error.contains("closed segment does not end with a newline"),
            "{error}"
        );
        Ok(())
    }

    #[test]
    fn rotation_after_threshold_and_tiling() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let paths = RelayPaths::new(dir.path());
        let clock = FixedClock::new(1_791_878_400_000);
        let mut log = EventLog::init(&paths, PROJECT, &clock)?;
        let mut previous = None;
        let mut next: u64 = 1;
        let mut rotated = false;
        let mut batches = 0;
        while !rotated {
            batches += 1;
            if batches > 20 {
                return Err("rotation never triggered".into());
            }
            let batch = batch_from(next, previous.clone(), 200)?;
            previous = batch.last().map(|e| e.hash.clone());
            next += 200;
            rotated = log.append(&batch, Durability::Relaxed, &NoFaults)?;
        }
        // 1000 events closed the first segment (1000 = 0x3e8).
        assert_eq!(next, 1001);
        assert_eq!(log.manifest().active_segment, "00000000000003e9-open.jsonl");
        assert!(
            paths
                .events_dir
                .join("0000000000000001-00000000000003e8.jsonl")
                .exists()
        );
        // A further batch lives in the fresh active segment without
        // rotating again.
        let batch = batch_from(next, previous.clone(), 5)?;
        assert!(!log.append(&batch, Durability::Relaxed, &NoFaults)?);
        let (reopened, _) = EventLog::open(&paths, OpenMode::Writer, &clock)?;
        assert_eq!(reopened.tail().last_sequence, 1005);
        assert_eq!(reopened.read_all()?.len(), 1005);
        Ok(())
    }

    #[test]
    fn sequence_gap_fails_with_location() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let paths = RelayPaths::new(dir.path());
        let clock = FixedClock::new(1_791_878_400_000);
        {
            let mut log = EventLog::init(&paths, PROJECT, &clock)?;
            let batch = batch_from(1, None, 2)?;
            log.append(&batch, Durability::Durable, &NoFaults)?;
        }
        // Drop the last (second) line and append a doctored event that
        // claims sequence 4 while chaining correctly.
        let active = paths.events_dir.join("0000000000000001-open.jsonl");
        let bytes = std::fs::read(&active)?;
        let text = String::from_utf8(bytes)?;
        let mut lines: Vec<&str> = text.trim_end_matches('\n').split('\n').collect();
        lines.pop();
        let doctored = {
            let first = batch_from(1, None, 1)?.pop().ok_or("empty")?;
            let mut env = batch_from(4, Some(first.hash.clone()), 9)?
                .pop()
                .ok_or("empty")?;
            env.sequence = 4;
            env.previous_hash = Some(first.hash.clone());
            env.hash = env.compute_hash();
            env
        };
        let mut new_text = lines.join("\n");
        new_text.push('\n');
        new_text.push_str(&String::from_utf8(doctored.to_line())?);
        std::fs::write(&active, new_text)?;
        let error = EventLog::open(&paths, OpenMode::Writer, &clock)
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(error.contains("sequence must be contiguous"), "{error}");
        assert!(error.contains("line 2"), "{error}");
        Ok(())
    }

    #[test]
    fn broken_hash_fails_with_segment_and_line() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let paths = RelayPaths::new(dir.path());
        let clock = FixedClock::new(1_791_878_400_000);
        {
            let mut log = EventLog::init(&paths, PROJECT, &clock)?;
            let batch = batch_from(1, None, 2)?;
            log.append(&batch, Durability::Durable, &NoFaults)?;
        }
        // Flip one byte inside the second event's payload without fixing
        // its hash: parse fails on hash verification.
        let active = paths.events_dir.join("0000000000000001-open.jsonl");
        let text = std::fs::read_to_string(&active)?;
        let mut lines: Vec<&str> = text.trim_end().split('\n').collect();
        let second = lines.last().ok_or("missing")?.replace("phase", "fase");
        lines.pop();
        let mut rewritten = lines.join("\n");
        rewritten.push('\n');
        rewritten.push_str(&second);
        rewritten.push('\n');
        std::fs::write(&active, rewritten)?;
        let error = EventLog::open(&paths, OpenMode::Writer, &clock)
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(error.contains("broken hash chain"), "{error}");
        assert!(error.contains("line 2"), "{error}");
        Ok(())
    }

    #[test]
    fn project_mismatch_and_duplicate_ids_fail() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let paths = RelayPaths::new(dir.path());
        let clock = FixedClock::new(1_791_878_400_000);
        {
            let mut log = EventLog::init(&paths, PROJECT, &clock)?;
            let batch = batch_from(1, None, 1)?;
            log.append(&batch, Durability::Durable, &NoFaults)?;
        }
        // Foreign project event, chained correctly.
        let active = paths.events_dir.join("0000000000000001-open.jsonl");
        let first_hash = batch_from(1, None, 1)?.pop().ok_or("empty")?.hash;
        let foreign = EventEnvelope::build(crate::envelope::EnvelopeDraft {
            project_id: "prj_00000000-0000-4000-8000-00000000dead".to_string(),
            sequence: 2,
            session_id: None,
            event_type: "run.interrupted".to_string(),
            timestamp: "2026-10-13T08:00:05.000Z".to_string(),
            correlation_id: "cor_00000000-0000-4000-8000-0000000000ff".to_string(),
            causation_id: None,
            payload: Json::object([("phase", Json::string("t")), ("hint", Json::string("h"))]),
            previous_hash: Some(first_hash),
            id: "evt_00000000-0000-4000-8000-0000000000ee".to_string(),
        })?;
        let mut text = std::fs::read_to_string(&active)?;
        text.push_str(&String::from_utf8(foreign.to_line())?);
        std::fs::write(&active, text)?;
        let error = EventLog::open(&paths, OpenMode::Writer, &clock)
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(error.contains("belongs to project"), "{error}");
        Ok(())
    }

    #[test]
    fn manifest_newer_store_version_fails_closed() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let paths = RelayPaths::new(dir.path());
        let clock = FixedClock::new(1_791_878_400_000);
        {
            let _log = EventLog::init(&paths, PROJECT, &clock)?;
        }
        let mut manifest = Manifest::read(&paths.manifest)?;
        manifest.store_version = 2;
        manifest.write_atomic(&paths.manifest)?;
        let error = EventLog::open(&paths, OpenMode::Writer, &clock)
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(error.contains("Upgrade Relay"), "{error}");
        Ok(())
    }

    #[test]
    fn refresh_picks_up_external_append_and_rotation() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let paths = RelayPaths::new(dir.path());
        let clock = FixedClock::new(1_791_878_400_000);
        let mut log = EventLog::init(&paths, PROJECT, &clock)?;
        let batch = batch_from(1, None, 2)?;
        log.append(&batch, Durability::Durable, &NoFaults)?;

        // A second writer appends via a separate open, then we refresh.
        let (mut other, _) = EventLog::open(&paths, OpenMode::Writer, &clock)?;
        let more = batch_from(3, other.tail().last_hash.clone(), 2)?;
        other.append(&more, Durability::Durable, &NoFaults)?;

        log.refresh(&clock)?;
        assert_eq!(log.tail().last_sequence, 4);
        assert_eq!(
            log.tail().last_hash.as_deref(),
            more.last().map(|e| e.hash.as_str())
        );
        Ok(())
    }

    #[test]
    fn rotation_crash_without_manifest_update_is_repaired() -> Result<(), Box<dyn std::error::Error>>
    {
        let dir = tempfile::tempdir()?;
        let paths = RelayPaths::new(dir.path());
        let clock = FixedClock::new(1_791_878_400_000);
        {
            let mut log = EventLog::init(&paths, PROJECT, &clock)?;
            let batch = batch_from(1, None, 1)?;
            log.append(&batch, Durability::Durable, &NoFaults)?;
            // Simulate: rename happened, manifest was never rewritten, fresh
            // active file was never created.
            std::fs::rename(
                paths.events_dir.join("0000000000000001-open.jsonl"),
                paths
                    .events_dir
                    .join("0000000000000001-0000000000000001.jsonl"),
            )?;
        }
        let (writer, _) = EventLog::open(&paths, OpenMode::Writer, &clock)?;
        assert_eq!(writer.tail().last_sequence, 1);
        assert_eq!(
            writer.manifest().active_segment,
            "0000000000000002-open.jsonl"
        );
        assert!(
            paths
                .events_dir
                .join("0000000000000002-open.jsonl")
                .exists()
        );
        // Reader mode tolerates the missing active file without writing.
        let (reader, _) = EventLog::open(&paths, OpenMode::Reader, &clock)?;
        assert_eq!(reader.read_all()?.len(), 1);
        Ok(())
    }
}
