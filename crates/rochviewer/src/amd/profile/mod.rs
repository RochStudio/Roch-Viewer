// Roch Viewer -- a read-only memory-controller and timing viewer.
// Copyright (C) 2026 Roch Studio
//
// This file follows ZenStates-Core and ZenTimings by irusanov
// (https://github.com/irusanov), both GPL-3.0. Register numbers, bit fields
// and the bounds applied to decoded values were taken from or checked against
// that work, and the comments below say where. Copyright in those parts
// remains with their authors; this file is distributed under the same licence
// they are, which is what makes that use permitted.
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

//! AM5 timing profile and lazy read-only runtime for Roch Viewer.
//!
//! The Python built `RUNTIME = Am5Runtime()` and `TIMINGS =
//! build_timings(RUNTIME)` as the module was imported. Here
//! [`build_timings`] creates the runtime itself and every row closure shares
//! it through an `Arc`; nothing reads hardware until a row is read. Building
//! the table asks for two things only, both non-privileged: the CPU name
//! (WMI, to pick Ryzen 7000's training rows) and the logical-processor count
//! (to draw one clock row per processor), exactly as the Python did.
//!
//! Two Python helpers have no counterpart because what they managed does
//! not exist here. `_import_call` imported each hardware module inside the
//! getter that needed it, to keep startup light; a Rust module is linked,
//! not imported, and none of them runs until it is called, so the getters
//! call the readers directly. `_wmi_connection` kept one WMI connection for
//! the profile; [`crate::wmi_util`] owns the connection now, one per thread,
//! which is what that cache and its fresh-connection retry were for.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

use regex::Regex;

use crate::amd::adl::PMLOG_SENSORS;
use crate::amd::apob::{
    ApobLayout, GraniteRidgeApobReader, RAW_TRAINING_FIELDS, TrainingValue, TrainingValues,
    find_ccdl_run, find_ccdl_wr,
};
use crate::amd::power_metrics::{METRICS, format_power, metric_by_key};
use crate::amd::smn::SmnTransport;
use crate::amd::smn_mcfg::McfgSmnReader;
use crate::amd::smu_clocks::SmuClocks;
use crate::amd::smu_power::SmuPower;
use crate::amd::smu_voltages::SmuVoltages;
use crate::amd::timings::{ALL_OFFSETS, DecodedChannel, Registers, UMC_BASES, UmcValue, decode_channel};
use crate::hardware::driver_path::DLL_NAME;
use crate::memo::{Memo, MemoMap};
use crate::platform_profiles::{is_granite_ridge_cpu, is_raphael_cpu};
use crate::amd::py_float::PyFloat;
use crate::pyfmt::{float_str, round_int};
use crate::row::{ActionFn, Extra, PredFn, Row, Side, Value};
use crate::sensors::am5_board_rails::{BOARD_FANS, BOARD_VOLTAGES};
use crate::sensors::voltage_rails::{
    PER_MODULE_RAILS, RAILS, SOURCE_PM_TABLE, SOURCE_PMIC, SOURCE_SUPERIO, format_volts, rail_by_key,
};
use crate::wmi_util::{self, WmiValue};

#[cfg(test)]
mod tests;

#[cfg(test)]
mod parity;

#[cfg(test)]
mod scenarios;

pub const EM_DASH: &str = "—";

/// Python's `str.strip()` with no argument, which this profile ported as
/// `trim()`. The two disagree on the four information separators,
/// ``-``: Python strips them as whitespace and Rust keeps them, so a
/// firmware string ending in one read differently.
trait PyStrip {
    fn py_strip(&self) -> &str;
}

impl PyStrip for str {
    fn py_strip(&self) -> &str {
        crate::system_identity::py_strip(self)
    }
}

// Voltages, power limits and temperatures are all live sensor readings, so they
// share one tab rather than being split by which unit they happen to use.
pub const SENSOR_TAB: &str = "Sensors";

// What training settled on, and what the controller was configured with:
// neither is a timing, and each fills a page of its own.
pub const SKEW_TAB: &str = "Training";
// The two Training sections the controller settings and the raw training
// codes are drawn under.
pub const OTHER_SETTINGS_CATEGORY: &str = "Other Settings";
// Raw APOB bytes, not ohms or volts: the heading says so, so the numbers are
// not compared as if they were measurements.
pub const RAW_TRAINING_CATEGORY: &str = "Misc (raw)";

// What the processor was configured to allow, as opposed to what it is
// drawing. The live halves stay on Telemetry, which keeps a maximum and is
// where a limit is worth watching a load against.
pub const LIMITS_CATEGORY: &str = "Limits";

// Privileged reads contend with other monitoring tools for the PCI/SMBus
// mutexes, so a single failure must not blank a row for the whole session.
pub const LAZY_READ_ATTEMPTS: u32 = 3;

// Voltages, currents and power move constantly, so a successful read is only
// reused briefly. Timings are not covered by this: they cannot change without
// a reboot, and re-reading them would burn privileged transactions for nothing.
pub const LIVE_CACHE_SECONDS: f64 = 0.75;

/// The training-block names [`TRAINING_FIELDS`] adds to the raw codes.
const DECODED_TRAINING_FIELDS: &[&str] = &[
    "rtt_nom_wr", "rtt_nom_rd", "rtt_wr", "rtt_park", "rtt_park_dqs",
    "ca_odt_a", "ck_odt_a", "cs_odt_a",
    "ca_odt_b", "ck_odt_b", "cs_odt_b",
    "proc_odt_pu", "proc_odt_pd",
    "proc_ca_ds", "proc_ck_ds", "proc_cs_ds",
    // proc_dq_ds belongs here beside its own pull-up and pull-down: all three
    // come out of the APOB training block. Leaving it off sent it to the UMC
    // decode instead, which has no such field, so value() answered with an em
    // dash for something that had been read correctly. The per-channel path
    // reads the block directly and was never affected, which is why both
    // Summary and Training kept showing it and nothing looked wrong.
    "proc_dq_ds", "proc_dq_ds_pu", "proc_dq_ds_pd",
    "dram_dq_ds_pu", "dram_dq_ds_pd",
    // Ryzen 7000's single processor ODT and DRAM drive strength, where Ryzen
    // 9000 has a pull-up and pull-down of each.
    "proc_odt", "dram_dq_ds",
];

/// Every name [`Am5Runtime::value`] sends to the APOB training block rather
/// than to the UMC register decode: the raw training codes plus the decoded
/// terminations and drive strengths.
pub static TRAINING_FIELDS: LazyLock<BTreeSet<&'static str>> = LazyLock::new(|| {
    RAW_TRAINING_FIELDS
        .iter()
        .map(|(name, _offset)| *name)
        .chain(DECODED_TRAINING_FIELDS.iter().copied())
        .collect()
});

/// One reading as the Python held it: an int, a float, a flag, text, or the
/// em dash it substituted for a value that could not be read.
#[derive(Clone, Debug, PartialEq)]
pub enum Reading {
    Dash,
    Int(i64),
    Float(f64),
    Bool(bool),
    Text(String),
}

impl Reading {
    /// Python's `value == EM_DASH`.
    pub fn is_dash(&self) -> bool {
        match self {
            Reading::Dash => true,
            Reading::Text(text) => text == EM_DASH,
            _ => false,
        }
    }

    /// Python's `float(value)`; `None` where that raised.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Reading::Int(value) => Some(*value as f64),
            Reading::Float(value) => Some(*value),
            Reading::Bool(value) => Some(if *value { 1.0 } else { 0.0 }),
            Reading::Text(text) => {
                let text = text.py_strip();
                text.parse::<f64>().ok().filter(|_| !text.is_empty())
            }
            Reading::Dash => None,
        }
    }

    /// Python's `int(value)`; `None` where that raised.
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Reading::Int(value) => Some(*value),
            Reading::Float(value) if value.is_finite() => Some(value.trunc() as i64),
            Reading::Bool(value) => Some(*value as i64),
            Reading::Text(text) => text.py_strip().parse().ok(),
            _ => None,
        }
    }

    /// Python truthiness.
    pub fn truthy(&self) -> bool {
        match self {
            Reading::Dash => true,
            Reading::Int(value) => *value != 0,
            Reading::Float(value) => *value != 0.0,
            Reading::Bool(value) => *value,
            Reading::Text(text) => !text.is_empty(),
        }
    }
}

/// Python's `str(value)`.
impl fmt::Display for Reading {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Reading::Dash => f.write_str(EM_DASH),
            Reading::Int(value) => write!(f, "{value}"),
            Reading::Float(value) => f.write_str(&float_str(*value)),
            Reading::Bool(true) => f.write_str("True"),
            Reading::Bool(false) => f.write_str("False"),
            Reading::Text(text) => f.write_str(text),
        }
    }
}

impl From<&UmcValue> for Reading {
    fn from(value: &UmcValue) -> Self {
        match value {
            UmcValue::Int(value) => Reading::Int(*value),
            UmcValue::Float(value) => Reading::Float(*value),
            UmcValue::Bool(value) => Reading::Bool(*value),
            UmcValue::Text(value) => Reading::Text(value.clone()),
        }
    }
}

impl From<&TrainingValue> for Reading {
    fn from(value: &TrainingValue) -> Self {
        match value {
            TrainingValue::Text(text) => Reading::Text(text.clone()),
            TrainingValue::Raw(byte) => Reading::Int(*byte as i64),
        }
    }
}

fn current_cpu_name() -> String {
    // Through the same cached lookup as the other processor fields, so
    // platform detection and the identity rows share one WMI connection
    // rather than opening a second one that costs another second.
    processor_facts().text("name").unwrap_or_default()
}

/// Return `message` when a transport has no confirmed offsets yet.
///
/// Fail closed: with nothing confirmed for this platform every row stays blank
/// rather than showing a guess.
///
/// The Python looked the table up by module and attribute name so the import
/// stayed lazy; the tables are constants here, so the caller passes whether
/// the one it means is populated.
fn offsets_gate(confirmed: bool, message: &str) -> String {
    if confirmed {
        return String::new();
    }
    message.to_string()
}

/// What a caught panic said: the Python's `str(exc)`.
fn panic_message(panic: &(dyn std::any::Any + Send)) -> String {
    if let Some(text) = panic.downcast_ref::<&str>() {
        return text.to_string();
    }
    if let Some(text) = panic.downcast_ref::<String>() {
        return text.clone();
    }
    "unexpected failure".into()
}

/// Run `f`, turning a panic into `Err(message)`: the Python's
/// `except Exception as exc` around a privileged read, so a reader that
/// fails in a way nobody planned for still leaves a status line rather than
/// a poisoned runtime.
fn guarded<T>(f: impl FnOnce() -> T) -> Result<T, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).map_err(|panic| panic_message(&*panic))
}

// ---------------------------------------------------------------------------
// The read/cache/retry policy every privileged source shares
// ---------------------------------------------------------------------------

/// Python truthiness for what a source returns: a result object is true
/// whenever it is there, a dict of readings when it holds any.
pub trait Truthy {
    fn truthy(&self) -> bool;
}

impl<T> Truthy for Option<T> {
    fn truthy(&self) -> bool {
        self.is_some()
    }
}

impl<K, V> Truthy for BTreeMap<K, V> {
    fn truthy(&self) -> bool {
        !self.is_empty()
    }
}

pub type SourceRead<T> = Box<dyn Fn() -> Result<T, String> + Send + Sync>;
pub type SourceDescribe<T> = Box<dyn Fn(&T) -> String + Send + Sync>;
pub type SourceGate = Box<dyn Fn() -> String + Send + Sync>;

/// One lazily-read hardware source: read, cache briefly, keep last good.
///
/// All the privileged reads follow the same policy, so it lives here once:
///
///   * a success is cached for `ttl` seconds (`None` = for the session,
///     which is what the clock read wants since clocks cannot change);
///   * a failure keeps the previous value, because these reads compete for the
///     PCI/SMBus/ISA mutexes with other monitoring tools and losing a race is
///     routine — blanking a row for it is not;
///   * repeated failures give up after [`LAZY_READ_ATTEMPTS`] so a permanently
///     absent transport costs one dict lookup per tick instead of a full
///     bus scan.
///
/// `describe` turns a successful result into the status string; `label`
/// names the source in the failure strings.
///
/// `read` returning `Err` is the Python read raising; `Ok` with a falsy value
/// is it returning nothing.
pub struct LiveSource<T> {
    read: SourceRead<T>,
    describe: SourceDescribe<T>,
    label: &'static str,
    ttl: Option<Duration>,
    gate: Option<SourceGate>,
    // How long a failure stands before the next attempt. Zero, the
    // default, retries on the next ask. A source read by many rows in one
    // tick wants a gap, or one bad moment is retried by every row and
    // spends LAZY_READ_ATTEMPTS before the tick is over.
    retry_gap: Duration,
    failed_at: Option<Instant>,
    pub value: T,
    pub stamp: Option<Instant>,
    pub failures: u32,
    pub status: String,
}

impl<T: Clone + Truthy> LiveSource<T> {
    /// `ttl` and `retry_gap` are seconds; `ttl` `None` caches for the session.
    pub fn new(
        read: SourceRead<T>,
        describe: SourceDescribe<T>,
        label: &'static str,
        empty: T,
        ttl: Option<f64>,
        gate: Option<SourceGate>,
        retry_gap: f64,
    ) -> Self {
        LiveSource {
            read,
            describe,
            label,
            ttl: ttl.map(Duration::from_secs_f64),
            gate,
            retry_gap: Duration::from_secs_f64(retry_gap),
            failed_at: None,
            value: empty,
            stamp: None,
            failures: 0,
            status: "Not read".into(),
        }
    }

    fn fresh(&self) -> bool {
        let Some(stamp) = self.stamp else {
            return false;
        };
        match self.ttl {
            None => true,
            Some(ttl) => stamp.elapsed() < ttl,
        }
    }

    pub fn get(&mut self) -> T {
        if self.fresh() {
            return self.value.clone();
        }
        // "Ever succeeded" is what separates a transport that is absent from
        // one that merely lost a race; the stamp only tracks freshness.
        let ever_read = self.value.truthy();
        if !ever_read && self.failures >= LAZY_READ_ATTEMPTS {
            return self.value.clone(); // gave up; stop paying for the attempt
        }
        if !self.retry_gap.is_zero()
            && let Some(failed_at) = self.failed_at
            && failed_at.elapsed() < self.retry_gap
        {
            return self.value.clone(); // this tick has already tried
        }
        let reason = self.gate.as_ref().map(|gate| gate()).unwrap_or_default();
        if !reason.is_empty() {
            self.failures += 1;
            self.status = reason;
            return self.value.clone();
        }
        let result = match guarded(|| (self.read)()).and_then(|result| result) {
            Ok(result) => result,
            Err(exc) => {
                self.failures += 1;
                self.failed_at = Some(Instant::now());
                self.status = format!("{} read failed: {exc}", self.label);
                return self.value.clone();
            }
        };
        if result.truthy() {
            self.status = (self.describe)(&result);
            self.value = result;
            self.stamp = Some(Instant::now());
            self.failures = 0;
            self.failed_at = None;
        } else {
            self.failures += 1;
            self.failed_at = Some(Instant::now());
            self.status = if ever_read {
                format!("{} stale (bus busy)", self.label)
            } else {
                format!("{} unavailable", self.label)
            };
        }
        self.value.clone()
    }
}

// ---------------------------------------------------------------------------
// The runtime
// ---------------------------------------------------------------------------

/// What the Python's `GraniteRidgeApobReader` offered the runtime: a read,
/// and what it left behind. A trait so a test can stand in for the reader,
/// as the Python tests' `FakeTrainingReader` did.
pub trait TrainingReader {
    /// The decoded record, or `None` with [`TrainingReader::last_error`]
    /// saying why.
    fn read(&mut self) -> Option<TrainingValues>;
    /// The whole table, kept whether or not the record parsed.
    fn raw_table(&self) -> Vec<u8>;
    /// Per-channel records, keyed `cha` / `chb`, when the geometry
    /// attributed them.
    fn channel_values(&self) -> BTreeMap<&'static str, TrainingValues>;
    fn channel_record_addresses(&self) -> BTreeMap<&'static str, u64>;
    fn table_address(&self) -> u64;
    fn record_address(&self) -> u64;
    fn last_error(&self) -> String;
}

impl TrainingReader for GraniteRidgeApobReader {
    fn read(&mut self) -> Option<TrainingValues> {
        GraniteRidgeApobReader::read(self)
    }
    fn raw_table(&self) -> Vec<u8> {
        self.raw_table.clone()
    }
    fn channel_values(&self) -> BTreeMap<&'static str, TrainingValues> {
        self.channel_values.clone()
    }
    fn channel_record_addresses(&self) -> BTreeMap<&'static str, u64> {
        self.channel_record_addresses.clone()
    }
    fn table_address(&self) -> u64 {
        self.table_address.unwrap_or(0)
    }
    fn record_address(&self) -> u64 {
        self.record_address.unwrap_or(0)
    }
    fn last_error(&self) -> String {
        self.last_error.clone()
    }
}

