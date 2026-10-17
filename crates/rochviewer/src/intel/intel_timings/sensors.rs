//! The Sensors tab and the Voltages snapshot.
//!
//! Voltages are live readings, unlike everything on the Timings tab, which
//! cannot change without a reboot. They were previously stranded: VCCSA and
//! VDDQ TX sat at the bottom of System Info among configuration rows, and the
//! SA/TX Voltage copies on the CPU tab were never displayed at all, because
//! "CPU" is not one of the tabs select_tab_names renders. Both move here, onto
//! the same Sensors tab the AM5 profile uses.
//!
//! The rows carry a callable rather than a value, so the one-second refresh in
//! the window re-reads them instead of showing whatever was true at startup.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};

use crate::memory::ddr_generation::{DdrGeneration, detect_ddr_generation};
use crate::platform_profiles::Platform;
use crate::row::TextFn;

use super::build::Build;
use super::dict::{T, V, lazy, text};
use super::identity::{get_board_manufacturer, get_motherboard_display};
use super::registers::{
    get_bclk_rd, get_core_max_temp, get_dram_frequency, get_mclk, get_ring_freq, get_sa, get_tx, get_uclk,
    is_arrow_lake_platform,
};
use super::DDR5_TIMING_PLATFORMS;

pub const SENSOR_TAB: &str = "Sensors";

/// Read one Super I/O rail, importing that path only when it is used.
fn board_rail(key: &str) -> Option<String> {
    crate::sensors::board_sensors::rail_text(key)
}

/// Read one channel's DIMM temperature, importing SMBus only when used.
///
/// Where the sensor lives depends on the memory generation, so both are
/// tried. A DDR4 module carries a separate JC-42.4 sensor at 0x18-0x1F; on
/// DDR5 that part is gone and the sensor moved inside the SPD5 hub at
/// 0x50-0x57. Sweeping 0x18-0x1F on a DDR5 board finds nothing at all, which
/// is why these two rows read blank on one.
///
/// DDR5 is tried first because its answer is the identified one: the hub is
/// asked to confirm it is a hub before its reading is used, while a JC-42.4
/// sensor has no such handshake.
fn dimm_temperature(channel: &str) -> Option<String> {
    if let Some(celsius) = ddr5_dimm_temperature(channel) {
        return Some(format!("{celsius:.1} °C"));
    }
    crate::memory::ddr4_tsod::temperature_text(channel, None)
}

/// Read one channel's SPD5 hub sensor, or `None` when there is no hub.
fn ddr5_dimm_temperature(channel: &str) -> Option<f64> {
    use crate::intel::intel_pch_smbus::{CONTROLLER_OFFSETS, SPD_HUB_ADDRESSES, new_reader};
    let factory = || new_reader();
    crate::memory::ddr5_pmic::read_dimm_temperatures(Some(&factory), Some(&CONTROLLER_OFFSETS), Some(&SPD_HUB_ADDRESSES))
        .get(channel)
        .copied()
}

/// Read one board sensor, importing those paths only when they are used.
///
/// The Super I/O first, then the ACPI EC for what it does not carry. On this
/// bench the VRM channel is absent from the Super I/O entirely and lives only
/// on the EC, which is why that row was blank rather than the board lacking
/// the sensor.
fn board_temperature(key: &str) -> Option<String> {
    if let Some(text) = crate::sensors::board_sensors::temperature_text(key).filter(|t| !t.is_empty()) {
        return Some(text);
    }
    ec_temperature(key)
}

/// Read one sensor from the ACPI EC, on boards whose map is known.
///
/// Gated on the board vendor: the EC protocol is standard but the register
/// map is not, and an unknown vendor's byte at the same address would decode
/// to a plausible temperature rather than an error.
fn ec_temperature(key: &str) -> Option<String> {
    if !crate::sensors::asus_ec::is_asus_board(&get_motherboard_display()) {
        return None;
    }
    crate::sensors::asus_ec::temperature_text(key)
}

/// Read one RAPL domain, importing that path only when it is used.
fn package_power(domain: &str) -> Option<String> {
    crate::intel::intel_rapl::power_text(domain)
}

/// Read one DRAM rail from the DIMM's own PMIC over the PCH SMBus.
///
/// The decode is JESD301 and shared with the AM5 path; only the bus differs,
/// so this passes the Intel transport and its address lists into the same
/// reader rather than duplicating the VID arithmetic.
///
/// Nothing to do on DDR4, where the module has no PMIC and its rails come
/// from the board. Asked anyway, the reader sweeps 0x48-0x4F looking for one:
/// 0x4F answers on the Z790-P bench, it is not a PMIC, and its registers
/// decoded to a VPP of 1.500 V -- a value DDR4's 2.5 V rail cannot hold, read
/// off a device that was never asked what it was. The rows are absent on that
/// generation, so this is the second line rather than the first; a sweep that
/// can find a wrong answer should not be run at all.
fn dram_rail(key: &str) -> Option<String> {
    if detect_ddr_generation() == DdrGeneration::Ddr4 {
        return None;
    }
    use crate::intel::intel_pch_smbus::{CONTROLLER_OFFSETS, PMIC_ADDRESSES, new_reader};
    let factory = || new_reader();
    let volts =
        crate::memory::ddr5_pmic::read_dram_rails(Some(&factory), Some(&CONTROLLER_OFFSETS), Some(&PMIC_ADDRESSES))
            .get(key)
            .copied()?;
    Some(format!("{volts:.3} V"))
}

