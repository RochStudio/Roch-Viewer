//! Live CPU core clocks, without a driver.
//!
//! The clocks a tuner watches -- BCLK, ring, MCLK -- all come out of MCHBAR and
//! are already read. Core clock is the one that does not: the P-state ratio lives
//! in IA32_PERF_STATUS, and this project's driver maps physical memory and I/O
//! ports but has no RDMSR. So the number HWiNFO shows next to each core is not
//! reachable the way HWiNFO reaches it.
//!
//! Two routes were measured before this one was chosen:
//!
//! ```text
//!   CallNtPowerInformation  0.013 ms, and useless. CurrentMhz reads 3187 for
//!                           every logical processor on this bench -- the nominal
//!                           frequency, not the running one -- while the cores
//!                           are at 5.7 GHz. A constant that looks like a reading.
//!   WMI perf class          real numbers, 452 ms per query. The telemetry window
//!                           ticks once a second; half a second of that on one
//!                           group is not a cost this project pays.
//!   PDH                     the same counters WMI wraps, read natively: 0.12 ms.
//! ```
//!
//! So this is PDH. What it gives is an average over the sampling interval rather
//! than an instantaneous ratio, which is why it reads a little under HWiNFO --
//! 5436-5671 MHz per thread against HWiNFO's 5700 while it was open beside it.
//! That is a different measurement, not a worse one, and the rows are named for
//! what it is.
//!
//! SAFETY - read-only, and no driver involved: performance counters any user can
//! read, through PdhAddEnglishCounter so the paths do not depend on the display
//! language.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use windows::Win32::System::Performance::{
    PDH_FMT, PDH_FMT_COUNTERVALUE_ITEM_W, PDH_HCOUNTER, PDH_HQUERY, PdhAddEnglishCounterW,
    PdhCollectQueryData, PdhGetFormattedCounterArrayW, PdhOpenQueryW,
};
use windows::Win32::System::Threading::{ALL_PROCESSOR_GROUPS, GetActiveProcessorCount};
use windows::core::{HSTRING, PCWSTR};

pub const PDH_FMT_DOUBLE: u32 = 0x0000_0200;
pub const PDH_MORE_DATA: u32 = 0x8000_07D2;
pub const PDH_CSTATUS_VALID_DATA: u32 = 0x0000_0000;
pub const PDH_CSTATUS_NEW_DATA: u32 = 0x0000_0001;

// "Processor Information" rather than "Processor": the older object does not
// break out processor groups, and reports nothing useful above 64 threads.
pub const PERFORMANCE_COUNTER: &str = r"\Processor Information(*)\% Processor Performance";
pub const UTILITY_COUNTER: &str = r"\Processor Information(*)\% Processor Utility";
pub const FREQUENCY_COUNTER: &str = r"\Processor Information(_Total)\Processor Frequency";

/// Instances PDH adds on top of the per-thread ones. "_Total" is the machine
/// and "0,_Total" is processor group 0; both are aggregates and neither is a
/// core, so a maximum taken across instances has to exclude them.
pub const TOTAL_INSTANCE_SUFFIX: &str = "_Total";

/// A performance ratio is a percentage of nominal, and turbo puts it above 100.
/// Past this it is not a clock, it is a counter that wrapped.
pub const MAX_PERFORMANCE_PERCENT: f64 = 1000.0;

/// The nominal frequency is megahertz, not a percentage, and needs its own
/// bound. Sharing the percentage one rejected a perfectly good 3187 MHz and
/// left every clock row blank.
pub const MAX_NOMINAL_MHZ: f64 = 100000.0;

/// One collection serves every row that asks within this window.
///
/// These are rate counters: each collection is measured against the one before
/// it, so two taken microseconds apart divide by almost nothing. The three
/// clock rows read in the same tick did exactly that, and the second and third
/// came back at 7410 MHz -- the first row's real 5675 MHz, then noise. Sharing
/// one collection across the tick is what makes them the same measurement.
pub const MIN_SAMPLE_INTERVAL_S: f64 = 0.25;