/// Opens the SMN transport. `Err` is the Python constructor raising.
pub type SmnReaderFactory = Box<dyn Fn() -> Result<Box<dyn SmnTransport>, String> + Send + Sync>;
/// Opens the APOB reader for a record layout.
pub type TrainingReaderFactory =
    Box<dyn Fn(ApobLayout) -> Result<Box<dyn TrainingReader>, String> + Send + Sync>;
/// The CPU name, from wherever it comes.
pub type CpuNameFactory = Box<dyn Fn() -> String + Send + Sync>;

/// Where an [`Am5Runtime`] gets its transports. The defaults are the real
/// ones; the Python took the same three as constructor arguments.
pub struct Am5RuntimeOptions {
    pub reader_factory: SmnReaderFactory,
    pub training_reader_factory: TrainingReaderFactory,
    pub cpu_name_factory: CpuNameFactory,
}

impl Default for Am5RuntimeOptions {
    fn default() -> Self {
        Am5RuntimeOptions {
            reader_factory: Box::new(|| {
                McfgSmnReader::new().map(|reader| Box::new(reader) as Box<dyn SmnTransport>)
            }),
            training_reader_factory: Box::new(|layout| {
                Ok(Box::new(GraniteRidgeApobReader::new(layout)) as Box<dyn TrainingReader>)
            }),
            cpu_name_factory: Box::new(current_cpu_name),
        }
    }
}

/// `{rail or sensor key: reading}`, as the PMIC, Super I/O and SPD hub
/// readers return them.
pub type Readings = BTreeMap<&'static str, f64>;

/// The UMC snapshot and what the read said about itself.
#[derive(Default)]
struct UmcState {
    attempted: bool,
    decoded: Option<DecodedChannel>,
    channels: BTreeMap<u32, DecodedChannel>,
    raw: Registers,
    active_base: Option<u32>,
    status: String,
}

/// The APOB training record and what the read said about itself.
#[derive(Default)]
struct TrainingState {
    attempted: bool,
    training: Option<TrainingValues>,
    channels: BTreeMap<&'static str, TrainingValues>,
    apob_table: Vec<u8>,
    status: String,
}

#[derive(Default)]
struct CcdlState {
    attempted: bool,
    run: Option<(u32, u32, u32)>,
}

#[derive(Default)]
struct AgesaState {
    attempted: bool,
    agesa: Option<String>,
}

/// One entry per privileged source; the read/cache/retry policy lives in
/// [`LiveSource`] so it exists once rather than five times.
struct Sources {
    clocks: Mutex<LiveSource<Option<SmuClocks>>>,
    voltages: Mutex<LiveSource<Option<SmuVoltages>>>,
    power: Mutex<LiveSource<Option<SmuPower>>>,
    dram: Mutex<LiveSource<Readings>>,
    board: Mutex<LiveSource<Readings>>,
    board_monitor: Mutex<LiveSource<Readings>>,
    board_temp: Mutex<LiveSource<Readings>>,
    dimm_temp: Mutex<LiveSource<Readings>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Load one AM5 UMC snapshot lazily and cache it for the UI session.
pub struct Am5Runtime {
    reader_factory: SmnReaderFactory,
    training_reader_factory: TrainingReaderFactory,
    cpu_name_factory: CpuNameFactory,
    cpu_name_cache: Mutex<Option<String>>,
    umc: Mutex<UmcState>,
    training: Mutex<TrainingState>,
    ccdl: Mutex<CcdlState>,
    agesa: Mutex<AgesaState>,
    sources: Sources,
}

fn source_runtime(weak: &Weak<Am5Runtime>) -> Result<Arc<Am5Runtime>, String> {
    weak.upgrade().ok_or_else(|| "the runtime is gone".to_string())
}

impl Am5Runtime {
    /// The runtime on the real transports. Nothing is read until asked.
    pub fn new() -> Arc<Self> {
        Self::with_options(Am5RuntimeOptions::default())
    }

    pub fn with_options(options: Am5RuntimeOptions) -> Arc<Self> {
        Arc::new_cyclic(|weak: &Weak<Am5Runtime>| {
            let clocks_runtime = weak.clone();
            let voltages_runtime = weak.clone();
            let power_runtime = weak.clone();
            let sources = Sources {
                clocks: Mutex::new(LiveSource::new(
                    Box::new(move || Ok(source_runtime(&clocks_runtime)?.read_clocks())),
                    Box::new(|result: &Option<SmuClocks>| match result {
                        Some(r) => format!(
                            "RSMU PM-table 0x{:06X} @ 0x{:X}{}",
                            r.version,
                            r.table_base,
                            if r.verified { "" } else { " — unverified" }
                        ),
                        None => String::new(),
                    }),
                    "FCLK/UCLK",
                    None,
                    None, // clocks cannot change; cache for the session
                    None,
                    0.0,
                )),
                voltages: Mutex::new(LiveSource::new(
                    Box::new(move || Ok(source_runtime(&voltages_runtime)?.read_voltages())),
                    Box::new(|result: &Option<SmuVoltages>| match result {
                        Some(r) => format!(
                            "RSMU PM-table 0x{:06X} @ 0x{:X} — {} rail(s)",
                            r.version,
                            r.table_base,
                            r.values.len()
                        ),
                        None => String::new(),
                    }),
                    "Voltages",
                    None,
                    Some(LIVE_CACHE_SECONDS),
                    Some(Box::new(|| {
                        offsets_gate(
                            !crate::amd::smu_voltages::CONFIRMED_VOLTAGE_OFFSETS.is_empty(),
                            "Voltages unavailable — no confirmed PM-table offset \
                             (run COLLECT_VOLTAGE_REPORT.bat)",
                        )
                    })),
                    0.0,
                )),
                power: Mutex::new(LiveSource::new(
                    Box::new(move || Ok(source_runtime(&power_runtime)?.read_power())),
                    Box::new(|result: &Option<SmuPower>| match result {
                        Some(r) => format!(
                            "RSMU PM-table 0x{:06X} @ 0x{:X} — READ-ONLY",
                            r.version, r.table_base
                        ),
                        None => String::new(),
                    }),
                    "Power limits",
                    None,
                    Some(LIVE_CACHE_SECONDS),
                    Some(Box::new(|| {
                        offsets_gate(
                            !crate::amd::smu_power::CONFIRMED_POWER_OFFSETS.is_empty(),
                            "Power limits unavailable — no confirmed PM-table offset",
                        )
                    })),
                    0.0,
                )),
                dram: Mutex::new(LiveSource::new(
                    Box::new(|| Ok(crate::memory::ddr5_pmic::read_dram_rails(None, None, None))),
                    Box::new(|r: &Readings| format!("DDR5 PMIC READ-ONLY — {} rail(s)", r.len())),
                    "DDR5 PMIC",
                    Readings::new(),
                    Some(LIVE_CACHE_SECONDS),
                    None,
                    0.0,
                )),
                board: Mutex::new(LiveSource::new(
                    Box::new(|| Ok(crate::sensors::am5_board_rails::read_board_rails())),
                    Box::new(|r: &Readings| format!("Super I/O READ-ONLY — {} rail(s)", r.len())),
                    "Super I/O",
                    Readings::new(),
                    Some(LIVE_CACHE_SECONDS),
                    None,
                    0.0,
                )),
                // The board's own thermistors, off the same Super I/O the rails
                // come from. One read serves every row on the tick: three rows
                // each unlocking the configuration window would treble the ISA
                // mutex traffic for one set of numbers.
                // The rest of the board's Super I/O on the boards it is mapped
                // for: supplies, fans and the x16 slot probe, in one pass.
                board_monitor: Mutex::new(LiveSource::new(
                    Box::new(|| Ok(crate::sensors::am5_board_rails::read_board_monitor())),
                    Box::new(|r: &Readings| format!("Super I/O READ-ONLY — {} reading(s)", r.len())),
                    "Board monitor",
                    Readings::new(),
                    Some(LIVE_CACHE_SECONDS),
                    None,
                    // Fifteen rows share it: a failed read stands for the tick.
                    LIVE_CACHE_SECONDS,
                )),
                board_temp: Mutex::new(LiveSource::new(
                    Box::new(|| Ok(crate::sensors::board_sensors::read_board_temperatures())),
                    Box::new(|r: &Readings| format!("Super I/O READ-ONLY — {} sensor(s)", r.len())),
                    "Board sensors",
                    Readings::new(),
                    Some(LIVE_CACHE_SECONDS),
                    None,
                    0.0,
                )),
                dimm_temp: Mutex::new(LiveSource::new(
                    Box::new(|| Ok(crate::memory::ddr5_pmic::read_dimm_temperatures(None, None, None))),
                    Box::new(|r: &Readings| {
                        let parts: Vec<String> = r
                            .iter()
                            .map(|(channel, celsius)| {
                                format!("Ch{} {:.1} C", channel.to_uppercase(), PyFloat(*celsius))
                            })
                            .collect();
                        format!("DDR5 SPD hub READ-ONLY — {}", parts.join(", "))
                    }),
                    "DIMM sensor",
                    Readings::new(),
                    Some(LIVE_CACHE_SECONDS),
                    None,
                    0.0,
                )),
            };
            Am5Runtime {
                reader_factory: options.reader_factory,
                training_reader_factory: options.training_reader_factory,
                cpu_name_factory: options.cpu_name_factory,
                cpu_name_cache: Mutex::new(None),
                umc: Mutex::new(UmcState { status: "Not read".into(), ..UmcState::default() }),
                training: Mutex::new(TrainingState {
                    status: "Not read".into(),
                    ..TrainingState::default()
                }),
                ccdl: Mutex::new(CcdlState::default()),
                agesa: Mutex::new(AgesaState::default()),
                sources,
            }
        })
    }

    // Status strings live on the source objects; expose them under the names
    // the rows and the System Info tab already use.

    pub fn clock_status(&self) -> String {
        lock(&self.sources.clocks).status.clone()
    }

    pub fn voltage_status(&self) -> String {
        lock(&self.sources.voltages).status.clone()
    }

    pub fn power_status(&self) -> String {
        lock(&self.sources.power).status.clone()
    }

    pub fn dram_rail_status(&self) -> String {
        lock(&self.sources.dram).status.clone()
    }

    pub fn board_rail_status(&self) -> String {
        lock(&self.sources.board).status.clone()
    }

    /// The UMC read's status line, as last set. [`Am5Runtime::value`] or a
    /// Status row is what triggers the read.
    pub fn status(&self) -> String {
        lock(&self.umc).status.clone()
    }

    /// The APOB read's status line, as last set.
    pub fn training_status(&self) -> String {
        lock(&self.training).status.clone()
    }

    /// The primary channel's raw registers, `{offset: value}`.
    pub fn raw(&self) -> Registers {
        lock(&self.umc).raw.clone()
    }

    /// The SMN base the primary channel was read from.
    pub fn active_base(&self) -> Option<u32> {
        lock(&self.umc).active_base
    }

    /// Return the CPU name, queried at most once.
    ///
    /// The underlying lookup is a WMI query. It is cached because the answer
    /// cannot change while the machine is running, because WMI is slow enough
    /// to matter when a caller repeats every second, and because WMI needs COM
    /// initialised on the calling thread -- so a background refresh must not
    /// be the thing that first triggers it.
    pub fn cpu_name(&self) -> String {
        let mut cache = lock(&self.cpu_name_cache);
        if cache.is_none() {
            let name = guarded(|| (self.cpu_name_factory)()).unwrap_or_default();
            *cache = Some(name);
        }
        cache.clone().unwrap_or_default()
    }

    fn load(&self) {
        let mut state = lock(&self.umc);
        if state.attempted {
            return;
        }
        state.attempted = true;
        if let Err(exc) = guarded(|| self.read_umc(&mut state)) {
            state.status = format!("AM5 SMN read failed: {exc}");
        }
    }

    /// The body of [`Am5Runtime::load`]: one batched read of both UMCs.
    fn read_umc(&self, state: &mut UmcState) {
        let reader = match (self.reader_factory)() {
            Ok(reader) => reader,
            Err(exc) => {
                state.status = format!("AM5 SMN read failed: {exc}");
                return;
            }
        };
        let addresses: Vec<u32> = UMC_BASES
            .iter()
            .flat_map(|base| ALL_OFFSETS.iter().map(move |offset| base + offset))
            .collect();
        let values = reader.read_many(&addresses);
        let mut last_error = String::new();
        let mut channels = BTreeMap::new();
        let mut primary_regs = None;
        let mut primary_base = None;
        for base in UMC_BASES {
            let mut regs = Registers::new();
            for offset in ALL_OFFSETS {
                match values.get(&(base + offset)).copied().flatten() {
                    Some(value) => {
                        regs.insert(offset, value);
                    }
                    None => {
                        let error = reader.last_error();
                        if !error.is_empty() {
                            last_error = error;
                        }
                    }
                }
            }
            let Some(decoded) = decode_channel(&regs) else {
                continue;
            };
            let umc_index = base / 0x100000;
            channels.insert(umc_index, decoded);
            if primary_base.is_none() {
                primary_regs = Some(regs);
                primary_base = Some(base);
            }
        }
        if let Some((first, _)) = channels.first_key_value() {
            state.decoded = channels.get(first).cloned();
            let labels: Vec<String> = channels.keys().map(|index| format!("UMC{index}")).collect();
            state.channels = channels;
            state.raw = primary_regs.unwrap_or_default();
            state.active_base = primary_base;
            state.status = format!("AMD SMN/MCFG READ-ONLY — {}", labels.join("/"));
            return;
        }
        let detail = if last_error.is_empty() { String::new() } else { format!(" ({last_error})") };
        state.status = format!("AM5 SMN read failed: no plausible UMC channel{detail}");
    }

    /// One named reading: a UMC timing, a training value, a clock, or the
    /// AGESA string. The em dash when it could not be read.
    pub fn value(&self, name: &str) -> Reading {
        if name == "agesa" {
            return Reading::Text(self.agesa());
        }
        if name == "fclk_mhz" || name == "uclk_mhz" {
            let Some(clocks) = self.clocks() else {
                return Reading::Dash;
            };
            let value = if name == "fclk_mhz" { clocks.fclk_mhz } else { clocks.uclk_mhz };
            // Whole MHz, as MCLK reads: validate_clocks has already rounded
            // them, so the float's ".0" was all "2133.0 MHz" added.
            return Reading::Int(round_int(value));
        }
        if TRAINING_FIELDS.contains(name) {
            self.load_training();
            let state = lock(&self.training);
            let Some(training) = &state.training else {
                return Reading::Dash;
            };
            return training.get(name).map(Reading::from).unwrap_or(Reading::Dash);
        }
        self.load();
        let state = lock(&self.umc);
        let Some(decoded) = &state.decoded else {
            return Reading::Dash;
        };
        decoded.get(name).map(Reading::from).unwrap_or(Reading::Dash)
    }

    /// `(tCCD_L, tCCD_L_WR, tCCD_L_WR2)` from the APOB, or `None`.
    ///
    /// All three, not just the one no register holds. ZenTimings moved the
    /// whole run out of the UMC because some boards never program 0x50198
    /// from the BIOS setting, so a register can disagree with what was asked
    /// for and be doing exactly what the firmware told it. The APOB follows
    /// the setting.
    ///
    /// The marker route is tried first because it does not need the
    /// registers to be right. Anchoring on them is kept as the fallback: it
    /// is what this project used before, and where the registers are correct
    /// the two agree anyway -- on this bench both give (21, 83, 42).
    pub fn ccdl_run(&self) -> Option<(u32, u32, u32)> {
        let mut state = lock(&self.ccdl);
        if state.attempted {
            return state.run;
        }
        state.attempted = true;
        state.run = guarded(|| self.locate_ccdl_run()).unwrap_or(None);
        state.run
    }

    /// The body of [`Am5Runtime::ccdl_run`].
    fn locate_ccdl_run(&self) -> Option<(u32, u32, u32)> {
        self.load();
        self.load_training();
        let table = lock(&self.training).apob_table.clone();
        let mut found = find_ccdl_run(&table);
        if found.is_none() {
            let (tccdl, tccdl_wr2) = {
                let umc = lock(&self.umc);
                let field = |name: &str| {
                    umc.decoded
                        .as_ref()
                        .and_then(|decoded| decoded.get(name))
                        .and_then(UmcValue::as_i64)
                        .and_then(|value| u32::try_from(value).ok())
                };
                (field("tCCD_L"), field("tCCD_L_WR2"))
            };
            if let Some(anchored) = find_ccdl_wr(&table, tccdl, tccdl_wr2)
                && let (Some(tccdl), Some(tccdl_wr2)) = (tccdl, tccdl_wr2)
            {
                found = Some((tccdl, anchored, tccdl_wr2));
            }
        }
        found
    }

