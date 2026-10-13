//! Segment naming, discovery, tiling validation, and rotation thresholds
//! (`docs/event-format.md` §4).
//!
//! Segment file names carry 16-digit zero-padded hexadecimal sequence
//! numbers. A closed segment is `<first>-<last>.jsonl` and is immutable;
//! the single active segment is `<first>-open.jsonl`. Because the numbers
//! are zero-padded, lexicographic name order equals numeric order, and
//! discovery is deterministic.

use std::path::Path;

use crate::error::Result;
use crate::error::StoreError;

/// Rotate the active segment after a durable append once it holds at least
/// this many events (§4).
pub const MAX_EVENTS_PER_SEGMENT: u64 = 1000;

/// …or at least this many bytes, whichever comes first (§4: 8 MiB).
pub const MAX_SEGMENT_BYTES: u64 = 8 * 1024 * 1024;

/// A parsed segment file name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SegmentName {
    /// `<first>-<last>.jsonl` — immutable.
    Closed {
        /// First sequence in the segment.
        first: u64,
        /// Last sequence in the segment.
        last: u64,
    },
    /// `<first>-open.jsonl` — may grow.
    Open {
        /// First sequence the segment will hold.
        first: u64,
    },
}

impl SegmentName {
    /// The first sequence this segment covers.
    pub fn first(&self) -> u64 {
        match self {
            SegmentName::Closed { first, .. } | SegmentName::Open { first } => *first,
        }
    }

    /// The file name for this segment.
    pub fn file_name(&self) -> String {
        match self {
            SegmentName::Closed { first, last } => format!("{first:016x}-{last:016x}.jsonl"),
            SegmentName::Open { first } => format!("{first:016x}-open.jsonl"),
        }
    }

    /// Parse a segment file name; `None` for anything that does not match
    /// the naming rules exactly.
    pub fn parse(name: &str) -> Option<SegmentName> {
        let stem = name.strip_suffix(".jsonl")?;
        if let Some((first, last)) = stem.split_once('-') {
            if last == "open" {
                let first = parse_hex16(first)?;
                if first == 0 {
                    return None;
                }
                return Some(SegmentName::Open { first });
            }
            let first = parse_hex16(first)?;
            let last = parse_hex16(last)?;
            if first == 0 || last < first {
                return None;
            }
            return Some(SegmentName::Closed { first, last });
        }
        None
    }
}

impl Ord for SegmentName {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.first()
            .cmp(&other.first())
            .then_with(|| match (self, other) {
                (SegmentName::Closed { last: a, .. }, SegmentName::Closed { last: b, .. }) => {
                    a.cmp(b)
                }
                (SegmentName::Closed { .. }, SegmentName::Open { .. }) => std::cmp::Ordering::Less,
                (SegmentName::Open { .. }, SegmentName::Closed { .. }) => {
                    std::cmp::Ordering::Greater
                }
                _ => std::cmp::Ordering::Equal,
            })
    }
}