// VCCSA and VDDQ TX come from the memory controller; DRAM and CPU AUX are
// board measurements that only the Super I/O reports. The temperatures come
// from the modules themselves over SMBus, and are the only readings in the
// Intel path that do not originate in the CPU or the board.
//
// A DDR4 module's thermal sensor is optional. A kit built without one does not
// answer, and that row reports nothing rather than a substitute.
/// One core clock from the performance counters, or `None`.
fn core_clock(key: &str) -> Option<String> {
    crate::sensors::cpu_clocks::clock_text(key)
}

// The row the per-processor breakdown hangs under, and the key that marks a
// row as one of those children. The telemetry window folds them away behind
// the parent: sixteen logical processors is a useful thing to be able to open
// and a poor thing to have to scroll past to reach the temperatures.
pub const CORE_EFFECTIVE_CLOCK_ROW: &str = "Core Effective Clock";
pub const PARENT_ROW_KEY: &str = "Parent";

/// One row per logical processor, folded under the aggregate row.
///
/// Built at import from what the counters actually enumerate rather than
/// from a core count: a machine whose counters do not answer gets no
/// children, and the parent row stands alone exactly as it did before.
fn per_core_clock_rows(category: &str, column: &str) -> Vec<T> {
    crate::sensors::cpu_clocks::core_labels()
        .into_iter()
        .enumerate()
        .map(|(index, label)| {
            T::new()
                .s("name", &label)
                .with("value", lazy(move || crate::sensors::cpu_clocks::core_clock_text(index)))
                .s("Category", category)
                .s("Tab", SENSOR_TAB)
                .s("Column", column)
                .s("read_type", "standard")
                .with("live", V::Bool(true))
                .s(PARENT_ROW_KEY, CORE_EFFECTIVE_CLOCK_ROW)
        })
        .collect()
}

// --- The card, live.
//
// The AM5 profile reads these through AMD's own libraries; here they come from
// NVML, which the graphics identity rows already use. Labels match the AM5
// side wherever the two report the same thing, so a row means one thing across
// both platforms rather than two names for one reading.
//
// Read once per tick rather than once per row: eight rows asking separately
// would open the same session eight times a second. The first row of the group
// refreshes the set and the rest read what it left, which is why the order
// below is load-bearing.
static GRAPHICS_SENSOR_CACHE: Mutex<Option<BTreeMap<String, String>>> = Mutex::new(None);

fn graphics_sensors(refresh: bool) -> BTreeMap<String, String> {
    let mut cache = GRAPHICS_SENSOR_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let empty = cache.as_ref().is_none_or(|readings| readings.is_empty());
    if refresh || empty {
        let readings = crate::gpu::nvidia_gpu::read_gpu_sensors();
        *cache = Some(readings.into_iter().map(|(k, v)| (k.to_string(), v)).collect());
    }
    cache.clone().unwrap_or_default()
}

/// One card reading. The first row of the group refreshes the set.
fn graphics_sensor(label: &str, first: bool) -> Option<String> {
    graphics_sensors(first).get(label).cloned()
}

pub const GRAPHICS_SENSOR_LABELS: [&str; 10] = [
    "GPU Clock",
    "GPU Memory Clock",
    "GPU Video Clock",
    "GPU Core Voltage",
    "GPU Temperature",
    "GPU Fan PWM",
    "GPU Utilization",
    "GPU Board Power (TBP)",
    // Directly under the power it is heading for: a power figure means little
    // without the number it is measured against.
    "GPU Board Power Limit",
    "GPU Memory Used",
];

/// One declared sensor row: (name, category, getter, column).
#[derive(Clone)]
pub struct SensorRow {
    pub name: &'static str,
    pub category: &'static str,
    pub getter: TextFn,
    pub column: &'static str,
}

fn sensor(name: &'static str, category: &'static str, column: &'static str, getter: impl Fn() -> Option<String> + Send + Sync + 'static) -> SensorRow {
    SensorRow { name, category, getter: Arc::new(getter), column }
}

fn graphics_sensor_rows() -> Vec<SensorRow> {
    GRAPHICS_SENSOR_LABELS
        .iter()
        .enumerate()
        .map(|(index, label)| {
            let label: &'static str = label;
            sensor(label, "Graphics", "Right", move || graphics_sensor(label, index == 0))
        })
        .collect()
}

/// How many hardware errors Windows has logged since this boot.
fn whea_errors() -> Option<String> {
    Some(crate::sensors::whea_errors::error_text())
}

// The one counter that belongs beside the readings rather than in them. A kit
// that boots and benchmarks can still be quietly correcting errors, and this
// is the only place in the tool that would say so.
//
// It reads the Windows event log, which has nothing to do with whose memory
// controller this is. It was on the AM5 profile alone only because that is
// where it happened to be written.
//
// It sits in the window that keeps a maximum, which is the shape that matters
// here: a count that ticked up once during a run is the whole point, and a tab
// showing the current value would lose it.
fn error_sensor_rows() -> Vec<SensorRow> {
    vec![sensor("WHEA Errors", "Errors", "Right", whea_errors)]
}

