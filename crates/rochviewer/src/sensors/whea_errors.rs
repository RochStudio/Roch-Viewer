//! How many hardware errors Windows has logged, and when the last one was.
//!
//! WHEA is the channel firmware and the CPU use to tell Windows that something
//! went wrong at the hardware level: a corrected memory error, a PCIe error, a
//! machine check. The count is the reading a memory tuner actually wants beside
//! the timings -- a kit that boots and passes a benchmark can still be quietly
//! correcting errors, and this is where that shows.
//!
//! Read from the event log rather than from hardware. The errors themselves are
//! in machine-check registers this project has no path to, but Windows has
//! already collected them, and a count it has written down is a reading rather
//! than a guess.
//!
//! Counted since the last boot, not for all time, because the question being
//! asked is about this configuration: a corrected error from three BIOS versions
//! ago says nothing about the settings in front of you.

use std::sync::OnceLock;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use windows::Win32::Foundation::ERROR_NO_MORE_ITEMS;
use windows::Win32::System::EventLog::{
    EVT_HANDLE, EvtClose, EvtNext, EvtQuery, EvtQueryChannelPath, EvtQueryReverseDirection,
};
use windows::core::HSTRING;

use crate::memo::Memo;
use crate::wmi_util;

pub const WHEA_PROVIDER: &str = "Microsoft-Windows-WHEA-Logger";
pub const EVENT_CHANNEL: &str = "System";

/// The query costs about 40 ms, and the Telemetry window ticks once a second.
/// Errors are rare enough that a stale count for a few seconds costs nothing,
/// and the log does not deserve to be walked sixty times a minute.
pub const CACHE_SECONDS: f64 = 5.0;

/// (when it was read, the count) -- the count `None` when the log would not
/// answer.
static CACHE: Memo<(f64, Option<u64>)> = Memo::new();

/// Seconds since the epoch, now (Python's `time.time`).
fn time_now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs_f64())
        .unwrap_or(0.0)
}

/// Days from 1970-01-01 to a proleptic Gregorian date.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let yoe = year - era * 400;
    let doy = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        _ => 28,
    }
}

/// A WMI CIM datetime -- `yyyymmddHHMMSS.ffffff+UUU` -- as seconds since the
/// epoch, or `None`.
///
/// The Python parsed the first fourteen characters as local time with
/// `time.mktime`. The CIM string carries its own offset from UTC in minutes,
/// which is that same local offset, so it is applied directly rather than
/// asking the time-zone rules again.
fn parse_cim_datetime(raw: &str) -> Option<f64> {
    if raw.len() < 14 || !raw.is_char_boundary(14) {
        return None;
    }
    let stamp = &raw[..14];
    if !stamp.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let field = |range: std::ops::Range<usize>| stamp[range].parse::<i64>().ok();
    let (year, month, day) = (field(0..4)?, field(4..6)?, field(6..8)?);
    let (hour, minute, second) = (field(8..10)?, field(10..12)?, field(12..14)?);
    if !(1..=12).contains(&month)
        || !(1..=days_in_month(year, month)).contains(&day)
        || hour > 23
        || minute > 59
        || second > 61
    {
        return None;
    }
    let offset_minutes = raw.get(21..25).and_then(|offset| {
        let (sign, digits) = offset.split_at(1);
        let minutes = digits.parse::<i64>().ok()?;
        match sign {
            "+" => Some(minutes),
            "-" => Some(-minutes),
            _ => None,
        }
    })?;
    let local = days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second;
    Some((local - offset_minutes * 60) as f64)
}

/// Seconds since the epoch when this Windows session started, or `None`.
fn boot_time() -> Option<f64> {
    let systems = wmi_util::query("Win32_OperatingSystem").ok()?;
    let system = systems.first()?;
    let raw = system.str_or_empty("LastBootUpTime");
    parse_cim_datetime(&raw)
}

/// An XPath selecting WHEA's own events, optionally only recent ones.
fn query(since: Option<f64>) -> String {
    let provider = format!("Provider[@Name='{WHEA_PROVIDER}']");
    let Some(since) = since else {
        return format!("*[System[{provider}]]");
    };
    // TimeCreated takes milliseconds back from now, which avoids having to
    // render a timestamp in the format the log wants.
    let milliseconds = (((time_now() - since) * 1000.0) as i64).max(0);
    format!("*[System[{provider} and TimeCreated[timediff(@SystemTime) <= {milliseconds}]]]")
}

/// Closes an event-log handle when dropped.
struct EvtHandle(EVT_HANDLE);

impl Drop for EvtHandle {
    fn drop(&mut self) {
        // SAFETY: the handle was returned by the event log and is closed once.
        unsafe {
            let _ = EvtClose(self.0);
        }
    }
}

