//! The Telemetry pop-out: every live reading, with statistics.
//!
//! Separate from the tabs because it behaves differently. The tabs read
//! settings, which do not move; this polls and keeps what each reading did
//! over time, which is the only way a rail that sags under load shows itself.
//! Everything that moves lives here: the board and CPU sensors that used to
//! be the Sensors tab, and each DIMM's own PMIC.
//!
//! Reading a PMIC channel costs one ADC channel select -- see
//! `rochviewer::memory::ddr5_telemetry` -- so the poll is deliberately
//! unhurried and stops the moment the window closes.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, CornerRadius, Rect, Sense, Ui, UiBuilder, pos2, vec2};

use rochviewer::memory::ddr5_telemetry::DimmTelemetry;
use rochviewer::memory::dimm_inventory::split_ic;

use super::popouts::{self, PopoutStyle};
use super::theme::Palette;

pub const POLL_MS: u64 = 1000;

/// Tall enough for two panels; a four-DIMM board scrolls.
pub const WINDOW_WIDTH: f32 = 600.0;
pub const WINDOW_HEIGHT: f32 = 800.0;

/// (key, label, unit, decimals). Order is the order they are shown.
pub const PARAMETERS: &[(&str, &str, &str, usize)] = &[
    ("hub_temp_c", "SPD Hub Temp", "°C", 2),
    ("vdd", "VDD (SWA)", "V", 3),
    ("vddq", "VDDQ (SWB)", "V", 3),
    ("vpp", "VPP (SWC)", "V", 3),
    // Named as the Voltages tab names the same PMIC readings: VIN_Bulk is the
    // module's 5 V input, the lower two the PMIC's LDO outputs.
    ("vin_bulk", "VIN (5V)", "V", 3),
    ("vout_1v8", "LDO 1.8V", "V", 3),
    ("vout_1v0", "LDO 1.0V", "V", 3),
    // The sum only. The three per-rail figures it is made of were shown
    // above it and said nothing the total does not: VDD carries almost all
    // of a DDR5 module's draw, so the split read as the total, a small
    // number and a rounding error on every board tried.
    ("power_w", "Total Power", "W", 3),
];

/// Millivolt readings are shown in volts; the rest are already in their unit.
pub const MILLIVOLT_KEYS: &[&str] = &["vdd", "vddq", "vpp", "vin_bulk", "vout_1v8", "vout_1v0"];

pub const COLUMNS: [&str; 5] = ["Parameter", "Current", "Min", "Max", "Average"];

/// Counts rather than readings. A count since boot only climbs, so its
/// maximum is its current value and its minimum and average say nothing:
/// these show the count alone, and the other three cells stay empty.
pub const COUNTER_ROWS: &[&str] = &["WHEA Errors"];

/// Groups that belong below the per-DIMM panels rather than above them. The
/// sensor tables exist before any DIMM has answered, so a group meant for
/// the foot of the window has to be sunk once the panels exist.
///
/// Errors is there because it is not a reading of anything: it is the counter
/// you check after a run rather than watch during one, and putting it between
/// the voltages and the modules interrupted two things that belong together.
/// Graphics is here because the card's sensors are a separate machine from
/// the one the rest of the window describes, and reading past them to reach
/// the modules put the memory tool's own subject last. Errors stays after it.
pub const TRAILING_GROUPS: [&str; 2] = ["Graphics", "Errors"];

/// A row with children carries one of these ahead of its name, and folds
/// them away until it is clicked. Sixteen logical processors is a useful
/// thing to be able to open and a poor thing to have to scroll past to reach
/// the temperatures, so they start closed.
pub const COLLAPSED_MARK: &str = "▸ ";
pub const EXPANDED_MARK: &str = "▾ ";
/// Children are indented under the parent rather than marked, which keeps
/// the Parameter column readable at a glance.
pub const CHILD_INDENT: &str = "    ";

/// Rows sampled between ticks as well as on them.
///
/// The window redraws once a second, which is often enough to read and not
/// often enough to see what a die temperature did in between: against
/// HWiNFO polling faster, our Core Max maximum came out 58 where its was 61
/// over the same run, while the minimum and the average agreed exactly. A
/// maximum that misses the peaks is the wrong number on the one row whose
/// peaks are the point.
///
/// Only rows that are cheap to read belong here. Core Max is a single MCHBAR
/// word, microseconds, so it can be read on the UI thread without the worker
/// the full tick needs.
pub const FAST_SAMPLE_LABELS: &[&str] = &["Core Max"];
pub const FAST_SAMPLE_MS: u64 = 200;

// Every line of a table is one CTkLabel high, as the Python drew it.
const LINE_HEIGHT: f32 = 28.0;
const TABLE_GAP: f32 = 10.0;
const NAME_MIN_WIDTH: f32 = 150.0;
const CELL_PAD: f32 = 6.0;
const FOOTER_HEIGHT: f32 = 28.0;
const RESET_WIDTH: f32 = 90.0;
const RESET_HEIGHT: f32 = 22.0;
const SWITCH_WIDTH: f32 = 32.0;
const SWITCH_HEIGHT: f32 = 16.0;

/// A displayed reading is text: "1.244 V", "38.7 / 300.0 W", "42.1 °C",
/// "Off". The statistics need the number, and the first one is always the
/// reading -- where a second follows it is the configured limit, which does
/// not move.
///
/// Returns `(value, unit, decimals)`; `(None, "", 0)` when there is no number
/// in it, which is how a row reading "Off" or an em dash keeps its place
/// without inventing statistics.
pub fn parse_reading(text: &str) -> (Option<f64>, String, usize) {
    let text = text.trim();
    let chars: Vec<char> = text.chars().collect();
    // The first match of -?\d+(?:\.\d+)?
    let digit = |i: usize| chars.get(i).is_some_and(|c| c.is_ascii_digit());
    let Some(start) = (0..chars.len()).find(|&i| digit(i) || (chars[i] == '-' && digit(i + 1))) else {
        return (None, String::new(), 0);
    };
    let mut end = if chars[start] == '-' { start + 1 } else { start };
    while digit(end) {
        end += 1;
    }
    let mut decimals = 0;
    if chars.get(end) == Some(&'.') && digit(end + 1) {
        end += 1;
        while digit(end) {
            end += 1;
            decimals += 1;
        }
    }
    let number: String = chars[start..end].iter().collect();
    // The unit: what follows the leading run of [-\d.\s/] when the text
    // opens with one.
    let lead = chars.iter().take_while(|c| c.is_ascii_digit() || c.is_whitespace() || matches!(c, '-' | '.' | '/')).count();
    let mut unit = if lead > 0 { chars[lead..].iter().collect::<String>().trim().to_string() } else { String::new() };
    if unit.chars().any(|c| c.is_ascii_digit()) {
        // A second number followed: the unit is what trails the last one.
        unit = unit.split_whitespace().last().unwrap_or("").to_string();
    }
    (number.parse().ok(), unit, decimals)
}

/// Min, max and running mean for one parameter of one DIMM.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Statistic {
    pub current: Option<f64>,
    pub minimum: Option<f64>,
    pub maximum: Option<f64>,
    pub total: f64,
    pub count: u64,
}

impl Statistic {
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn add(&mut self, value: Option<f64>) {
        let Some(value) = value else { return };
        self.current = Some(value);
        self.minimum = Some(self.minimum.map_or(value, |m| m.min(value)));
        self.maximum = Some(self.maximum.map_or(value, |m| m.max(value)));
        self.total += value;
        self.count += 1;
    }

    pub fn average(&self) -> Option<f64> {
        (self.count > 0).then(|| self.total / self.count as f64)
    }
}

/// Pull one parameter out of a telemetry entry, in display units.
pub fn reading_value(entry: &DimmTelemetry, key: &str) -> Option<f64> {
    let value = entry.get(key)?;
    Some(if MILLIVOLT_KEYS.contains(&key) { value / 1000.0 } else { value })
}

pub fn format_value(value: Option<f64>, unit: &str, decimals: usize) -> String {
    match value {
        None => "—".to_string(),
        Some(value) => format!("{value:.decimals$} {unit}"),
    }
}

pub fn format_elapsed(seconds: f64) -> String {
    let seconds = if seconds > 0.0 { seconds as u64 } else { 0 };
    format!("{:02}:{:02}:{:02}", seconds / 3600, (seconds % 3600) / 60, seconds % 60)
}

