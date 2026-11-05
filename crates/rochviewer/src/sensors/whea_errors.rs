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
//!
//! Linux has no WHEA, but it has the same events: the kernel writes machine
//! checks, PCIe AER reports and EDAC memory errors to its log. See
//! `count_errors` for how they are counted there.

#[cfg(windows)]
use std::sync::LazyLock;
use std::sync::OnceLock;
#[cfg(windows)]
use std::time::{SystemTime, UNIX_EPOCH};
use std::time::Instant;

#[cfg(windows)]
use windows::Win32::Foundation::ERROR_NO_MORE_ITEMS;
#[cfg(windows)]
use windows::Win32::System::EventLog::{
    EVT_HANDLE, EvtClose, EvtNext, EvtQuery, EvtQueryChannelPath, EvtQueryReverseDirection,
};
#[cfg(windows)]
use windows::core::HSTRING;

#[cfg(windows)]
use regex::Regex;

use crate::memo::Memo;
#[cfg(windows)]
use crate::wmi_util;

#[cfg(windows)]
pub const WHEA_PROVIDER: &str = "Microsoft-Windows-WHEA-Logger";
#[cfg(windows)]
pub const EVENT_CHANNEL: &str = "System";

/// The query costs about 40 ms, and the Telemetry window ticks once a second.
/// Errors are rare enough that a stale count for a few seconds costs nothing,
/// and the log does not deserve to be walked sixty times a minute.
pub const CACHE_SECONDS: f64 = 5.0;

/// (when it was read, the count) -- the count `None` when the log would not
/// answer.
static CACHE: Memo<(f64, Option<u64>)> = Memo::new();

/// Whether one line of the kernel's log opens a hardware error.
///
/// Each of these writes several lines per event, so what is counted is the one
/// line an event has exactly once, rather than every line that mentions it. A
/// count that said four for one corrected error would be worse than none.
#[cfg(not(windows))]
pub fn kernel_line_is_hardware_error(line: &str) -> bool {
    // PCIe AER: "PCIe Bus Error: severity=Corrected, type=Physical Layer, ...".
    line.contains("PCIe Bus Error:")
        // A machine check as decoded inline, and the summary written instead
        // when mcelog or rasdaemon is collecting them.
        || line.contains("mce: [Hardware Error]: Machine check events logged")
        || line.contains("mce: [Hardware Error]: Machine Check:")
        || line.contains("[Hardware Error]: Corrected error")
        || line.contains("[Hardware Error]: Uncorrected")
        // EDAC: "EDAC MC0: 1 CE memory read error on ...", or UE.
        || (line.contains("EDAC ") && (line.contains(" CE ") || line.contains(" UE ")))
}

/// Return the number of hardware errors logged, or `None` when the log cannot
/// be read.
///
/// `None` rather than zero on failure, as on Windows: zero is a claim that the
/// machine is clean, and a log this could not open has not said that. Seeing the
/// kernel's messages can take the `systemd-journal`, `wheel` or `adm` group, and
/// not having it reads as no answer.
#[cfg(not(windows))]
pub fn count_errors(since_boot: bool) -> Option<u64> {
    let mut journal = std::process::Command::new("journalctl");
    journal.args(["--dmesg", "--no-pager", "--quiet", "--output=cat"]);
    if since_boot {
        journal.arg("--boot");
    }
    let output = journal.stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null()).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    // An empty journal is a journal this user cannot see, not a clean machine:
    // the kernel has always had something to say by the time anyone asks.
    if text.trim().is_empty() {
        return None;
    }
    Some(text.lines().filter(|line| kernel_line_is_hardware_error(line)).count() as u64)
}

/// Seconds since the epoch, now (Python's `time.time`).
#[cfg(windows)]
fn time_now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs_f64())
        .unwrap_or(0.0)
}

#[cfg(windows)]
fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        _ => 28,
    }
}

/// `time.strptime(stamp, "%Y%m%d%H%M%S")`'s own pattern. Its alternations
/// are tried in this order and the match must then cover the whole stamp, or
/// strptime raised ("unconverted data remains").
#[cfg(windows)]
static STRPTIME_STAMP: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(\d\d\d\d)(1[0-2]|0[1-9]|[1-9])(3[01]|[12]\d|0[1-9]|[1-9]| [1-9])(2[0-3]|[0-1]\d|\d)([0-5]\d|\d)(6[0-1]|[0-5]\d|\d)")
        .expect("valid strptime pattern")
});