/// The declared live rows (the Python's `SENSOR_ROWS`).
pub fn sensor_rows(platform: Platform) -> Vec<SensorRow> {
    let mut rows = vec![
        // The clocks, which a tab can only ever show as an instant. What BCLK does
        // across a stability run, and how far the ring falls back under load, are
        // exactly the readings worth having a minimum and a maximum for.
        //
        // Bus and ring come out of MCHBAR and agree with HWiNFO exactly -- 100.0
        // MHz and 5000.0 MHz on this bench. The memory clocks follow them because
        // this is a memory tool and they are the reason the window is open.
        //
        // The core clocks are a different measurement and are named for it. There
        // is no RDMSR here, so the P-state ratio HWiNFO reads is out of reach;
        // these come from the Windows performance counters, which average across
        // the sampling interval. See cpu_clocks for the two routes measured and
        // rejected before that one.
        sensor("Bus Clock", "Clocks", "Left", move || Some(get_bclk_rd(platform))),
        sensor("Core Clock (avg)", "Clocks", "Left", || core_clock("core_avg")),
        sensor("Core Effective Clock", "Clocks", "Left", || core_clock("core_effective")),
        sensor("Ring/LLC Clock", "Clocks", "Left", move || Some(get_ring_freq(platform))),
        sensor("MCLK", "Clocks", "Left", || Some(get_mclk())),
        sensor("UCLK", "Clocks", "Left", || Some(get_uclk())),
        sensor("DRAM Frequency", "Clocks", "Left", move || Some(get_dram_frequency(platform))),
        // Temperatures and power share one section, the way the AM5 profile's
        // THERMAL & POWER block does, and read in that order: what the parts are
        // sitting at, then what the package is drawing to get there. They were two
        // groups only because the Intel profile grew the power rows last.
        //
        // One row per DIMM channel: the two sit in different airflow, and the
        // hotter one is what matters when tuning. Each carries its own peak.
        sensor("DIMM A Temp", "Thermal & Power", "Right", || dimm_temperature("a")),
        sensor("DIMM B Temp", "Thermal & Power", "Right", || dimm_temperature("b")),
        // The VRM leads the board sensors: it is what delivers the DRAM rail, so
        // it is the one that matters while that rail is being pushed.
        sensor("VRM Temp", "Thermal & Power", "Right", || board_temperature("vrm")),
        sensor("CPU Temp", "Thermal & Power", "Right", || board_temperature("cpu")),
        // Directly under the board's CPU channel, because the two are the same
        // subject measured in different places and the difference is the point.
        sensor("Core Max", "Thermal & Power", "Right", get_core_max_temp),
        sensor("CPU Socket Temp", "Thermal & Power", "Right", || board_temperature("socket")),
        sensor("PCH Temp", "Thermal & Power", "Right", || board_temperature("pch")),
        sensor("System Temp", "Thermal & Power", "Right", || board_temperature("system")),
        sensor("CPU Package Power", "Thermal & Power", "Left", || package_power("package")),
        sensor("CPU Cores Power", "Thermal & Power", "Left", || package_power("cores")),
        // VCCSA and CPU SA are the same rail measured in two places, the memory
        // controller and the board VRM, so they sit adjacent for comparison.
        sensor("DLVR Vcore", "Voltages", "Left", || board_rail("vcore")),
        sensor("VCCSA", "Voltages", "Left", || Some(get_sa())),
        sensor("CPU SA (VRM)", "Voltages", "Left", || board_rail("cpu_sa")),
        sensor("VDDQ TX", "Voltages", "Left", || Some(get_tx())),
        // VDD2 is the memory controller's own supply and the rail a DDR5 tuner
        // reaches for first, so it leads the memory side of the list rather than
        // trailing the auxiliary CPU rails.
        sensor("VDD2", "Voltages", "Left", || board_rail("vdd2")),
        sensor("VTT", "Voltages", "Left", || board_rail("vtt")),
        sensor("VCCIO", "Voltages", "Left", || board_rail("vccio")),
        sensor("CPU VNNAON", "Voltages", "Left", || board_rail("vnnaon")),
        sensor("CPU AUX", "Voltages", "Left", || board_rail("cpu_aux")),
        // DRAM against DRAM VDD is the same rail measured in two places, like
        // VCCSA against CPU SA above: the board's own sense point, then the
        // module's PMIC. A board that reports only one of them leaves the other
        // blank rather than borrowing.
        sensor("DRAM", "Voltages", "Left", || board_rail("vdimm")),
        sensor("DRAM VDD", "Voltages", "Left", || dram_rail("dram_vdd")),
        sensor("DRAM VDDQ", "Voltages", "Left", || dram_rail("dram_vddq")),
        sensor("DRAM VPP", "Voltages", "Left", || dram_rail("dram_vpp")),
    ];
    rows.extend(graphics_sensor_rows());
    rows.extend(error_sensor_rows());
    rows
}

