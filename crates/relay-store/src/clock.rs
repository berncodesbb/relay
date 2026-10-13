//! Injected boundary data: clocks and id generation.
//!
//! Replay and tests must be deterministic (`docs/plans/…-implementation.md`,
//! "Engineering constraints": clocks, randomness, and ids are injected
//! boundary data). Production code uses the real implementations; tests
//! inject [`FixedClock`] and [`SequentialIds`].

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::Result;

/// Produces Relay timestamps: RFC 3339 UTC with millisecond precision and a
/// `Z` suffix (`YYYY-MM-DDTHH:MM:SS.sssZ`, matching JS `Date.toISOString()`).
pub trait Clock: Send + Sync {
    /// Current time as a Relay timestamp.
    fn now_iso(&self) -> String;
}

/// Real wall clock.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_iso(&self) -> String {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        iso_from_epoch_millis(millis)
    }
}

/// Deterministic clock for tests: starts at a fixed value, advances only
/// when the test asks.
#[derive(Debug)]
pub struct FixedClock {
    current: Mutex<i64>,
}

impl FixedClock {
    /// Create a clock pinned to `initial` millis after the Unix epoch.
    pub fn new(initial_epoch_millis: i64) -> FixedClock {
        FixedClock {
            current: Mutex::new(initial_epoch_millis),
        }
    }

    /// Advance the clock by `millis`.
    pub fn advance(&self, millis: i64) {
        if let Ok(mut current) = self.current.lock() {
            *current += millis;
        }
    }
}

impl Clock for FixedClock {
    fn now_iso(&self) -> String {
        // Lock poisoning only happens if a panic occurred while held; the
        // store treats a stuck clock as epoch zero rather than panicking.
        let millis = self.current.lock().map(|v| *v).unwrap_or_default();
        iso_from_epoch_millis(millis)
    }
}

/// Convert epoch milliseconds to a Relay timestamp.
///
/// Civil-from-days per Howard Hinnant's algorithm; valid for the full range
/// Relay cares about (1970..9999). No calendar dependency is required.
pub fn iso_from_epoch_millis(epoch_millis: i64) -> String {
    let seconds = epoch_millis.div_euclid(1000);
    let millis_part = epoch_millis.rem_euclid(1000);
    let days = seconds.div_euclid(86_400);
    let secs_of_day = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = secs_of_day / 3600;
    let minute = (secs_of_day % 3600) / 60;
    let second = secs_of_day % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis_part:03}Z")
}

/// (year, month, day) for days since 1970-01-01.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097); // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let year = if m <= 2 { y + 1 } else { y };
    (year, m as u32, d as u32)
}

/// Parse a Relay timestamp back into epoch milliseconds, validating the
/// exact shape `YYYY-MM-DDTHH:MM:SS.sssZ` and the calendar.
///
/// Returns `None` for anything that is not exactly this format (a longer
/// fraction, an offset other than `Z`, or an impossible date).
pub fn parse_iso_to_epoch_millis(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    if bytes.len() != 24 {
        return None;
    }
    let digits: [&[u8]; 7] = [
        &bytes[0..4],   // year
        &bytes[5..7],   // month
        &bytes[8..10],  // day
        &bytes[11..13], // hour
        &bytes[14..16], // minute
        &bytes[17..19], // second
        &bytes[20..23], // millis
    ];
    for group in digits {
        if !group.iter().all(u8::is_ascii_digit) {
            return None;
        }
    }
    if bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
        || bytes[19] != b'.'
        || bytes[23] != b'Z'
    {
        return None;
    }
    let num = |range: std::ops::Range<usize>| -> i64 {
        text.get(range)
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(0)
    };
    let year = num(0..4);
    let month = num(5..7);
    let day = num(8..10);
    let hour = num(11..13);
    let minute = num(14..16);
    let second = num(17..19);
    let millis = num(20..23);
    if !(1..=12).contains(&month) || day < 1 || day > days_in_month(year, month) {
        return None;
    }
    if hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let leaps_through = |n: i64| n.div_euclid(4) - n.div_euclid(100) + n.div_euclid(400);
    // Leap days inside [1970, year), not since year 0.
    let leap_days = leaps_through(year - 1) - leaps_through(1969);
    let days_since_epoch = 365 * (year - 1970) + leap_days + day_of_year(year, month, day) - 1;
    Some(days_since_epoch * 86_400_000 + (hour * 3_600 + minute * 60 + second) * 1000 + millis)
}