    /// One of the three tCCD_L timings, preferring the APOB run.
    ///
    /// Falls back to the register decode, which is what the row showed
    /// before and is right whenever the firmware did program the register.
    pub fn ccdl_value(&self, name: &str) -> Option<i64> {
        if let Some(run) = self.ccdl_run() {
            return Some(match name {
                "tCCD_L" => run.0,
                "tCCD_L_WR" => run.1,
                _ => run.2,
            } as i64);
        }
        if name == "tCCD_L_WR" {
            return None;
        }
        lock(&self.umc)
            .decoded
            .as_ref()
            .and_then(|decoded| decoded.get(name))
            .and_then(UmcValue::as_i64)
    }

    /// tCCD_L_WR, or `None`: the one tCCD_L timing no UMC register holds.
    pub fn ccdl_wr(&self) -> Option<i64> {
        self.ccdl_value("tCCD_L_WR")
    }

    /// Lazy AGESA string from physical-memory marker scan.
    pub fn agesa(&self) -> String {
        let mut state = lock(&self.agesa);
        if !state.attempted {
            state.attempted = true;
            let version = guarded(read_agesa_version_inpout).unwrap_or_default().py_strip().to_string();
            state.agesa = if version.is_empty() { None } else { Some(version) };
        }
        state.agesa.clone().unwrap_or_else(|| EM_DASH.to_string())
    }

    /// FCLK/UCLK from the approved PM-table version only.
    pub fn clocks(&self) -> Option<SmuClocks> {
        lock(&self.sources.clocks).get()
    }

    /// Rail voltages from the approved PM-table version only.
    pub fn voltages(&self) -> Option<SmuVoltages> {
        lock(&self.sources.voltages).get()
    }

    /// DRAM VDD/VDDQ/VPP from the DIMM PMIC over SMBus.
    pub fn dram_rails(&self) -> Readings {
        lock(&self.sources.dram).get()
    }

    /// Super I/O rails (CPU VDDIO, VTT). Third transport, see module docs.
    pub fn board_rails(&self) -> Readings {
        lock(&self.sources.board).get()
    }

    /// The board's own thermistors, off the same Super I/O.
    pub fn board_temperatures(&self) -> Readings {
        lock(&self.sources.board_temp).get()
    }

    /// The board's supplies, fans and slot probe, where it is mapped.
    pub fn board_monitor(&self) -> Readings {
        lock(&self.sources.board_monitor).get()
    }

    /// PPT/TDC/EDC from the approved PM-table version only.
    pub fn power(&self) -> Option<SmuPower> {
        lock(&self.sources.power).get()
    }

    fn read_clocks(&self) -> Option<SmuClocks> {
        self.load();
        let umc_mclk = lock(&self.umc)
            .decoded
            .as_ref()
            .and_then(|decoded| decoded.get("mclk_mhz"))
            .and_then(UmcValue::as_f64);
        crate::amd::smu_clocks::read_smu_clocks(&self.cpu_name(), umc_mclk)
    }

    fn read_voltages(&self) -> Option<SmuVoltages> {
        crate::amd::smu_voltages::read_smu_voltages(&self.cpu_name())
    }

    fn read_power(&self) -> Option<SmuPower> {
        crate::amd::smu_power::read_smu_power(&self.cpu_name())
    }

    /// Each DIMM's own SPD hub sensor, `{channel: celsius}`.
    pub fn dimm_temperatures(&self) -> Readings {
        lock(&self.sources.dimm_temp).get()
    }

    /// Return one rail in volts, or `None` (the em dash) when it is
    /// unavailable.
    ///
    /// Which transport serves a rail is declared on the rail itself, so a rail
    /// can be moved between transports by editing its definition alone.
    pub fn voltage_value(&self, key: &str) -> Option<f64> {
        let source = rail_by_key(key).map(|rail| rail.source).unwrap_or(SOURCE_PM_TABLE);
        if source == SOURCE_PMIC {
            self.dram_rails().get(key).copied()
        } else if source == SOURCE_SUPERIO {
            self.board_rails().get(key).copied()
        } else {
            self.voltages().and_then(|result| result.values.get(key).copied())
        }
    }

    /// Return one UMC-channel timing (cha=UMC0, chb=UMC1) without mirroring.
    pub fn channel_umc_value(&self, name: &str, channel: &str) -> Reading {
        self.load();
        let index = if channel == "cha" { 0 } else { 1 };
        let state = lock(&self.umc);
        let Some(decoded) = state.channels.get(&index) else {
            return Reading::Dash;
        };
        if decoded.0.is_empty() {
            return Reading::Dash;
        }
        decoded.get(name).map(Reading::from).unwrap_or(Reading::Dash)
    }

    /// Return one exact-geometry-attributed value without mirroring.
    pub fn channel_training_value(&self, name: &str, channel: &str) -> Reading {
        self.load_training();
        let state = lock(&self.training);
        let Some(values) = state.channels.get(channel).filter(|values| !values.is_empty()) else {
            return Reading::Dash;
        };
        values.get(name).map(Reading::from).unwrap_or(Reading::Dash)
    }

    fn load_training(&self) {
        let mut state = lock(&self.training);
        if state.attempted {
            return;
        }
        state.attempted = true;
        if let Err(exc) = guarded(|| self.read_training(&mut state)) {
            state.status = format!("APOB training read failed: {exc}");
        }
    }

    /// The body of [`Am5Runtime::load_training`].
    fn read_training(&self, state: &mut TrainingState) {
        let name = self.cpu_name();
        let (layout, unverified) = if is_granite_ridge_cpu(&name) {
            (ApobLayout::Zen5, "")
        } else if is_raphael_cpu(&name) {
            // Experimental: ZenStates-Core's Zen 4 layout, never checked
            // against a Ryzen 7000 here. Said so in the status line.
            (ApobLayout::Zen4, " (unverified)")
        } else {
            state.status =
                "APOB training data disabled — desktop Ryzen 9000 or 7000 required".into();
            return;
        };
        let mut reader = match (self.training_reader_factory)(layout) {
            Ok(reader) => reader,
            Err(exc) => {
                state.status = format!("APOB training read failed: {exc}");
                return;
            }
        };
        let values = reader.read();
        // Kept whether or not the training record parsed: tCCD_L_WR is
        // found by scanning the table itself, not the decoded record.
        state.apob_table = reader.raw_table();
        if let Some(values) = values.filter(|values| !values.is_empty()) {
            state.training = Some(values);
            let channel_values = reader.channel_values();
            let both = |keys: Vec<&str>| keys == ["cha", "chb"];
            if both(channel_values.keys().copied().collect()) {
                state.channels = channel_values;
            }
            let addresses = reader.channel_record_addresses();
            if both(addresses.keys().copied().collect()) {
                state.status = format!(
                    "AMD APOB READ-ONLY — table 0x{:08X}, ChA 0x{:08X}, ChB 0x{:08X}",
                    reader.table_address(),
                    addresses["cha"],
                    addresses["chb"],
                );
            } else {
                // The marker rides on the table address, ahead of the
                // first comma, because that is the part the Status row
                // keeps.
                state.status = format!(
                    "AMD APOB READ-ONLY — table 0x{:08X}{unverified}, record 0x{:08X}",
                    reader.table_address(),
                    reader.record_address(),
                );
            }
            return;
        }
        let error = reader.last_error();
        state.status =
            if error.is_empty() { "APOB training data unavailable".into() } else { error };
    }
}

// ---------------------------------------------------------------------------
// Test seams
// ---------------------------------------------------------------------------

/// What the Python tests reached with `mock.patch` on a module attribute:
/// a reader this profile calls, replaced for the calling test thread only.
#[cfg(test)]
pub(crate) mod seams {
    use std::any::Any;
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;

    thread_local! {
        static HOOKS: RefCell<HashMap<&'static str, Rc<dyn Any>>> = RefCell::new(HashMap::new());
    }

    /// Replace the reader `name` on this thread.
    pub(crate) fn set<T: 'static>(name: &'static str, hook: T) {
        HOOKS.with(|hooks| hooks.borrow_mut().insert(name, Rc::new(hook)));
    }

    pub(crate) fn get<T: 'static>(name: &'static str) -> Option<Rc<T>> {
        let hook = HOOKS.with(|hooks| hooks.borrow().get(name).cloned())?;
        hook.downcast::<T>().ok()
    }
}

/// Look `name` up in the seams first, under test; otherwise call `$real`.
macro_rules! seam {
    ($name:literal, $ty:ty, |$hook:ident| $use:expr, $real:expr) => {{
        #[cfg(test)]
        {
            if let Some($hook) = seams::get::<$ty>($name) {
                return $use;
            }
        }
        $real
    }};
}

fn read_agesa_version_inpout() -> String {
    seam!("agesa", Box<dyn Fn() -> String>, |hook| hook(), crate::amd::agesa::read_agesa_version_inpout())
}

fn os_name() -> Option<String> {
    seam!("os_name", Box<dyn Fn() -> Option<String>>, |hook| hook(), crate::system_identity::os_name())
}

fn microcode() -> Option<String> {
    seam!("microcode", Box<dyn Fn() -> Option<String>>, |hook| hook(), crate::system_identity::microcode())
}

fn lpcio_name() -> Option<String> {
    seam!("lpcio_name", Box<dyn Fn() -> Option<String>>, |hook| hook(), crate::system_identity::lpcio_name())
}

fn clock_text(key: &str) -> Option<String> {
    seam!("clock_text", Box<dyn Fn(&str) -> Option<String>>, |hook| hook(key),
        crate::sensors::cpu_clocks::clock_text(key))
}

fn core_labels() -> Vec<String> {
    seam!("core_labels", Box<dyn Fn() -> Vec<String>>, |hook| hook(),
        crate::sensors::cpu_clocks::core_labels())
}

fn core_clock_text(index: usize) -> Option<String> {
    seam!("core_clock_text", Box<dyn Fn(usize) -> Option<String>>, |hook| hook(index),
        crate::sensors::cpu_clocks::core_clock_text(index))
}

fn whea_error_text() -> String {
    seam!("whea", Box<dyn Fn() -> String>, |hook| hook(), crate::sensors::whea_errors::error_text())
}

fn read_dimm_telemetry() -> Vec<crate::memory::ddr5_telemetry::DimmTelemetry> {
    seam!("dimm_telemetry", Box<dyn Fn() -> Vec<crate::memory::ddr5_telemetry::DimmTelemetry>>,
        |hook| hook(), crate::memory::ddr5_telemetry::read_dimm_telemetry(None, None, None, None, None))
}

fn read_spd_identity() -> Vec<crate::memory::ddr5_spd::SpdIdentity> {
    seam!("spd_identity", Box<dyn Fn() -> Vec<crate::memory::ddr5_spd::SpdIdentity>>,
        |hook| hook(), crate::memory::ddr5_spd::read_identity(None, false, None))
}

fn read_gpu() -> Result<Option<crate::gpu::radeon::RadeonGpu>, String> {
    seam!("read_gpu", Box<dyn Fn() -> Result<Option<crate::gpu::radeon::RadeonGpu>, String>>,
        |hook| hook(), crate::gpu::radeon::read_gpu(None))
}

fn read_gpu_sensors() -> BTreeMap<&'static str, String> {
    seam!("gpu_sensors", Box<dyn Fn() -> BTreeMap<&'static str, String>>, |hook| hook(),
        crate::amd::adl::read_sensors(None))
}

fn board_power_limit_text() -> String {
    seam!("board_power_limit", Box<dyn Fn() -> String>, |hook| hook(),
        crate::amd::adlx::limit_text(crate::amd::adlx::Watts::Unread))
}

// ---------------------------------------------------------------------------
// Row getters
// ---------------------------------------------------------------------------

type Getter = Arc<dyn Fn() -> String + Send + Sync>;

fn getter(f: impl Fn() -> String + Send + Sync + 'static) -> Getter {
    Arc::new(f)
}

fn lazy(getter: Getter) -> Value {
    Value::Lazy(Arc::new(move || Some(getter())))
}

fn format_value(runtime: &Arc<Am5Runtime>, name: &'static str, suffix: &'static str) -> Getter {
    let runtime = runtime.clone();
    getter(move || {
        let value = runtime.value(name);
        if value.is_dash() { value.to_string() } else { format!("{value}{suffix}") }
    })
}

fn format_training_channel(runtime: &Arc<Am5Runtime>, name: &'static str, channel: &'static str) -> Getter {
    let runtime = runtime.clone();
    getter(move || runtime.channel_training_value(name, channel).to_string())
}

fn format_umc_channel(
    runtime: &Arc<Am5Runtime>,
    name: &'static str,
    channel: &'static str,
    suffix: &'static str,
) -> Getter {
    let runtime = runtime.clone();
    getter(move || {
        let value = runtime.channel_umc_value(name, channel);
        if value.is_dash() { value.to_string() } else { format!("{value}{suffix}") }
    })
}

/// `"%.<decimals>f<suffix>" % float(value)`; the em dash where `float()`
/// would have raised.
fn decimal_text(value: &Reading, decimals: usize, suffix: &str) -> String {
    match value.as_f64() {
        Some(number) => format!("{:.decimals$}{suffix}", PyFloat(number)),
        None => EM_DASH.to_string(),
    }
}

fn format_umc_channel_decimal(
    runtime: &Arc<Am5Runtime>,
    name: &'static str,
    channel: &'static str,
    decimals: usize,
    suffix: &'static str,
) -> Getter {
    let runtime = runtime.clone();
    getter(move || {
        let value = runtime.channel_umc_value(name, channel);
        if value.is_dash() {
            return value.to_string();
        }
        decimal_text(&value, decimals, suffix)
    })
}

fn format_decimal(runtime: &Arc<Am5Runtime>, name: &'static str, decimals: usize, suffix: &'static str) -> Getter {
    let runtime = runtime.clone();
    getter(move || {
        let value = runtime.value(name);
        if value.is_dash() {
            return value.to_string();
        }
        decimal_text(&value, decimals, suffix)
    })
}

fn format_voltage(runtime: &Arc<Am5Runtime>, key: &'static str) -> Getter {
    let runtime = runtime.clone();
    getter(move || match runtime.voltage_value(key) {
        None => EM_DASH.to_string(),
        Some(volts) => format_volts(volts),
    })
}

fn voltage_status(runtime: &Am5Runtime) -> String {
    runtime.voltages();
    runtime.voltage_status()
}

fn format_power_row(runtime: &Arc<Am5Runtime>, key: &'static str) -> Getter {
    let runtime = runtime.clone();
    getter(move || {
        let Some(result) = runtime.power() else {
            return EM_DASH.to_string();
        };
        let metric = metric_by_key(key).expect("a metric key from METRICS");
        format_power(metric, result.values.get(key).copied(), result.limits.get(key).copied())
            .unwrap_or_else(|| EM_DASH.to_string())
    })
}

/// One power limit on its own, without the live draw beside it.
///
/// Telemetry pairs each limit with what is being drawn against it, because
/// that is what a tuner watches while a load runs. Misc wants the other
/// half: what the processor was configured to allow, which does not move
/// and is not a reading.
fn power_limit(runtime: &Arc<Am5Runtime>, key: &'static str) -> Getter {
    let runtime = runtime.clone();
    getter(move || {
        let Some(result) = runtime.power() else {
            return EM_DASH.to_string();
        };
        let metric = metric_by_key(key).expect("a metric key from METRICS");
        format_power(metric, None, result.limits.get(key).copied()).unwrap_or_else(|| EM_DASH.to_string())
    })
}

/// The temperature ceiling alone, without the current reading.
///
/// Read rather than assumed, for the reason [`thermal_limit`] gives: this
/// bench reports 90 C where 95 was once taken for granted, so a board that
/// sets it differently would be quietly misstated.
fn thermal_limit_only(runtime: &Arc<Am5Runtime>) -> Getter {
    let runtime = runtime.clone();
    getter(move || {
        let Some(power) = runtime.power() else {
            return EM_DASH.to_string();
        };
        match power.temperatures.get("cpu_limit") {
            None => EM_DASH.to_string(),
            Some(limit) => format!("{:.1} °C", PyFloat(*limit)),
        }
    })
}

fn power_status(runtime: &Am5Runtime) -> String {
    runtime.power();
    runtime.power_status()
}

fn dram_frequency(runtime: &Am5Runtime) -> String {
    let value = runtime.value("mclk_mhz");
    if value.is_dash() {
        return value.to_string();
    }
    match value.as_i64() {
        Some(mclk) => format!("{} MT/s", mclk * 2),
        None => EM_DASH.to_string(),
    }
}