/// A module's identity as the panel shows it: SMBIOS names the module and
/// the slot; the SPD adds the module vendor, which firmware often leaves as
/// "Unknown".
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TelemetryModule {
    pub channel: Option<String>,
    pub slot: Option<String>,
    pub capacity_gb: i64,
    pub rank_numeric: Option<String>,
    pub ic: Option<String>,
    pub part_number: Option<String>,
    pub spd_vendor: Option<String>,
    pub module_manufacturer: Option<String>,
}

fn non_empty(value: &Option<String>) -> Option<&str> {
    value.as_deref().filter(|v| !v.is_empty())
}

/// The heading over one DIMM's table: which module, and which PMIC.
pub fn panel_title(index: usize, entry: &DimmTelemetry, module: Option<&TelemetryModule>) -> String {
    let slot = module
        .and_then(|m| non_empty(&m.slot))
        .map(str::to_string)
        .unwrap_or_else(|| if entry.channel.is_empty() { "?".to_string() } else { entry.channel.to_uppercase() });
    format!("DIMM {index}  |  {slot}  |  PMIC 0x{:02X}", entry.pmic_address)
}

pub type IdentityLines = Vec<(String, String)>;

/// The identity lines under a heading, as label/value pairs.
///
/// Two different makers appear here and the labels keep them apart: the
/// module vendor is whose name is on the stick, while the DRAM manufacturer
/// is who made the chips on it. They are rarely the same company -- G.Skill
/// sells a module carrying SK hynix silicon.
pub fn panel_identity(entry: &DimmTelemetry, module: Option<&TelemetryModule>) -> (IdentityLines, IdentityLines) {
    let empty = TelemetryModule::default();
    let module = module.unwrap_or(&empty);
    let (maker, die) = split_ic(module.ic.as_deref());
    let mut pmic = entry.vendor.clone().filter(|v| !v.is_empty()).unwrap_or_else(|| "—".to_string());
    if let Some(revision) = entry.revision.as_deref().filter(|r| !r.is_empty()) {
        pmic = format!("{pmic} rev {revision}");
    }
    let dash = |value: Option<&str>| value.unwrap_or("—").to_string();

    // Left reads down the chips themselves; right reads down the stick they
    // are on and what powers it. Both makers are named for what they made --
    // the DRAM and the module -- because "manufacturer" alone fits either.
    // Worded as the SPD tab words the same facts.
    let mut left: IdentityLines = vec![
        ("DRAM Manuf.".into(), maker),
        ("DRAM Die".into(), die),
        ("Ranks".into(), dash(non_empty(&module.rank_numeric))),
    ];
    let mut right: IdentityLines = vec![
        ("Part Number".into(), dash(non_empty(&module.part_number))),
        ("Module Manuf.".into(), dash(non_empty(&module.spd_vendor).or(non_empty(&module.module_manufacturer)))),
        (
            "Module Size".into(),
            if module.capacity_gb != 0 { format!("{} GB", module.capacity_gb) } else { "—".into() },
        ),
        ("PMIC".into(), pmic),
    ];
    // The two sides are drawn in pairs, so the shorter one is padded rather
    // than cutting the longer one short.
    while left.len() < right.len() {
        left.push((String::new(), String::new()));
    }
    while right.len() < left.len() {
        right.push((String::new(), String::new()));
    }
    (left, right)
}

/// Which statistic a cell belongs to: a sensor row by (group, label), or one
/// parameter of one DIMM.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum StatKey {
    Sensor(String, String),
    Dimm(usize, &'static str),
}

impl StatKey {
    pub fn sensor(group: &str, label: &str) -> Self {
        StatKey::Sensor(group.to_string(), label.to_string())
    }

    fn label(&self) -> &str {
        match self {
            StatKey::Sensor(_, label) => label,
            StatKey::Dimm(_, key) => key,
        }
    }
}

pub type SensorRead = Arc<dyn Fn() -> String + Send + Sync>;

/// One sensor row: its label, how to read it, and the row it folds under
/// (the per-processor effective clocks under their aggregate), if any.
#[derive(Clone)]
pub struct SensorReading {
    pub label: String,
    pub read: SensorRead,
    pub parent: Option<String>,
}

pub type SensorGroup = (String, Vec<SensorReading>);

/// Read every sensor row. Off the UI thread; touches nothing it draws.
pub fn read_sensors(groups: &[SensorGroup]) -> Vec<(StatKey, String)> {
    let mut readings = Vec::new();
    for (title, group) in groups {
        for reading in group {
            let text = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (reading.read)()))
                .unwrap_or_else(|_| "—".to_string());
            readings.push((StatKey::sensor(title, &reading.label), text));
        }
    }
    readings
}

/// The background for one data row, alternating from an unshaded one.
/// `None` is transparent: a caller with no band colour gets none.
pub fn band(band_colour: Option<Color32>, data_row: usize) -> Option<Color32> {
    if data_row % 2 == 1 { band_colour } else { None }
}

/// One row of a table, as far as folding and revealing go.
#[derive(Clone, Debug, PartialEq)]
pub struct Record {
    pub parent: Option<String>,
    pub pending: bool,
}

/// How one row is drawn this frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Stripe {
    /// Folded away or not read yet: no height at all, so a hidden row never
    /// holds open a block of empty table.
    Hidden,
    Shown(Option<Color32>),
}

/// Colour one table's bands by what is currently visible.
///
/// Banding by the row's position in the table breaks as soon as rows can be
/// hidden: the alternation counts rows nobody can see, so closing a group
/// leaves two shaded rows adjacent. Counting only the visible ones keeps the
/// stripes alternating whatever is open.
pub fn restripe(records: &[Record], expanded: &HashMap<String, bool>, band_colour: Option<Color32>) -> Vec<Stripe> {
    let mut visible = 0;
    records
        .iter()
        .map(|record| {
            let folded = record.parent.as_ref().is_some_and(|p| !expanded.get(p).copied().unwrap_or(false));
            if folded || record.pending {
                return Stripe::Hidden;
            }
            let stripe = Stripe::Shown(band(band_colour, visible));
            visible += 1;
            stripe
        })
        .collect()
}

/// Where each part of the window goes: the sensor groups, with the DIMM
/// panels after them and the trailing groups sunk below the panels once the
/// panels exist.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Group(usize),
    Panels,
}

pub fn section_order(titles: &[&str], panels_built: bool) -> Vec<Section> {
    if !panels_built {
        return (0..titles.len()).map(Section::Group).collect();
    }
    let mut order: Vec<Section> =
        (0..titles.len()).filter(|&i| !TRAILING_GROUPS.contains(&titles[i])).map(Section::Group).collect();
    order.push(Section::Panels);
    for trailing in TRAILING_GROUPS {
        order.extend((0..titles.len()).filter(|&i| titles[i] == trailing).map(Section::Group));
    }
    order
}

/// Whether a tick should start, or wait for the one still reading.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PollStep {
    StartReader,
    WaitATick,
}

/// One tick's scheduling.
///
/// The reads happen on a worker because they block: a DIMM's eight ADC
/// channels need 12 ms each to settle, which is 183 ms per tick of two
/// modules, and on the UI thread that is 183 ms in every second where the
/// window will not scroll.
///
/// A tick that overruns its interval is skipped rather than queued, the way
/// the main window's refresh does it: falling behind should cost samples,
/// not build a backlog against the same mutexes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PollState {
    pub reading: bool,
}

impl PollState {
    pub fn poll(&mut self) -> PollStep {
        if self.reading {
            return PollStep::WaitATick;
        }
        self.reading = true;
        PollStep::StartReader
    }

    pub fn finished(&mut self) {
        self.reading = false;
    }
}

pub type TelemetryRead = Arc<dyn Fn() -> Vec<DimmTelemetry> + Send + Sync>;
pub type ModulesRead = Arc<dyn Fn() -> Vec<TelemetryModule> + Send + Sync>;

/// What one tick read.
#[derive(Default)]
struct TickResult {
    readings: Vec<(StatKey, String)>,
    entries: Vec<DimmTelemetry>,
    modules: HashMap<String, TelemetryModule>,
    failure: Option<String>,
}