// Extra NCT6798D readings shown by HWiNFO on the ASUS Z790-A D4. The common
// rows above already provide Vcore, VTT, DRAM, CPU VCCSA and VCCIN_AUX; these
// complete that chip's voltage block and its two additional CPU temperatures.
// They are installed only when the detected board sensor is an NCT6798D, so
// another Super I/O does not gain a page of blank, board-specific inputs.
fn nct6798d_sensor_rows() -> Vec<SensorRow> {
    vec![
        sensor("CPU (Weighted Value)", "Thermal & Power", "Right", || board_temperature("cpu_weighted")),
        sensor("CPU Package", "Thermal & Power", "Right", || board_temperature("cpu_package")),
        sensor("+5V", "Voltages", "Left", || board_rail("plus5v")),
        sensor("AVSB", "Voltages", "Left", || board_rail("avsb")),
        sensor("3VCC", "Voltages", "Left", || board_rail("v3cc")),
        sensor("+12V", "Voltages", "Left", || board_rail("plus12v")),
        sensor("VIN8", "Voltages", "Left", || board_rail("vin8")),
        sensor("VIN4", "Voltages", "Left", || board_rail("vin4")),
        sensor("3VSB_ATX", "Voltages", "Left", || board_rail("v3sb_atx")),
        sensor("BAT_3V", "Voltages", "Left", || board_rail("bat_3v")),
        sensor("CPU L2", "Voltages", "Left", || board_rail("cpu_l2")),
        sensor("VIN2", "Voltages", "Left", || board_rail("vin2")),
        sensor("VIN9", "Voltages", "Left", || board_rail("vin9")),
        sensor("VHIF", "Voltages", "Left", || board_rail("vhif")),
    ]
}

/// Read one board fan, importing that path only when it is used.
fn board_fan(key: &str) -> Option<String> {
    crate::sensors::board_sensors::fan_text(key)
}

/// Read one board fan's duty, importing that path only when it is used.
fn board_fan_duty(key: &str, refresh: bool) -> Option<String> {
    crate::sensors::board_sensors::fan_duty_text(key, refresh)
}

// The NCT6687D's own inputs beyond the common rows, as HWiNFO lists them on
// the MSI Z790MPOWER. Each carries the board-sensor key it reads; a row is
// installed only when this board's map has that key, so the same chip on
// another board -- whose inputs are wired differently -- does not gain rows
// it has no confirmed reading for.
fn nct6687d_sensor_rows() -> Vec<(SensorRow, &'static str)> {
    vec![
        (sensor("T0", "Thermal & Power", "Right", || board_temperature("t0")), "t0"),
        (sensor("T1", "Thermal & Power", "Right", || board_temperature("t1")), "t1"),
        (sensor("+12V", "Voltages", "Left", || board_rail("plus12v")), "plus12v"),
        (sensor("+5V", "Voltages", "Left", || board_rail("plus5v")), "plus5v"),
        (sensor("VIN3", "Voltages", "Left", || board_rail("vin3")), "vin3"),
        (sensor("VIN7", "Voltages", "Left", || board_rail("vin7")), "vin7"),
        (sensor("+3.3V", "Voltages", "Left", || board_rail("plus3v3")), "plus3v3"),
        (sensor("CPU Fan", "Fans", "Left", || board_fan("cpu_fan")), "cpu_fan"),
        // The first duty row refreshes all three; see fan_duty_text.
        (sensor("CPU Fan Duty", "Fans", "Left", || board_fan_duty("cpu_fan", true)), "duty:cpu_fan"),
        (sensor("PUMP1", "Fans", "Left", || board_fan("pump1")), "pump1"),
        (sensor("PUMP1 Duty", "Fans", "Left", || board_fan_duty("pump1", false)), "duty:pump1"),
        (sensor("System 1", "Fans", "Left", || board_fan("system1")), "system1"),
        (sensor("System 1 Duty", "Fans", "Left", || board_fan_duty("system1", false)), "duty:system1"),
    ]
}

/// The NCT6687D rows this board's sensor maps can actually fill.
pub fn nct6687d_rows() -> Vec<SensorRow> {
    use crate::sensors::board_sensors::{nct668x_fan_duties, nct668x_fans, nct668x_rails, nct668x_temperatures};
    let mut mapped: Vec<String> = Vec::new();
    mapped.extend(nct668x_rails().iter().map(|(key, _)| key.to_string()));
    mapped.extend(nct668x_temperatures().iter().map(|(key, _)| key.to_string()));
    mapped.extend(nct668x_fans().iter().map(|(key, _)| key.to_string()));
    mapped.extend(nct668x_fan_duties().iter().map(|(key, _)| format!("duty:{key}")));
    nct6687d_sensor_rows()
        .into_iter()
        .filter(|(_, key)| mapped.iter().any(|m| m == key))
        .map(|(row, _)| row)
        .collect()
}

/// Stable-sort one category's rows by a declared name order; unnamed rows
/// keep their order after the named ones.
fn ordered(rows: &[SensorRow], category: &str, names: &[&str]) -> Vec<SensorRow> {
    let mut group: Vec<SensorRow> = rows.iter().filter(|row| row.category == category).cloned().collect();
    group.sort_by_key(|row| names.iter().position(|n| *n == row.name).unwrap_or(names.len()));
    group
}

fn of_category(rows: &[SensorRow], category: &str) -> Vec<SensorRow> {
    rows.iter().filter(|row| row.category == category).cloned().collect()
}