/// The wall-clock fields of a WMI CIM datetime -- `yyyymmddHHMMSS.ffffff+UUU`
/// -- as `[year, month, day, hour, minute, second]`, or `None` where the
/// Python's `time.strptime(raw[:14], "%Y%m%d%H%M%S")` raised.
///
/// Only the first fourteen characters: the Python never read the offset
/// after them, so a stamp without one is still a boot time.
#[cfg(windows)]
fn parse_cim_stamp(raw: &str) -> Option<[i64; 6]> {
    if raw.chars().count() < 14 {
        return None;
    }
    let stamp: String = raw.chars().take(14).collect();
    let found = STRPTIME_STAMP.captures(&stamp)?;
    if found.get(0)?.end() != stamp.len() {
        return None;
    }
    let field = |index: usize| found[index].trim().parse::<i64>().ok();
    let fields = [field(1)?, field(2)?, field(3)?, field(4)?, field(5)?, field(6)?];
    // strptime builds a date from these, which refuses year 0 and a day
    // past the month's end.
    let [year, month, day, ..] = fields;
    if year < 1 || day > days_in_month(year, month) {
        return None;
    }
    Some(fields)
}

/// Layout of the C runtime's `struct tm`.
#[cfg(windows)]
#[repr(C)]
struct Tm {
    tm_sec: i32,
    tm_min: i32,
    tm_hour: i32,
    tm_mday: i32,
    tm_mon: i32,
    tm_year: i32,
    tm_wday: i32,
    tm_yday: i32,
    tm_isdst: i32,
}

#[cfg(windows)]
unsafe extern "C" {
    /// The UCRT's 64-bit `mktime`, which is what CPython's `time.mktime`
    /// calls on Windows.
    fn _mktime64(tm: *mut Tm) -> i64;
}

/// Python's `time.mktime` on strptime's fields: the wall-clock time read as
/// local time by the C runtime, under its own daylight-saving rules for that
/// date (strptime leaves `tm_isdst` at -1), or `None` where it raised.
#[cfg(windows)]
fn local_mktime(fields: [i64; 6]) -> Option<f64> {
    let [year, month, day, hour, minute, second] = fields;
    let mut tm = Tm {
        tm_sec: second as i32,
        tm_min: minute as i32,
        tm_hour: hour as i32,
        tm_mday: day as i32,
        tm_mon: month as i32 - 1,
        tm_year: (year - 1900) as i32,
        // CPython's sentinel: still -1 afterwards means mktime failed.
        tm_wday: -1,
        tm_yday: 0,
        tm_isdst: -1,
    };
    // SAFETY: `tm` is a valid, exclusively borrowed struct tm.
    let seconds = unsafe { _mktime64(&mut tm) };
    if seconds == -1 && tm.tm_wday == -1 {
        return None;
    }
    Some(seconds as f64)
}

/// Seconds since the epoch when a CIM boot stamp says the session started.
///
/// The Python parsed the first fourteen characters as local time with
/// `time.mktime` and never looked at the offset the string carries. The two
/// agree except across a daylight-saving change, where WMI writes the boot
/// time under the bias in force now and mktime applies the one in force on
/// the boot date; the hour between them decides which WHEA events count, so
/// the Python's reading is kept rather than the offset's.
#[cfg(windows)]
fn boot_time_from(raw: &str, mktime: impl Fn([i64; 6]) -> Option<f64>) -> Option<f64> {
    mktime(parse_cim_stamp(raw)?)
}

/// Seconds since the epoch when this Windows session started, or `None`.
#[cfg(windows)]
fn boot_time() -> Option<f64> {
    let systems = wmi_util::query("Win32_OperatingSystem").ok()?;
    let system = systems.first()?;
    let raw = system.str_or_empty("LastBootUpTime");
    boot_time_from(&raw, local_mktime)
}

/// An XPath selecting WHEA's own events, optionally only recent ones.
#[cfg(windows)]
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
#[cfg(windows)]
struct EvtHandle(EVT_HANDLE);