struct TickRequest {
    need_modules: bool,
}

fn worker(
    groups: Arc<Vec<SensorGroup>>,
    read_telemetry: TelemetryRead,
    read_modules: ModulesRead,
    requests: Receiver<TickRequest>,
    results: Arc<Mutex<Option<TickResult>>>,
    repaint: impl Fn(),
) {
    // Any value backed by WMI needs COM on this thread; wmi_util keeps a
    // connection per thread and initialises it on first use.
    while let Ok(request) = requests.recv() {
        let mut result = TickResult::default();
        let read = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let readings = read_sensors(&groups);
            let entries = read_telemetry();
            let modules = if !entries.is_empty() && request.need_modules {
                read_modules()
                    .into_iter()
                    .map(|m| (m.channel.clone().unwrap_or_default().to_lowercase(), m))
                    .collect()
            } else {
                HashMap::new()
            };
            (readings, entries, modules)
        }));
        match read {
            Ok((readings, entries, modules)) => {
                result.readings = readings;
                result.entries = entries;
                result.modules = modules;
            }
            Err(panic) => {
                let message = panic
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
                    .unwrap_or_else(|| "a reader panicked".into());
                result.failure = Some(message);
            }
        }
        *results.lock().unwrap_or_else(|e| e.into_inner()) = Some(result);
        repaint();
    }
}

struct Panel {
    title: String,
    left: IdentityLines,
    right: IdentityLines,
}

/// One window, one panel per populated DIMM, polling while it is open.
pub struct DimmTelemetryWindow {
    pub open: bool,
    pub palette: Palette,
    /// A change to the "open automatically" switch, for the main window to
    /// save; `None` when there is nothing to save.
    pub auto_open_changed: Option<bool>,
    sensor_groups: Arc<Vec<SensorGroup>>,
    /// Sensor row keys that stay hidden until a tick reads them: a fan header
    /// with nothing on it, a board channel on a board it is not mapped for.
    /// Decided by the worker's readings, never by a read on this thread, and
    /// a row that answers late -- a transport busy at open, a zero-RPM fan
    /// that spins up -- still appears.
    pending: HashSet<StatKey>,
    read_telemetry: TelemetryRead,
    read_modules: ModulesRead,
    poll: Duration,
    poll_state: PollState,
    next_poll: Instant,
    elapsed: f64,
    poll_failed: bool,
    stats: HashMap<StatKey, Statistic>,
    cells: HashMap<(StatKey, usize), String>,
    panels: Vec<Panel>,
    panels_built: bool,
    /// Which parents are currently open. Children keep polling while folded
    /// away, so their minimum and maximum cover the whole run rather than
    /// starting from whenever the row was first opened.
    expanded: HashMap<String, bool>,
    auto_open: bool,
    /// Rows whose statistics are fed between ticks, and the keys to skip
    /// when the tick comes round: fed from both, the average would count the
    /// same row five times a second and once a second.
    fast_rows: Vec<(StatKey, SensorRead)>,
    fast_keys: HashSet<StatKey>,
    next_fast: Instant,
    requests: Option<Sender<TickRequest>>,
    results: Arc<Mutex<Option<TickResult>>>,
}

impl DimmTelemetryWindow {
    pub fn new(
        palette: Palette,
        read_telemetry: TelemetryRead,
        read_modules: ModulesRead,
        auto_open: bool,
        sensor_groups: Vec<SensorGroup>,
        reveal_on_read: HashSet<StatKey>,
    ) -> Self {
        let mut cells = HashMap::new();
        let mut pending = HashSet::new();
        for (title, group) in &sensor_groups {
            for reading in group {
                let key = StatKey::sensor(title, &reading.label);
                for column in 1..COLUMNS.len() {
                    cells.insert((key.clone(), column), "—".to_string());
                }
                if reveal_on_read.contains(&key) {
                    pending.insert(key);
                }
            }
        }
        let fast_rows: Vec<(StatKey, SensorRead)> = sensor_groups
            .iter()
            .flat_map(|(title, group)| {
                group
                    .iter()
                    .filter(|r| FAST_SAMPLE_LABELS.contains(&r.label.as_str()))
                    .map(move |r| (StatKey::sensor(title, &r.label), r.read.clone()))
            })
            .collect();
        let fast_keys = fast_rows.iter().map(|(key, _)| key.clone()).collect();
        let now = Instant::now();
        DimmTelemetryWindow {
            open: true,
            palette,
            auto_open_changed: None,
            sensor_groups: Arc::new(sensor_groups),
            pending,
            read_telemetry,
            read_modules,
            poll: Duration::from_millis(POLL_MS),
            poll_state: PollState::default(),
            // The first tick goes almost at once, as the Python's after(50).
            next_poll: now + Duration::from_millis(50),
            elapsed: 0.0,
            poll_failed: false,
            stats: HashMap::new(),
            cells,
            panels: Vec::new(),
            panels_built: false,
            expanded: HashMap::new(),
            auto_open,
            fast_rows,
            fast_keys,
            next_fast: now + Duration::from_millis(FAST_SAMPLE_MS),
            requests: None,
            results: Arc::new(Mutex::new(None)),
        }
    }