/// Return live rows for the detected board-monitor chip.
pub fn sensor_rows_for_chip(chip_name: Option<&str>, platform: Platform) -> Vec<SensorRow> {
    let chip = chip_name.unwrap_or("").to_uppercase();
    let base = sensor_rows(platform);
    if chip == "NCT6687D" {
        let extra = nct6687d_rows();
        if extra.is_empty() {
            return base;
        }
        let mut rows = base;
        rows.extend(extra);
        // HWiNFO's own order for this chip.
        let thermal_order = [
            "DIMM A Temp", "DIMM B Temp", "CPU Temp", "Core Max",
            "System Temp", "VRM Temp", "PCH Temp", "CPU Socket Temp",
            "T0", "T1", "CPU Package Power", "CPU Cores Power",
        ];
        let voltage_order = [
            "+12V", "+5V", "DLVR Vcore", "VIN3", "VDD2", "CPU SA (VRM)",
            "CPU AUX", "VIN7", "+3.3V",
        ];
        let mut out = of_category(&rows, "Clocks");
        out.extend(ordered(&rows, "Thermal & Power", &thermal_order));
        out.extend(ordered(&rows, "Voltages", &voltage_order));
        out.extend(of_category(&rows, "Fans"));
        out.extend(of_category(&rows, "Graphics"));
        out.extend(of_category(&rows, "Errors"));
        return out;
    }
    if chip != "NCT6798D" {
        return base;
    }

    let mut rows = base;
    rows.extend(nct6798d_sensor_rows());
    let thermal_order = [
        "DIMM A Temp", "DIMM B Temp", "VRM Temp", "System Temp",
        "CPU (Weighted Value)", "CPU Package", "CPU Temp", "Core Max",
        "CPU Socket Temp", "PCH Temp", "CPU Package Power",
        "CPU Cores Power",
    ];
    let voltage_order = [
        "DLVR Vcore", "+5V", "AVSB", "3VCC", "+12V", "VIN8", "VIN4",
        "3VSB_ATX", "BAT_3V", "VTT", "DRAM", "CPU L2", "VIN2",
        "CPU SA (VRM)", "CPU AUX", "VIN9", "VHIF",
    ];
    let mut out = of_category(&rows, "Clocks");
    out.extend(ordered(&rows, "Thermal & Power", &thermal_order));
    out.extend(ordered(&rows, "Voltages", &voltage_order));
    out.extend(of_category(&rows, "Graphics"));
    out.extend(of_category(&rows, "Errors"));
    out
}

fn board_sensor_chip_name() -> Option<String> {
    crate::sensors::board_sensors::board_sensor_profile().and_then(|profile| profile.chip_name())
}

// Rows the sensor rows replace, wherever the earlier passes left them.
pub const SUPERSEDED_SENSOR_ROWS: [(&str, &str); 4] =
    [("VCCSA", "System Info"), ("VDDQ TX", "System Info"), ("SA Voltage", "CPU"), ("TX Voltage", "CPU")];

// Rows for hardware LGA 1851 does not have, dropped rather than left blank.
//
// A row is listed here only when the reading does not exist on this platform,
// never when it exists but is not implemented yet. DRAM VDD/VDDQ/VPP and the
// DIMM temperatures stay on the tab for exactly that reason: the modules do
// report them, they are simply not reachable until the DDR5 SPD transport is
// written. A blank there means "not read yet"; a row here would mean "not
// there at all", and the two must not look the same.
//
//   VDDQ TX          not a rail on this socket.
//   VCCSA            MCHBAR 0x591C fed it on earlier sockets and reads 0 here;
//                    "CPU SA (VRM)" is the live measurement of the same rail.
//   CPU AUX          not a rail on this socket.
//   CPU Socket Temp  neither ITE chip carries a socket channel: IT8696E
//                    reports System1/PCH/CPU/PCIEX16/VRM MOS and IT87952E
//                    reports PCIEX4/System2.
//   DRAM             the board has no VDIMM sense channel; the module's own
//                    PMIC rail in DRAM VDD is the measurement that exists.
pub const ARROW_LAKE_ABSENT_SENSOR_ROWS: &[&str] = &["VDDQ TX", "VCCSA", "CPU AUX", "CPU Socket Temp", "DRAM"];

// Rows an LGA 1700 DDR5 board has no reading for. Same rule as the list above:
// listed only where the reading does not reach this platform at all.
//
//   VCCIO       not on the Super I/O this project drives. HWiNFO reports an
//               "IVR VCCIO Analog" on the Z790 bench, but through the ASUS EC
//               -- a transport nothing here speaks. The rail is real and
//               unreachable, which is why it is dropped rather than blank.
//   CPU VNNAON  an LGA 1851 rail. Nothing on the Z790 bench reports one, on
//               the Super I/O or anywhere else.
//   DRAM        the NCT6798D carries no VDIMM sense channel. The module's own
//               PMIC is the only VDD measurement this board has.
// VTT joins them for the reason the DDR4 list already gives at length: it is
// a Skylake-era board rail, and no LGA 1700 VRM or Super I/O channel reports
// one. That was written about DDR4 and is not a DDR4 fact -- it is about the
// socket. Confirmed on MSI Z790MPOWER, where HWiNFO enumerates every channel
// this board's NCT6687D answers on and none of them is a VTT.
pub const LGA1700_DDR5_ABSENT_SENSOR_ROWS: &[&str] = &["VCCIO", "CPU VNNAON", "DRAM", "VTT"];

// Rows that read correctly here and still come off the tab, which is a
// different thing from the list above and is kept separate so the two are
// never read as the same statement.
//
// All of these are reported per module in the Telemetry window instead, each
// with its own minimum, maximum and average:
//
//   DRAM VDD/VDDQ/VPP  one aggregated row has to pick a module to believe, and
//                      cannot show that the two sticks differ -- which they do
//                      here, by 15 mV on VPP.
//   DIMM A/B Temp      the same reading, under the panel of the module it
//                      belongs to. The tab pair named a channel; the panels
//                      name the slot, the part and the PMIC alongside it.
pub const LGA1700_DDR5_PER_DIMM_SENSOR_ROWS: &[&str] =
    &["DRAM VDD", "DRAM VDDQ", "DRAM VPP", "DIMM A Temp", "DIMM B Temp"];