/// How a channel-setting row words one channel's raw value.
type Formatter = fn(&Reading) -> String;
/// `read(channel)`: that channel's raw value or the em dash.
type ChannelRead = Arc<dyn Fn(&str) -> Reading + Send + Sync>;

/// A Training row read once per channel: ChA from UMC0, ChB from UMC1.
///
/// `read(channel)` returns that channel's raw value or the em dash;
/// `formatter` words a raw value. The shared value is one reading when
/// both channels agree and both, labelled, when they do not. Where only one
/// channel answers -- a single-DIMM system has no UMC1 -- it is that
/// reading: the ChB value itself stays a dash, so nothing is filled in for
/// the channel that was not read.
fn channel_setting_row(
    label: &str,
    read: ChannelRead,
    category: &str,
    column: &str,
    formatter: Option<Formatter>,
) -> Row {
    let side = move |channel: &'static str| -> Getter {
        let read = read.clone();
        getter(move || {
            let value = read(channel);
            if value.is_dash() {
                return value.to_string();
            }
            match formatter {
                Some(shown) => shown(&value),
                None => value.to_string(),
            }
        })
    };
    let (side_a, side_b) = (side("cha"), side("chb"));
    let both = {
        let (side_a, side_b) = (side_a.clone(), side_b.clone());
        getter(move || {
            let (a, b) = (side_a(), side_b());
            if a == b || b == EM_DASH {
                return a;
            }
            if a == EM_DASH {
                return b;
            }
            format!("A: {a} | B: {b}")
        })
    };
    let mut row = make_row(label, lazy(both), category, SKEW_TAB, column);
    row.a = Some(Side::value(lazy(side_a)).named("ChA"));
    row.b = Some(Side::value(lazy(side_b)).named("ChB"));
    // Tells the window to show the one channel that answered on its own
    // rather than pair it with a dash; see TimingGUI._read_compact_value.
    row.lone_channel_alone = true;
    row
}

static AMBLE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?P<length>[\d.]+ tCK) - (?P<pattern>[01]+) Pattern$").unwrap());

/// A preamble or postamble as its length: "4 tCK", not the pattern too.
///
/// The pattern is kept only where two settings share a length, which on
/// DDR5 is the read preamble's two 2 tCK encodings, "2 tCK (0010)" and
/// "2 tCK (1110)" -- the same rule the Intel Training tab words them by.
/// A reserved code, or anything not shaped like a length and a pattern,
/// reads as decoded.
pub fn brief_amble(value: &str, shared_lengths: &[&str]) -> String {
    let Some(found) = AMBLE.captures(value) else {
        return value.to_string();
    };
    let length = &found["length"];
    if shared_lengths.contains(&length) {
        return format!("{length} ({})", &found["pattern"]);
    }
    length.to_string()
}

pub fn brief_read_preamble(value: &str) -> String {
    brief_amble(value, &["2 tCK"])
}

fn brief_amble_reading(value: &Reading) -> String {
    brief_amble(&value.to_string(), &[])
}

fn brief_read_preamble_reading(value: &Reading) -> String {
    brief_read_preamble(&value.to_string())
}

fn enabled_text(value: &Reading) -> String {
    if value.truthy() { "Enabled".into() } else { "Disabled".into() }
}

fn nitro_channel(runtime: &Am5Runtime, channel: &str) -> Reading {
    let parts: Vec<Reading> = ["nitro_rx", "nitro_tx", "nitro_ctrl"]
        .iter()
        .map(|key| runtime.channel_umc_value(key, channel))
        .collect();
    if parts.iter().any(Reading::is_dash) {
        return Reading::Dash;
    }
    Reading::Text(format!("{}/{}/{}", parts[0], parts[1], parts[2]))
}

fn status(runtime: &Am5Runtime) -> String {
    runtime.load();
    runtime.status()
}

fn training_status(runtime: &Am5Runtime) -> String {
    runtime.load_training();
    runtime.training_status()
}

// ---------------------------------------------------------------------------
// System Info: identity, firmware and the card
// ---------------------------------------------------------------------------

// One reading per field, kept for the session. Every field this serves is
// identity or firmware configuration, and none of it can change without a
// reboot. Opening a WMI connection costs about a second, which was invisible
// while these rows were drawn once and never read again -- and became the
// whole telemetry tick the moment Bus Clock joined a window polling at 1 Hz.
pub(crate) static SYSTEM_INFO_CACHE: MemoMap<&'static str, String> = MemoMap::new();

/// Return a lazy WMI value without querying WMI while the table is built.
fn system_info_value(field: &'static str) -> Getter {
    getter(move || {
        if let Some(value) = SYSTEM_INFO_CACHE.get(&field) {
            return value;
        }
        let value = read_system_info(field);
        if value == EM_DASH {
            // Not cached: the transport may simply have been busy, and a
            // blank held for the session would outlive its reason.
            return value;
        }
        SYSTEM_INFO_CACHE.insert(field, value.clone());
        value
    })
}

/// One WMI reading, uncached. See [`system_info_value`].
///
/// The Python kept one WMI connection here, opened once, because opening one
/// costs about a second and this module had been opening a fresh one for
/// every field -- nine seconds before the Advanced window, which reads every
/// row, could draw. It retried once on a fresh connection when the kept one
/// failed: a kept connection can go stale, and a COM object does not travel
/// between threads, and the retry told "this reading is unavailable" apart
/// from "that handle was". `wmi_util` owns the connection now, one per
/// thread, which settles both: there is no handle here to go stale or to
/// carry to another thread.
fn read_system_info(field: &str) -> String {
    read_system_info_from(field)
}

/// The first `Win32_*` instance's text field, stripped, or `None` when the
/// class or the field is absent -- where the Python's attribute access
/// raised and the reading fell back to the em dash.
fn first_text(class_name: &str, field: &str) -> Result<Option<String>, ()> {
    let objects = wmi_util::query(class_name).map_err(|_| ())?;
    Ok(objects.first().map(|object| object.str_or_empty(field).py_strip().to_string()))
}

/// One WMI reading through the shared connection.
fn read_system_info_from(field: &str) -> String {
    let attempt = || -> Result<String, ()> {
        match field {
            "cpu" => {
                let name = processor_facts().text("name").unwrap_or_default().py_strip().to_string();
                Ok(if name.is_empty() { EM_DASH.into() } else { name })
            }
            "manufacturer" => {
                let maker =
                    processor_facts().text("manufacturer").unwrap_or_default().py_strip().to_string();
                Ok(if maker.is_empty() { EM_DASH.into() } else { maker })
            }
            "l3_cache" => Ok(cache_text(processor_facts().get("l3_kb"))),
            "cores" => {
                let facts = processor_facts();
                let (Some(cores), Some(threads)) = (facts.text("cores"), facts.text("threads")) else {
                    return Ok(EM_DASH.into());
                };
                // Bare numbers, "8/16": the row's name already says which is
                // cores and which threads.
                Ok(format!("{cores}/{threads}"))
            }
            "board" => {
                let boards = wmi_util::query("Win32_BaseBoard").map_err(|_| ())?;
                let Some(board) = boards.first() else {
                    return Ok(EM_DASH.into());
                };
                Ok(board.text("Product").ok_or(())?.py_strip().to_string())
            }
            "board_revision" => {
                let version = first_text("Win32_BaseBoard", "Version")?.unwrap_or_default();
                Ok(board_revision_text(Some(&version)))
            }
            "board_vendor" => {
                // Reported verbatim; the full legal name is what the board
                // actually publishes and is not abbreviated here.
                let boards = wmi_util::query("Win32_BaseBoard").map_err(|_| ())?;
                let Some(board) = boards.first() else {
                    return Ok(EM_DASH.into());
                };
                let vendor = board.str_or_empty("Manufacturer").py_strip().to_string();
                Ok(if vendor.is_empty() { EM_DASH.into() } else { vendor })
            }
            "bios" => {
                let bios = wmi_util::query("Win32_BIOS").map_err(|_| ())?;
                let Some(first) = bios.first() else {
                    return Ok(EM_DASH.into());
                };
                Ok(first.text("SMBIOSBIOSVersion").ok_or(())?.py_strip().to_string())
            }
            "bios_date" => {
                // The version string alone does not say how old a build is,
                // and vendors reuse short version numbers across releases.
                let bios = wmi_util::query("Win32_BIOS").map_err(|_| ())?;
                let Some(first) = bios.first() else {
                    return Ok(EM_DASH.into());
                };
                let raw: Vec<char> = first.str_or_empty("ReleaseDate").chars().collect();
                // WMI datetime: yyyymmddHHMMSS.ffffff+UUU. Only the date is
                // meaningful for a firmware build.
                if raw.len() < 8 || !raw[..8].iter().all(char::is_ascii_digit) {
                    return Ok(EM_DASH.into());
                }
                let part = |range: std::ops::Range<usize>| raw[range].iter().collect::<String>();
                Ok(format!("{}-{}-{}", part(0..4), part(4..6), part(6..8)))
            }
            "agesa" => {
                // Filled by Am5Runtime::agesa() via physical marker scan; keep a
                // direct fallback here for any non-runtime callers.
                let version = read_agesa_version_inpout().py_strip().to_string();
                Ok(if version.is_empty() { EM_DASH.into() } else { version })
            }
            "bclk" => {
                let clock = processor_facts().get("ext_clock").cloned();
                let missing = match &clock {
                    None | Some(WmiValue::Null) => true,
                    Some(WmiValue::Str(text)) => text.is_empty() || text == "0",
                    Some(WmiValue::Int(0)) | Some(WmiValue::UInt(0)) | Some(WmiValue::Bool(false)) => true,
                    Some(WmiValue::Float(value)) => *value == 0.0,
                    Some(_) => false,
                };
                if missing {
                    return Ok(EM_DASH.into());
                }
                Ok(format!("{} MHz", clock.and_then(|c| c.to_text()).unwrap_or_default()))
            }
            "gpu" => {
                let mut names: Vec<String> = Vec::new();
                for adapter in wmi_util::query("Win32_VideoController").map_err(|_| ())? {
                    let name = adapter.str_or_empty("Name").py_strip().to_string();
                    // Skip the fallback driver Windows installs before the
                    // real one; it is not the card in the machine.
                    if !name.is_empty() && !name.contains("Basic Display") && !names.contains(&name) {
                        names.push(name);
                    }
                }
                Ok(if names.is_empty() { EM_DASH.into() } else { names.join(" / ") })
            }
            "os" => {
                let systems = wmi_util::query("Win32_OperatingSystem").map_err(|_| ())?;
                let Some(system) = systems.first() else {
                    return Ok(EM_DASH.into());
                };
                let caption = system.str_or_empty("Caption").py_strip().replace("Microsoft ", "");
                let build = system.str_or_empty("BuildNumber").py_strip().to_string();
                Ok(if build.is_empty() { caption } else { format!("{caption} (build {build})") })
            }
            "memory" => {
                let mut total: i64 = 0;
                for item in wmi_util::query("Win32_PhysicalMemory").map_err(|_| ())? {
                    total += item.int("Capacity").ok_or(())?;
                }
                Ok(if total != 0 {
                    format!("{} GB", total.div_euclid(1024 * 1024 * 1024))
                } else {
                    EM_DASH.into()
                })
            }
            _ => Ok(EM_DASH.into()),
        }
    };
    attempt().unwrap_or_else(|()| EM_DASH.into())
}

/// Return a lazy per-DIMM value for the System Info rows.
///
/// Size and rank come from SMBIOS Type 17, which is firmware reporting what it
/// read from the modules at boot. The DRAM maker and die come from the modules
/// themselves over SPD, since SMBIOS does not carry the DRAM component at all.
///
/// Reported for the set of installed modules rather than one slot, since the
/// controller settings above these rows are equally set-wide. A mixed kit
/// prints each distinct value instead of hiding the mismatch.
fn dimm_value(field: &'static str) -> Getter {
    use crate::memory::dimm_inventory::{rank_numeric, read_modules, shared_value, slots_used, split_ic};

    getter(move || {
        if field == "slots" {
            // A board fact rather than a module one: how many sockets
            // there are and how many are filled.
            return slots_used().filter(|text| !text.is_empty()).unwrap_or_else(|| EM_DASH.into());
        }

        if field == "part_number" {
            return shared_value(&read_modules(false), |module| module.get("part_number"));
        }

        if field == "module_manufacturer" {
            // Whose name is on the stick, which is not who made the chips
            // on it. The SPD carries it; SMBIOS often says "Unknown",
            // which is a placeholder rather than a name.
            let spd = read_spd_identity();
            if !spd.is_empty() {
                let named = shared_value(&spd, |entry| entry.get("module_manufacturer"));
                if named != EM_DASH {
                    return named;
                }
            }
            return shared_value(&read_modules(false), |module| {
                let maker = module.module_manufacturer.py_strip().to_lowercase();
                if maker.is_empty() || maker == "unknown" {
                    None
                } else {
                    Some(module.module_manufacturer.clone())
                }
            });
        }

        // SPD first: read from the DIMM, not inferred from its SKU. The
        // part-number table only stands in when the SMBus path is shut --
        // no driver, no admin -- and it is a lookup, not a reading.
        if field == "dram_manufacturer" || field == "dram_die" {
            let spd = read_spd_identity();
            if !spd.is_empty() {
                return shared_value(&spd, |entry| entry.get(field));
            }
            let maker = field == "dram_manufacturer";
            return shared_value(&read_modules(false), |module| {
                let (manufacturer, die) = split_ic(Some(&module.ic));
                Some(if maker { manufacturer } else { die })
            });
        }

        // Read from the modules over SPD, like the maker and die above:
        // SMBIOS carries neither the serial nor the date the module was
        // assembled, and both are per-module facts a mixed kit will
        // disagree about, which shared_value shows rather than hides.
        if field == "serial_number" || field == "manufacture_date" {
            let spd = read_spd_identity();
            if !spd.is_empty() {
                return shared_value(&spd, |entry| entry.get(field));
            }
            if field == "serial_number" {
                return shared_value(&read_modules(false), |module| module.get("serial_number"));
            }
            return EM_DASH.into();
        }

        if field == "channels" {
            // How many the controller is actually running, counted from
            // what is populated rather than from the socket count: two
            // sticks in one channel is a different machine from one in
            // each, and only the modules say which this is.
            let channels: BTreeSet<String> = read_modules(false)
                .into_iter()
                .filter_map(|module| module.channel.filter(|channel| !channel.is_empty()))
                .collect();
            return if channels.is_empty() { EM_DASH.into() } else { channels.len().to_string() };
        }

        let modules = read_modules(false);
        if field == "size" {
            return shared_value(&modules, |module| {
                Some(if module.capacity_gb != 0 {
                    format!("{} GB", module.capacity_gb)
                } else {
                    EM_DASH.into()
                })
            });
        }
        if field == "rank" {
            return shared_value(&modules, |module| Some(rank_numeric(module.rank_count)));
        }
        EM_DASH.into()
    })
}

// What the silicon is called, keyed by what CPUID reports. A decode table is
// not a hardcoded value -- "Granite Ridge" is a name for a code read from the
// hardware, and the entry below is the one this bench answers: family 0x1A,
// model 0x44. The repo's own probe gate names the same pair, independently.
//
// The node is a property of the silicon with nothing on the machine to read it
// from, so it is only reported for a part the table names. An unlisted part
// gets neither a code name nor a node rather than inheriting its neighbour's.
pub const AMD_SILICON: &[((u32, u32), (&str, &str))] = &[((0x1A, 0x44), ("Granite Ridge", "4 nm"))];

// 00:00.0 on an AM5 board. The host bridge is the same die as the cores, so
// the name comes from CPUID and only the revision comes from the bridge --
// the same split the Intel side uses for the same reason.
pub const HOST_BRIDGE_DEVICES: &[(u16, &str)] = &[(0x14D8, "Granite Ridge")];

// 00:14.3, the FCH's LPC bridge. This is the southbridge in AMD's
// architecture, and it is integrated in the SoC: it answers the same whichever
// discrete chipset the board carries, so it is named FCH rather than "B850".
// The board's marketing chipset name is not something this can read, and
// printing one would be a guess wearing a real revision number.
pub const FCH_DEVICES: &[(u16, &str)] = &[(0x790E, "FCH")];

/// The `Win32_Processor` fields this profile needs, copied out of the record.
///
/// Keyed by the Python dict's names; each value is what WMI returned, or
/// absent where the Python's `getattr` would have given `None`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProcessorFacts {
    pub fields: BTreeMap<&'static str, WmiValue>,
}