/// One counter's instances: `(instance name, value)`.
pub type CounterRows = Vec<(String, f64)>;

/// One collection: counter key ("performance", "utility", "frequency") to
/// its instances. Shared, so every row served from one collection holds the
/// very same sample.
pub type Sample = Arc<BTreeMap<&'static str, CounterRows>>;

/// The PDH calls a query is made of, so the sharing rule can be exercised
/// without the interop.
trait PdhSource: Send {
    /// Open the query and add the counters; `false` when it cannot be done.
    fn open(&mut self) -> bool;
    /// `PdhCollectQueryData`: 0 on success.
    fn collect_query_data(&mut self) -> u32;
    /// Every added counter's instances.
    fn arrays(&mut self) -> BTreeMap<&'static str, CounterRows>;
}

/// One open PDH query, kept for the life of the process.
///
/// Opening a query and adding counters costs more than collecting from one,
/// and a rate counter needs two collections before it means anything, so a
/// query per tick would never produce a reading at all.
struct Pdh<S: PdhSource> {
    source: S,
    monotonic: fn() -> f64,
    opened: bool,
    primed: bool,
    failed: bool,
    sampled_at: Option<f64>,
    sample: Sample,
}

impl<S: PdhSource> Pdh<S> {
    fn new(source: S, monotonic: fn() -> f64) -> Self {
        Pdh {
            source,
            monotonic,
            opened: false,
            primed: false,
            failed: false,
            sampled_at: None,
            sample: Arc::new(BTreeMap::new()),
        }
    }

    fn open(&mut self) -> bool {
        if self.opened || self.failed {
            return self.opened;
        }
        if self.source.open() {
            self.opened = true;
        } else {
            self.failed = true;
        }
        self.opened
    }

    /// Return `{key: [(instance, value)]}`, or an empty sample when
    /// unavailable.
    fn collect(&mut self) -> Sample {
        if !self.open() {
            return Arc::new(BTreeMap::new());
        }
        let now = (self.monotonic)();
        if self.sampled_at.is_some_and(|sampled_at| now - sampled_at < MIN_SAMPLE_INTERVAL_S) {
            return self.sample.clone();
        }
        if self.source.collect_query_data() != 0 {
            return Arc::new(BTreeMap::new());
        }
        // The first collection after opening establishes the baseline
        // a rate counter is measured against; it has no value yet.
        //
        // Stamped as though it were a sample, so the interval guard
        // above holds the next collection off until a real interval
        // has passed. Without that, the row after this one collected
        // microseconds later and read 5834 MHz -- and the telemetry
        // window keeps maxima for the session, so one spurious first
        // sample would have sat in the Max column until it was reset.
        if !self.primed {
            self.primed = true;
            self.sampled_at = Some(now);
            self.sample = Arc::new(BTreeMap::new());
            return Arc::new(BTreeMap::new());
        }
        self.sample = Arc::new(self.source.arrays());
        self.sampled_at = Some(now);
        self.sample.clone()
    }
}

/// The real PDH query.
struct WindowsPdh {
    query: PDH_HQUERY,
    counters: Vec<(&'static str, PDH_HCOUNTER)>,
}

// SAFETY: the query and counter handles are opaque PDH handles, used only
// while the owning `Pdh` is locked.
unsafe impl Send for WindowsPdh {}

impl WindowsPdh {
    fn new() -> Self {
        WindowsPdh { query: PDH_HQUERY::default(), counters: Vec::new() }
    }