// Rows an LGA 1700 DDR4 board has no reading for. Two kinds, both absences:
//
//   VDD2        DDR4 has no VDD2 rail.  NCT6798D input 10 is the board's DIMM
//               voltage on the ASUS Z790-A D4, so this platform exposes that
//               live channel as DRAM instead.  The same input is VDD2 on the
//               tested DDR5 Z790 APEX; the platform filter chooses the label.
//   CPU VNNAON  an LGA 1851 rail, as on the DDR5 list. Raptor Lake has none.
//   VTT         DDR4 terminates the command bus at half VDDQ, but that is
//               derived on the module. The separate VTT supply this row was
//               written for is a Skylake-era board rail, and no LGA 1700 VRM
//               or Super I/O channel reports one.
//   VCCIO       folded into VCCSA and VDDQ TX on this socket, and unreachable
//               for the same reason it is on the DDR5 list.
//
// The three PMIC rails are the second kind. A DDR4 module has no PMIC at all
// -- its rails are supplied by the board -- so there is nothing to read, and
// what the code did instead is why DRAM VPP is on this list despite having
// printed a value. read_dram_rails sweeps 0x48-0x4F for a PMIC; on this bench
// 0x4F answers, it is not a PMIC, and its registers decoded to a VPP of
// 1.500 V. DDR4 VPP is 2.5 V, so the number was not merely from the wrong
// device, it was not a voltage this rail can hold. See _dram_rail, which now
// declines the sweep on DDR4 rather than relying on the row being absent.
//
// VCCSA is on the list for neither reason. It reads correctly here -- 1.201 V
// against the VRM's 1.200 V -- and comes off because that is the same rail
// twice, which is exactly why it comes off the DDR5 board. See
// LGA1700_DDR5_DUPLICATE_SENSOR_ROWS; what remains is the VRM-side figure,
// under the rail's own name.
pub const LGA1700_DDR4_ABSENT_SENSOR_ROWS: &[&str] =
    &["VDD2", "VTT", "VCCIO", "CPU VNNAON", "DRAM VDD", "DRAM VDDQ", "DRAM VPP", "VCCSA"];

// Rows dropped because the row beside them already carries the reading.
//
//   VCCSA  the system agent rail measured on-die through MCHBAR. "SA" below is
//          the same rail measured at the board VRM, and the two sat adjacent
//          precisely to be compared. Keeping one means the tab reports the
//          rail once; they differ by about 20 mV here, and what remains is the
//          VRM-side figure.
pub const LGA1700_DDR5_DUPLICATE_SENSOR_ROWS: &[&str] = &["VCCSA"];

// Row labels that differ by platform: {platform: {declared name: label}}.
//
// A rename, not a different row -- the rail, the register and the reading are
// the same either way, and only the name a tuner is looking for changes.
//
// The row is declared "DLVR Vcore" because on LGA 1851 the Super I/O channel
// is the DLVR output, and naming that stage is the point there. On LGA 1700 the
// same channel is the VRM's core rail: HWiNFO calls it plain "Vcore" reading
// this chip, so carrying "DLVR" here would name a stage the reading does not
// come from.
// "CPU SA (VRM)" said which of two measurement points it was, because VCCSA
// sat beside it reading the same rail on-die. With VCCSA gone from this
// platform the qualifier distinguishes it from nothing, so the row takes the
// rail's own name.
//
// Both LGA 1700 memory variants take the same two names. They are one socket
// reading one Super I/O; which DRAM generation is fitted has nothing to do
// with what the core and system-agent channels are called, and listing only
// the DDR5 half left the DDR4 board saying "DLVR" for a stage it does not have.
pub const LGA1700_SENSOR_ROW_LABELS: &[(&str, &str)] = &[
    ("DLVR Vcore", "Vcore"),
    ("CPU SA (VRM)", "CPU VCCSA"),
    ("CPU Temp", "CPU"),
    ("PCH Temp", "PCH"),
    ("System Temp", "Motherboard"),
];

/// `{platform: labels}`.
pub fn sensor_row_labels(platform: Platform) -> &'static [(&'static str, &'static str)] {
    match platform {
        Platform::Lga1700Ddr4 | Platform::Lga1700Ddr5 => LGA1700_SENSOR_ROW_LABELS,
        _ => &[],
    }
}

/// The label a row carries on this platform. Pure, so it can be tested.
pub fn sensor_row_label(platform: Platform, name: &str) -> String {
    sensor_row_labels(platform)
        .iter()
        .find(|(declared, _)| *declared == name)
        .map(|(_, label)| label.to_string())
        .unwrap_or_else(|| name.to_string())
}

// Rows a particular board does not wire, whatever its socket supports. Kept
// apart from the platform lists because the reason is different: those rows
// are absent from the silicon, these are present in it and not brought out.
//
//   CPU Socket Temp  the NCT6798D on the ASUS Z790 APEX has no socket channel.
//                    It reports Motherboard, CPU (weighted), CPU Package, CPU
//                    and PCH -- the same five HWiNFO lists against that chip
//                    -- and none of them is the socket. The row read blank on
//                    every tick, which in this tool means "not read yet" and
//                    said the wrong thing about a channel that is not there.
pub const ASUS_ABSENT_SENSOR_ROWS: &[&str] = &["CPU Socket Temp"];