impl ProcessorFacts {
    pub fn get(&self, key: &str) -> Option<&WmiValue> {
        self.fields.get(key).filter(|value| !matches!(value, WmiValue::Null))
    }

    /// `str(facts.get(key))`, or `None` where the value is absent.
    pub fn text(&self, key: &str) -> Option<String> {
        self.get(key).and_then(WmiValue::to_text)
    }
}

pub(crate) static PROCESSOR_FACTS: Memo<Arc<ProcessorFacts>> = Memo::new();

/// The `Win32_Processor` fields this profile needs, read once.
///
/// Querying Win32_Processor costs about a second, and it is the query rather
/// than the connection: sharing one connection between callers changed
/// nothing measurable, while asking once and keeping the answer removed four
/// seconds. Eight rows read this class -- CPU, Vendor, Cores / Threads,
/// BCLK, Code Name, Technology, Chipset and DRAM Ratio -- and each ran its
/// own query, so the Advanced window paid a second per row before drawing.
///
/// The values are copied out rather than the record being kept, so nothing
/// here can lazily re-query later or carry a COM object onto another thread.
/// A failed read is not cached: the transport may simply have been busy, and
/// a blank held for the session would outlive its reason.
pub fn processor_facts() -> Arc<ProcessorFacts> {
    if let Some(facts) = PROCESSOR_FACTS.get() {
        return facts;
    }
    let Ok(processors) = wmi_util::query("Win32_Processor") else {
        return Arc::new(ProcessorFacts::default());
    };
    let Some(first) = processors.first() else {
        return Arc::new(ProcessorFacts::default());
    };
    let mut fields = BTreeMap::new();
    for (key, property) in [
        ("processor_id", "ProcessorId"),
        ("ext_clock", "ExtClock"),
        ("name", "Name"),
        ("manufacturer", "Manufacturer"),
        ("cores", "NumberOfCores"),
        ("threads", "NumberOfLogicalProcessors"),
        ("socket", "SocketDesignation"),
        // Kilobytes, as Win32_Processor reports it: 98304 on a 9850X3D.
        ("l3_kb", "L3CacheSize"),
    ] {
        if let Some(value) = first.get(property) {
            fields.insert(key, value.clone());
        }
    }
    let facts = Arc::new(ProcessorFacts { fields });
    PROCESSOR_FACTS.set(facts.clone());
    facts
}

// Board-revision strings firmware leaves unfilled. The X870 bench's Gigabyte
// board reports "x.x"; the other two are the common AMI and OEM defaults.
pub const BOARD_REVISION_PLACEHOLDERS: &[&str] =
    &["x.x", "default string", "to be filled by o.e.m.", "none", "n/a"];

/// The board revision, or the em dash where the firmware left none.
pub fn board_revision_text(version: Option<&str>) -> String {
    let version = version.unwrap_or("").py_strip();
    if version.is_empty() || BOARD_REVISION_PLACEHOLDERS.contains(&version.to_lowercase().as_str()) {
        return EM_DASH.into();
    }
    version.to_string()
}

/// A cache size from Win32_Processor's kilobytes: "96 MB", "512 KB".
pub fn cache_text(kilobytes: Option<&WmiValue>) -> String {
    let Some(kilobytes) = kilobytes.and_then(|value| match value {
        // int() of a float string raises; of a whole float it truncates.
        WmiValue::Str(text) => text.py_strip().parse::<i64>().ok(),
        other => other.to_i64(),
    }) else {
        return EM_DASH.into();
    };
    if kilobytes <= 0 {
        return EM_DASH.into();
    }
    if kilobytes % 1024 == 0 {
        return format!("{} MB", kilobytes / 1024);
    }
    format!("{kilobytes} KB")
}

/// Return `(code name, node)` for this CPU, or `(None, None)`.
pub fn cpu_silicon() -> (Option<&'static str>, Option<&'static str>) {
    let Some(processor_id) = processor_facts().text("processor_id").filter(|id| !id.is_empty()) else {
        return (None, None);
    };
    let Ok(key) = crate::system_identity::decode_wmi_processor_id(Some(&processor_id)) else {
        return (None, None);
    };
    match AMD_SILICON.iter().find(|(code, _)| *code == key) {
        Some((_, (name, node))) => (Some(name), Some(node)),
        None => (None, None),
    }
}

fn silicon_value(index: usize) -> Getter {
    getter(move || {
        let (name, node) = cpu_silicon();
        let found = if index == 0 { name } else { node };
        found.unwrap_or(EM_DASH).to_string()
    })
}

fn cpu_package() -> String {
    let value = processor_facts().text("socket").unwrap_or_default().py_strip().to_string();
    if value.is_empty() { EM_DASH.into() } else { value }
}

fn cpu_signature() -> String {
    let processor_id = processor_facts().text("processor_id");
    match crate::system_identity::decode_wmi_processor_signature(processor_id.as_deref()) {
        Ok((family, model, stepping)) => format!("{family} / 0x{model:02X} / {stepping}"),
        Err(_) => EM_DASH.into(),
    }
}

/// The host bridge: the silicon's code name, and its revision.
fn chipset() -> String {
    let Some((device, revision)) = crate::system_identity::pci_device_and_revision(0x00, 0) else {
        return EM_DASH.into();
    };
    let Some((_, bridge)) = HOST_BRIDGE_DEVICES.iter().find(|(id, _)| *id == device) else {
        return EM_DASH.into();
    };
    let name = cpu_silicon().0.unwrap_or(bridge);
    format!("AMD {name} rev. {revision:02X}")
}

/// The FCH, by the device ID of its LPC bridge, with its revision.
fn southbridge() -> String {
    let Some((device, revision)) = crate::system_identity::pci_device_and_revision(0x14, 3) else {
        return EM_DASH.into();
    };
    let Some((_, name)) = FCH_DEVICES.iter().find(|(id, _)| *id == device) else {
        return EM_DASH.into();
    };
    format!("AMD {name} rev. {revision:02X}")
}

/// The installed card, read once: `Some(None)` when there is none.
pub(crate) static GPU_CACHE: Memo<Option<Arc<crate::gpu::radeon::RadeonGpu>>> = Memo::new();

/// One field of the installed card, read lazily and cached for the run.
///
/// The card cannot change while the machine is on, and eight rows asking
/// separately would open the display class key eight times.
fn gpu_value(key: &'static str) -> Getter {
    getter(move || {
        let card = match GPU_CACHE.get() {
            Some(card) => card,
            None => match read_gpu() {
                Ok(card) => {
                    let card = card.map(Arc::new);
                    GPU_CACHE.set(card.clone());
                    card
                }
                Err(_) => return EM_DASH.into(),
            },
        };
        if key == "rops_tmus" {
            return crate::gpu::radeon::rops_tmus_text(card.as_deref()).unwrap_or_else(|| EM_DASH.into());
        }
        if key == "cores" {
            return match card.as_ref().and_then(|card| card.cores) {
                None => EM_DASH.into(),
                Some(cores) => format!("{cores} Unified"),
            };
        }
        card.and_then(|card| card.text(key)).unwrap_or_else(|| EM_DASH.into())
    })
}

/// One of the platform-neutral identity readings, read lazily.
///
/// Read inside the getter so a machine with no WMI, no registry access or no
/// Super I/O still builds its rows; the reading blanks, the tab does not fail
/// to draw.
fn identity(reader: &str) -> String {
    let found = match reader {
        "os_name" => os_name(),
        "microcode" => microcode(),
        "lpcio_name" => lpcio_name(),
        _ => None,
    };
    found.filter(|text| !text.is_empty()).unwrap_or_else(|| EM_DASH.into())
}

/// Where Python's `re.search(r"\((?:x64|x86)\)\s+", text, re.IGNORECASE)`
/// match ends, or `None`. Written out rather than left to regex-lite, whose
/// `\s` is ASCII only: Python's is every character `str.isspace()` accepts,
/// so "(x64)\x1f 24H2" or "(x64)" then a no-break space split there and did
/// not split here.
fn os_architecture_end(text: &str) -> Option<usize> {
    let is_space = |c: char| c.is_whitespace() || ('\x1c'..='\x1f').contains(&c);
    text.char_indices().find_map(|(start, _)| {
        let rest = &text[start..];
        let tag = rest.get(..5)?.to_ascii_lowercase();
        if tag != "(x64)" && tag != "(x86)" {
            return None;
        }
        let spaces: usize = rest[5..].chars().take_while(|c| is_space(*c)).map(char::len_utf8).sum();
        (spaces > 0).then_some(start + 5 + spaces)
    })
}

/// The OS edition (0) or its release and build (1).
///
/// Split after the architecture, the way the Intel tab splits the same
/// reading into OS and OS Version. Whole, "Microsoft Windows 11 Professional
/// (x64) 23H2 Build 22631.6060" was 427px on its own, too wide for half of
/// System Info.
pub fn os_part(index: usize) -> String {
    let text = identity("os_name");
    let Some(end) = os_architecture_end(&text) else {
        return if index == 0 { text } else { EM_DASH.into() };
    };
    let part = if index == 0 { text[..end].py_strip() } else { text[end..].py_strip() };
    if part.is_empty() { EM_DASH.into() } else { part.to_string() }
}

/// Build one display row.
///
/// Optional flags the UI reads are set on the returned row: `live` marks a
/// row as telemetry that should be re-read while the window is open, and the
/// `rail_key` extra lets the Summary filter on rail identity instead of
/// display label.
fn make_row(name: &str, value: Value, category: &str, tab: &str, column: &str) -> Row {
    Row::new(name, category, tab, column).with_value(value)
}

fn flag(mut row: Row, key: &str) -> Row {
    row.extra.insert(key.to_string(), Extra::Bool(true));
    row
}

fn live(row: Row) -> Row {
    row.live()
}

/// Render the UCLK:MCLK ratio, the 1:1 vs 1:2 question.
///
/// Derived rather than read: both clocks are already on the tab, but the
/// relationship between them is what actually characterises a memory setup.
fn uclk_ratio(runtime: &Am5Runtime) -> String {
    let clocks = runtime.clocks();
    let mclk = runtime.value("mclk_mhz");
    let Some(clocks) = clocks.filter(|_| !mclk.is_dash()) else {
        return EM_DASH.into();
    };
    let uclk = clocks.uclk_mhz;
    if uclk == 0.0 || !mclk.truthy() {
        return EM_DASH.into();
    }
    let Some(mclk) = mclk.as_f64() else {
        return EM_DASH.into();
    };
    let ratio = mclk / uclk;
    // Only 1:1 and 1:2 exist on this platform; anything else is a bad read.
    if (ratio - 1.0).abs() < 0.05 {
        return "1:1".into();
    }
    if (ratio - 2.0).abs() < 0.05 {
        return "1:2".into();
    }
    EM_DASH.into()
}

/// The effective DDR data-rate multiplier relative to BCLK.
///
/// Derived from two rows already on the tab rather than read, because the
/// firmware sets AM5 memory speed in MT/s and the multiplier is what that
/// works out to. DDR5-6000 over a 100 MHz base is 60.
fn dram_ratio(runtime: &Am5Runtime) -> String {
    let mclk = runtime.value("mclk_mhz");
    if mclk.is_dash() {
        return EM_DASH.into();
    }
    let base = match processor_facts().get("ext_clock") {
        None => Some(0.0),
        Some(WmiValue::Str(text)) if text.is_empty() => Some(0.0),
        Some(value) => value.to_f64(),
    };
    let (Some(base), Some(mclk)) = (base, mclk.as_f64()) else {
        return EM_DASH.into();
    };
    if base == 0.0 {
        return EM_DASH.into();
    }
    // Two decimals at most, and none that are zero: 64, not 64.00,
    // while an off-100 base clock still shows its 59.7.
    format!("{:.2}", PyFloat(2.0 * mclk / base)).trim_end_matches('0').trim_end_matches('.').to_string()
}

fn temperature(runtime: &Arc<Am5Runtime>, key: &'static str) -> Getter {
    let runtime = runtime.clone();
    getter(move || {
        let Some(power) = runtime.power() else {
            return EM_DASH.into();
        };
        match power.temperatures.get(key) {
            None => EM_DASH.into(),
            Some(celsius) => format!("{:.1} °C", PyFloat(*celsius)),
        }
    })
}

/// One module's SPD hub sensor. An empty slot has no hub, so it stays blank.
pub fn dimm_temperature(runtime: &Arc<Am5Runtime>, channel: &'static str) -> Value {
    let runtime = runtime.clone();
    lazy(getter(move || match runtime.dimm_temperatures().get(channel) {
        None => EM_DASH.into(),
        Some(celsius) => format!("{:.1} °C", PyFloat(*celsius)),
    }))
}

/// True when the controller refreshes on tRFC rather than tRFC2.
fn refresh_is_normal(runtime: &Am5Runtime, channel: Option<&str>) -> bool {
    let mode = match channel {
        None => runtime.value("refresh_mode"),
        Some(channel) => runtime.channel_umc_value("refresh_mode", channel),
    };
    mode == Reading::Text("Normal".into())
}

/// Render tREFI as the time it actually is, not the cycle count.
///
/// tREFI is how long the controller may go between refreshes, and a count of
/// memory clocks only means something once you know the clock. 65535 cycles
/// at 4100 MHz is 15984 ns.
///
/// Whole nanoseconds, matching tRFCns beside it. The two hundredths this
/// used to print came from dividing a cycle count by a clock, not from any
/// precision the reading has, and a refresh window is not tuned to a
/// picosecond.
fn format_refi_ns(runtime: &Arc<Am5Runtime>, channel: Option<&'static str>) -> Getter {
    let runtime = runtime.clone();
    getter(move || {
        let cycles = match channel {
            None => runtime.value("tREFI"),
            Some(channel) => runtime.channel_umc_value("tREFI", channel),
        };
        let mclk = runtime.value("mclk_mhz");
        if cycles.is_dash() || mclk.is_dash() {
            return EM_DASH.into();
        }
        let (Some(megahertz), Some(cycles)) = (mclk.as_f64(), cycles.as_f64()) else {
            return EM_DASH.into();
        };
        if megahertz <= 0.0 {
            return EM_DASH.into();
        }
        format!("{:.0} ns", PyFloat(cycles / megahertz * 1000.0))
    })
}

/// Render tRFCns, which means different things per refresh mode.
///
/// In Normal mode the controller refreshes on tRFC, so one interval is shown.
/// Otherwise refresh is split between the fine-grain (tRFC2) and same-bank
/// (tRFCsb) intervals, so both are shown — the decoder already switches
/// tRFC_ns to tRFC2 outside Normal mode.
///
/// The unit is named once, after the values: "117/95 ns" rather than
/// "117 ns / 95 ns". Both intervals are in the same unit, so saying it twice
/// spent a third of the column on repeating it.
fn format_rfc_ns(runtime: &Arc<Am5Runtime>, channel: Option<&'static str>) -> Getter {
    let runtime = runtime.clone();
    getter(move || {
        let read = |name: &str| match channel {
            None => runtime.value(name),
            Some(channel) => runtime.channel_umc_value(name, channel),
        };
        let primary = read("tRFC_ns");
        if primary.is_dash() {
            return EM_DASH.into();
        }
        let Some(primary) = primary.as_f64() else {
            return EM_DASH.into();
        };
        if refresh_is_normal(&runtime, channel) {
            return format!("{:.0} ns", PyFloat(primary));
        }
        let same_bank = read("tRFCsb_ns");
        if same_bank.is_dash() {
            return format!("{:.0} ns", PyFloat(primary));
        }
        match same_bank.as_f64() {
            Some(same_bank) => format!("{:.0}/{:.0} ns", PyFloat(primary), PyFloat(same_bank)),
            None => EM_DASH.into(),
        }
    })
}

fn dual(mut row: Row, a: Getter, b: Getter) -> Row {
    row.a = Some(Side::value(lazy(a)).named("ChA"));
    row.b = Some(Side::value(lazy(b)).named("ChB"));
    row
}

/// Build a UMC timing row carrying both channels.
///
/// Every UMC register is decoded per channel, so each timing shows ChA and ChB
/// independently rather than mirroring UMC0's value across both.  The shared
/// `value` stays the primary channel, which is what the Summary column and
/// the text dump use.
fn umc_row(
    runtime: &Arc<Am5Runtime>,
    label: &'static str,
    category: &str,
    column: &str,
    key: Option<&'static str>,
    decimals: Option<usize>,
    suffix: &'static str,
    dim: Option<PredFn>,
) -> Row {
    let key = key.unwrap_or(label);
    let (shared, side_a, side_b) = match decimals {
        None => (
            format_value(runtime, key, suffix),
            format_umc_channel(runtime, key, "cha", suffix),
            format_umc_channel(runtime, key, "chb", suffix),
        ),
        Some(decimals) => (
            format_decimal(runtime, key, decimals, suffix),
            format_umc_channel_decimal(runtime, key, "cha", decimals, suffix),
            format_umc_channel_decimal(runtime, key, "chb", decimals, suffix),
        ),
    };
    let mut row = dual(make_row(label, lazy(shared), category, "Timings", column), side_a, side_b);
    row.dim = dim;
    row
}