    /// The text of one cell (column 1 Current .. 4 Average).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn cell(&self, key: &StatKey, column: usize) -> Option<&str> {
        self.cells.get(&(key.clone(), column)).map(String::as_str)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn is_pending(&self, key: &StatKey) -> bool {
        self.pending.contains(key)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn statistic(&self, key: &StatKey) -> Option<&Statistic> {
        self.stats.get(key)
    }

    pub fn running_text(&self) -> String {
        format!("Running: {}", format_elapsed(self.elapsed))
    }

    /// Show a row held back until it read. Its table comes back with it,
    /// since a table is drawn whenever any of its rows is.
    fn reveal_row(&mut self, key: &StatKey) {
        self.pending.remove(key);
    }

    /// Write the sensor readings into their cells. UI thread only.
    ///
    /// The Current column keeps the row's own text, so a reading that pairs
    /// a value with its limit -- "38.7 / 300.0 W" -- still reads the way it
    /// does everywhere else, while the statistics track the first number,
    /// which is the part that moves.
    pub fn apply_sensors(&mut self, readings: Vec<(StatKey, String)>) {
        for (key, text) in readings {
            if self.pending.contains(&key) && !matches!(text.trim(), "" | "\u{2014}" | "N/A") {
                self.reveal_row(&key);
            }
            let (value, unit, decimals) = parse_reading(&text);
            let statistic = self.stats.entry(key.clone()).or_default();
            // A fast-sampled row is already being fed, five times a second.
            // Adding here as well would weight this one sample like five and
            // pull the average toward whatever the tick happened to catch.
            if !self.fast_keys.contains(&key) {
                statistic.add(value);
            }
            let tracked = [statistic.minimum, statistic.maximum, statistic.average()];
            self.cells.insert((key.clone(), 1), text);
            if COUNTER_ROWS.contains(&key.label()) {
                for column in 2..=4 {
                    self.cells.insert((key.clone(), column), String::new());
                }
                continue;
            }
            for (column, value) in (2..=4).zip(tracked) {
                self.cells.insert((key.clone(), column), format_value(value, &unit, decimals));
            }
        }
    }

    /// Feed the between-tick statistics for rows whose peaks matter.
    ///
    /// On the UI thread, unlike the full tick: these rows are a register
    /// read each, and handing them to a worker would cost more in thread
    /// handoff than the read itself. Nothing here writes a cell -- the cells
    /// are redrawn on the next tick, with the extremes this collected.
    fn fast_sample(&mut self) {
        for (key, read) in &self.fast_rows {
            let Ok(text) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| read())) else {
                continue;
            };
            if let (Some(value), _, _) = parse_reading(&text) {
                self.stats.entry(key.clone()).or_default().add(Some(value));
            }
        }
    }

    fn build_panels(&mut self, entries: &[DimmTelemetry], modules: &HashMap<String, TelemetryModule>) {
        for (index, entry) in entries.iter().enumerate() {
            let module = modules.get(entry.channel);
            let (left, right) = panel_identity(entry, module);
            self.panels.push(Panel { title: panel_title(index, entry, module), left, right });
            for (key, ..) in PARAMETERS {
                for column in 1..COLUMNS.len() {
                    self.cells.insert((StatKey::Dimm(index, key), column), "—".to_string());
                }
            }
        }
        self.panels_built = true;
    }

    /// Draw one tick's readings and book the next. UI thread only.
    ///
    /// The next tick is booked whatever happens here, so a fault stops a
    /// reading rather than the window: in the Python a mismatch between two
    /// drawing calls once raised on the first tick and the poll was never
    /// rescheduled, which left the timer at 00:00:00 -- reading as "nothing
    /// to report" rather than "broken".
    fn apply_tick(&mut self, result: TickResult) {
        if let Some(failure) = &result.failure {
            self.report_failure(failure);
        }
        self.apply_sensors(result.readings);
        if !result.entries.is_empty() && !self.panels_built {
            self.build_panels(&result.entries, &result.modules);
        }
        for (index, entry) in result.entries.iter().enumerate() {
            for &(key, _label, unit, decimals) in PARAMETERS {
                let statistic = self.stats.entry(StatKey::Dimm(index, key)).or_default();
                statistic.add(reading_value(entry, key));
                let shown = [statistic.current, statistic.minimum, statistic.maximum, statistic.average()];
                for (column, value) in (1..=4).zip(shown) {
                    self.cells.insert((StatKey::Dimm(index, key), column), format_value(value, unit, decimals));
                }
            }
        }
        self.elapsed += self.poll.as_secs_f64();
        self.poll_state.finished();
        self.next_poll = Instant::now() + self.poll;
    }

    /// Say what went wrong once, rather than every tick from now on.
    fn report_failure(&mut self, failure: &str) {
        if !self.poll_failed {
            self.poll_failed = true;
            eprintln!("Telemetry poll failed: {failure}");
        }
    }

    pub fn reset_statistics(&mut self) {
        for statistic in self.stats.values_mut() {
            statistic.reset();
        }
        self.elapsed = 0.0;
        for text in self.cells.values_mut() {
            *text = "—".to_string();
        }
    }

    fn start_worker(&mut self, ctx: &egui::Context) {
        if self.requests.is_some() {
            return;
        }
        let (sender, receiver) = mpsc::channel();
        self.requests = Some(sender);
        let groups = self.sensor_groups.clone();
        let read_telemetry = self.read_telemetry.clone();
        let read_modules = self.read_modules.clone();
        let results = self.results.clone();
        let ctx = ctx.clone();
        let viewport = ctx.viewport_id();
        let spawned = std::thread::Builder::new().name("telemetry-reader".into()).spawn(move || {
            worker(groups, read_telemetry, read_modules, receiver, results, move || ctx.request_repaint_of(viewport))
        });
        if let Err(error) = spawned {
            self.report_failure(&error.to_string());
        }
    }

    /// Run the clock: apply a finished tick, start the next, sample the fast
    /// rows. Called every frame.
    fn run(&mut self, ctx: &egui::Context) {
        self.start_worker(ctx);
        if let Some(result) = self.results.lock().ok().and_then(|mut r| r.take()) {
            self.apply_tick(result);
        }
        let now = Instant::now();
        if now >= self.next_poll {
            match self.poll_state.poll() {
                PollStep::StartReader => {
                    let request = TickRequest { need_modules: !self.panels_built };
                    if self.requests.as_ref().is_none_or(|r| r.send(request).is_err()) {
                        self.poll_state.finished();
                    }
                    // Booked again when the tick lands.
                    self.next_poll = now + Duration::from_secs(3600);
                }
                PollStep::WaitATick => self.next_poll = now + self.poll,
            }
        }
        if !self.fast_rows.is_empty() && now >= self.next_fast {
            self.fast_sample();
            self.next_fast = now + Duration::from_millis(FAST_SAMPLE_MS);
        }
        let mut wake = self.next_poll.saturating_duration_since(now);
        if !self.fast_rows.is_empty() {
            wake = wake.min(self.next_fast.saturating_duration_since(now));
        }
        ctx.request_repaint_after(wake);
    }

    /// Draw the window. Called from the viewport's own frame.
    pub fn ui(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        if ctx.input(|i| i.viewport().close_requested()) {
            self.open = false;
            return;
        }
        self.run(&ctx);
        let palette = self.palette;
        let style = PopoutStyle::new(&palette);
        egui::CentralPanel::default().frame(egui::Frame::new().fill(palette.bg)).show(ui, |ui| {
            let full = ui.max_rect();
            let footer = Rect::from_min_max(
                pos2(full.left() + 6.0, full.bottom() - 4.0 - FOOTER_HEIGHT),
                pos2(full.right() - 6.0, full.bottom() - 4.0),
            );
            let body = Rect::from_min_max(pos2(full.left() + 4.0, full.top() + 4.0), pos2(full.right() - 4.0, footer.top() - 4.0));
            ui.scope_builder(UiBuilder::new().max_rect(body), |ui| self.body(ui, &style));
            self.footer(ui, &style, footer);
        });
    }

    fn body(&mut self, ui: &mut Ui, style: &PopoutStyle) {
        style.apply_scrollbar(ui);
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            // The bar keeps its place when everything fits, so a filter
            // never moves the value columns sideways.
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
            .show(ui, |ui| {
            let width = ui.available_width();
            let groups = self.sensor_groups.clone();
            let titles: Vec<&str> = groups.iter().map(|(t, _)| t.as_str()).collect();
            for section in section_order(&titles, self.panels_built) {
                match section {
                    Section::Group(index) => self.sensor_table(ui, style, width, &groups[index]),
                    Section::Panels => {
                        for index in 0..self.panels.len() {
                            self.dimm_panel(ui, style, width, index);
                        }
                    }
                }
            }
        });
    }

    fn sensor_table(&mut self, ui: &mut Ui, style: &PopoutStyle, width: f32, group: &SensorGroup) {
        let (title, readings) = group;
        // Which labels have children, so a parent knows to draw a toggle and
        // a child knows to start hidden.
        let parents: HashSet<&str> = readings.iter().filter_map(|r| r.parent.as_deref()).collect();
        let records: Vec<Record> = readings
            .iter()
            .map(|r| Record { parent: r.parent.clone(), pending: self.pending.contains(&StatKey::sensor(title, &r.label)) })
            .collect();
        // A table whose every row is still waiting for a reading is not
        // drawn at all.
        if !records.is_empty() && records.iter().all(|r| r.pending) {
            return;
        }
        let stripes = restripe(&records, &self.expanded, Some(self.palette.band));
        let lines: Vec<TableLine> = readings
            .iter()
            .zip(stripes)
            .map(|(reading, stripe)| {
                let name = if parents.contains(reading.label.as_str()) {
                    let mark = if self.expanded.get(&reading.label).copied().unwrap_or(false) { EXPANDED_MARK } else { COLLAPSED_MARK };
                    format!("{mark}{}", reading.label)
                } else if reading.parent.is_some() {
                    format!("{CHILD_INDENT}{}", reading.label)
                } else {
                    reading.label.clone()
                };
                TableLine {
                    name,
                    key: StatKey::sensor(title, &reading.label),
                    toggle: parents.contains(reading.label.as_str()).then(|| reading.label.clone()),
                    stripe,
                }
            })
            .collect();
        let heading = title.to_uppercase();
        if let Some(toggled) = draw_table(ui, style, &self.palette, width, Some(&heading), &lines, &self.cells) {
            let open = self.expanded.entry(toggled).or_insert(false);
            *open = !*open;
            ui.ctx().request_repaint();
        }
        ui.add_space(TABLE_GAP);
    }

    fn dimm_panel(&mut self, ui: &mut Ui, style: &PopoutStyle, width: f32, index: usize) {
        let palette = self.palette;
        let panel = &self.panels[index];
        heading_line(ui, style, &palette, width, &panel.title);

        // The identity block: two label/value pairs to a line, the labels in
        // fixed columns and the values taking what is left.
        ui.add_space(2.0);
        let measure = |text: &str| ui.painter().layout_no_wrap(text.to_string(), style.font.clone(), palette.text).size().x;
        let natural = |items: &IdentityLines, value: bool, pad: f32| {
            items.iter().map(|(l, v)| measure(if value { v } else { l }) + pad).fold(0.0f32, f32::max)
        };
        let col0 = natural(&panel.left, false, 12.0).max(140.0);
        let col2 = natural(&panel.right, false, 12.0).max(165.0);
        let nat1 = natural(&panel.left, true, 4.0);
        let nat3 = natural(&panel.right, true, 4.0);
        let extra = ((width - col0 - col2 - nat1 - nat3) / 2.0).max(0.0);
        let starts = [0.0, col0, col0 + nat1 + extra, col0 + nat1 + extra + col2];
        for ((left_label, left_value), (right_label, right_value)) in panel.left.iter().zip(&panel.right) {
            let (rect, _) = ui.allocate_exact_size(vec2(width, LINE_HEIGHT), Sense::hover());
            let y = rect.center().y;
            let painter = ui.painter();
            for (start, text, color, pad) in [
                (starts[0], left_label, palette.text, CELL_PAD),
                (starts[1], left_value, palette.text, 2.0),
                (starts[2], right_label, palette.text, CELL_PAD),
                (starts[3], right_value, palette.text, 2.0),
            ] {
                painter.text(pos2(rect.left() + start + pad, y), egui::Align2::LEFT_CENTER, text, style.font.clone(), color);
            }
        }
        ui.add_space(4.0);

        // Banded through the same rule the sensor tables use. Nothing here
        // folds away, so every row is visible and the stripes run in order.
        let records = vec![Record { parent: None, pending: false }; PARAMETERS.len()];
        let stripes = restripe(&records, &HashMap::new(), Some(palette.band));
        let lines: Vec<TableLine> = PARAMETERS
            .iter()
            .zip(stripes)
            .map(|(&(key, label, ..), stripe)| TableLine { name: label.to_string(), key: StatKey::Dimm(index, key), toggle: None, stripe })
            .collect();
        draw_table(ui, style, &palette, width, None, &lines, &self.cells);
        ui.add_space(TABLE_GAP);
    }

    fn footer(&mut self, ui: &mut Ui, style: &PopoutStyle, footer: Rect) {
        let palette = self.palette;
        let painter = ui.painter().clone();
        painter.text(footer.left_center(), egui::Align2::LEFT_CENTER, self.running_text(), style.font.clone(), palette.text);

        let reset_rect = Rect::from_center_size(pos2(footer.right() - RESET_WIDTH / 2.0, footer.center().y), vec2(RESET_WIDTH, RESET_HEIGHT));
        let reset = ui.interact(reset_rect, ui.scope_id().with("reset"), Sense::click());
        let fill = if reset.hovered() { palette.tab_unselected_hover } else { palette.bg };
        painter.rect_filled(reset_rect, CornerRadius::same(6), fill);
        painter.text(reset_rect.center(), egui::Align2::CENTER_CENTER, "Reset Stats", style.bold.clone(), palette.text);
        if reset.clicked() {
            self.reset_statistics();
            ui.ctx().request_repaint();
        }

        // Opening this window costs an SMBus poll every second, so whether
        // it opens with the app is remembered rather than assumed.
        let text_width = painter.layout_no_wrap("Auto-open".into(), style.font.clone(), palette.text).size().x;
        let text_right = reset_rect.left() - 10.0;
        let switch_right = text_right - text_width - 7.0;
        let switch_rect = Rect::from_min_max(
            pos2(switch_right - SWITCH_WIDTH, footer.center().y - SWITCH_HEIGHT / 2.0),
            pos2(switch_right, footer.center().y + SWITCH_HEIGHT / 2.0),
        );
        let hit = Rect::from_min_max(switch_rect.left_top(), pos2(text_right, switch_rect.bottom()));
        let toggle = ui.interact(hit.expand2(vec2(0.0, 3.0)), ui.scope_id().with("auto-open"), Sense::click());
        if toggle.clicked() {
            self.auto_open = !self.auto_open;
            self.auto_open_changed = Some(self.auto_open);
            // The main window saves it, on its next pass.
            ui.ctx().request_repaint_of(egui::ViewportId::ROOT);
        }
        draw_switch(&painter, &palette, switch_rect, self.auto_open, toggle.hovered());
        painter.text(pos2(text_right, footer.center().y), egui::Align2::RIGHT_CENTER, "Auto-open", style.font.clone(), palette.text);
    }
}