/// Return the number of WHEA events, or `None` when the log cannot be read.
///
/// `None` rather than zero on failure. Zero is a claim that the machine is
/// clean, and a log this could not open has not said that.
pub fn count_errors(since_boot: bool) -> Option<u64> {
    let since = if since_boot { boot_time() } else { None };
    // SAFETY: plain wevtapi calls on handles this function owns.
    let handle = unsafe {
        EvtQuery(
            None,
            &HSTRING::from(EVENT_CHANNEL),
            &HSTRING::from(query(since)),
            EvtQueryChannelPath.0 | EvtQueryReverseDirection.0,
        )
    }
    .ok()?;
    let handle = EvtHandle(handle);

    let mut found = 0u64;
    let mut events = [0isize; 64];
    loop {
        let mut returned = 0u32;
        // SAFETY: `events` holds 64 slots and `returned` says how many were
        // filled; each filled one is closed below.
        let next = unsafe { EvtNext(handle.0, &mut events, u32::MAX, 0, &mut returned) };
        match next {
            Ok(()) => {
                let returned = (returned as usize).min(events.len());
                if returned == 0 {
                    break;
                }
                for event in &events[..returned] {
                    drop(EvtHandle(EVT_HANDLE(*event)));
                }
                found += returned as u64;
            }
            Err(error) if error.code() == ERROR_NO_MORE_ITEMS.to_hresult() => break,
            // A partial walk is still a lower bound, but reporting it as the
            // count would understate the machine's errors. Say nothing instead.
            Err(_) => return None,
        }
    }
    Some(found)
}

/// The row's text for a count: the number, or an em dash when the log did
/// not answer.
pub fn error_text_for(count: Option<u64>) -> String {
    match count {
        Some(count) => count.to_string(),
        None => "—".into(),
    }
}

/// The row's text: a count, or an em dash when the log did not answer.
///
/// "Not supplied" and "the log would not answer" are both absences, and they
/// have to be told apart: one means go and read, the other means say nothing.
/// So the reading form is this one, and a supplied count is
/// [`error_text_for`].
pub fn error_text() -> String {
    error_text_for(cached_count())
}

/// Seconds on a clock that only moves forward (Python's `time.monotonic`).
fn monotonic() -> f64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64()
}

/// The count, re-read at most every CACHE_SECONDS.
pub fn cached_count() -> Option<u64> {
    cached_count_at(monotonic(), || count_errors(true))
}

/// [`cached_count`] at a given moment, counting with `count`.
fn cached_count_at(now: f64, count: impl FnOnce() -> Option<u64>) -> Option<u64> {
    if let Some((_read_at, value)) = CACHE.get().filter(|(read_at, _)| now - read_at < CACHE_SECONDS) {
        return value;
    }
    let value = count();
    CACHE.set((now, value));
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    // -- QueryTest: the XPath asks for WHEA's own events, and only recent ones.

    #[test]
    fn it_selects_the_whea_provider() {
        assert!(query(None).contains("Microsoft-Windows-WHEA-Logger"));
    }

    #[test]
    fn without_a_boot_time_it_asks_for_everything() {
        assert!(!query(None).contains("timediff"));
    }

    #[test]
    fn with_a_boot_time_it_asks_only_since_then() {
        // A corrected error from three BIOS versions ago says nothing about
        // the settings in front of you.
        let text = query(Some(time_now() - 60.0));
        assert!(text.contains("timediff"));
    }

    // -- ErrorTextTest -----------------------------------------------------

    #[test]
    fn a_count_reads_as_a_number() {
        assert_eq!(error_text_for(Some(0)), "0");
        assert_eq!(error_text_for(Some(3)), "3");
    }

    #[test]
    fn an_unreadable_log_is_not_zero() {
        // Zero is a claim that the machine is clean. A log that would not
        // open has not said that.
        assert_eq!(error_text_for(None), "\u{2014}");
    }

    // -- CacheTest ---------------------------------------------------------

    #[test]
    fn the_log_is_not_walked_once_a_second() {
        let counted = Cell::new(0);
        let count = || {
            counted.set(counted.get() + 1);
            Some(0)
        };
        cached_count_at(100.0, count);
        cached_count_at(101.0, count);
        cached_count_at(102.0, count);
        assert_eq!(counted.get(), 1);
    }

    #[test]
    fn it_is_re_read_once_the_cache_is_old() {
        let counted = Cell::new(0);
        let count = || {
            counted.set(counted.get() + 1);
            Some(0)
        };
        cached_count_at(100.0, count);
        cached_count_at(100.0 + CACHE_SECONDS + 1.0, count);
        assert_eq!(counted.get(), 2);
    }

    #[test]
    fn a_failed_read_is_cached_as_a_failure_not_a_zero() {
        assert_eq!(cached_count_at(100.0, || None), None);
        assert_eq!(cached_count_at(101.0, || Some(0)), None);
    }

    // -- The boot-time parse the query is built from -----------------------

    #[test]
    fn a_cim_datetime_is_read_with_its_own_offset() {
        // 2026-10-16 08:30:00 at UTC+2 is 06:30:00 UTC.
        assert_eq!(parse_cim_datetime("20261016083000.500000+120"), Some(1_792_132_200.0));
        assert_eq!(parse_cim_datetime("19700101000000.000000+000"), Some(0.0));
        assert_eq!(parse_cim_datetime("19700101000000.000000-060"), Some(3600.0));
        assert_eq!(parse_cim_datetime("2026101608"), None);
        assert_eq!(parse_cim_datetime("20260231083000.000000+000"), None);
    }

    #[test]
    fn boot_time_comes_from_wmi() {
        use crate::wmi_util::{FakeWmi, WmiObject};
        FakeWmi::new()
            .class(
                "Win32_OperatingSystem",
                vec![WmiObject::new().with("LastBootUpTime", "19700102000000.000000+000")],
            )
            .install();
        assert_eq!(boot_time(), Some(86_400.0));
        FakeWmi::new().failing("Win32_OperatingSystem").install();
        assert_eq!(boot_time(), None);
    }

    #[test]
    #[ignore = "needs the machine's event log"]
    fn the_live_log_answers() {
        assert!(count_errors(false).is_some());
    }
}