    fn array(&self, counter: PDH_HCOUNTER) -> CounterRows {
        let mut size = 0u32;
        let mut count = 0u32;
        // SAFETY: a sizing call with no buffer, as PDH documents.
        let status = unsafe {
            PdhGetFormattedCounterArrayW(counter, PDH_FMT(PDH_FMT_DOUBLE), &mut size, &mut count, None)
        };
        if status != PDH_MORE_DATA || count == 0 {
            return Vec::new();
        }
        // The items, then the instance names they point into: `size` bytes.
        // PDH_FMT_COUNTERVALUE_ITEM_W's union is eight-byte aligned, so the
        // status word inside each item is followed by padding (the windows
        // crate's struct lays that out), and the buffer is allocated in
        // eight-byte units to match.
        let mut buffer = vec![0u64; (size as usize).div_ceil(8)];
        let items = buffer.as_mut_ptr() as *mut PDH_FMT_COUNTERVALUE_ITEM_W;
        // SAFETY: the buffer holds the `size` bytes PDH asked for.
        let status = unsafe {
            PdhGetFormattedCounterArrayW(counter, PDH_FMT(PDH_FMT_DOUBLE), &mut size, &mut count, Some(items))
        };
        if status != 0 {
            return Vec::new();
        }
        let available = (buffer.len() * 8) / std::mem::size_of::<PDH_FMT_COUNTERVALUE_ITEM_W>();
        let count = (count as usize).min(available);
        // SAFETY: PDH filled `count` items at the start of the buffer.
        let items = unsafe { std::slice::from_raw_parts(items, count) };
        items
            .iter()
            .filter(|item| {
                item.FmtValue.CStatus == PDH_CSTATUS_VALID_DATA
                    || item.FmtValue.CStatus == PDH_CSTATUS_NEW_DATA
            })
            .map(|item| {
                // SAFETY: the name points into the buffer, and PDH_FMT_DOUBLE
                // selects the union's double.
                let name = unsafe { item.szName.to_string() }.unwrap_or_default();
                (name, unsafe { item.FmtValue.Anonymous.doubleValue })
            })
            .collect()
    }
}

impl PdhSource for WindowsPdh {
    fn open(&mut self) -> bool {
        let mut query = PDH_HQUERY::default();
        // SAFETY: plain PDH calls on handles this object owns for the life of
        // the process.
        unsafe {
            if PdhOpenQueryW(PCWSTR::null(), 0, &mut query) != 0 {
                return false;
            }
            let mut counters = Vec::new();
            for (key, path) in [
                ("performance", PERFORMANCE_COUNTER),
                ("utility", UTILITY_COUNTER),
                ("frequency", FREQUENCY_COUNTER),
            ] {
                let mut handle = PDH_HCOUNTER::default();
                if PdhAddEnglishCounterW(query, &HSTRING::from(path), 0, &mut handle) != 0 {
                    continue;
                }
                counters.push((key, handle));
            }
            if !counters.iter().any(|(key, _)| *key == "performance") {
                // No processor performance counter.
                return false;
            }
            PdhCollectQueryData(query);
            self.query = query;
            self.counters = counters;
        }
        true
    }

    fn collect_query_data(&mut self) -> u32 {
        // SAFETY: the query was opened by `open`.
        unsafe { PdhCollectQueryData(self.query) }
    }

    fn arrays(&mut self) -> BTreeMap<&'static str, CounterRows> {
        self.counters.iter().map(|(key, handle)| (*key, self.array(*handle))).collect()
    }
}

/// Seconds on a clock that only moves forward (Python's `time.monotonic`).
fn monotonic() -> f64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64()
}

static QUERY: OnceLock<Mutex<Pdh<WindowsPdh>>> = OnceLock::new();

/// One collection from the process's query.
fn collect() -> Sample {
    let query = QUERY.get_or_init(|| Mutex::new(Pdh::new(WindowsPdh::new(), monotonic)));
    query.lock().unwrap_or_else(|e| e.into_inner()).collect()
}

fn rows<'a>(sample: &'a BTreeMap<&'static str, CounterRows>, key: &str) -> &'a [(String, f64)] {
    sample.get(key).map(Vec::as_slice).unwrap_or(&[])
}

/// Drop the aggregate instances, leaving one entry per logical processor.
fn per_core(rows: &[(String, f64)]) -> Vec<f64> {
    rows.iter()
        .filter(|(name, value)| {
            !name.ends_with(TOTAL_INSTANCE_SUFFIX) && 0.0 <= *value && *value <= MAX_PERFORMANCE_PERCENT
        })
        .map(|(_, value)| *value)
        .collect()
}