pub const BOARD_ABSENT_SENSOR_ROWS: &[(&str, &[&str])] = &[("ASUS", ASUS_ABSENT_SENSOR_ROWS)];

/// Rows the named board maker does not wire. Pure, so it can be tested
/// without being on that board.
///
/// Matched on a prefix of the upper-cased name: SMBIOS spells the same maker
/// "ASUSTeK COMPUTER INC." here and "ASUS" elsewhere.
pub fn board_absent_sensor_rows(manufacturer: Option<&str>) -> Vec<&'static str> {
    let name = manufacturer.unwrap_or("").to_uppercase();
    for (prefix, rows) in BOARD_ABSENT_SENSOR_ROWS {
        if name.starts_with(prefix) {
            return rows.to_vec();
        }
    }
    Vec::new()
}

/// Return the sensor rows this platform does not install.
///
/// Pure, so which rows a platform drops can be checked without importing the
/// table and without being on that platform.
pub fn absent_sensor_rows(platform: Platform, arrow_lake: bool, manufacturer: Option<&str>) -> Vec<&'static str> {
    let board = board_absent_sensor_rows(manufacturer);
    let mut rows: Vec<&'static str> = if arrow_lake {
        ARROW_LAKE_ABSENT_SENSOR_ROWS.to_vec()
    } else if platform == Platform::Lga1700Ddr5 {
        [LGA1700_DDR5_ABSENT_SENSOR_ROWS, LGA1700_DDR5_PER_DIMM_SENSOR_ROWS, LGA1700_DDR5_DUPLICATE_SENSOR_ROWS]
            .concat()
    } else if platform == Platform::Lga1700Ddr4 {
        LGA1700_DDR4_ABSENT_SENSOR_ROWS.to_vec()
    } else {
        Vec::new()
    };
    rows.extend(board);
    rows
}

/// `{"a": "A1", ...}` for each channel exactly one installed module fills.
///
/// The DIMM telemetry names its channel by SPD hub, "a" or "b", and the slot
/// inventory names a module's channel by its slot's letter, so the two meet
/// on that letter -- the same pairing the rows' CHA and CHB names already
/// stood on. A channel with two modules, or none, is left out.
pub fn dimm_slots_by_channel(modules: &[(Option<String>, Option<String>)]) -> BTreeMap<String, String> {
    let mut slots: Vec<(String, Vec<String>)> = Vec::new();
    for (slot, channel) in modules {
        let slot = crate::system_identity::py_strip(slot.as_deref().unwrap_or("")).to_string();
        let channel = crate::system_identity::py_strip(channel.as_deref().unwrap_or("")).to_lowercase();
        if !slot.is_empty() && !channel.is_empty() {
            match slots.iter_mut().find(|(c, _)| *c == channel) {
                Some((_, found)) => found.push(slot),
                None => slots.push((channel, vec![slot])),
            }
        }
    }
    slots.into_iter().filter(|(_, found)| found.len() == 1).map(|(channel, found)| (channel, found[0].clone())).collect()
}

fn installed_dimm_slots() -> BTreeMap<String, String> {
    let modules: Vec<(Option<String>, Option<String>)> = crate::memory::dimm_inventory::read_modules(false)
        .into_iter()
        .map(|module| (module.slot, module.channel))
        .collect();
    dimm_slots_by_channel(&modules)
}

const DIMM_RAILS: [(&str, &str); 6] = [
    ("vdd", "VDD"),
    ("vddq", "VDDQ"),
    ("vpp", "VPP"),
    ("vin_bulk", "VIN"),
    ("vout_1v8", "1.8V output"),
    ("vout_1v0", "1.0V output"),
];