impl PartialOrd for SegmentName {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

fn parse_hex16(text: &str) -> Option<u64> {
    if text.len() != 16 {
        return None;
    }
    u64::from_str_radix(text, 16).ok()
}

/// The result of scanning an `events/` directory.
#[derive(Debug, Clone, Default)]
pub struct SegmentListing {
    /// Closed segments in sequence order.
    pub closed: Vec<SegmentName>,
    /// The active segment, when present.
    pub active: Option<SegmentName>,
    /// Files in `events/` that do not match segment naming. Reported by
    /// `doctor`; they never block opening (§4).
    pub unknown_files: Vec<String>,
}

/// Deterministically discover segments in `events_dir` (§4): entries
/// matching the name patterns, sorted lexicographically.
pub fn discover(events_dir: &Path) -> Result<SegmentListing> {
    let entries = std::fs::read_dir(events_dir).map_err(|error| StoreError::Segments {
        reason: format!(
            "cannot read events directory at {}: {error}",
            events_dir.display()
        ),
    })?;
    let mut listing = SegmentListing::default();
    for entry in entries {
        let entry = entry.map_err(|error| StoreError::Segments {
            reason: format!(
                "cannot enumerate events directory at {}: {error}",
                events_dir.display()
            ),
        })?;
        let file_type = entry.file_type().map_err(|error| StoreError::Segments {
            reason: format!(
                "cannot inspect entry {} in {}: {error}",
                entry.path().display(),
                events_dir.display()
            ),
        })?;
        let name = entry.file_name().to_string_lossy().to_string();
        if !file_type.is_file() {
            listing.unknown_files.push(format!("{name} (not a file)"));
            continue;
        }
        match SegmentName::parse(&name) {
            Some(segment @ SegmentName::Closed { .. }) => listing.closed.push(segment),
            Some(SegmentName::Open { .. }) => {
                if listing.active.is_some() {
                    return Err(StoreError::Segments {
                        reason: format!(
                            "multiple active (…-open.jsonl) segments found, including {name}; \
                             only one active segment may exist"
                        ),
                    });
                }
                listing.active = SegmentName::parse(&name);
            }
            None => listing.unknown_files.push(name),
        }
    }
    listing.closed.sort();
    Ok(listing)
}

/// Validate that discovered segments tile the sequence space contiguously:
/// closed segments start at 1, are contiguous and non-overlapping, and end
/// exactly one below the active segment's first sequence (§4).
pub fn validate_tiling(listing: &SegmentListing) -> Result<()> {
    let mut expected_next: u64 = 1;
    for segment in &listing.closed {
        match segment {
            SegmentName::Closed { first, last } => {
                if *first != expected_next {
                    return Err(StoreError::Segments {
                        reason: format!(
                            "closed segment {} starts at sequence {first}, but the sequence \
                             space requires {expected_next}: closed segments must tile the log \
                             contiguously from 1",
                            segment.file_name()
                        ),
                    });
                }
                expected_next = last + 1;
            }
            SegmentName::Open { .. } => {
                return Err(StoreError::Segments {
                    reason: "internal error: open segment among closed segments".to_string(),
                });
            }
        }
    }
    if let Some(SegmentName::Open {
        first: active_first,
    }) = listing.active
    {
        if active_first != expected_next {
            return Err(StoreError::Segments {
                reason: format!(
                    "active segment starts at sequence {active_first}, but closed segments \
                     end one below {expected_next}"
                ),
            });
        }
    } else if !listing.closed.is_empty() {
        // No active segment: tolerated only as a rotation crash artifact; the
        // caller (log open) recreates it at `expected_next`. Doctor reports it.
        return Ok(());
    }
    Ok(())
}

/// Whether the active segment should rotate after a durable append that left
/// it with `events` events and `bytes` bytes (§4 thresholds).
pub fn should_rotate(events: u64, bytes: u64) -> bool {
    events >= MAX_EVENTS_PER_SEGMENT || bytes >= MAX_SEGMENT_BYTES
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_format_and_parse() {
        assert_eq!(
            SegmentName::Closed {
                first: 1,
                last: 500
            }
            .file_name(),
            "0000000000000001-00000000000001f4.jsonl"
        );
        assert_eq!(
            SegmentName::parse("0000000000000001-00000000000001f4.jsonl"),
            Some(SegmentName::Closed {
                first: 1,
                last: 500
            })
        );
        assert_eq!(
            SegmentName::parse("00000000000001f5-open.jsonl"),
            Some(SegmentName::Open { first: 501 })
        );
        assert_eq!(SegmentName::parse("1-2.jsonl"), None);
        assert_eq!(
            SegmentName::parse("0000000000000002-0000000000000001.jsonl"),
            None
        );
        assert_eq!(SegmentName::parse("0000000000000000-open.jsonl"), None);
        assert_eq!(SegmentName::parse("notes.txt"), None);
    }

    #[test]
    fn lexicographic_order_equals_numeric_order() {
        let names = [
            "0000000000000001-0000000000000002.jsonl",
            "0000000000000003-open.jsonl",
            "000000000000000a-000000000000000f.jsonl",
        ];
        let mut parsed: Vec<Option<SegmentName>> = names.map(SegmentName::parse).to_vec();
        parsed.sort();
        let firsts: Vec<u64> = parsed
            .iter()
            .map(|s| s.as_ref().map(|s| s.first()).unwrap_or_default())
            .collect();
        assert_eq!(firsts, vec![1, 3, 10]);
    }

    #[test]
    fn tiling_detects_gaps_and_overlaps() {
        let ok = SegmentListing {
            closed: vec![
                SegmentName::Closed { first: 1, last: 10 },
                SegmentName::Closed {
                    first: 11,
                    last: 20,
                },
            ],
            active: Some(SegmentName::Open { first: 21 }),
            unknown_files: vec![],
        };
        assert!(validate_tiling(&ok).is_ok());

        let gap = SegmentListing {
            closed: vec![
                SegmentName::Closed { first: 1, last: 10 },
                SegmentName::Closed {
                    first: 12,
                    last: 20,
                },
            ],
            active: Some(SegmentName::Open { first: 21 }),
            unknown_files: vec![],
        };
        let error = validate_tiling(&gap).err();
        assert!(
            error
                .as_ref()
                .is_some_and(|e| e.to_string().contains("requires 11")),
            "{error:?}"
        );

        let overlap = SegmentListing {
            closed: vec![
                SegmentName::Closed { first: 1, last: 10 },
                SegmentName::Closed {
                    first: 10,
                    last: 20,
                },
            ],
            active: Some(SegmentName::Open { first: 21 }),
            unknown_files: vec![],
        };
        assert!(validate_tiling(&overlap).is_err());

        let wrong_active = SegmentListing {
            closed: vec![SegmentName::Closed { first: 1, last: 10 }],
            active: Some(SegmentName::Open { first: 22 }),
            unknown_files: vec![],
        };
        assert!(validate_tiling(&wrong_active).is_err());

        let not_from_one = SegmentListing {
            closed: vec![SegmentName::Closed { first: 2, last: 10 }],
            active: Some(SegmentName::Open { first: 11 }),
            unknown_files: vec![],
        };
        assert!(validate_tiling(&not_from_one).is_err());
    }

    #[test]
    fn rotation_thresholds() {
        assert!(!should_rotate(999, 1000));
        assert!(should_rotate(1000, 1000));
        assert!(should_rotate(10, MAX_SEGMENT_BYTES));
        assert!(!should_rotate(10, MAX_SEGMENT_BYTES - 1));
    }
}