/// One of the three tCCD_L timings, read from the APOB run.
///
/// All three come from the APOB now, not just tCCD_L_WR. The registers are
/// kept as the fallback rather than the source: some boards never program
/// 0x50198 from the BIOS setting, and there a register disagreeing with what
/// was asked for is the firmware's doing rather than a misread.
///
/// The run is accepted only when every marker in the table agrees, so both
/// channels show the same value rather than an invented per-channel split.
fn ccdl_row(runtime: &Arc<Am5Runtime>, name: &'static str, category: &str) -> Row {
    let value = {
        let runtime = runtime.clone();
        getter(move || match runtime.ccdl_value(name) {
            None => EM_DASH.into(),
            Some(found) => found.to_string(),
        })
    };
    dual(make_row(name, lazy(value.clone()), category, "Timings", "Left"), value.clone(), value)
}

/// Build a common row plus separately decoded, geometry-attributed values.
fn training_row(
    runtime: &Arc<Am5Runtime>,
    label: &str,
    key: &'static str,
    category: &str,
    column: &str,
    tab: &str,
) -> Row {
    dual(
        make_row(label, lazy(format_value(runtime, key, "")), category, tab, column),
        format_training_channel(runtime, key, "cha"),
        format_training_channel(runtime, key, "chb"),
    )
}

/// Ryzen 7000's training rows: one record, not one per channel. The
/// geometry that attributes a record to a channel is known only for Ryzen
/// 9000, so plain rows show the record's value instead of two columns of
/// dashes.
fn single_training_row(
    runtime: &Arc<Am5Runtime>,
    label: &str,
    key: &'static str,
    category: &str,
    column: &str,
    tab: &str,
) -> Row {
    make_row(label, lazy(format_value(runtime, key, "")), category, tab, column)
}

pub const VOLTAGE_CATEGORY: &str = "Voltages";

// Temperatures and the limits they are measured against read as one picture:
// what the chip is doing, and how close that is to what it is allowed to do.
// Thermal Limit is literally a temperature against a limit, so splitting the
// two put a row's two halves either side of a heading.
pub const THERMAL_POWER_CATEGORY: &str = "Thermal & Power";

// Counters that should read zero, and mean something the moment they do not.
pub const ERROR_CATEGORY: &str = "Errors";

// The card's own sensors, which move like the CPU's and belong in the window
// that keeps their extremes rather than on a tab showing one instant.
pub const GRAPHICS_CATEGORY: &str = "Graphics";

/// Build the board-wide rail rows, in one section, core outward.
///
/// Each row still carries its supply domain in `rail_group`, so anything
/// that wants to group by domain can; the display simply does not.
///
/// The DRAM rails are not here: they are per-module, and each DIMM's own
/// panel reads them from its own PMIC. See PER_MODULE_RAILS.
fn voltage_rows(runtime: &Arc<Am5Runtime>) -> Vec<Row> {
    RAILS
        .iter()
        .filter(|rail| !PER_MODULE_RAILS.contains(&rail.key))
        .map(|rail| {
            // One section, not one per supply domain. Five headings for eight
            // rails cost more lines than the rails themselves, and RAILS is
            // already ordered core outward, so the domains still read in order
            // without a heading announcing each one.
            live(make_row(
                rail.label,
                lazy(format_voltage(runtime, rail.key)),
                VOLTAGE_CATEGORY,
                SENSOR_TAB,
                "Left",
            ))
            .with_extra("rail_key", Extra::Str(rail.key.into()))
            .with_extra("rail_group", Extra::Str(rail.group.into()))
        })
        .collect()
}

type Snapshot = Arc<BTreeMap<String, String>>;

/// One voltage snapshot, re-taken each time the Voltages tab is opened.
///
/// Every row reads the same cached snapshot, so a tab's worth of rails is one
/// pass over the SMU and the PMICs rather than one per row. Each row carries
/// the cache's `refresh`: the window calls it when the tab is shown, then
/// re-reads the rows, so what the tab shows is as old as the last time it was
/// opened rather than as old as the session. Telemetry is still where a rail
/// is watched.
fn voltage_snapshot_rows(runtime: &Arc<Am5Runtime>) -> Vec<Row> {
    let cache: Arc<Mutex<Option<Snapshot>>> = Arc::new(Mutex::new(None));
    let snapshot = {
        let cache = cache.clone();
        let runtime = runtime.clone();
        Arc::new(move || -> Snapshot {
            let mut slot = lock(&cache);
            if let Some(found) = slot.as_ref() {
                return found.clone();
            }
            let mut values = BTreeMap::new();
            for rail in RAILS {
                if !PER_MODULE_RAILS.contains(&rail.key) {
                    let read = format_voltage(&runtime, rail.key);
                    let value = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| read()))
                        .unwrap_or_else(|_| EM_DASH.into());
                    values.insert(rail.key.to_string(), value);
                }
            }
            let modules = std::panic::catch_unwind(read_dimm_telemetry).unwrap_or_default();
            for channel in ["cha", "chb"] {
                let entries: Vec<_> = modules.iter().filter(|m| m.channel == &channel[2..]).collect();
                for key in ["vdd", "vddq", "vpp", "vin_bulk", "vout_1v8", "vout_1v0"] {
                    // Do not arbitrarily pick a stick when a channel has several.
                    let raw = if entries.len() == 1 { entries[0].get(key) } else { None };
                    values.insert(
                        format!("{channel}{key}"),
                        match raw {
                            Some(raw) => format!("{:.3} V", PyFloat(raw / 1000.0)),
                            None => EM_DASH.into(),
                        },
                    );
                }
            }
            let found = Arc::new(values);
            *slot = Some(found.clone());
            found
        })
    };
    let refresh: ActionFn = {
        let cache = cache.clone();
        Arc::new(move || *lock(&cache) = None)
    };
    let read = |key: String| {
        let snapshot = snapshot.clone();
        getter(move || snapshot().get(&key).cloned().unwrap_or_else(|| EM_DASH.into()))
    };

    // The "Reading mode" row that used to head the tab said the snapshot was
    // taken at startup and needed a restart to update. It is re-taken on
    // every visit now, so there is nothing left for it to explain.
    let mut rows = Vec::new();
    for rail in RAILS {
        if !PER_MODULE_RAILS.contains(&rail.key) {
            let mut row = make_row(
                &format!("{} snapshot", rail.label),
                lazy(read(rail.key.to_string())),
                "CPU and motherboard",
                "Voltages",
                "Left",
            )
            .with_display_name(rail.label);
            row.refresh = Some(refresh.clone());
            rows.push(row);
        }
    }
    // The PMIC's own names for what it reports: VIN_Bulk is the module's 5 V
    // input, and the two lower rails are its LDO outputs. The row names stay
    // as they were, which is what the Summary and the tests find them by.
    for (channel, label) in [("cha", "CHA"), ("chb", "CHB")] {
        for (key, name, shown) in [
            ("vdd", "VDD", "VDD"),
            ("vddq", "VDDQ", "VDDQ"),
            ("vpp", "VPP", "VPP"),
            ("vin_bulk", "VIN", "VIN (5V)"),
            ("vout_1v8", "1.8V output", "LDO 1.8V"),
            ("vout_1v0", "1.0V output", "LDO 1.0V"),
        ] {
            let mut row = make_row(
                &format!("{label} {name}"),
                lazy(read(format!("{channel}{key}"))),
                &format!("{label} memory"),
                "Voltages",
                "Right",
            )
            .with_display_name(format!("{label} {shown}"));
            row.refresh = Some(refresh.clone());
            rows.push(row);
        }
    }
    rows
}

/// Python's `str.splitlines()`: every line boundary Python recognises.
fn py_splitlines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((index, c)) = chars.next() {
        let boundary = matches!(
            c,
            '\n' | '\r' | '\x0b' | '\x0c' | '\x1c' | '\x1d' | '\x1e' | '\u{85}' | '\u{2028}' | '\u{2029}'
        );
        if !boundary {
            continue;
        }
        lines.push(&text[start..index]);
        let mut end = index + c.len_utf8();
        if c == '\r'
            && let Some((next, '\n')) = chars.peek().copied()
        {
            chars.next();
            end = next + 1;
        }
        start = end;
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

/// A failure's opening sentence, without the explanation that follows.
///
/// The missing-driver message runs on into a list of the directories it
/// searched, one per line. Passed through whole it made the Status row on
/// System Info 1500px wide and five lines tall; the driver notice above the
/// tabs already carries the explanation. Split on ". " rather than "." for
/// the reason the notice gives: the first sentence ends in a filename.
pub fn status_headline(text: &str) -> String {
    let lines = py_splitlines(text.py_strip());
    let Some(first) = lines.first() else {
        return String::new();
    };
    first.split(". ").next().unwrap_or("").py_strip().trim_end_matches('.').to_string()
}

/// Why no transport could read, when the driver is that reason.
fn driver_problem(statuses: &[&str]) -> Option<String> {
    for status in statuses {
        if status.contains(&format!("{DLL_NAME} not found")) {
            return Some(format!("{DLL_NAME} not found"));
        }
        if status.contains("but could not load it") {
            return Some(format!("{DLL_NAME} could not be loaded"));
        }
    }
    None
}

/// Shorten one transport's status to what it adds beyond succeeding.
///
/// A message that does not start with the success prefix is a failure, and
/// its headline is kept: the line should grow exactly when it has something
/// to say.
pub fn status_tail(text: &str, prefix: &str, ok: &str) -> String {
    let text = text.py_strip();
    if text.is_empty() {
        return EM_DASH.into();
    }
    if !text.starts_with(prefix) {
        return status_headline(text);
    }
    match text.split_once('—') {
        Some((_label, tail)) if !tail.py_strip().is_empty() => tail.py_strip().to_string(),
        _ => ok.to_string(),
    }
}

static HEX_NUMBER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"0x[0-9A-Fa-f]+").unwrap());

/// Compact the PM-table status, keeping the version it was gated on.
///
/// The version is the gate: a BIOS shipping a different one blanks every rail
/// and every power reading, so it is the first thing worth seeing.
pub fn pm_table_segment(status: &str) -> String {
    let status = status.py_strip();
    if !status.starts_with("RSMU") {
        let headline = status_headline(status);
        return if headline.is_empty() { EM_DASH.into() } else { headline };
    }
    let (head, detail) = match status.split_once('—') {
        Some((head, tail)) => (head, tail.py_strip().replace("rail(s)", "rails")),
        None => (status, String::new()),
    };
    let version = HEX_NUMBER.find(head).map(|found| found.as_str()).unwrap_or("?");
    format!("PM-table {version} ({})", if detail.is_empty() { "ok" } else { detail.as_str() })
}

/// One line for every transport: which answered, and with what.
///
/// Four rows said four versions of "fine" on a working machine. This says it
/// once, and the rows themselves stay in the dump for when it is not fine.
fn status_summary(runtime: &Arc<Am5Runtime>) -> Getter {
    let runtime = runtime.clone();
    getter(move || {
        // Without the driver every transport fails the same way, and four
        // copies of that say less than one.
        if let Some(problem) = driver_problem(&[&status(&runtime), &training_status(&runtime)]) {
            return format!("No register reads: {problem}");
        }
        let smn = format!("SMN {}", status_tail(&status(&runtime), "AMD SMN/MCFG READ-ONLY", "ok"));
        // The APOB record addresses stay in the dump; the table address is
        // the part worth carrying here.
        let apob_tail = status_tail(&training_status(&runtime), "AMD APOB READ-ONLY", "ok");
        let apob = format!("APOB {}", apob_tail.split(',').next().unwrap_or(""));
        [smn, apob].join(" · ")
    })
}

/// The SMU's half of the status: the PM table, then the power read.
///
/// A row of its own rather than more of the Status line. All four transports
/// on one line came to 546px on a working machine, wider than half of System
/// Info; a second line in the same row made every row on the tab as tall as
/// it.
fn smu_status_summary(runtime: &Arc<Am5Runtime>) -> Getter {
    let runtime = runtime.clone();
    getter(move || {
        if driver_problem(&[&status(&runtime), &training_status(&runtime)]).is_some() {
            return EM_DASH.into();
        }
        if is_raphael_cpu(&runtime.cpu_name()) {
            // Ryzen 7000 reads its clocks from the PM table and nothing else:
            // no voltage or power offset is known for it, so those stay off
            // rather than being shown as failures.
            runtime.clocks();
            let status = runtime.clock_status();
            if status.starts_with("RSMU")
                && let Some(version) = HEX_NUMBER.find(&status)
            {
                return format!("PM-table {} clocks (unverified)", version.as_str());
            }
            let headline = status_headline(&status);
            return if headline.is_empty() { EM_DASH.into() } else { headline };
        }
        let power = status_tail(&power_status(&runtime), "RSMU", "ok");
        [
            pm_table_segment(&voltage_status(&runtime)),
            // The power read is its own sequence and can fail on its own, so
            // it is named even when there is nothing to report but success.
            format!("power {}", if power == "READ-ONLY" { "ok" } else { power.as_str() }),
        ]
        .join(" · ")
    })
}

/// Tctl against the limit it is measured against, as the other limits read.
///
/// Both halves are read; neither is assumed. The limit is a setting a BIOS or
/// a tuning tool can move -- this bench reports 90.0 C, not the 95 that was
/// once assumed -- so a constant here would quietly misstate every board that
/// sets it differently.
fn thermal_limit(runtime: &Arc<Am5Runtime>) -> Getter {
    let runtime = runtime.clone();
    getter(move || {
        let Some(power) = runtime.power() else {
            return EM_DASH.into();
        };
        let (Some(current), Some(limit)) =
            (power.temperatures.get("cpu"), power.temperatures.get("cpu_limit"))
        else {
            return EM_DASH.into();
        };
        format!("{:.1} / {:.1} °C", PyFloat(*current), PyFloat(*limit))
    })
}

pub const CLOCK_CATEGORY: &str = "Clocks";

// The row the per-processor clocks fold under, and the key that says so.
pub const CORE_EFFECTIVE_CLOCK_ROW: &str = "Core Effective Clock";
pub const PARENT_ROW_KEY: &str = "Parent";

/// One of the aggregate core clocks, or an em dash if none were read.
fn core_clock(key: &'static str) -> Getter {
    getter(move || clock_text(key).filter(|text| !text.is_empty()).unwrap_or_else(|| EM_DASH.into()))
}

/// One row per logical processor, folded under the aggregate row.
///
/// Built from what the counters actually enumerate rather than from a core
/// count, so a machine whose counters do not answer gets no children and the
/// parent row stands alone.
fn per_core_clock_rows() -> Vec<Row> {
    let labels = core_labels();

    let reader = |index: usize| {
        // core_clock_text returns None before the first interval has been
        // measured, and a row's text goes to the label verbatim: without this
        // every processor would read "None" for the first tick.
        getter(move || core_clock_text(index).filter(|text| !text.is_empty()).unwrap_or_else(|| EM_DASH.into()))
    };

    labels
        .iter()
        .enumerate()
        .map(|(index, label)| {
            live(make_row(label, lazy(reader(index)), CLOCK_CATEGORY, SENSOR_TAB, "Left"))
                .with_extra(PARENT_ROW_KEY, Extra::Str(CORE_EFFECTIVE_CLOCK_ROW.into()))
        })
        .collect()
}

/// The clocks, which a tab can only ever show as one instant.
///
/// What FCLK does across a stability run, and how far the cores fall back
/// under load, are exactly the readings worth a minimum and a maximum.
///
/// The core clocks are a different measurement from anything else in this
/// project and are named for it. They come from the Windows performance
/// counters, which average across the sampling interval rather than
/// reporting an instant; see cpu_clocks for the two routes measured and
/// rejected before that one.
///
/// MCLK, UCLK, FCLK and DRAM Frequency are not repeated here, though the
/// Intel window does carry them. Two reasons, either of them enough. They
/// are read once and cached for the session because they cannot change
/// without a reboot, so four columns of statistics would be the same number
/// four times -- and this window exists for what moves. And row names are
/// the key every other view joins on, so a second row called MCLK would
/// quietly win the Summary's lookup over the one on System Info.
fn clock_rows(runtime: &Arc<Am5Runtime>) -> Vec<Row> {
    let clock = |name: &str, value: Getter| live(make_row(name, lazy(value), CLOCK_CATEGORY, SENSOR_TAB, "Left"));
    let mut rows = vec![
        // Fixed too, but kept: it is the reference the core ratios multiply,
        // and one flat row that says what the machine is clocked from is
        // worth its line.
        clock("Bus Clock", system_info_value("bclk")),
        clock("Core Clock (avg)", core_clock("core_avg")),
        clock(CORE_EFFECTIVE_CLOCK_ROW, core_clock("core_effective")),
        // Named as HWiNFO names them, which also keeps them distinct from the
        // System Info rows called FCLK and UCLK -- a row name is the key every
        // other view joins on, so the same name twice would collide.
        clock("Infinity Fabric Clock (FCLK)", format_value(runtime, "fclk_mhz", " MHz")),
        clock("Memory Controller Clock (UCLK)", format_value(runtime, "uclk_mhz", " MHz")),
    ];
    rows.extend(per_core_clock_rows());
    rows
}