/// The machine-wide instance, which PDH names "_Total" exactly.
///
/// The bound is a parameter because these rows are not all percentages: the
/// nominal frequency comes through the same shape in megahertz
/// (`MAX_PERFORMANCE_PERCENT` is the usual bound).
fn total(rows: &[(String, f64)], maximum: f64) -> Option<f64> {
    let (_, value) = rows.iter().find(|(name, _)| name == TOTAL_INSTANCE_SUFFIX)?;
    (0.0 <= *value && *value <= maximum).then_some(*value)
}

/// Sort key for a PDH processor instance.
///
/// They are named "group,processor" -- "0,0", "0,1", "0,10" -- which sorts
/// wrong as text: "0,10" lands between "0,1" and "0,2", so a list of cores
/// ordered by name is not ordered by core.
fn instance_order(name: &str) -> Vec<i64> {
    name.split(',')
        .map(|part| part.trim().parse::<i64>())
        .collect::<Result<Vec<_>, _>>()
        .unwrap_or_else(|_| vec![1 << 30])
}

/// Name one logical processor by its place in the ordered list.
///
/// Logical processor, not core: which of them share a physical core, and
/// which are performance or efficiency cores, is topology this has no way to
/// read. A plain number claims nothing extra.
///
/// By position rather than by the number inside the instance name, because
/// that number restarts in each processor group -- a machine with two groups
/// has an instance "0,0" and an instance "1,0", and naming both "CPU 0"
/// would collide.
pub fn processor_label(position: usize) -> String {
    format!("CPU {position}")
}

/// How many logical processors to expect, without reading a counter.
///
/// The rows are built at import, and the counters cannot answer then: these
/// are rates, so the first collection only establishes a baseline and the
/// real one comes an interval later. Asking the counters for the row list
/// returned nothing and the breakdown was simply absent.
///
/// Windows' own count is the same set PDH enumerates, and needs no sample.
pub fn core_count() -> usize {
    // SAFETY: a plain query with no arguments to get wrong.
    unsafe { GetActiveProcessorCount(ALL_PROCESSOR_GROUPS) as usize }
}

/// `[(label, megahertz)]` effective clock per logical processor, or [].
///
/// Shares a collection with read_clocks through the interval cache, so a
/// parent row and its expanded children all describe the same sample.
pub fn read_core_clocks() -> Vec<(String, f64)> {
    read_core_clocks_from(&collect())
}

/// [`read_core_clocks`] over one given collection.
pub fn read_core_clocks_from(sample: &BTreeMap<&'static str, CounterRows>) -> Vec<(String, f64)> {
    let utility = rows(sample, "utility");
    if utility.is_empty() {
        return Vec::new();
    }
    let Some(nominal) = total(rows(sample, "frequency"), MAX_NOMINAL_MHZ).filter(|n| *n != 0.0)
    else {
        return Vec::new();
    };
    let mut cores: Vec<&(String, f64)> = utility
        .iter()
        .filter(|(name, value)| {
            !name.ends_with(TOTAL_INSTANCE_SUFFIX) && 0.0 <= *value && *value <= MAX_PERFORMANCE_PERCENT
        })
        .collect();
    cores.sort_by_key(|(name, _)| instance_order(name));
    cores
        .into_iter()
        .enumerate()
        .map(|(position, (_name, value))| (processor_label(position), nominal * value / 100.0))
        .collect()
}

/// One logical processor's effective clock, or `None`.
pub fn core_clock_text(index: usize) -> Option<String> {
    let cores = read_core_clocks();
    let (_, megahertz) = cores.get(index)?;
    Some(format!("{megahertz:.0} MHz"))
}

/// The logical processors to draw rows for, in order.
///
/// From the count rather than from a reading; see core_count. A processor
/// the counters turn out not to report reads nothing, which is the same
/// thing every unreadable row in this project does.
pub fn core_labels() -> Vec<String> {
    (0..core_count()).map(processor_label).collect()
}