#[cfg(windows)]
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
#[cfg(windows)]
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

    #[cfg(windows)]
    #[test]
    fn it_selects_the_whea_provider() {
        assert!(query(None).contains("Microsoft-Windows-WHEA-Logger"));
    }

    #[cfg(windows)]
    #[test]
    fn without_a_boot_time_it_asks_for_everything() {
        assert!(!query(None).contains("timediff"));
    }

    #[cfg(windows)]
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

    #[cfg(not(windows))]
    #[test]
    fn each_kernel_event_is_counted_once() {
        // One AER report, as the kernel writes it: four lines, one error.
        let aer = [
            "pcieport 0000:00:1c.0: AER: Corrected error message received from 0000:02:00.0",
            "nvme 0000:02:00.0: PCIe Bus Error: severity=Corrected, type=Physical Layer, (Receiver ID)",
            "nvme 0000:02:00.0:   device [10ec:5772] error status/mask=00000001/0000e000",
            "nvme 0000:02:00.0:    [ 0] RxErr                  (First)",
        ];
        assert_eq!(aer.iter().filter(|line| kernel_line_is_hardware_error(line)).count(), 1);
        // A machine check, decoded inline.
        let mce = [
            "mce: [Hardware Error]: Machine Check: 0 Bank 5: be00000000800400",
            "mce: [Hardware Error]: TSC 0 ADDR fef1a140 MISC d012000100000000",
            "mce: [Hardware Error]: PROCESSOR 2:c06f2 TIME 1762300000 SOCKET 0 APIC 0",
        ];
        assert_eq!(mce.iter().filter(|line| kernel_line_is_hardware_error(line)).count(), 1);
        assert!(kernel_line_is_hardware_error("mce: [Hardware Error]: Machine check events logged"));
        assert!(kernel_line_is_hardware_error("EDAC MC0: 1 CE memory read error on CPU_SrcID#0_Channel#0_DIMM#0"));
    }

    #[cfg(not(windows))]
    #[test]
    fn an_ordinary_boot_log_counts_nothing() {
        for line in [
            "EDAC MC: Ver: 3.0.0",
            "i801_smbus 0000:80:1f.4: SPD Write Disable is set",
            "pcieport 0000:00:1c.0: AER: enabled with IRQ 123",
            "mce: CPU0: Thermal monitoring enabled (TM1)",
            "RAS: Correctable Errors collector initialized.",
        ] {
            assert!(!kernel_line_is_hardware_error(line), "{line}");
        }
    }

    #[cfg(windows)]
    mod boot_time {
    use super::*;

    // -- The boot-time parse the query is built from -----------------------

    /// A stand-in for the C runtime's mktime at a fixed UTC-4, so the tests
    /// do not depend on the machine's time zone.
    fn utc_minus_4(fields: [i64; 6]) -> Option<f64> {
        let [year, month, day, hour, minute, second] = fields;
        let days = (1970..year).map(|y| if days_in_month(y, 2) == 29 { 366 } else { 365 }).sum::<i64>()
            + (1..month).map(|m| days_in_month(year, m)).sum::<i64>()
            + day
            - 1;
        Some((days * 86_400 + hour * 3600 + minute * 60 + second + 4 * 3600) as f64)
    }

    #[test]
    fn the_boot_stamp_is_local_time_and_its_offset_is_not_read() {
        // Python 1.1.0 reads raw[:14] through strptime and mktime; the
        // "-300" WMI wrote is never consulted, so under a UTC-4 mktime this
        // is 15:00 UTC, not the 16:00 the offset would give.
        assert_eq!(boot_time_from("20261031110000.000000-300", utc_minus_4), Some(1_793_458_800.0));
        assert_eq!(boot_time_from("20261031110000.000000-240", utc_minus_4), Some(1_793_458_800.0));
        // And a stamp with no offset at all is still a boot time.
        assert_eq!(boot_time_from("20261031110000", utc_minus_4), Some(1_793_458_800.0));
    }

    #[test]
    fn the_stamp_is_parsed_as_strptime_parses_it() {
        // Each of these raised in the Python (short, a month past 12 that
        // leaves "unconverted data", 31 February, year 0, a non-digit).
        for raw in ["2026101608", "20261316082857", "20260231083000", "00000101000000", "2026101608285x"] {
            assert_eq!(parse_cim_stamp(raw), None, "{raw}");
        }
        assert_eq!(parse_cim_stamp("20261016082857.500000-240"), Some([2026, 10, 16, 8, 28, 57]));
    }

    #[test]
    fn mktime_is_the_c_runtimes() {
        // Whatever the zone, mktime and the UTC fields it reports back agree
        // with the epoch: 2026-10-16 08:28:57 local is within a day of
        // 1792139337 (the same fields read as UTC).
        let seconds = local_mktime([2026, 10, 16, 8, 28, 57]).unwrap();
        assert!((seconds - 1_792_139_337.0).abs() <= 14.0 * 3600.0, "{seconds}");
    }

    #[test]
    fn boot_time_comes_from_wmi() {
        use crate::wmi_util::{FakeWmi, WmiObject};
        FakeWmi::new()
            .class(
                "Win32_OperatingSystem",
                vec![WmiObject::new().with("LastBootUpTime", "20261016082857.500000-240")],
            )
            .install();
        assert_eq!(boot_time(), local_mktime([2026, 10, 16, 8, 28, 57]));
        FakeWmi::new().failing("Win32_OperatingSystem").install();
        assert_eq!(boot_time(), None);
    }

    #[test]
    #[ignore = "needs the machine's event log"]
    fn the_live_log_answers() {
        assert!(count_errors(false).is_some());
    }
    }
}