// Which Super I/O thermistors this board is confirmed to carry, and what to
// call them. The channel map came from a Z790-P, where channel meanings are a
// board decision rather than a chip one, so every channel was re-checked here
// by loading all sixteen threads and watching what moved:
//
//   cpu     41.0 -> 75.0 -> 43.0, tracking Tctl within 0.5 C the whole way
//   vrm     29.0 -> 32.0 -> 30.5, rising and falling with the load
//   system  31.0 -> 31.5, an ambient sensor behaving like one
//
// Left out, and why:
//
//   pch     exactly 29.0 through a 34 C swing. A chipset that does not care
//           about CPU load and a channel stuck at a constant look identical
//           from here, and nothing distinguishes them without a second board.
//   socket  reads 0.0: the header is not populated on this board.
//
// The board *rails* off the same chip are all left out. Two of the four do
// not decode at all here, and the one labelled vcore reads 1.206 V against a
// real Vcore of 1.173 -- it is following VDDCR_SOC. Wrong names on real
// numbers is the worst thing this tool can do, so none of them ship until
// each is confirmed the way these three were.
pub const BOARD_TEMPERATURES: &[(&str, &str)] =
    &[("CPU Temp (board)", "cpu"), ("VRM Temp (board)", "vrm"), ("System Temp", "system")];

// Telemetry's fan section, which the Intel boards already fill.
pub const FAN_CATEGORY: &str = "Fans";

fn board_temperature(runtime: &Arc<Am5Runtime>, key: &'static str) -> Getter {
    let runtime = runtime.clone();
    getter(move || match runtime.board_temperatures().get(key) {
        None => EM_DASH.into(),
        Some(celsius) => format!("{:.1} °C", PyFloat(*celsius)),
    })
}

/// The board's thermistors, beside the ones the CPU reports itself.
///
/// Two paths to the CPU temperature is the point rather than a duplication:
/// the board's sensor is what the fan curve actually runs on, and a
/// disagreement between it and Tctl is worth being able to see.
fn board_temperature_rows(runtime: &Arc<Am5Runtime>) -> Vec<Row> {
    BOARD_TEMPERATURES
        .iter()
        .map(|&(label, key)| {
            live(make_row(label, lazy(board_temperature(runtime, key)), THERMAL_POWER_CATEGORY, SENSOR_TAB, "Right"))
        })
        .collect()
}

/// How a board-monitor reading is worded: the Python's `%` format.
#[derive(Clone, Copy)]
enum BoardForm {
    /// `"%.1f °C"`
    Celsius,
    /// `"%.3f V"`
    Volts,
    /// `"%d RPM"`
    Rpm,
}

impl BoardForm {
    fn apply(self, value: f64) -> String {
        match self {
            BoardForm::Celsius => format!("{:.1} °C", PyFloat(value)),
            BoardForm::Volts => format!("{:.3} V", PyFloat(value)),
            BoardForm::Rpm => format!("{} RPM", value.trunc() as i64),
        }
    }
}

/// PCH and the x16 slot, on the boards the monitor is mapped for.
///
/// Not in BOARD_TEMPERATURES: that map is shared by every board chip, and on
/// the Nuvoton benches the PCH channel was never confirmed. These two come
/// from the board-gated monitor, where each was read beside a reference tool.
fn board_monitor_temperature_rows(runtime: &Arc<Am5Runtime>) -> Vec<Row> {
    [("PCH Temp", "pch"), ("PCIEX16 Temp", "pciex16")]
        .into_iter()
        .map(|(label, key)| {
            flag(
                live(make_row(
                    label,
                    lazy(board_reading(runtime, key, BoardForm::Celsius)),
                    THERMAL_POWER_CATEGORY,
                    SENSOR_TAB,
                    "Right",
                )),
                "hide_when_blank",
            )
        })
        .collect()
}

/// One board-monitor reading, formatted, or the em dash.
fn board_reading(runtime: &Arc<Am5Runtime>, key: &'static str, form: BoardForm) -> Getter {
    let runtime = runtime.clone();
    getter(move || match runtime.board_monitor().get(key) {
        None => EM_DASH.into(),
        Some(value) => form.apply(*value),
    })
}

/// The board's supplies after the CPU's rails, then its fans.
///
/// Hidden where they read nothing: on any board the monitor is not mapped
/// for, and for a fan header with nothing on it.
fn board_monitor_rows(runtime: &Arc<Am5Runtime>) -> Vec<Row> {
    let mut rows: Vec<Row> = BOARD_VOLTAGES
        .iter()
        .map(|&(key, label, _register, _divider, _band)| {
            flag(
                live(make_row(label, lazy(board_reading(runtime, key, BoardForm::Volts)), VOLTAGE_CATEGORY, SENSOR_TAB, "Left")),
                "hide_when_blank",
            )
        })
        .collect();
    rows.extend(BOARD_FANS.iter().map(|&(key, label, _low, _high)| {
        flag(
            live(make_row(label, lazy(board_reading(runtime, key, BoardForm::Rpm)), FAN_CATEGORY, SENSOR_TAB, "Left")),
            "hide_when_blank",
        )
    }));
    rows
}