/// The switch as customtkinter draws it: a rounded track, a round knob at
/// the end it is set to.
fn draw_switch(painter: &egui::Painter, palette: &Palette, rect: Rect, on: bool, hovered: bool) {
    let (track_off, knob, knob_hover) = if palette.dark {
        (Color32::from_rgb(0x4A, 0x4D, 0x50), Color32::from_rgb(0xD5, 0xD9, 0xDE), Color32::WHITE)
    } else {
        (Color32::from_rgb(0x93, 0x9B, 0xA2), Color32::from_rgb(0x5C, 0x5C, 0x5C), Color32::from_rgb(0x33, 0x33, 0x33))
    };
    // On, the track takes the hover colour the tool buttons use.
    let track = if on { palette.tab_unselected_hover } else { track_off };
    let radius = rect.height() / 2.0;
    painter.rect_filled(rect.shrink2(vec2(0.0, 3.0)), CornerRadius::same((radius - 3.0) as u8), track);
    let x = if on { rect.right() - radius } else { rect.left() + radius };
    painter.circle_filled(pos2(x, rect.center().y), radius, if hovered { knob_hover } else { knob });
}

struct TableLine {
    name: String,
    key: StatKey,
    /// The parent label this line opens and closes, when it has children.
    toggle: Option<String>,
    stripe: Stripe,
}

fn heading_line(ui: &mut Ui, style: &PopoutStyle, palette: &Palette, width: f32, text: &str) {
    let (rect, _) = ui.allocate_exact_size(vec2(width, LINE_HEIGHT), Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::ZERO, palette.bg);
    ui.painter().text(pos2(rect.left() + CELL_PAD, rect.center().y), egui::Align2::LEFT_CENTER, text, style.bold.clone(), palette.text);
}