/// Return `{key: megahertz}` for the core clocks, or {}.
///
/// ```text
/// core_avg       what the busy cores averaged over the interval.
/// core_max       the fastest single logical processor.
/// core_effective the same scale with idle time counted, which is the
///                quantity HWiNFO calls the effective clock.
/// ```
pub fn read_clocks() -> BTreeMap<&'static str, f64> {
    read_clocks_from(&collect())
}

/// [`read_clocks`] over one given collection.
pub fn read_clocks_from(sample: &BTreeMap<&'static str, CounterRows>) -> BTreeMap<&'static str, f64> {
    let mut found = BTreeMap::new();
    let performance = rows(sample, "performance");
    if performance.is_empty() {
        return found;
    }
    let Some(nominal) = total(rows(sample, "frequency"), MAX_NOMINAL_MHZ).filter(|n| *n != 0.0)
    else {
        return found;
    };

    if let Some(total) = total(performance, MAX_PERFORMANCE_PERCENT) {
        found.insert("core_avg", nominal * total / 100.0);
    }
    let cores = per_core(performance);
    if let Some(max) = cores.iter().copied().reduce(f64::max) {
        found.insert("core_max", nominal * max / 100.0);
    }
    if let Some(utility) = total(rows(sample, "utility"), MAX_PERFORMANCE_PERCENT) {
        found.insert("core_effective", nominal * utility / 100.0);
    }
    found
}

/// One clock, written the way the other clock rows are, or `None`.
pub fn clock_text(key: &str) -> Option<String> {
    read_clocks().get(key).map(|megahertz| format!("{megahertz:.0} MHz"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// One collection from the Z790-P bench, idling: sixteen logical processors
    /// around 176% of a 3187 MHz nominal, plus the two aggregate instances PDH
    /// adds. Utility is the same scale with idle time counted.
    fn bench_rows() -> BTreeMap<&'static str, CounterRows> {
        let mut performance: CounterRows =
            (0..16).map(|core| (format!("0,{core}"), 176.0 + core as f64 * 0.1)).collect();
        performance.extend([("0,_Total".to_string(), 173.8), ("_Total".to_string(), 173.8)]);
        let mut utility: CounterRows = (0..16).map(|core| (format!("0,{core}"), 2.0)).collect();
        utility.extend([("0,_Total".to_string(), 2.9), ("_Total".to_string(), 2.9)]);
        BTreeMap::from([
            ("performance", performance),
            ("utility", utility),
            ("frequency", vec![("_Total".to_string(), 3187.0)]),
        ])
    }

    fn bench(overrides: &[(&'static str, CounterRows)]) -> BTreeMap<&'static str, CounterRows> {
        let mut rows = bench_rows();
        for (key, value) in overrides {
            rows.insert(key, value.clone());
        }
        rows
    }

    fn named(rows: &[(&str, f64)]) -> CounterRows {
        rows.iter().map(|(name, value)| (name.to_string(), *value)).collect()
    }

    fn assert_close(actual: f64, expected: f64, places: i32) {
        assert!(
            (actual - expected).abs() < 0.5 * 10f64.powi(-places),
            "{actual} != {expected} to {places} places"
        );
    }

    // -- InstanceSelectionTest ---------------------------------------------

    #[test]
    fn the_total_is_the_machine_wide_instance() {
        assert_eq!(total(&bench_rows()["performance"], MAX_PERFORMANCE_PERCENT), Some(173.8));
    }

    #[test]
    fn the_group_total_is_not_mistaken_for_the_machine() {
        // PDH names them "_Total" and "0,_Total". Matching on a suffix would
        // take whichever came first.
        let rows = named(&[("0,_Total", 1.0), ("_Total", 2.0)]);
        assert_eq!(total(&rows, MAX_PERFORMANCE_PERCENT), Some(2.0));
    }

    #[test]
    fn per_core_excludes_every_aggregate() {
        let cores = per_core(&bench_rows()["performance"]);
        assert_eq!(cores.len(), 16);
        assert!(!cores.contains(&173.8));
    }

    #[test]
    fn a_wrapped_counter_is_not_a_clock() {
        let rows = named(&[("0,0", MAX_PERFORMANCE_PERCENT + 1.0), ("0,1", 176.0)]);
        assert_eq!(per_core(&rows), vec![176.0]);
    }

    #[test]
    fn the_nominal_frequency_has_its_own_bound() {
        // It comes through the same shape in megahertz, not as a percentage.
        // Sharing the percentage bound rejected a good 3187 MHz and left every
        // clock row blank.
        let rows = bench_rows();
        let frequency = &rows["frequency"];
        assert!(frequency[0].1 > MAX_PERFORMANCE_PERCENT);
        assert_eq!(total(frequency, MAX_NOMINAL_MHZ), Some(3187.0));
        assert_eq!(total(frequency, MAX_PERFORMANCE_PERCENT), None);
    }

    // -- ReadClocksTest ----------------------------------------------------

    #[test]
    fn the_bench_sample_gives_the_clocks_it_was_running() {
        let found = read_clocks_from(&bench_rows());
        assert_close(found["core_avg"], 3187.0 * 1.738, 1);
        assert_close(found["core_max"], 3187.0 * 1.775, 1);
        assert_close(found["core_effective"], 3187.0 * 0.029, 1);
    }

    #[test]
    fn the_maximum_is_a_core_and_not_the_average() {
        let found = read_clocks_from(&bench_rows());
        assert!(found["core_max"] > found["core_avg"]);
    }

    #[test]
    fn the_effective_clock_is_far_below_the_core_clock_at_idle() {
        // The distinction the two rows exist to show: the cores run at 5.5 GHz
        // when they run, and hardly run at all.
        let found = read_clocks_from(&bench_rows());
        assert!(found["core_effective"] < found["core_avg"] / 10.0);
    }

    #[test]
    fn no_sample_reports_nothing_rather_than_zero() {
        assert!(read_clocks_from(&BTreeMap::new()).is_empty());
    }

    #[test]
    fn a_sample_without_a_nominal_frequency_reports_nothing() {
        // Every value here is a percentage of it, so it is not optional.
        assert!(read_clocks_from(&bench(&[("frequency", vec![])])).is_empty());
    }

    #[test]
    fn a_missing_utility_counter_does_not_lose_the_core_clocks() {
        let found = read_clocks_from(&bench(&[("utility", vec![])]));
        assert!(found.contains_key("core_avg"));
        assert!(!found.contains_key("core_effective"));
    }

    // -- PerCoreTest: the breakdown behind the aggregate row.

    #[test]
    fn one_entry_per_logical_processor() {
        let cores = read_core_clocks_from(&bench_rows());
        assert_eq!(cores.len(), 16);
        let labels: Vec<String> = cores.iter().map(|(label, _)| label.clone()).collect();
        assert_eq!(labels, (0..16).map(|i| format!("CPU {i}")).collect::<Vec<_>>());
    }

    #[test]
    fn the_values_are_the_utility_counter_not_the_performance_one() {
        // Effective clock counts idle time; the performance counter does not.
        // Reading the wrong one would put 5.5 GHz on every idle core.
        for (_label, megahertz) in read_core_clocks_from(&bench_rows()) {
            assert_close(megahertz, 3187.0 * 0.02, 1);
        }
    }

    #[test]
    fn instances_sort_by_processor_and_not_by_name() {
        // PDH names them "0,0", "0,1", "0,10". As text, "0,10" lands between
        // "0,1" and "0,2", so a list ordered by name is not ordered by core.
        let mut names = vec!["0,0", "0,1", "0,10", "0,2"];
        names.sort_by_key(|name| instance_order(name));
        assert_eq!(names, vec!["0,0", "0,1", "0,2", "0,10"]);
    }

    #[test]
    fn a_second_processor_group_does_not_collide() {
        // The number inside the instance name restarts per group, so naming a
        // row after it would give two rows called CPU 0.
        let rows = BTreeMap::from([
            ("utility", named(&[("0,0", 2.0), ("1,0", 4.0), ("_Total", 3.0)])),
            ("frequency", named(&[("_Total", 3187.0)])),
        ]);
        let labels: Vec<String> =
            read_core_clocks_from(&rows).into_iter().map(|(label, _)| label).collect();
        assert_eq!(labels, vec!["CPU 0", "CPU 1"]);
    }

    #[test]
    fn the_aggregates_are_not_listed_as_processors() {
        let cores = read_core_clocks_from(&bench_rows());
        assert!(!cores.iter().any(|(label, _)| label == "_Total"));
    }

    #[test]
    fn no_sample_reports_no_processors() {
        assert!(read_core_clocks_from(&BTreeMap::new()).is_empty());
    }

    #[test]
    fn the_row_list_does_not_depend_on_a_reading() {
        // Rows are built at import, when these rate counters have only a
        // baseline and no value yet. Deriving the list from a reading gave an
        // empty one and the breakdown was simply absent.
        let labels = core_labels();
        assert!(!labels.is_empty());
        assert_eq!(labels[0], processor_label(0));
    }

    // -- SampleSharingTest: rate counters divide by the interval, so the
    // interval must be real.

    // Microseconds on the fake clock, per test thread through a thread-local.
    thread_local! {
        static NOW_US: AtomicU64 = const { AtomicU64::new(0) };
    }

    fn fake_now() -> f64 {
        NOW_US.with(|now| now.load(Ordering::SeqCst)) as f64 / 1e6
    }

    fn advance(seconds: f64) {
        NOW_US.with(|now| now.fetch_add((seconds * 1e6).round() as u64, Ordering::SeqCst));
    }

    /// Stands in for the whole PDH layer: the sharing rule is what is under
    /// test, not the interop.
    struct FakeSource;

    impl PdhSource for FakeSource {
        fn open(&mut self) -> bool {
            true
        }
        fn collect_query_data(&mut self) -> u32 {
            0
        }
        fn arrays(&mut self) -> BTreeMap<&'static str, CounterRows> {
            BTreeMap::from([
                ("performance", named(&[("_Total", 173.8)])),
                ("frequency", named(&[("_Total", 3187.0)])),
            ])
        }
    }

    fn query() -> Pdh<FakeSource> {
        Pdh::new(FakeSource, fake_now)
    }

    #[test]
    fn reads_inside_the_window_share_one_collection() {
        let mut query = query();
        query.collect(); // primes
        advance(MIN_SAMPLE_INTERVAL_S); // first real sample
        let first = query.collect();
        advance(MIN_SAMPLE_INTERVAL_S / 4.0);
        assert!(Arc::ptr_eq(&query.collect(), &first));
    }

    #[test]
    fn priming_holds_the_next_collection_off() {
        // The row after the priming one collected microseconds later and read
        // 5834 MHz. The window keeps maxima for the session, so that one
        // sample would have sat in the Max column until it was reset.
        let mut query = query();
        assert!(query.collect().is_empty());
        advance(MIN_SAMPLE_INTERVAL_S / 100.0);
        assert!(query.collect().is_empty());
    }

    #[test]
    fn a_read_past_the_window_takes_a_fresh_sample() {
        let mut query = query();
        query.collect();
        advance(MIN_SAMPLE_INTERVAL_S);
        let first = query.collect();
        advance(MIN_SAMPLE_INTERVAL_S * 2.0);
        assert!(!Arc::ptr_eq(&query.collect(), &first));
    }

    #[test]
    #[ignore = "needs the machine's performance counters"]
    fn the_live_counters_answer_after_an_interval() {
        read_clocks();
        std::thread::sleep(std::time::Duration::from_millis(600));
        read_clocks();
        std::thread::sleep(std::time::Duration::from_millis(600));
        assert!(read_clocks().contains_key("core_avg"));
    }
}