/// The card's sensors from the last query, and when it was taken.
pub(crate) static GPU_SENSOR_CACHE: Memo<(Instant, Arc<BTreeMap<&'static str, String>>)> = Memo::new();

/// One of the card's sensors, from a query shared across the tick.
///
/// ADL returns the whole sensor block in a single call, so all nine rows are
/// served by one trip through the driver. The cache is cleared each time the
/// first row is asked, which is what makes "once a tick" true rather than
/// "once ever".
fn gpu_sensor(label: &'static str) -> Getter {
    getter(move || {
        let now = Instant::now();
        let sensors = match GPU_SENSOR_CACHE.get() {
            Some((taken, sensors)) if now.duration_since(taken) <= Duration::from_secs_f64(0.5) => sensors,
            _ => {
                let sensors = Arc::new(read_gpu_sensors());
                GPU_SENSOR_CACHE.set((now, sensors.clone()));
                sensors
            }
        };
        sensors.get(label).cloned().unwrap_or_else(|| EM_DASH.into())
    })
}

/// The ceiling board power is measured against, from ADLX.
fn board_power_limit() -> String {
    board_power_limit_text()
}

/// The card's sensors, read through AMD's own driver libraries.
///
/// The readings come from atiadlxx; the limit is the one row atiadlxx has no
/// answer for, so it comes from ADLX. It sits directly under board power
/// because a power figure means little without the number it is heading for.
fn graphics_rows() -> Vec<Row> {
    let mut rows: Vec<Row> = PMLOG_SENSORS
        .iter()
        .map(|&(_index, label, _unit, _scale)| {
            live(make_row(label, lazy(gpu_sensor(label)), GRAPHICS_CATEGORY, SENSOR_TAB, "Right"))
        })
        .collect();
    rows.push(live(make_row(
        "GPU Board Power Limit",
        lazy(getter(board_power_limit)),
        GRAPHICS_CATEGORY,
        SENSOR_TAB,
        "Right",
    )));
    rows
}

/// How many hardware errors Windows has logged since this boot.
fn whea_errors() -> String {
    whea_error_text()
}

/// The one counter that belongs beside the readings rather than in them.
///
/// A kit that boots and benchmarks can still be quietly correcting errors,
/// and this is the only place in the tool that would say so. It sits in the
/// window that keeps a maximum, which is the shape that matters here: a
/// count that ticked up once during a run is the whole point, and a tab
/// showing the current value would lose it.
fn error_rows() -> Vec<Row> {
    vec![live(make_row("WHEA Errors", lazy(getter(whea_errors)), ERROR_CATEGORY, SENSOR_TAB, "Right"))]
}

/// Temperature sensors. The DIMM one is the only reading in this project
/// that comes from the module itself rather than the CPU or the board.
fn temperature_rows(runtime: &Arc<Am5Runtime>) -> Vec<Row> {
    let sensor = |name: &str, value: Getter| {
        live(make_row(name, lazy(value), THERMAL_POWER_CATEGORY, SENSOR_TAB, "Right"))
    };
    vec![
        // Tctl and the limit it is measured against, in one row: the separate
        // "Thermal Limit" row that used to sit below the budgets repeated this
        // temperature verbatim and added only the limit.
        sensor("CPU Temp", thermal_limit(runtime)),
        // The DIMM sensors are not here: each module's own panel in the Sensor
        // Telemetry window carries its SPD hub temperature next to the rails
        // from the same PMIC, which is where it belongs.
        // The I/O die, where the memory controller actually lives, and the
        // cache. Both climb far less than the cores, which is what made them
        // hard to tell apart from the die readings at idle and easy under
        // load.
        sensor("IOD Average", temperature(runtime, "iod_average")),
        sensor("IOD Hotspot", temperature(runtime, "iod_hotspot")),
        sensor("L3 Temp", temperature(runtime, "l3")),
        sensor("VDDCR_VDD VRM", temperature(runtime, "vdd_vrm")),
        sensor("VDDCR_SOC VRM", temperature(runtime, "soc_vrm")),
        sensor("VDD_MISC VRM", temperature(runtime, "misc_vrm")),
    ]
}

/// Build the Power Limits rows: the ZenStates limits, read-only.
fn power_rows(runtime: &Arc<Am5Runtime>) -> Vec<Row> {
    METRICS
        .iter()
        // Scalar is on Misc instead. It has no live half to pair with -- it
        // is a setting, and this window is for readings that move -- and two
        // rows of one name would collide, since names are how the tabs look
        // rows up across the profile.
        .filter(|metric| metric.key != "scalar")
        .map(|metric| {
            live(make_row(
                metric.label,
                lazy(format_power_row(runtime, metric.key)),
                THERMAL_POWER_CATEGORY,
                SENSOR_TAB,
                "Right",
            ))
        })
        // The thermal budget is not appended here: CPU Temp already reads Tctl
        // against its limit, in the same value-against-limit shape.
        .collect()
}

// The System Info tab, in sections, laid out the way the Intel tab is: machine
// identity on the left, configured clocks and graphics on the right. It was
// one column once, for fear the board row would clip at half the window, and
// that column came to 930px on a tab 633px tall -- the bottom third cut off
// with no scrollbar to reach it. Status closes the left column, which is the
// shorter one.
//
// A row that is not named here would have no section at all, which is why the
// lookup below fails loudly rather than defaulting to a "General" heading --
// a row quietly filed under a leftover heading is one nobody notices.
pub const SYSTEM_INFO_SECTIONS: &[(&str, &str, &[&str])] = &[
    ("System", "Left", &["OS", "OS Version", "Platform"]),
    (
        "Processor",
        "Left",
        &[
            "CPU", "CPU Package", "CPU Signature", "Code Name", "Vendor", "Technology",
            "Cores / Threads", "L3 Cache", "Microcode",
        ],
    ),
    (
        "Motherboard",
        "Left",
        &[
            "Manufacturer", "Model", "Board Revision", "BIOS", "BIOS Date", "Chipset", "Southbridge",
            "LPCIO", "AGESA",
        ],
    ),
    (
        "Status",
        "Left",
        &["Status", "SMU Status", "Read Status", "Training Status", "Voltage Status", "Power Status"],
    ),
    ("Clocks", "Right", &["BCLK", "MCLK", "FCLK", "UCLK", "DRAM Frequency", "UCLK:MCLK", "DRAM Ratio"]),
    (LIMITS_CATEGORY, "Right", &["Temp Limit", "PPT Limit", "TDC Limit", "EDC Limit", "Scalar"]),
    (
        "Graphics",
        "Right",
        &[
            "GPU", "Board Manufacturer", "GPU Code Name", "GPU Revision", "GPU Technology", "Cores",
            "ROPs / TMUs", "Memory Size", "Memory Type", "Memory Vendor", "Bus Width", "Resizable BAR",
            "PCIe Link", "VBIOS", "Driver Version", "Driver Date",
        ],
    ),
];

/// Each System Info row's section.
pub static SECTION_OF: LazyLock<BTreeMap<&'static str, &'static str>> = LazyLock::new(|| {
    SYSTEM_INFO_SECTIONS
        .iter()
        .flat_map(|(title, _column, names)| names.iter().map(move |name| (*name, *title)))
        .collect()
});

/// Each System Info row's column.
pub static COLUMN_OF: LazyLock<BTreeMap<&'static str, &'static str>> = LazyLock::new(|| {
    SYSTEM_INFO_SECTIONS
        .iter()
        .flat_map(|(_title, column, names)| names.iter().map(move |name| (*name, *column)))
        .collect()
});

/// The full AM5 table, on a runtime of its own. All hardware values remain
/// lazy: nothing is read until a row is.
pub fn build_timings() -> Vec<Row> {
    build_timings_with(&Am5Runtime::new())
}

/// Build the AM5-only UI table on `runtime`; all hardware values remain lazy.
pub fn build_timings_with(runtime: &Arc<Am5Runtime>) -> Vec<Row> {
    let info = |name: &str, value: Value| -> Row {
        make_row(name, value, SECTION_OF[name], "System Info", COLUMN_OF[name])
    };
    let info_fn = |name: &str, value: fn() -> String| info(name, lazy(getter(value)));
    let with_runtime = |f: fn(&Am5Runtime) -> String| -> Value {
        let runtime = runtime.clone();
        lazy(getter(move || f(&runtime)))
    };

    // Sectioned the way the Intel tab is: what the machine is, the board, the
    // clock chain, what is installed in it, how the controller is set, the
    // card, and what could and could not be read. The rows are built in this
    // order because a category whose rows are not contiguous draws as two
    // headings with the same name.
    let mut rows = vec![
        // System
        info_fn("OS", || os_part(0)),
        info_fn("OS Version", || os_part(1)),
        info("Platform", Value::text("AM5")),
        // Processor
        info("CPU", lazy(system_info_value("cpu"))),
        info_fn("CPU Package", cpu_package).with_display_name("Package"),
        info_fn("CPU Signature", cpu_signature).with_display_name("Family / Model / Step"),
        info("Code Name", lazy(silicon_value(0))),
        // "Vendor", not "Manufacturer": the board takes that name below, the
        // way CPU-Z uses it, and two rows cannot share one.
        info("Vendor", lazy(system_info_value("manufacturer"))),
        info("Technology", lazy(silicon_value(1))),
        info("Cores / Threads", lazy(system_info_value("cores"))),
        // The X3D parts' headline figure: 96 MB on a 9850X3D.
        info("L3 Cache", lazy(system_info_value("l3_cache"))),
        info_fn("Microcode", || identity("microcode")),
        // Motherboard
        info("Manufacturer", lazy(system_info_value("board_vendor"))),
        info("Model", lazy(system_info_value("board"))),
        // Blank where the firmware left a placeholder, as the X870 bench's
        // does ("x.x").
        info("Board Revision", lazy(system_info_value("board_revision"))),
        info("BIOS", lazy(system_info_value("bios"))),
        // Beside the firmware version it dates.
        info("BIOS Date", lazy(system_info_value("bios_date"))),
        info_fn("Chipset", chipset),
        info_fn("Southbridge", southbridge),
        info_fn("LPCIO", || identity("lpcio_name")),
        info("AGESA", lazy(format_value(runtime, "agesa", ""))),
        // Status. Two rows covering every transport: the memory controller's
        // reads, then the SMU's. The four rows they replace are kept below,
        // marked diagnostic: the tab shows the summary, the dump keeps the
        // full text including the APOB record addresses.
        live(info("Status", lazy(status_summary(runtime)))),
        live(info("SMU Status", lazy(smu_status_summary(runtime)))).with_display_name("SMU"),
        flag(info("Read Status", with_runtime(status)), "diagnostic"),
        flag(info("Training Status", with_runtime(training_status)), "diagnostic"),
        flag(live(info("Voltage Status", with_runtime(voltage_status))), "diagnostic"),
        flag(live(info("Power Status", with_runtime(power_status))), "diagnostic"),
        // Clocks
        info("BCLK", lazy(system_info_value("bclk"))),
        info("MCLK", lazy(format_value(runtime, "mclk_mhz", " MHz"))),
        info("FCLK", lazy(format_value(runtime, "fclk_mhz", " MHz"))),
        info("UCLK", lazy(format_value(runtime, "uclk_mhz", " MHz"))),
        info("DRAM Frequency", with_runtime(dram_frequency)),
        info("UCLK:MCLK", with_runtime(uclk_ratio)),
        info("DRAM Ratio", with_runtime(dram_ratio)),
        // System-wide values consumed by the custom SPD page. They remain
        // lazy rows so the UI can share the same hardware readers without
        // repeating a Memory section in System Info.
        make_row("Channels", lazy(dimm_value("channels")), "System", "SPD", "FullWidth"),
        make_row("Memory Capacity", lazy(system_info_value("memory")), "System", "SPD", "FullWidth"),
        // What the processor was configured to allow, as opposed to what it
        // is drawing. The live halves stay on Telemetry, which keeps a
        // maximum and is where a limit is worth watching a load against;
        // these are the settings. They were the last thing on the Misc tab
        // once its per-channel rows moved to Training, and five rows did not
        // earn a tab, so they sit under the clocks here. Live, because a
        // tuning tool can move them without a reboot.
        live(info("Temp Limit", lazy(thermal_limit_only(runtime)))),
        live(info("PPT Limit", lazy(power_limit(runtime, "ppt")))),
        live(info("TDC Limit", lazy(power_limit(runtime, "tdc")))),
        live(info("EDC Limit", lazy(power_limit(runtime, "edc")))),
        // Scalar keeps its own name: it is one value either way, so there is
        // no "limit" half to distinguish it from.
        live(info("Scalar", lazy(format_power_row(runtime, "scalar")))),
        // Graphics is the final System Info block. The card's own name comes
        // from the display class key rather than WMI: it is the string the
        // driver registered, which is what GPU-Z and CPU-Z both show.
        info("GPU", lazy(gpu_value("name"))),
        info("Board Manufacturer", lazy(gpu_value("board_manufacturer"))),
        info("GPU Code Name", lazy(gpu_value("code_name"))),
        info("GPU Revision", lazy(gpu_value("revision_text"))),
        info("GPU Technology", lazy(gpu_value("technology"))),
        info("Cores", lazy(gpu_value("cores"))),
        info("ROPs / TMUs", lazy(gpu_value("rops_tmus"))),
        info("Memory Size", lazy(gpu_value("memory_size"))),
        info("Memory Type", lazy(gpu_value("memory_type"))),
        // Blank, and expected to stay blank: see radeon_gpu on why the memory
        // vendor is not reachable from anything this tool can read.
        info("Memory Vendor", lazy(gpu_value("memory_vendor"))),
        info("Bus Width", lazy(gpu_value("bus_width"))),
        // Read from the card's Resizable BAR capability, which reports the
        // size the BAR is programmed to. Nothing is written: the usual way to
        // size a BAR is to write all ones and read back the mask.
        info("Resizable BAR", lazy(gpu_value("resizable_bar"))),
        // The slot's link at the root port, not the card's own, which on a
        // Radeon is the link inside the card's PCIe switch.
        info("PCIe Link", lazy(gpu_value("pcie_link"))),
        info("VBIOS", lazy(gpu_value("vbios"))),
        info("Driver Version", lazy(gpu_value("driver_version"))),
        info("Driver Date", lazy(gpu_value("driver_date"))),
    ];

    let umc = |key: &'static str| -> ChannelRead {
        let runtime = runtime.clone();
        Arc::new(move |channel: &str| runtime.channel_umc_value(key, channel))
    };

    let setting = |label: &str, read: ChannelRead, formatter: Option<Formatter>| {
        channel_setting_row(label, read, OTHER_SETTINGS_CATEGORY, "Left", formatter)
    };

    // The controller settings. They are neither identity nor timing: what
    // the controller was configured to do. Each channel's controller holds
    // its own copy -- one BIOS setting normally writes both the same -- so
    // they are read per channel, and Training is the page that shows a
    // reading per channel. They had been on a Misc tab of their own, and
    // before that under a Misc tab AM5 never drew, reaching only the Advanced
    // window. Added after the drive strengths below, so Other Settings sits
    // under ODT and the raw codes under Drive Strength. The section names are
    // the ones ZenTimings users know.
    let nitro: ChannelRead = {
        let runtime = runtime.clone();
        Arc::new(move |channel: &str| nitro_channel(&runtime, channel))
    };
    let mut channel_settings = vec![
        setting("Refresh Mode", umc("refresh_mode"), None),
        // The raw fine-granularity field behind the mode above. Refresh Mode
        // already reads it -- Normal where it is zero, FGR or Mixed where it
        // is not -- and what its non-zero values mean on DDR5 is not
        // documented anywhere this project reads from; ZenStates-Core tests
        // it only for zero too. So it stays in the Advanced window, as a
        // number, and off the tab, where a bare "0" explained nothing.
        flag(setting("FGR", umc("fgr"), None), "diagnostic"),
        setting("Read Preamble", umc("read_preamble"), Some(brief_read_preamble_reading)),
        setting("Write Preamble", umc("write_preamble"), Some(brief_amble_reading)),
        setting("Read Postamble", umc("read_postamble"), Some(brief_amble_reading)),
        setting("Write Postamble", umc("write_postamble"), Some(brief_amble_reading)),
        setting("ECC", umc("ecc"), None),
        // Without "Mode", as the Summary labels them; the rows keep their
        // full names, which is what the Summary finds them by.
        setting("Gear Down Mode", umc("gdm"), Some(enabled_text)).with_display_name("Gear Down"),
        setting("Power Down Mode", umc("powerdown"), Some(enabled_text)).with_display_name("Power Down"),
        setting("BGS", umc("bgs"), Some(enabled_text)),
        setting("BGS Alt", umc("bgs_alt"), Some(enabled_text)),
        setting("Nitro Rx/Tx/Ctrl", nitro, None).with_display_name("Nitro"),
    ];
    // The raw training codes, from each channel's own training record. Raw
    // APOB bytes, not decoded levels -- see RAW_TRAINING_FIELDS -- so they
    // are shown as numbers.
    channel_settings.extend(RAW_TRAINING_FIELDS.iter().map(|&(name, _offset)| {
        let runtime = runtime.clone();
        channel_setting_row(
            name,
            Arc::new(move |channel: &str| runtime.channel_training_value(name, channel)),
            RAW_TRAINING_CATEGORY,
            "Right",
            None,
        )
    }));

    // Clocks lead the window, the way they do on the Intel side: what the
    // machine is running at, before what that costs it in heat and power.
    rows.extend(clock_rows(runtime));
    // Temperatures lead the shared section, then the limits. What the CPU
    // reports about itself first, then what the board measures around it.
    rows.extend(temperature_rows(runtime));
    rows.extend(board_temperature_rows(runtime));
    rows.extend(board_monitor_temperature_rows(runtime));
    rows.extend(power_rows(runtime));
    rows.extend(voltage_rows(runtime));
    rows.extend(board_monitor_rows(runtime));
    rows.extend(voltage_snapshot_rows(runtime));
    rows.extend(graphics_rows());
    rows.extend(error_rows());

    let primary = ["tCL", "tRCDRD", "tRCDWR", "tRP", "tRAS", "tRC"];
    // Write recovery first, then the row-to-row and write-to-read pairs kept
    // long/short together, matching the Summary priority order.
    let secondary = ["tWR", "tRRD_L", "tRRD_S", "tWTR_L", "tWTR_S", "tRTP", "tFAW", "tCWL"];
    // tRFCns precedes tRFC so Summary priority and Timings order match.
    let refresh = ["tREFI", "tREFIns", "tRFCns", "tRFC", "tRFC2", "tRFCsb"];

    // What used to be one 28-row "Tertiary" block, split into the groups a
    // tuner actually reads together and dealt across both columns so neither
    // one runs off the bottom of the tab. Order within a column is the order
    // these appear here.
    // Stagger sits last. The Timings tab orders its sections from
    // TIMINGS_SECTION_ORDER and is unaffected by this, but the Summary's
    // middle column follows the order rows appear here, and stagger belongs
    // after the CAS-to-CAS group there rather than splitting the turnarounds
    // from the same-direction groups.
    // Preamble stays ahead of the mode-register group so tRDPRE and tWRPRE
    // still precede tMOD, the order the Summary tail also uses.
    //
    // CAS to CAS stays ahead of Stagger for the Summary's sake, which is what
    // the note above is about: Power down sitting between them here costs
    // nothing, because tCKE and tXP are both in the Summary's priority list
    // and so never reach the leftovers that this order feeds.
    let tertiary_left: &[(&str, &[&'static str])] = &[
        ("CAS to CAS", &["tCCD_L", "tCCD_L_WR", "tCCD_L_WR2"]),
        ("Power down", &["tCKE", "tXP"]),
        ("Stagger", &["tSTAG", "tSTAGsb"]),
        ("Mode register", &["tMRD", "tMOD", "tMRDPDA", "tMODPDA"]),
    ];
    let tertiary_right: &[(&str, &[&'static str])] = &[
        ("Turnaround", &["tRDWR", "tWRRD"]),
        ("Read to read", &["tRDRDSCL", "tRDRDSC", "tRDRDSD", "tRDRDDD"]),
        ("Write to write", &["tWRWRSCL", "tWRWRSC", "tWRWRSD", "tWRWRDD"]),
        ("PHY", &["tPHYWRD", "tPHYRDL", "tPHYWRL"]),
        ("Preamble / postamble", &["tRDPRE", "tRDPOST", "tWRPRE", "tWRPOST"]),
    ];

    let tertiary_rows = |groups: &[(&str, &[&'static str])], column: &str| -> Vec<Row> {
        let mut found = Vec::new();
        for (category, names) in groups {
            for &name in names.iter() {
                if name.starts_with("tCCD_L") {
                    // All three come from the APOB run rather than the UMC;
                    // see amd::apob::find_ccdl_run for why the registers are
                    // only the fallback.
                    found.push(ccdl_row(runtime, name, category));
                } else {
                    found.push(umc_row(runtime, name, category, column, None, None, "", None));
                }
            }
        }
        found
    };

    // Timings tab layout. A section's column comes from its rows. The Timings
    // tab then sorts each column by TIMINGS_SECTION_ORDER, so the order here
    // is not what the tab draws -- it is what the Summary's middle column
    // follows, which is why CAS to CAS still precedes Stagger above.
    //   left   Primary -> Secondary -> tertiary_left
    //   right  Refresh timings -> tertiary_right
    rows.extend(primary.into_iter().map(|name| umc_row(runtime, name, "Primary", "Left", None, None, "", None)));
    // The command rate closes the primary group. It reads as a timing -- 1T or
    // 2T is how long the controller holds a command -- so it belongs under tRC
    // rather than among the controller settings, and the Summary already
    // places it in the same spot.
    rows.push(make_row("CR", lazy(format_value(runtime, "cmd_rate", "")), "Primary", "Timings", "Left"));
    rows.extend(secondary.into_iter().map(|name| umc_row(runtime, name, "Secondary", "Left", None, None, "", None)));
    for name in refresh {
        let row = match name {
            "tREFIns" => {
                // The refresh interval in real time, beside the cycle count it is
                // derived from. Built by hand for the same reason tRFCns is: it
                // is two readings divided, not a register.
                dual(
                    make_row("tREFIns", lazy(format_refi_ns(runtime, None)), "Refresh timings", "Timings", "Right"),
                    format_refi_ns(runtime, Some("cha")),
                    format_refi_ns(runtime, Some("chb")),
                )
            }
            "tRFCns" => {
                // Derived from the active tRFC and MCLK, and it shows two intervals
                // outside Normal refresh mode, so it is built by hand.
                dual(
                    make_row("tRFCns", lazy(format_rfc_ns(runtime, None)), "Refresh timings", "Timings", "Right"),
                    format_rfc_ns(runtime, Some("cha")),
                    format_rfc_ns(runtime, Some("chb")),
                )
            }
            "tRFC" => {
                // Outside Normal mode the controller refreshes on tRFC2, so tRFC is
                // not in effect; dim it rather than implying it applies.
                let dim_runtime = runtime.clone();
                let dim: PredFn = Arc::new(move || !refresh_is_normal(&dim_runtime, None));
                umc_row(runtime, name, "Refresh timings", "Right", None, None, "", Some(dim))
            }
            "tRFC2" | "tRFCsb" => {
                let dim_runtime = runtime.clone();
                let dim: PredFn = Arc::new(move || refresh_is_normal(&dim_runtime, None));
                umc_row(runtime, name, "Refresh timings", "Right", None, None, "", Some(dim))
            }
            _ => umc_row(runtime, name, "Refresh timings", "Right", None, None, "", None),
        };
        rows.push(row);
    }
    rows.extend(tertiary_rows(tertiary_left, "Left"));

    let rtt_rows: &[(&str, &'static str)] = &[
        ("RTT WR", "rtt_wr"),
        ("RTT Nom RD", "rtt_nom_rd"),
        ("RTT Nom WR", "rtt_nom_wr"),
        ("RTT Park", "rtt_park"),
        ("RTT Park DQS", "rtt_park_dqs"),
    ];
    let odt_rows: &[(&str, &'static str)] = &[
        ("CA ODT A", "ca_odt_a"),
        ("CK ODT A", "ck_odt_a"),
        ("CS ODT A", "cs_odt_a"),
        ("CA ODT B", "ca_odt_b"),
        ("CK ODT B", "ck_odt_b"),
        ("CS ODT B", "cs_odt_b"),
    ];
    let mut drive_rows: &[(&str, &'static str)] = &[
        ("Proc ODT Pu", "proc_odt_pu"),
        ("Proc ODT Pd", "proc_odt_pd"),
        ("Proc CA DS", "proc_ca_ds"),
        ("Proc CK DS", "proc_ck_ds"),
        ("Proc CS DS", "proc_cs_ds"),
        // The one setting on the board's DDR Bus Configuration page that had
        // no row here. It heads the pull up and pull down pair because it is
        // what sets them: changing it in BIOS moved all three together.
        ("Proc DQ DS", "proc_dq_ds"),
        ("Proc DQ DS Pu", "proc_dq_ds_pu"),
        ("Proc DQ DS Pd", "proc_dq_ds_pd"),
        ("DRAM DQ DS Pu", "dram_dq_ds_pu"),
        ("DRAM DQ DS Pd", "dram_dq_ds_pd"),
    ];
    let mut make_training_row: fn(&Arc<Am5Runtime>, &str, &'static str, &str, &str, &str) -> Row =
        training_row;
    if is_raphael_cpu(&runtime.cpu_name()) {
        // Ryzen 7000's record has one processor ODT and one DRAM drive
        // strength where Ryzen 9000's has a pull-up and a pull-down of each,
        // so it gets those two rows in place of the four pairs.
        drive_rows = &[
            ("Proc ODT", "proc_odt"),
            ("Proc CA DS", "proc_ca_ds"),
            ("Proc CK DS", "proc_ck_ds"),
            ("Proc CS DS", "proc_cs_ds"),
            ("Proc DQ DS", "proc_dq_ds"),
            ("DRAM DQ DS", "dram_dq_ds"),
        ];
        // One record, not one per channel: the geometry that attributes a
        // record to a channel is known only for Ryzen 9000. Plain rows show
        // the record's value instead of two columns of dashes.
        make_training_row = single_training_row;
    }
    // Training, not Timings. These three groups are what memory training settled
    // on -- terminations, on-die termination and drive strengths -- rather
    // than intervals the controller was told to wait, and they filled the
    // Timings tab's right-hand column with twenty rows of a different kind of
    // thing. The Summary still gathers them by category, not by tab.
    rows.extend(rtt_rows.iter().map(|&(label, key)| make_training_row(runtime, label, key, "RTT", "Left", SKEW_TAB)));
    rows.extend(odt_rows.iter().map(|&(label, key)| make_training_row(runtime, label, key, "ODT", "Left", SKEW_TAB)));
    rows.extend(
        drive_rows
            .iter()
            .map(|&(label, key)| make_training_row(runtime, label, key, "Drive Strength", "Right", SKEW_TAB)),
    );
    rows.extend(channel_settings);
    rows.extend(tertiary_rows(tertiary_right, "Right"));
    rows
}

/// Python's `apply_formula`, kept beside the table for the dispatcher: a
/// missing value is the em dash, and a formula, where there is one, words
/// the rest. No AM5 row carries a register read, so the window never calls
/// it for one; it is here so the AM5 backend answers the same question the
/// other two do.
pub fn apply_formula(value: Option<String>, formula: Option<&dyn Fn(String) -> String>) -> String {
    let Some(value) = value else {
        return EM_DASH.into();
    };
    match formula {
        Some(formula) => formula(value),
        None => value,
    }
}