/// Draw one table and return the parent label clicked this frame, if any.
///
/// The name column is at least 150px and grows to its longest name; the four
/// value columns share what is left equally, as Tk's uniform grid shared it.
fn draw_table(
    ui: &mut Ui,
    style: &PopoutStyle,
    palette: &Palette,
    width: f32,
    heading: Option<&str>,
    lines: &[TableLine],
    cells: &HashMap<(StatKey, usize), String>,
) -> Option<String> {
    if let Some(heading) = heading {
        heading_line(ui, style, palette, width, heading);
    }
    let measure = |text: &str, bold: bool| {
        let font = if bold { style.bold.clone() } else { style.font.clone() };
        ui.painter().layout_no_wrap(text.to_string(), font, palette.text).size().x
    };
    let name_width = lines
        .iter()
        .filter(|l| l.stripe != Stripe::Hidden)
        .map(|l| measure(&l.name, false))
        .fold(measure(COLUMNS[0], true), f32::max);
    let col0 = (name_width + 2.0 * CELL_PAD).max(NAME_MIN_WIDTH);
    // One width for all four, as Tk's uniform group gave them: the share of
    // what is left, or the widest cell when one needs more ("38.7 / 300.0 W").
    let widest = lines
        .iter()
        .filter(|l| l.stripe != Stripe::Hidden)
        .flat_map(|l| (1..COLUMNS.len()).map(move |c| (l, c)))
        .map(|(l, c)| measure(cells.get(&(l.key.clone(), c)).map(String::as_str).unwrap_or("—"), false))
        .chain(COLUMNS[1..].iter().map(|name| measure(name, true)))
        .fold(0.0f32, f32::max);
    let column = ((width - col0) / 4.0).max(widest + 2.0 * CELL_PAD);
    let start = |c: usize| if c == 0 { 0.0 } else { col0 + column * (c - 1) as f32 };

    let (rect, _) = ui.allocate_exact_size(vec2(width, LINE_HEIGHT), Sense::hover());
    for (c, name) in COLUMNS.iter().enumerate() {
        ui.painter().text(pos2(rect.left() + start(c) + CELL_PAD, rect.center().y), egui::Align2::LEFT_CENTER, *name, style.bold.clone(), palette.text);
    }

    let mut toggled = None;
    for line in lines {
        let Stripe::Shown(fill) = line.stripe else { continue };
        let (rect, _) = ui.allocate_exact_size(vec2(width, LINE_HEIGHT), Sense::hover());
        if !ui.is_rect_visible(rect) {
            continue;
        }
        if let Some(fill) = fill {
            ui.painter().rect_filled(rect, CornerRadius::ZERO, fill);
        }
        let y = rect.center().y;
        let name_pos = pos2(rect.left() + CELL_PAD, y);
        let name_rect = ui.painter().text(name_pos, egui::Align2::LEFT_CENTER, &line.name, style.font.clone(), palette.text);
        if let Some(parent) = &line.toggle {
            let hit = Rect::from_min_max(pos2(rect.left(), rect.top()), pos2(rect.left() + col0, rect.bottom())).union(name_rect);
            let response = ui.interact(hit, ui.scope_id().with(("toggle", parent)), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
            if response.clicked() {
                toggled = Some(parent.clone());
            }
        }
        for c in 1..COLUMNS.len() {
            let text = cells.get(&(line.key.clone(), c)).map(String::as_str).unwrap_or("—");
            ui.painter().text(pos2(rect.left() + start(c) + CELL_PAD, y), egui::Align2::LEFT_CENTER, text, style.font.clone(), palette.value);
        }
    }
    toggled
}

/// The builder for the Telemetry viewport, placed at `position`.
pub fn viewport_builder(position: Option<(f32, f32)>) -> egui::ViewportBuilder {
    let builder = egui::ViewportBuilder::default()
        .with_title("Telemetry")
        .with_inner_size([WINDOW_WIDTH, WINDOW_HEIGHT])
        .with_resizable(true);
    popouts::finish_builder(builder, position)
}

impl Drop for DimmTelemetryWindow {
    fn drop(&mut self) {
        // The poll stops the moment the window closes: dropping the sender
        // ends the worker once any read in flight returns.
        self.requests = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rochviewer::memory::ddr5_telemetry::PmicTelemetry;

    fn dimm(channel: &'static str) -> DimmTelemetry {
        DimmTelemetry {
            channel,
            hub_address: 0x50,
            pmic_address: 0x48,
            controller: 0,
            hub_temp_c: None,
            vendor: None,
            revision: None,
            telemetry: PmicTelemetry::default(),
        }
    }

    fn window_with(groups: Vec<SensorGroup>, reveal: HashSet<StatKey>) -> DimmTelemetryWindow {
        DimmTelemetryWindow::new(Palette::new(false), Arc::new(Vec::new), Arc::new(Vec::new), false, groups, reveal)
    }

    fn reading(label: &str, text: &'static str) -> SensorReading {
        SensorReading { label: label.into(), read: Arc::new(move || text.to_string()), parent: None }
    }

    // -- parse_reading ------------------------------------------------------

    #[test]
    fn a_plain_value_gives_its_number_unit_and_precision() {
        assert_eq!(parse_reading("1.244 V"), (Some(1.244), "V".into(), 3));
        assert_eq!(parse_reading("42.1 °C"), (Some(42.1), "°C".into(), 1));
        assert_eq!(parse_reading("2.00 x"), (Some(2.0), "x".into(), 2));
    }

    #[test]
    fn a_value_paired_with_its_limit_tracks_the_value() {
        // "38.7 / 300.0 W" is the reading and the configured limit. Only the
        // first moves, so only the first is worth a minimum and a maximum.
        assert_eq!(parse_reading("38.7 / 300.0 W"), (Some(38.7), "W".into(), 1));
    }

    #[test]
    fn a_unit_is_never_read_as_part_of_a_second_number() {
        assert_eq!(parse_reading("117 ns / 95 ns"), (Some(117.0), "ns".into(), 0));
    }

    #[test]
    fn a_reading_with_no_number_keeps_its_place() {
        for text in ["Off", "Hi-Z", "—", ""] {
            assert_eq!(parse_reading(text), (None, String::new(), 0), "{text}");
        }
    }

    #[test]
    fn a_negative_reading_is_a_number() {
        assert_eq!(parse_reading("-0.5 V"), (Some(-0.5), "V".into(), 1));
    }

    // -- Statistic ----------------------------------------------------------

    #[test]
    fn it_tracks_the_extremes_and_the_mean() {
        let mut statistic = Statistic::new();
        for value in [1.5, 1.515, 1.5] {
            statistic.add(Some(value));
        }
        assert_eq!(statistic.current, Some(1.5));
        assert_eq!(statistic.minimum, Some(1.5));
        assert_eq!(statistic.maximum, Some(1.515));
        assert!((statistic.average().unwrap() - 1.505).abs() < 1e-9);
    }

    #[test]
    fn a_missing_reading_does_not_count_as_zero() {
        let mut statistic = Statistic::new();
        statistic.add(Some(1.5));
        statistic.add(None);
        assert_eq!(statistic.minimum, Some(1.5));
        assert_eq!(statistic.count, 1);
    }

    #[test]
    fn nothing_read_has_no_statistics() {
        let statistic = Statistic::new();
        assert_eq!(statistic.average(), None);
        assert_eq!(statistic.minimum, None);
    }

    #[test]
    fn reset_clears_everything() {
        let mut statistic = Statistic::new();
        statistic.add(Some(2.0));
        statistic.reset();
        assert_eq!(statistic.current, None);
        assert_eq!(statistic.maximum, None);
        assert_eq!(statistic.count, 0);
    }

    // -- formatting ---------------------------------------------------------

    #[test]
    fn values_carry_their_unit_and_precision() {
        assert_eq!(format_value(Some(1.4405), "V", 3), "1.440 V");
        assert_eq!(format_value(None, "V", 3), "—");
    }

    #[test]
    fn the_timer_reads_as_a_clock() {
        assert_eq!(format_elapsed(0.0), "00:00:00");
        assert_eq!(format_elapsed(3725.0), "01:02:05");
        assert_eq!(format_elapsed(-5.0), "00:00:00");
    }

    // -- panels -------------------------------------------------------------

    fn panel_entry() -> DimmTelemetry {
        DimmTelemetry { pmic_address: 0x49, vendor: Some("Richtek".into()), revision: Some("2.1".into()), ..dimm("a") }
    }

    fn panel_module() -> TelemetryModule {
        TelemetryModule {
            slot: Some("A2".into()),
            capacity_gb: 16,
            rank_numeric: Some("1R".into()),
            ic: Some("SK hynix A-die".into()),
            part_number: Some("F5-6000J2636G16G".into()),
            spd_vendor: Some("G.Skill".into()),
            ..TelemetryModule::default()
        }
    }

    fn lines(pairs: &[(&str, &str)]) -> IdentityLines {
        pairs.iter().map(|(l, v)| (l.to_string(), v.to_string())).collect()
    }

    #[test]
    fn the_heading_names_the_module_and_its_pmic() {
        assert_eq!(panel_title(0, &panel_entry(), Some(&panel_module())), "DIMM 0  |  A2  |  PMIC 0x49");
    }

    #[test]
    fn an_unnamed_slot_falls_back_to_the_channel() {
        let module = TelemetryModule { slot: None, ..TelemetryModule::default() };
        assert!(panel_title(0, &panel_entry(), Some(&module)).contains('A'));
    }

    #[test]
    fn the_identity_lines_carry_what_was_read() {
        let (left, right) = panel_identity(&panel_entry(), Some(&panel_module()));
        assert_eq!(left[..3], lines(&[("DRAM Manuf.", "SK hynix"), ("DRAM Die", "A-die"), ("Ranks", "1R")])[..]);
        assert_eq!(
            right,
            lines(&[
                ("Part Number", "F5-6000J2636G16G"),
                ("Module Manuf.", "G.Skill"),
                ("Module Size", "16 GB"),
                ("PMIC", "Richtek rev 2.1"),
            ])
        );
    }

    #[test]
    fn the_sides_are_padded_to_the_same_length() {
        // They are drawn in pairs; a short side must not cut the long one off.
        let (left, right) = panel_identity(&panel_entry(), Some(&panel_module()));
        assert_eq!(left.len(), right.len());
        assert_eq!(left.last().unwrap(), &(String::new(), String::new()));
    }

    #[test]
    fn the_module_vendor_is_not_labelled_as_the_dram_maker() {
        // G.Skill sells the stick; SK hynix made the chips on it. Two labels,
        // because they are two different companies.
        let (left, right) = panel_identity(&panel_entry(), Some(&panel_module()));
        let right: HashMap<_, _> = right.into_iter().collect();
        let left: HashMap<_, _> = left.into_iter().collect();
        assert_eq!(right["Module Manuf."], "G.Skill");
        assert_eq!(left["DRAM Manuf."], "SK hynix");
    }

    #[test]
    fn a_module_that_read_nothing_still_renders() {
        let (left, right) = panel_identity(&dimm("b"), None);
        assert_eq!(left[0], ("DRAM Manuf.".to_string(), "—".to_string()));
        let right: HashMap<_, _> = right.into_iter().collect();
        assert_eq!(right["Module Size"], "—");
        assert_eq!(right["PMIC"], "—");
        assert_eq!(right["Part Number"], "—");
    }

    // -- counter rows ---------------------------------------------------------

    fn apply(label: &str, texts: &[&str]) -> HashMap<usize, String> {
        let key = StatKey::sensor("Errors", label);
        let mut window = window_with(vec![("Errors".into(), vec![reading(label, "0")])], HashSet::new());
        for text in texts {
            window.apply_sensors(vec![(key.clone(), text.to_string())]);
        }
        (1..5).map(|c| (c, window.cell(&key, c).unwrap().to_string())).collect()
    }

    #[test]
    fn whea_shows_its_count_alone() {
        assert!(COUNTER_ROWS.contains(&"WHEA Errors"));
        let shown = apply("WHEA Errors", &["0", "1"]);
        let expected: HashMap<usize, String> =
            [(1, "1"), (2, ""), (3, ""), (4, "")].into_iter().map(|(c, t)| (c, t.to_string())).collect();
        assert_eq!(shown, expected);
    }

    #[test]
    fn a_reading_still_keeps_its_statistics() {
        let shown = apply("VRM Temp", &["36.5 °C", "37.0 °C"]);
        assert!((2..5).all(|c| !shown[&c].is_empty()));
        assert_eq!(shown[&2], "36.5 °C");
        assert_eq!(shown[&3], "37.0 °C");
        assert_eq!(shown[&4], "36.8 °C");
    }

    // -- parameters -----------------------------------------------------------

    #[test]
    fn only_the_total_power_is_shown() {
        // The three per-rail figures the total is made of are still decoded
        // and still available to a caller; they are not worth a row each,
        // because VDD carries nearly all of a DDR5 module's draw and the
        // split reads as the total, a small number and a rounding error.
        let keys: Vec<&str> = PARAMETERS.iter().map(|p| p.0).collect();
        assert!(keys.contains(&"power_w"));
        for key in ["power_swa_w", "power_swb_w", "power_swc_w"] {
            assert!(!keys.contains(&key));
        }
    }

    #[test]
    fn the_per_rail_powers_are_still_decoded() {
        // Dropping the rows must not drop the readings: the total is taken
        // from the same decode, and a caller asking for a rail still gets it.
        let mut entry = dimm("a");
        entry.telemetry.power_swb_w = Some(0.135);
        assert_eq!(reading_value(&entry, "power_swb_w"), Some(0.135));
    }

    #[test]
    fn the_power_row_is_watts_not_millivolts() {
        for key in ["power_swa_w", "power_swb_w", "power_swc_w", "power_w"] {
            assert!(!MILLIVOLT_KEYS.contains(&key));
        }
    }

    #[test]
    fn millivolt_readings_are_shown_in_volts() {
        let mut entry = dimm("a");
        entry.telemetry.vdd = Some(1515);
        assert_eq!(reading_value(&entry, "vdd"), Some(1.515));
    }

    #[test]
    fn other_readings_pass_through() {
        let mut entry = dimm("a");
        entry.hub_temp_c = Some(27.75);
        entry.telemetry.power_w = Some(0.242);
        assert_eq!(reading_value(&entry, "hub_temp_c"), Some(27.75));
        assert_eq!(reading_value(&entry, "power_w"), Some(0.242));
    }

    #[test]
    fn a_missing_reading_is_absent_not_zero() {
        assert_eq!(reading_value(&dimm("a"), "vdd"), None);
    }

    // -- the Sensors tab is a window --------------------------------------------

    #[test]
    fn the_sensors_tab_is_a_window_now() {
        use rochviewer::display_values::{WINDOWED_TABS, select_tab_names};
        use rochviewer::row::Row;
        let rows = vec![
            Row::new("a", "c", "Timings", "Left"),
            Row::new("b", "c", "System Info", "Left"),
            Row::new("c", "c", "Sensors", "Left"),
        ];
        assert!(WINDOWED_TABS.contains(&"Sensors"));
        assert!(!select_tab_names(&rows).contains(&"Sensors".to_string()));
        assert_eq!(select_tab_names(&rows), vec!["Summary", "System Info", "Timings"]);
    }

    #[test]
    fn the_other_tabs_are_untouched() {
        use rochviewer::display_values::select_tab_names;
        use rochviewer::row::Row;
        let rows: Vec<Row> = ["Timings", "Training", "Jedec", "RTL", "System Info"]
            .iter()
            .map(|tab| Row::new("x", "c", *tab, "Left"))
            .collect();
        assert_eq!(select_tab_names(&rows), vec!["Summary", "System Info", "Timings", "Training", "RTL", "Jedec"]);
    }

    // -- banding ----------------------------------------------------------------

    const BAND: Color32 = Color32::from_rgb(1, 2, 3);

    #[test]
    fn rows_alternate_starting_unshaded() {
        assert_eq!(band(Some(BAND), 0), None);
        assert_eq!(band(Some(BAND), 1), Some(BAND));
        assert_eq!(band(Some(BAND), 2), None);
    }

    #[test]
    fn a_theme_with_no_band_colour_stays_plain() {
        // The window takes its palette from whoever opens it, so a caller
        // built before banding existed has to keep working.
        assert_eq!(band(None, 1), None);
    }

    fn records() -> Vec<Record> {
        let record = |parent: Option<&str>| Record { parent: parent.map(str::to_string), pending: false };
        vec![
            record(None),                         // Bus Clock
            record(None),                         // Core Clock (avg)
            record(None),                         // Core Effective Clock, the parent
            record(Some("Core Effective Clock")), // CPU 0
            record(Some("Core Effective Clock")), // CPU 1
            record(None),                         // Ring/LLC Clock
        ]
    }

    fn stripes(expanded: &HashMap<String, bool>) -> Vec<Option<Color32>> {
        restripe(&records(), expanded, Some(BAND))
            .into_iter()
            .filter_map(|s| match s {
                Stripe::Shown(fill) => Some(fill),
                Stripe::Hidden => None,
            })
            .collect()
    }

    #[test]
    fn a_hidden_group_does_not_break_the_alternation() {
        // Banding by position in the table counts rows nobody can see, so
        // closing a group of two left the row after it repeating the shade
        // of the row before.
        assert_eq!(stripes(&HashMap::new()), vec![None, Some(BAND), None, Some(BAND)]);
    }

    #[test]
    fn an_open_group_bands_its_children_in_sequence() {
        let expanded = HashMap::from([("Core Effective Clock".to_string(), true)]);
        assert_eq!(stripes(&expanded), vec![None, Some(BAND), None, Some(BAND), None, Some(BAND)]);
    }

    #[test]
    fn a_hidden_row_takes_no_height() {
        // The Python's filler label held a hidden row's height open unless it
        // was hidden too. Here a hidden row is not drawn at all.
        let stripes = restripe(&records(), &HashMap::new(), Some(BAND));
        assert_eq!(stripes[3], Stripe::Hidden);
        assert_eq!(stripes[4], Stripe::Hidden);
        let pending = [Record { parent: None, pending: true }];
        assert_eq!(restripe(&pending, &HashMap::new(), Some(BAND)), vec![Stripe::Hidden]);
    }

    #[test]
    fn the_marks_say_which_way_the_row_opens() {
        assert_ne!(COLLAPSED_MARK, EXPANDED_MARK);
        assert!(CHILD_INDENT.trim().is_empty());
    }

    // -- poll scheduling ----------------------------------------------------------

    #[test]
    fn a_free_window_starts_a_reader() {
        let mut state = PollState::default();
        assert_eq!(state.poll(), PollStep::StartReader);
        assert!(state.reading);
    }

    #[test]
    fn a_busy_window_waits_a_tick_instead_of_stacking_up() {
        let mut state = PollState { reading: true };
        assert_eq!(state.poll(), PollStep::WaitATick);
        assert!(state.reading);
        state.finished();
        assert_eq!(state.poll(), PollStep::StartReader);
    }

    #[test]
    fn the_reads_do_not_touch_a_widget() {
        // The worker returns values and the UI thread writes them; reading
        // only calls the readers, and a reader that fails reads as a dash.
        let groups = vec![
            ("Clocks".to_string(), vec![reading("Bus Clock", "100 MHz")]),
            (
                "Voltages".to_string(),
                vec![SensorReading { label: "VDDCR_VDD".into(), read: Arc::new(|| panic!("OSError")), parent: None }],
            ),
        ];
        assert_eq!(
            read_sensors(&groups),
            vec![
                (StatKey::sensor("Clocks", "Bus Clock"), "100 MHz".to_string()),
                (StatKey::sensor("Voltages", "VDDCR_VDD"), "—".to_string()),
            ]
        );
    }

    #[test]
    fn a_tick_lands_and_books_the_next() {
        let mut window = window_with(vec![("Clocks".into(), vec![reading("Bus Clock", "100.00 MHz")])], HashSet::new());
        window.poll_state.reading = true;
        let mut entry = dimm("a");
        entry.telemetry.vdd = Some(1440);
        let module = TelemetryModule { channel: Some("A".into()), slot: Some("A1".into()), ..TelemetryModule::default() };
        window.apply_tick(TickResult {
            readings: vec![(StatKey::sensor("Clocks", "Bus Clock"), "100.00 MHz".into())],
            entries: vec![entry],
            modules: HashMap::from([("a".to_string(), module)]),
            failure: None,
        });
        assert!(!window.poll_state.reading);
        assert!(window.panels_built);
        assert_eq!(window.panels[0].title, "DIMM 0  |  A1  |  PMIC 0x48");
        assert_eq!(window.cell(&StatKey::Dimm(0, "vdd"), 1), Some("1.440 V"));
        assert_eq!(window.cell(&StatKey::Dimm(0, "vpp"), 4), Some("—"));
        assert_eq!(window.running_text(), "Running: 00:00:01");
        window.reset_statistics();
        assert_eq!(window.running_text(), "Running: 00:00:00");
        assert_eq!(window.cell(&StatKey::Dimm(0, "vdd"), 1), Some("—"));
        assert_eq!(window.statistic(&StatKey::Dimm(0, "vdd")).unwrap().count, 0);
    }

    // -- trailing groups ----------------------------------------------------------

    #[test]
    fn the_groups_that_sink_are_graphics_then_errors() {
        // Order matters: Errors lands last, which is where a counter that
        // should stay zero belongs.
        assert_eq!(TRAILING_GROUPS, ["Graphics", "Errors"]);
    }

    #[test]
    fn a_trailing_group_is_moved_after_the_panels() {
        let titles = ["Clocks", "Errors", "Graphics", "Voltages"];
        assert_eq!(
            section_order(&titles, true),
            vec![Section::Group(0), Section::Group(3), Section::Panels, Section::Group(2), Section::Group(1)]
        );
        // Before any module has answered, the groups keep their own order.
        assert_eq!(section_order(&titles, false), (0..4).map(Section::Group).collect::<Vec<_>>());
    }

    #[test]
    fn a_group_that_was_never_built_is_skipped() {
        assert_eq!(section_order(&["Clocks"], true), vec![Section::Group(0), Section::Panels]);
    }

    // -- fast-sampled rows --------------------------------------------------------

    #[test]
    fn core_max_is_sampled_between_ticks() {
        assert!(FAST_SAMPLE_LABELS.contains(&"Core Max"));
    }

    #[test]
    fn it_samples_several_times_a_tick() {
        // No point being "fast" at a rate the tick already covers.
        const { assert!(FAST_SAMPLE_MS < 1000) };
        const { assert!(FAST_SAMPLE_MS >= 50) };
    }

    #[test]
    fn only_cheap_rows_are_listed() {
        // These are read on the UI thread, unlike the full tick. A row that
        // blocks -- a DIMM's ADC needs 12ms a channel -- would stall the
        // window five times a second.
        assert_eq!(FAST_SAMPLE_LABELS, &["Core Max"]);
    }

    #[test]
    fn a_fast_row_is_not_also_counted_on_the_tick() {
        // Both would weight the tick's one sample like the five between it,
        // pulling the average toward whatever that sample happened to catch.
        let key = StatKey::sensor("Thermal & Power", "Core Max");
        let mut window = window_with(vec![("Thermal & Power".into(), vec![reading("Core Max", "40.0 °C")])], HashSet::new());
        window.fast_sample();
        window.fast_sample();
        assert_eq!(window.statistic(&key).unwrap().count, 2);
        window.apply_sensors(vec![(key.clone(), "80.0 °C".into())]);
        assert_eq!(window.statistic(&key).unwrap().count, 2);
        // The tick still writes the row's own text.
        assert_eq!(window.cell(&key, 1), Some("80.0 °C"));
        assert_eq!(window.cell(&key, 3), Some("40.0 °C"));
    }

    #[test]
    fn double_counting_would_move_the_average() {
        // Why the guard above exists, in numbers: the same readings counted
        // twice at one rate and once at another do not average the same.
        let mut fed_once = Statistic::new();
        let mut fed_twice = Statistic::new();
        for value in [40.0, 40.0, 40.0, 40.0, 80.0] {
            fed_once.add(Some(value));
            fed_twice.add(Some(value));
        }
        fed_twice.add(Some(80.0));
        assert_eq!(fed_once.maximum, fed_twice.maximum);
        assert_ne!(format!("{:.3}", fed_once.average().unwrap()), format!("{:.3}", fed_twice.average().unwrap()));
    }

    #[test]
    fn closing_stops_the_poll() {
        // The Python cancelled both of its timers on close; here the worker
        // ends when the window is dropped.
        let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = reads.clone();
        let read: TelemetryRead = Arc::new(move || {
            counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Vec::new()
        });
        let mut window = DimmTelemetryWindow::new(Palette::new(false), read, Arc::new(Vec::new), false, Vec::new(), HashSet::new());
        let ctx = egui::Context::default();
        window.start_worker(&ctx);
        window.requests.as_ref().unwrap().send(TickRequest { need_modules: true }).unwrap();
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 1);
        let results = window.results.clone();
        assert_eq!(Arc::strong_count(&results), 3);
        drop(window);
        // With the window gone its worker is too: the last other holder of
        // the results slot has let go of it.
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(Arc::strong_count(&results), 1);
    }

    // -- reveal on read (test_x870_board_monitor.py RevealOnReadTest) -------------

    fn reveal_window() -> (DimmTelemetryWindow, StatKey) {
        let key = StatKey::sensor("Fans", "System Fan 1");
        let window = window_with(vec![("Fans".into(), vec![reading("System Fan 1", "—")])], HashSet::from([key.clone()]));
        (window, key)
    }

    #[test]
    fn a_blank_reading_keeps_it_hidden() {
        let (mut window, key) = reveal_window();
        for text in ["", "\u{2014}", "N/A"] {
            window.apply_sensors(vec![(key.clone(), text.to_string())]);
        }
        assert!(window.is_pending(&key));
    }

    #[test]
    fn the_first_reading_reveals_it() {
        let (mut window, key) = reveal_window();
        window.apply_sensors(vec![(key.clone(), "\u{2014}".to_string())]);
        assert!(window.is_pending(&key));
        window.apply_sensors(vec![(key.clone(), "1403 RPM".to_string())]);
        assert!(!window.is_pending(&key));
    }

    // -- parse_reading on board readings (test_nct6687d_board_map.py) -------------

    #[test]
    fn rpm_is_read_whole_and_written_without_a_separator() {
        // The telemetry statistics take the first plain number in the text;
        // "1,010 RPM" would have read as 1.
        assert_eq!(parse_reading("1010 RPM"), (Some(1010.0), "RPM".into(), 0));
    }

    #[test]
    fn the_bench_bytes_decode_to_whole_percentages() {
        assert_eq!(parse_reading("40 %"), (Some(40.0), "%".into(), 0));
    }
}