/// Build one startup voltage snapshot from the Intel telemetry readers.
///
/// The live rows remain on ``Sensors`` for the Telemetry window, where their
/// changing values have min/max/average context.  This second view deliberately
/// caches the same readers once for the whole app session, matching the AM5
/// Voltages tab rather than polling a settings page once a second.
pub(crate) fn voltage_snapshot_rows(
    platform: Platform,
    absent: &[&str],
    sensor_rows: &[SensorRow],
    slot_names: Option<BTreeMap<String, String>>,
) -> Vec<T> {
    let summary_voltage_names: Vec<&str> =
        self::sensor_rows(platform).iter().filter(|row| row.category == "Voltages").map(|row| row.name).collect();
    let voltage_rows: Vec<SensorRow> = sensor_rows
        .iter()
        .filter(|row| row.category == "Voltages" && !absent.contains(&row.name))
        .cloned()
        .collect();

    let snapshot_rows = voltage_rows.clone();
    let cache: Arc<OnceLock<BTreeMap<String, String>>> = Arc::new(OnceLock::new());
    let snapshot = {
        let cache = cache.clone();
        move || -> BTreeMap<String, String> {
            cache
                .get_or_init(|| {
                    let mut values = BTreeMap::new();
                    for row in &snapshot_rows {
                        let label = sensor_row_label(platform, row.name);
                        let reading = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (row.getter)()))
                            .ok()
                            .flatten()
                            .filter(|text| !text.is_empty())
                            .unwrap_or_else(|| "\u{2014}".to_string());
                        values.insert(label, reading);
                    }
                    if DDR5_TIMING_PLATFORMS.contains(&platform) {
                        let modules =
                            crate::memory::ddr5_telemetry::read_dimm_telemetry(None, None, None, None, None);
                        for channel in ["a", "b"] {
                            let entries: Vec<_> =
                                modules.iter().filter(|module| module.channel.to_lowercase() == channel).collect();
                            for (key, _label) in DIMM_RAILS {
                                // One controller channel can address more than one DIMM.
                                // A shared row must not silently choose which module wins.
                                let raw = if entries.len() == 1 { entries[0].get(key) } else { None };
                                values.insert(
                                    format!("{channel}{key}"),
                                    match raw {
                                        Some(raw) => format!("{:.3} V", raw / 1000.0),
                                        None => "\u{2014}".to_string(),
                                    },
                                );
                            }
                        }
                    }
                    values
                })
                .clone()
        }
    };
    let snapshot = Arc::new(snapshot);

    // On LGA1700 DDR5 the memory rails are named for the module each PMIC is
    // on, "DIMM A1", rather than the channel; and the reading-mode note says
    // just what it is, as the tab has no refresh to point at.
    let lga1700_ddr5 = platform == Platform::Lga1700Ddr5;
    let slot_names = slot_names.unwrap_or_else(|| if lga1700_ddr5 { installed_dimm_slots() } else { BTreeMap::new() });
    let mut rows = vec![T::new()
        .s("name", "Reading mode")
        .with(
            "value",
            text(if lga1700_ddr5 {
                "Snapshot at startup"
            } else {
                "Snapshot at startup \u{2014} reopen app to update"
            }),
        )
        .s("Category", "Snapshot")
        .s("Tab", "Voltages")
        .s("Column", "Left")
        .s("read_type", "standard")];
    for row in &voltage_rows {
        let label = sensor_row_label(platform, row.name);
        let snapshot = snapshot.clone();
        let key = label.clone();
        rows.push(
            T::new()
                .s("name", &format!("{label} snapshot"))
                .s("display_name", &label)
                .with(
                    "value",
                    lazy(move || Some(snapshot().get(&key).cloned().unwrap_or_else(|| "\u{2014}".to_string()))),
                )
                .s("Category", "CPU and motherboard")
                .s("Tab", "Voltages")
                .s("Column", row.column)
                .s("read_type", "standard")
                // Chip-specific diagnostic inputs remain available in Voltages,
                // while Summary keeps its compact set of primary rails.
                .with("show_on_summary", V::Bool(summary_voltage_names.contains(&row.name))),
        );
    }
    if DDR5_TIMING_PLATFORMS.contains(&platform) {
        for (channel, channel_label) in [("a", "CHA"), ("b", "CHB")] {
            let slot = slot_names.get(channel).cloned();
            for (key, rail_label) in DIMM_RAILS {
                let name = format!("{channel_label} {rail_label}");
                let cache_key = format!("{channel}{key}");
                let snapshot = snapshot.clone();
                let category = match &slot {
                    Some(slot) => format!("DIMM {slot}"),
                    None => format!("{channel_label} memory"),
                };
                let display = match &slot {
                    Some(slot) => format!("{slot} {rail_label}"),
                    None => name.clone(),
                };
                rows.push(
                    T::new()
                        .s("name", &name)
                        .with(
                            "value",
                            lazy(move || {
                                Some(snapshot().get(&cache_key).cloned().unwrap_or_else(|| "\u{2014}".to_string()))
                            }),
                        )
                        .s("Category", &category)
                        .s("display_name", &display)
                        .s("Tab", "Voltages")
                        .s("Column", "Right")
                        .s("read_type", "standard"),
                );
            }
        }
    }
    rows
}

impl Build {
    pub(crate) fn install_sensors_tab(&mut self) {
        self.rows.retain(|timing| {
            let key = (timing.name().unwrap_or_default(), timing.tab().unwrap_or_default());
            !SUPERSEDED_SENSOR_ROWS.iter().any(|(name, tab)| (*name, *tab) == key)
        });

        let platform = self.platform;
        let chip_name = board_sensor_chip_name();
        let mut absent = absent_sensor_rows(platform, is_arrow_lake_platform(), Some(&get_board_manufacturer()));
        if chip_name.as_deref().unwrap_or("").to_uppercase() == "NCT6798D" {
            // This chip exposes the 1.040 V VTT input shown by HWiNFO. Other
            // LGA1700 board profiles keep the platform fallback that omits it.
            absent.retain(|name| *name != "VTT");
        }

        let live_rows = sensor_rows_for_chip(chip_name.as_deref(), platform);
        self.rows.extend(voltage_snapshot_rows(platform, &absent, &live_rows, None));

        for row in live_rows {
            if absent.contains(&row.name) {
                continue;
            }
            let label = sensor_row_label(platform, row.name);
            // Keep the declared name as the lookup key.  LGA1700 displays
            // "CPU Temp" as HWiNFO's shorter "CPU", but making that display
            // label the key collides with the processor identity row and lets
            // Summary show a temperature where the CPU model belongs.
            let mut timing = T::new()
                .s("name", row.name)
                .with("value", V::Text(row.getter.clone()))
                .s("Category", row.category)
                .s("Tab", SENSOR_TAB)
                .s("Column", row.column)
                .s("read_type", "standard")
                .with("live", V::Bool(true));
            if label != row.name {
                timing.set_str("display_name", &label);
            }
            self.rows.push(timing);
            if row.name == CORE_EFFECTIVE_CLOCK_ROW {
                self.rows.extend(per_core_clock_rows(row.category, row.column));
            }
        }
    }
}