fn is_leap_year(year: i64) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap_year(year) {
                29
            } else {
                28
            }
        }
        _ => 0,
    }
}

fn day_of_year(year: i64, month: i64, day: i64) -> i64 {
    let mut month_cursor = 1;
    let mut total = 0;
    while month_cursor < month {
        total += days_in_month(year, month_cursor);
        month_cursor += 1;
    }
    total + day
}

/// Generates Relay identifiers: `<prefix>_<uuid-v4-lowercase>`.
pub trait IdGen: Send + Sync {
    /// Produce a fresh id with the given prefix.
    fn new_id(&self, prefix: &str) -> Result<String>;
}

/// Real random ids (`uuid` v4, lowercase hyphenated).
#[derive(Debug, Default, Clone, Copy)]
pub struct UuidIdGen;

impl IdGen for UuidIdGen {
    fn new_id(&self, prefix: &str) -> Result<String> {
        let uuid = uuid::Uuid::new_v4();
        Ok(format!("{prefix}_{uuid}"))
    }
}

/// Deterministic id generator for tests: UUID-v4-shaped ids with a counter
/// in the node field, so envelope validation accepts them.
#[derive(Debug)]
pub struct SequentialIds {
    counter: AtomicU64,
}

impl SequentialIds {
    /// Create a generator starting at `first`.
    pub fn new(first: u64) -> SequentialIds {
        SequentialIds {
            counter: AtomicU64::new(first),
        }
    }
}

impl Default for SequentialIds {
    fn default() -> Self {
        SequentialIds::new(1)
    }
}

impl IdGen for SequentialIds {
    fn new_id(&self, prefix: &str) -> Result<String> {
        let n = self.counter.fetch_add(1, Ordering::SeqCst);
        Ok(format!("{prefix}_00000000-0000-4000-8000-{n:012x}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_roundtrip() {
        let text = "2026-10-13T08:00:00.123Z";
        let millis = parse_iso_to_epoch_millis(text);
        assert_eq!(millis, Some(1_791_878_400_123));
        assert_eq!(iso_from_epoch_millis(millis.unwrap_or_default()), text);
    }

    #[test]
    fn iso_rejects_wrong_shapes() {
        assert_eq!(parse_iso_to_epoch_millis("2026-10-13T08:00:00Z"), None);
        assert_eq!(
            parse_iso_to_epoch_millis("2026-10-13T08:00:00.123+02:00"),
            None
        );
        assert_eq!(parse_iso_to_epoch_millis("2026-13-13T08:00:00.123Z"), None);
        assert_eq!(parse_iso_to_epoch_millis("2026-02-30T08:00:00.123Z"), None);
        assert_eq!(parse_iso_to_epoch_millis("2026-02-29T08:00:00.123Z"), None);
        assert!(parse_iso_to_epoch_millis("2024-02-29T08:00:00.123Z").is_some());
        assert_eq!(parse_iso_to_epoch_millis("2026-10-13T24:00:00.123Z"), None);
    }

    #[test]
    fn fixed_clock_is_deterministic() {
        let clock = FixedClock::new(1_791_878_400_123);
        assert_eq!(clock.now_iso(), "2026-10-13T08:00:00.123Z");
        clock.advance(7);
        assert_eq!(clock.now_iso(), "2026-10-13T08:00:00.130Z");
    }
}
