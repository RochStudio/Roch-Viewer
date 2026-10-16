//! Board sensors -- temperatures and rails -- read over Super I/O.
//!
//! What the board measures rather than what the memory controller does, which is
//! why it sits here and not under either platform. Both call it: the Intel path
//! for the rails below, and the AM5 profile for the board temperatures, which on
//! that bench come off a Nuvoton NCT6687D.
//!
//! It was named `intel_board_sensors` and lived in the Intel package, so every
//! AM5 board temperature was fetched by importing an Intel module. Nothing ran
//! that should not have -- this module's body reads no hardware -- but the
//! dispatcher promises the platforms stay apart, and a name is most of what makes
//! that promise legible.
//!
//! The rails are still Intel's. The memory controller reports VCCSA and VDDQ TX
//! directly through MCHBAR, so those two need nothing from here. DRAM voltage and
//! the CPU auxiliary rail are board measurements instead: nothing in the memory
//! controller carries them, and they reach the system only through the Super I/O
//! chip's sensor block.
//!
//! The transport is board-agnostic, but the index-to-rail assignment inside the
//! sensor block is chosen by the board vendor and does not carry between models.
//! So the Intel rail map here stays its own rather than borrowing the AM5 one,
//! exactly as `superio_lpc` warns.
//!
//! `CONFIRMED_RAILS` stays empty until an index has been matched against a
//! known-good reading on the specific board. An unconfirmed index prints a
//! believable but false voltage, which is worse than printing nothing.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::hardware::HwResult;
use crate::intel::intel_timings::get_motherboard_display;
use crate::memo::Memo;
use crate::pyfmt::round_int;
use crate::sensors::ite_superio::IteSuperIoReader;
use crate::sensors::nct679x::{self, Nct679xReader};
use crate::sensors::superio_lpc::{SuperIoReader, decode_sensor_volts};

// Sensor LSB, repeated from superio_lpc rather than imported: that module pulls
// in lowlevel_io, and reading a board rail must not drag the driver in with it.
// Several channels sit behind a 2:1 divider and decode at half the step.
pub const SENSOR_STEP_VOLTS: f64 = 0.000125;
pub const SENSOR_STEP_DIVIDED: f64 = SENSOR_STEP_VOLTS / 2.0;

/// A map from a reading's key to the NCT668x word (or NCT679x byte) it is
/// read from.
pub type AddressMap = &'static [(&'static str, u16)];
/// A map from a rail's key to `(sensor address, volts per count)`.
pub type RailMap = &'static [(&'static str, (u16, f64))];
/// A map from a temperature's key to `(ITE chip, register)`.
pub type IteTemperatureMap = &'static [(&'static str, (&'static str, u8))];
/// A map from a rail's key to `(ITE chip, register, divider)`.
pub type IteRailMap = &'static [(&'static str, (&'static str, u8, f64))];

/// The value `key` maps to in one of the tables here, as a Python dict lookup.
fn lookup<V: Copy>(table: &[(&str, V)], key: &str) -> Option<V> {
    table.iter().find(|(k, _)| *k == key).map(|(_, value)| *value)
}

/// rail key -> (row label, min volts, max volts).
/// The bands are what the rail can physically be on a desktop Intel board, used
/// only to reject a decode that cannot be right.
pub const INTEL_RAILS: &[(&str, (&str, f64, f64))] = &[
    ("vdimm", ("DRAM", 0.90, 2.10)),
    ("cpu_aux", ("CPU AUX", 0.80, 2.50)),
    ("vcore", ("Vcore", 0.40, 1.80)),
    ("cpu_sa", ("CPU SA (VRM)", 0.60, 1.70)),
    // The memory-controller supply. HWiNFO calls it IMC VDD; ASUS BIOS and the
    // DDR5 tuning world call it VDD2, which is the name a tuner is looking for.
    // Band set around the DDR5 range: 1.10 V is roughly JEDEC, and past 1.60 V
    // the number is a failed decode rather than an aggressive setting.
    ("vdd2", ("VDD2", 0.90, 1.60)),
    // The termination rail. Nominally around 1.05 V on this socket; past the
    // bounds below the number is a failed decode rather than a setting.
    ("vtt", ("VTT", 0.60, 1.60)),
    // Always-on VccNN island and the I/O rail. Both are board measurements on
    // LGA 1851 and read straight off the Super I/O.
    ("vnnaon", ("CPU VNNAON", 0.40, 1.40)),
    ("vccio", ("VCCIO", 0.80, 1.80)),
    // NCT6798D inputs exposed by HWiNFO on the ASUS Z790-A D4. VIN labels
    // intentionally stay generic: they are the board's own unrenamed inputs.
    ("plus5v", ("+5V", 4.50, 5.50)),
    ("avsb", ("AVSB", 3.00, 3.60)),
    ("v3cc", ("3VCC", 3.00, 3.60)),
    ("plus12v", ("+12V", 10.80, 13.20)),
    ("vin8", ("VIN8", 0.00, 2.50)),
    ("vin4", ("VIN4", 0.00, 2.50)),
    ("v3sb_atx", ("3VSB_ATX", 3.00, 3.60)),
    ("bat_3v", ("BAT_3V", 2.00, 3.60)),
    ("cpu_l2", ("CPU L2", 0.40, 1.60)),
    ("vin2", ("VIN2", 0.00, 2.50)),
    ("vin9", ("VIN9", 0.00, 2.50)),
    ("vhif", ("VHIF", 1.00, 2.20)),
    // NCT6687D inputs HWiNFO lists on the MSI Z790MPOWER. VIN3 and VIN7 keep
    // the chip's own names, as HWiNFO does: MSI does not label them.
    ("vin3", ("VIN3", 0.00, 2.50)),
    ("vin7", ("VIN7", 0.00, 2.50)),
    ("plus3v3", ("+3.3V", 3.00, 3.60)),
];

/// rail key -> (sensor address, volts per count).
///
/// Confirmed on MSI PRO Z790-P WIFI DDR4 (MS-7E06), NCT6687D at config port
/// 0x4E, sensor window 0x0A20, against HWiNFO's reading of this same chip taken
/// on the same boot:
///
/// ```text
///   DRAM     index 4  0x0128  full step -> 1.572 V.  HWiNFO "DRAM" 1.572 V,
///                                          exact, and the BIOS is set to 1.57.
///   CPU AUX  index 6  0x012C  full step -> 1.816 V.  HWiNFO "CPU AUX" 1.812 V,
///                                          4 mV apart across two separate
///                                          reads of a live rail.
/// ```
///
/// Neither uses the 2:1 divider. Two further channels corroborate the window
/// without being claimed as rails: index 5 halves to 1.292 V against the
/// 1.291 V this machine reports for VCCSA out of MCHBAR (HWiNFO "CPU SA"
/// 1.288 V), and index 8 halves to 3.336 V, the board's +3.3 V rail.
///
/// This mapping is board-specific. It must be re-derived on any other model.
/// Vcore and CPU SA were established a second way, because a rail that moves
/// cannot be pinned by matching one earlier capture. Loading all sixteen threads
/// and re-sampling the block, 0x0124 fell 2.6080 -> 2.5240 (1.306 -> 1.262 V
/// halved) and recovered on release, by far the largest movement in the window
/// and the droop signature only a core rail has. At idle it halves to 1.306 V
/// against HWiNFO's Vcore 1.310.
///
/// 0x012A halves to 1.290 V, within 2 mV of HWiNFO's CPU SA 1.288 and within
/// 1 mV of the 1.291 V this machine reports for VCCSA out of MCHBAR: two
/// independent instruments and a second transport agreeing on one rail.
///
/// The same run corroborated the two rails above without being aimed at them:
/// under full CPU load 0x0128 (DRAM) and 0x012C (CPU AUX) did not move, which is
/// what a memory rail and an auxiliary rail should do.
pub const CONFIRMED_RAILS: RailMap = &[
    ("vdimm", (0x128, 0.000125)),
    ("cpu_aux", (0x12C, 0.000125)),
    ("vcore", (0x124, 0.0000625)),
    ("cpu_sa", (0x12A, 0.0000625)),
];

/// The same window on a second MSI board, and the reason the warning above is
/// not boilerplate: MSI Z790MPOWER (MS-7E01), NCT6687D, read against HWiNFO on
/// the same boot.
///
/// ```text
///   idx  addr    step   reads     HWiNFO
///   2    0x0124  half   1.288 V   Vcore 1.284
///   3    0x0126  half   0.764 V   VIN3 0.760
///   4    0x0128  full   1.380 V   CPU VDD2 1.380      <- exact
///   5    0x012A  half   1.216 V   CPU SA 1.216        <- exact
///   6    0x012C  full   1.796 V   CPU AUX 1.796       <- exact
///   7    0x012E  half   1.520 V   VIN7 1.520          <- exact
///   8    0x0130  half   3.344 V   +3.3V 3.344         <- exact
/// ```
///
/// Index 4 is the entry that matters. On MS-7E06 it is the DRAM rail at
/// 1.572 V; here it is CPU VDD2 at 1.380 V, and HWiNFO names it so while
/// reporting this board's DIMM rails separately out of the module PMICs. Same
/// chip, same address, different rail -- the board decides what it wires to a
/// Super I/O input, and a map derived on one model is a claim about that model
/// only. Carrying MS-7E06's map here unchanged would have printed VDD2's
/// voltage under the name DRAM.
///
/// vdimm is deliberately absent rather than pointed elsewhere: no index in this
/// window reads the 1.470 V this board's modules are at, because they are read
/// from the PMICs instead.
///
/// The rest of the window, matched the same way on a later boot:
///
/// ```text
///   idx  addr    reads                 HWiNFO
///   0    0x0120  2.004 V x 6  = 12.024  +12V 12.024       <- exact
///   1    0x0122  2.024 V x 2.5 = 5.060  +5V 5.060-5.070
///   3    0x0126  half 0.840             VIN3 0.840-0.842
///   7    0x012E  half 1.518             VIN7 1.518-1.520
///   8    0x0130  half 3.344             +3.3V 3.344       <- exact
/// ```
///
/// +12V and +5V sit behind the board's own dividers, so their step is the
/// sensor LSB times the divider ratio: 6 and 2.5. Indices 9-15 read zero.
pub const NCT668X_BOARD_RAILS: &[(&str, RailMap)] = &[(
    "MS-7E01",
    &[
        ("plus12v", (0x120, 0.000125 * 6.0)),
        ("plus5v", (0x122, 0.000125 * 2.5)),
        ("vcore", (0x124, 0.0000625)),
        ("vin3", (0x126, 0.0000625)),
        ("vdd2", (0x128, 0.000125)),
        ("cpu_sa", (0x12A, 0.0000625)),
        ("cpu_aux", (0x12C, 0.000125)),
        ("vin7", (0x12E, 0.0000625)),
        ("plus3v3", (0x130, 0.0000625)),
    ],
)];

/// The board's model code, for picking a rail map. The board name is the
/// Intel timing module's `get_motherboard_display`, as it was in the Python,
/// where the import was deferred into this function because the Intel timing
/// module reaches this one the same way, and only inside functions, so
/// neither was imported while the other was still loading.
fn board_model_tag() -> String {
    get_motherboard_display().to_uppercase()
}

/// The first per-board map whose model tag this board's name carries.
fn board_map<T: Copy>(maps: &[(&str, T)]) -> Option<T> {
    let model = board_model_tag();
    maps.iter().find(|(tag, _)| model.contains(tag)).map(|(_, map)| *map)
}

/// The rail map for this board, falling back to the derived default.
pub fn nct668x_rails() -> RailMap {
    board_map(NCT668X_BOARD_RAILS).unwrap_or(CONFIRMED_RAILS)
}

/// Full capture kept for the record, since it is what the mapping was chosen
/// from and what a re-derivation on another board would be compared against.
///
/// Captured on MSI PRO Z790-P WIFI DDR4 (MS-7E06), NCT6687D at config port 0x4E,
/// sensor window 0x0A20, while the board ran DDR4-4000 18-16-16-28. Decoding the
/// nine responding channels at both steps:
///
/// ```text
///   index  addr    raw     full step   halved
///   0      0x0120  0x3EA0    2.004 V   1.002 V
///   1      0x0122  0x3E40    1.992 V   0.996 V
///   2      0x0124  0x51A0    2.612 V   1.306 V
///   3      0x0126  0x0620    0.196 V   0.098 V
///   4      0x0128  0x3120    1.572 V   0.786 V
///   5      0x012A  0x50C0    2.584 V   1.292 V
///   6      0x012C  0x38C0    1.816 V   0.908 V
///   7      0x012E  0x5EC0    3.032 V   1.516 V
///   8      0x0130  0xD080    6.672 V   3.336 V
/// ```
///
/// Indices 9-15 read zero.
///
/// Before the HWiNFO comparison, index 4 at the full step (1.572 V) and index 7
/// halved (1.516 V) were both believable for this kit, and the raw listing
/// printed by superio_probe made a third look right as well: that report decodes
/// with a legacy raw/1000 helper rather than the sensor step, which turns
/// index 3 into a plausible-looking 1.568 V when the rail is actually 0.196 V.
/// The measurement, not the resemblance, is what settled it.
pub const STEP_REFERENCE_CAPTURE: &[(u16, u16)] = &[
    (0x120, 0x3EA0),
    (0x122, 0x3E40),
    (0x124, 0x51A0),
    (0x126, 0x0620),
    (0x128, 0x3120),
    (0x12A, 0x50C0),
    (0x12C, 0x38C0),
    (0x12E, 0x5EC0),
    (0x130, 0xD080),
];

// --- Board temperatures.
//
// The same chip reports the board's thermal sensors one window below the
// voltages, as a whole number of degrees in the high byte and 256ths in the
// low byte.
//
// Confirmed on the same board against HWiNFO reading this chip. Sweeping
// 0x100-0x11E returned seven live sensors and then zeros, in HWiNFO's own
// listed order, at values that line up sensor for sensor:
//
//   0x100 CPU          0x102 System      0x104 MOS       0x106 PCH
//   0x108 CPU Socket   0x10A T0          0x10C T1
//
// The anchor is 0x10A: it reads about 11 C against HWiNFO's T0 of 11.5 C, and
// nothing else on this board sits anywhere near 11 C, so the alignment is not
// a coincidence of similar numbers. The small offsets elsewhere are the minutes
// between the two captures, and 0x100 is furthest out because the CPU had been
// working.
//
// T0 and T1 are the board's external thermistor headers. They are read but not
// displayed: T0's 11 C is a header with nothing attached, and a row that
// reports an unconnected probe as a temperature is worse than no row.
pub const TEMPERATURE_FRACTION: f64 = 256.0;

/// key -> (row label, min C, max C).
/// The high byte is a signed whole degree, so a reading cannot exceed 127 no
/// matter what the sensor does. The upper bounds below sit under that on
/// purpose: past them the number is a failed read rather than a hot board.
pub const INTEL_TEMPERATURES: &[(&str, (&str, f64, f64))] = &[
    ("cpu", ("CPU Temp", -20.0, 120.0)),
    ("vrm", ("VRM Temp", -20.0, 125.0)),
    ("socket", ("CPU Socket Temp", -20.0, 120.0)),
    ("pch", ("PCH Temp", -20.0, 120.0)),
    ("system", ("System Temp", -20.0, 100.0)),
    ("motherboard", ("Motherboard", -20.0, 100.0)),
    ("cpu_weighted", ("CPU (Weighted Value)", -20.0, 120.0)),
    ("cpu_package", ("CPU Package", -20.0, 120.0)),
    // The external thermistor headers. Shown only where a board's map carries
    // them, which is only where one was confirmed connected.
    ("t0", ("T0", -20.0, 120.0)),
    ("t1", ("T1", -20.0, 120.0)),
];

pub const CONFIRMED_TEMPERATURES: AddressMap = &[
    ("cpu", 0x100),
    ("system", 0x102),
    ("vrm", 0x104),
    ("pch", 0x106),
    ("socket", 0x108),
];

/// On the MSI Z790MPOWER both headers read what HWiNFO reads for them -- T0
/// 16.5 C and T1 32.0 C on the same boot, exact -- and HWiNFO names them no
/// further than T0 and T1. T1 is shown. T0 is not: 16.5 C sits well under the
/// room it is in, the reading an empty header gives, as the MS-7E06's T0 did
/// at 11 C with nothing attached. That board keeps the five.
pub const NCT668X_BOARD_TEMPERATURES: &[(&str, AddressMap)] = &[(
    "MS-7E01",
    // CONFIRMED_TEMPERATURES plus t1.
    &[
        ("cpu", 0x100),
        ("system", 0x102),
        ("vrm", 0x104),
        ("pch", 0x106),
        ("socket", 0x108),
        ("t1", 0x10C),
    ],
)];

// --- Board fans.
//
// The same chip counts fan speed one window above the voltages, a whole RPM
// per word. Confirmed on the MSI Z790MPOWER against HWiNFO on the same boot,
// in HWiNFO's order, the rest of the block reading zero:
//
//   0x140  1010 RPM  CPU       HWiNFO 1,003-1,183
//   0x142  2870 RPM  PUMP1     HWiNFO 2,870-2,877
//   0x144  1468 RPM  System 1  HWiNFO 1,433-1,468
//
// Read only, like everything else here: the chip also drives these fans, and
// nothing in this project writes a speed or a duty cycle.

/// key -> (row label, min RPM, max RPM). 0 is allowed: a fan the board has
/// stopped is a reading, not a failure.
pub const FANS: &[(&str, (&str, i64, i64))] = &[
    ("cpu_fan", ("CPU Fan", 0, 10000)),
    ("pump1", ("PUMP1", 0, 10000)),
    ("system1", ("System 1", 0, 10000)),
];

pub const NCT668X_BOARD_FANS: &[(&str, AddressMap)] =
    &[("MS-7E01", &[("cpu_fan", 0x140), ("pump1", 0x142), ("system1", 0x144)])];

/// The fan map for this board; none where no header was confirmed.
pub fn nct668x_fans() -> AddressMap {
    board_map(NCT668X_BOARD_FANS).unwrap_or(&[])
}

/// Return `{fan key: rpm}` for this board's confirmed fan headers.
pub fn read_board_fans() -> BTreeMap<&'static str, i64> {
    let mut values = BTreeMap::new();
    let Some(profile) = board_sensor_profile() else {
        return values;
    };
    let BoardSensorProfile::Nct668x { reader, fans, .. } = &*profile else {
        return values;
    };
    for &(key, address) in fans.iter() {
        let Ok(word) = reader.read_word(address) else {
            continue;
        };
        let rpm = (word as i64) & 0xFFFF;
        let (_label, minimum, maximum) = lookup(FANS, key).unwrap_or(("", 0, 0));
        if minimum <= rpm && rpm <= maximum {
            values.insert(key, rpm);
        }
    }
    values
}

/// Format one fan for a row, or `None`. No thousands separator: the
/// telemetry statistics read the first plain number in the text.
pub fn fan_text(key: &str) -> Option<String> {
    read_board_fans().get(key).map(|rpm| format!("{rpm} RPM"))
}

// --- Board fan duty.
//
// The PWM the chip is driving each header with, one byte per header from
// 0x160, in the same order as the speed words at 0x140: 0 is off and 255 is
// full duty. Read on the MSI Z790MPOWER, three samples a second apart, all
// steady:
//
//   0x160  0x66  40 %   CPU fan at 983-1001 RPM
//   0x161  0xFF  100 %  PUMP1 at 2870 RPM -- a pump the board runs flat out
//   0x162  0x99  60 %   System 1 at 1437-1444 RPM
//
// Each duty sits where its header's speed says it should, and the pump at a
// full 255 with its speed pinned is the one reading a wrong decode could not
// fake. Only the three connected headers are mapped: an empty header's duty
// is a setting driving nothing.
//
// Read only, like the speeds: nothing in this project writes a duty.
pub const NCT668X_BOARD_FAN_DUTIES: &[(&str, AddressMap)] =
    &[("MS-7E01", &[("cpu_fan", 0x160), ("pump1", 0x161), ("system1", 0x162)])];

pub const FAN_DUTY_FULL_SCALE: u32 = 255;

/// The fan-duty map for this board; none where no header was confirmed.
pub fn nct668x_fan_duties() -> AddressMap {
    board_map(NCT668X_BOARD_FAN_DUTIES).unwrap_or(&[])
}

/// A duty byte as a whole percentage of full scale.
pub fn decode_fan_duty(raw: u32) -> i64 {
    round_int(((raw & 0xFF) * 100) as f64 / FAN_DUTY_FULL_SCALE as f64)
}

/// Return `{fan key: percent}` for this board's confirmed headers.
pub fn read_board_fan_duties() -> BTreeMap<&'static str, i64> {
    let mut values = BTreeMap::new();
    let Some(profile) = board_sensor_profile() else {
        return values;
    };
    let BoardSensorProfile::Nct668x { reader, fan_duties, .. } = &*profile else {
        return values;
    };
    for &(key, address) in fan_duties.iter() {
        let Ok(bytes) = reader.read_bytes(address, 1) else {
            continue;
        };
        values.insert(key, decode_fan_duty(bytes[0] as u32));
    }
    values
}

/// The last duties read, so one read serves every duty row of a tick: the rows
/// ask one at a time, and reading all the headers for each put three times the
/// reads on the chip. The first row of the tick refreshes the set.
static FAN_DUTIES: Memo<BTreeMap<&'static str, i64>> = Memo::new();

/// Format one fan's duty for a row, or `None`.
///
/// `refresh` rereads every header; without it the row takes what the last
/// read left, unless that read had nothing for it.
pub fn fan_duty_text(key: &str, refresh: bool) -> Option<String> {
    let mut duties = FAN_DUTIES.get().unwrap_or_default();
    if refresh || !duties.contains_key(key) {
        duties = read_board_fan_duties();
        FAN_DUTIES.set(duties.clone());
    }
    duties.get(key).map(|duty| format!("{duty} %"))
}

/// The temperature map for this board, falling back to the five.
pub fn nct668x_temperatures() -> AddressMap {
    board_map(NCT668X_BOARD_TEMPERATURES).unwrap_or(CONFIRMED_TEMPERATURES)
}

// --- ASUS Z790 boards, Nuvoton NCT6798D (chip 0xD42B, config port 0x2E,
// monitor base 0x290). The complete voltage labels and scaling below were
// checked on the ROG STRIX Z790-A GAMING WIFI D4 against HWiNFO 8.52.
//
// A different chip family and a different transport; see nct679x. Sensors are
// single bytes addressed as (bank << 8) | register.
//
// Confirmed against HWiNFO reading this same chip on the same boot, and then
// each candidate put through an idle -> all-core load -> idle cycle, because a
// static capture only shows that a number resembles a reading:
//
//   CPU      0x491  33 C idle, 41-42 C loaded, 34 C recovered. HWiNFO's CPU
//                   read 33 C at capture with a 41 C maximum: the same value
//                   at rest and the same ceiling under load.
//   PCH      0x401  49 C, unmoved by an all-core load, against HWiNFO's PCH
//                   49 C. A PCH that tracked CPU load would not be a PCH.
//   System   0x027  31 C, flat, against HWiNFO's Motherboard 31 C. This is the
//                   conventional SYSTIN register on this family.
//
// 0x0FB is deliberately absent. It reads exactly 36, the only register in the
// whole space matching HWiNFO's "CPU Package 36 C", and it was still wrong: it
// did not move one degree across a full load cycle. It is the register this
// mapping would have shipped on resemblance alone.
//
// VRM and CPU Socket have no entry because this chip does not carry them on
// this board. HWiNFO lists no VRM or socket sensor under the NCT6798D; its
// "VR VCC Temperature" comes from SVID telemetry, a different transport
// entirely. Two blank rows are the honest report.
pub const NCT6798D_TEMPERATURES: AddressMap = &[
    // HWiNFO shows the weighted value equal to Motherboard on this ASUS
    // profile; both are the chip's AUXTIN4/SYSTIN input at 0x027.
    ("motherboard", 0x027),
    ("cpu_weighted", 0x027),
    // PECI_0 is the package source; CPUTIN is the board CPU source.
    ("cpu_package", 0x073),
    ("cpu", 0x491),
    ("pch", 0x401),
    ("system", 0x027),
];

/// rail key -> (sensor address, volts per count). See `nct679x::decode_volts`.
///
/// ```text
///   Vcore      0x480  0.009  the only load-tracking channel in the block: it
///                     droops under an all-core load and recovers, which is a
///                     core rail and nothing else.
///
///                     The step is 9 mV, not the 8 mV most of this block uses.
///                     That was got wrong first time by taking the droop as
///                     proof of the channel and assuming the block's usual step
///                     came with it: raw 0x92 -> 1.314 V at 9 mV, exactly
///                     HWiNFO's Vcore for this chip, where 8 mV gives 1.176 V,
///                     seventeen counts adrift and far outside anything
///                     sampling explains. The droop identified the channel; it
///                     said nothing about the scale.
///
///                     9 mV also puts Vcore in the same family as VDD2 below,
///                     whose 18 mV is 2 x 9 and was pinned independently by a
///                     two-count HWiNFO range. This block is not uniformly
///                     8 mV, which is the trap.
///   VDD2/DRAM  0x48A  0.018  the input is wired according to the board:
///                     HWiNFO identifies it as IMC VDD on the DDR5 Z790 APEX,
///                     while ASUS' NCT6798D DDR4 layout uses input 10 for DIMM
///                     voltage.  The UI selects VDD2 on DDR5 and DRAM on DDR4.
///                     On the Z790-A D4, raw 0x53 -> 1.494 V, matching the
///                     expected DIMM rail for the active DDR4-4200 profile.
///                     On the APEX, 0x4C -> 1.368 V; HWiNFO's own
///                     1.332-1.368 V range is raw 74-76, which fixes the 18 mV
///                     step independently of the single-point match.
///   CPU SA     0x48D  0.016  0x4A -> 1.184 V, exactly HWiNFO's IVR VCCSA.
///                     Distinct from the VCCSA this project reads from MCHBAR
///                     (1.204 V here): VRM-side against on-die, the same rail
///                     measured in two places.
///   CPU AUX    0x48E  0.016  0x73 -> 1.840 V, exactly HWiNFO's VCCIN_AUX.
/// ```
///
/// The block is positional. Reading 0x480-0x48F straight through on the Z790-A
/// D4 gives Vcore, +5V, AVSB, 3VCC, +12V, VIN8, VIN4, 3VSB_ATX, BAT_3V, VTT,
/// DRAM, CPU L2, VIN2, CPU VCCSA, VCCIN_AUX and VIN9. A same-moment raw dump
/// decoded to 1.323, 5.040, 3.440, 3.328, 12.000, 1.112, 1.184, 3.440,
/// 3.216, 1.040, 1.494, 1.104, 1.056, 1.344, 1.840 and 0.968 V -- the HWiNFO
/// values in the supplied capture. VHIF is the separate 0x470 input at 1.808 V.
///
/// `vdimm` deliberately aliases input 10.  It is hidden on DDR5, where that
/// pin is VDD2 and DIMM rails come from each module's PMIC; `vdd2` is hidden
/// on DDR4, where the same board-family input is the DIMM supply.  Keeping the
/// electrical source in one map prevents the two labels from drifting apart.
pub const NCT6798D_RAILS: RailMap = &[
    ("vcore", (0x480, 0.009)),
    ("plus5v", (0x481, 0.040)),
    ("avsb", (0x482, 0.016)),
    ("v3cc", (0x483, 0.016)),
    ("plus12v", (0x484, 0.096)),
    ("vin8", (0x485, 0.008)),
    ("vin4", (0x486, 0.008)),
    ("v3sb_atx", (0x487, 0.016)),
    ("bat_3v", (0x488, 0.016)),
    // VTT 0x489 0.016 -> raw 0x41 gives 1.040 V and 0x42 gives 1.056 V, both
    // of which HWiNFO reported for this channel across one capture window as
    // its minimum and maximum. Two matched endpoints fix the step.
    ("vtt", (0x489, 0.016)),
    ("vdd2", (0x48A, 0.018)),
    ("vdimm", (0x48A, 0.018)),
    ("cpu_l2", (0x48B, 0.016)),
    ("vin2", (0x48C, 0.008)),
    ("cpu_sa", (0x48D, 0.016)),
    ("cpu_aux", (0x48E, 0.016)),
    ("vin9", (0x48F, 0.008)),
    // HWiNFO exposes VHIF after the sixteen contiguous ADC inputs. On this
    // chip it is the separate bank-4 input at 0x470.
    ("vhif", (0x470, 0.016)),
];

// Detected readers, one per transport. A refusal is cached alongside a
// success (as `None`).
static DETECTED_NCT668X: Memo<Option<Arc<SuperIoReader>>> = Memo::new();
static DETECTED_NCT679X: Memo<Option<Arc<Nct679xReader>>> = Memo::new();
static PROFILE: Memo<Option<Arc<BoardSensorProfile>>> = Memo::new();

/// Return a detected NCT668x reader, or `None` when this board has no usable
/// chip.
///
/// A refusal is cached alongside a success. Detection drives the ISA path and
/// Summary re-reads on every tick, so re-probing a chip that has already
/// declined would take the monitoring mutex once a second for an answer that
/// cannot change while the machine is running. `Err` -- the bus mutex not
/// being had -- is not cached.
fn detected_nct668x() -> HwResult<Option<Arc<SuperIoReader>>> {
    if let Some(cached) = DETECTED_NCT668X.get() {
        return Ok(cached);
    }
    let mut reader = SuperIoReader::new();
    if !reader.detect()? {
        DETECTED_NCT668X.set(None);
        return Ok(None);
    }
    // Detection unlocks and re-locks the configuration window, and the chip
    // cannot move while the machine is running, so keep it.
    let reader = Arc::new(reader);
    DETECTED_NCT668X.set(Some(reader.clone()));
    Ok(Some(reader))
}

/// The NCT679x counterpart of [`detected_nct668x`], cached the same way.
fn detected_nct679x() -> HwResult<Option<Arc<Nct679xReader>>> {
    if let Some(cached) = DETECTED_NCT679X.get() {
        return Ok(cached);
    }
    let mut reader = Nct679xReader::new();
    if !reader.detect()? {
        DETECTED_NCT679X.set(None);
        return Ok(None);
    }
    let reader = Arc::new(reader);
    DETECTED_NCT679X.set(Some(reader.clone()));
    Ok(Some(reader))
}

/// Decode one sensor word into degrees Celsius.
pub fn decode_temperature(raw: u32) -> f64 {
    let raw = raw & 0xFFFF;
    let mut whole = ((raw >> 8) & 0xFF) as i32;
    if whole > 127 {
        whole -= 256;
    }
    whole as f64 + (raw & 0xFF) as f64 / TEMPERATURE_FRACTION
}

/// Return the reading, or `None` when it cannot be that sensor.
pub fn validate_temperature(key: &str, celsius: f64) -> Option<f64> {
    let (_label, minimum, maximum) = lookup(INTEL_TEMPERATURES, key)?;
    (minimum <= celsius && celsius <= maximum).then_some(celsius)
}

// --- Gigabyte Z890 AORUS TACHYON ICE, ITE IT8696E (config port 0x2E, EC
// window 0x0A40) and IT87952E (0x4E, 0x0B10).
//
// A third chip family. ITE answers its own unlock sequence and reads sensors
// as single bytes through an index/data pair, so neither Nuvoton transport
// sees it and every row on this board read blank.
//
// Confirmed against HWiNFO reading these same chips on the same boot. The two
// parts do not share a voltage LSB, which is the detail that makes or breaks
// the decode -- IT8696E steps 12 mV, IT87952E steps 10.9 mV, and swapping them
// misses every rail by 8-10%:
//
//   VCCSA    IT8696E  0x26  109 x 12.0 mV -> 1.308 V.  HWiNFO VCCSA 1.308.
//   VDD2     IT87952E 0x21  101 x 10.9 mV -> 1.101 V.  HWiNFO CPU VDD2 1.111.
//
// Two channels corroborate the IT87952E step without being claimed as rails:
// 0x22 decodes to 0.807 V against HWiNFO's "PCH 0.82V" 0.814, and 0x25 to
// 1.798 V against its "PCH 1.8V" 1.815. On IT8696E, 0x24 lands exactly on
// iGPU VAXG 0.036 and 0x25 exactly on CPU VNNAON 0.768.
//
// Vcore is matched by value and band rather than by a load sweep: it read
// 1.428 V inside HWiNFO's own 1.020-1.464 V range for CPU DLVRin Vcore, and
// no other channel on either chip sits in that band. Worth re-confirming
// under load, the way the NCT6798D rails were.
//
// The rail map is board-specific and does not transfer to another model.
pub const ITE_RAILS: IteRailMap = &[
    ("cpu_sa", ("IT8696E", 0x26, 1.0)),
    ("vdd2", ("IT87952E", 0x21, 1.0)),
    ("vcore", ("IT8696E", 0x20, 1.0)),
    ("vnnaon", ("IT8696E", 0x25, 1.0)),
    ("vccio", ("IT87952E", 0x24, 1.0)),
];

/// HWiNFO's IT8696E block lists System1, PCH, CPU, PCIEX16 and VRM MOS, which
/// is the order the registers appear in. PCIEX16 has no row on the Sensors tab
/// and is read past rather than displayed.
pub const ITE_TEMPERATURES: IteTemperatureMap = &[
    ("system", ("IT8696E", 0x29)),
    ("pch", ("IT8696E", 0x2A)),
    ("cpu", ("IT8696E", 0x2B)),
    ("vrm", ("IT8696E", 0x2D)),
];

/// The board's sensor chip, the reader that answered for it, and the maps
/// confirmed for that chip on this board.
pub enum BoardSensorProfile {
    /// Nuvoton NCT668x, read as 16-bit words through the EC window.
    Nct668x {
        reader: Arc<SuperIoReader>,
        temperatures: AddressMap,
        rails: RailMap,
        fans: AddressMap,
        fan_duties: AddressMap,
    },
    /// Nuvoton NCT679x, read as bank-switched bytes.
    Nct679x { reader: Arc<Nct679xReader>, temperatures: AddressMap, rails: RailMap },
    /// ITE IT8696E / IT87952E, read through each chip's index/data pair.
    Ite { reader: Arc<IteSuperIoReader>, temperatures: IteTemperatureMap, rails: IteRailMap },
}

impl BoardSensorProfile {
    /// The chip the reader detected, as the identity rows name it.
    pub fn chip_name(&self) -> Option<String> {
        match self {
            BoardSensorProfile::Nct668x { reader, .. } => reader.chip_name.map(str::to_string),
            BoardSensorProfile::Nct679x { reader, .. } => reader.chip_name.map(str::to_string),
            BoardSensorProfile::Ite { reader, .. } => reader.chip_name.clone(),
        }
    }

    /// The reader's module, the last component of the Python reader's module
    /// path, which `LPCIO_VENDORS` maps to a vendor.
    pub fn reader_module(&self) -> &'static str {
        match self {
            BoardSensorProfile::Nct668x { .. } => "superio_lpc",
            BoardSensorProfile::Nct679x { .. } => "nct679x",
            BoardSensorProfile::Ite { .. } => "ite_superio",
        }
    }

    /// The temperature keys this profile maps, in map order.
    pub fn temperature_keys(&self) -> Vec<&'static str> {
        match self {
            BoardSensorProfile::Nct668x { temperatures, .. }
            | BoardSensorProfile::Nct679x { temperatures, .. } => {
                temperatures.iter().map(|(key, _)| *key).collect()
            }
            BoardSensorProfile::Ite { temperatures, .. } => {
                temperatures.iter().map(|(key, _)| *key).collect()
            }
        }
    }

    /// The rail keys this profile maps, in map order.
    pub fn rail_keys(&self) -> Vec<&'static str> {
        match self {
            BoardSensorProfile::Nct668x { rails, .. } | BoardSensorProfile::Nct679x { rails, .. } => {
                rails.iter().map(|(key, _)| *key).collect()
            }
            BoardSensorProfile::Ite { rails, .. } => rails.iter().map(|(key, _)| *key).collect(),
        }
    }

    /// The fan headers mapped on this board; only the NCT668x boards have any.
    pub fn fans(&self) -> AddressMap {
        match self {
            BoardSensorProfile::Nct668x { fans, .. } => fans,
            _ => &[],
        }
    }

    /// The fan-duty bytes mapped on this board; only the NCT668x boards have
    /// any.
    pub fn fan_duties(&self) -> AddressMap {
        match self {
            BoardSensorProfile::Nct668x { fan_duties, .. } => fan_duties,
            _ => &[],
        }
    }

    /// Every mapped temperature, decoded with this chip's own decode.
    pub fn read_temperatures(&self) -> BTreeMap<&'static str, f64> {
        match self {
            BoardSensorProfile::Nct668x { reader, temperatures, .. } => {
                read_temperatures(temperatures, |address| nct668x_temperature(reader, address))
            }
            BoardSensorProfile::Nct679x { reader, temperatures, .. } => {
                read_temperatures(temperatures, |address| nct679x_temperature(reader, address))
            }
            BoardSensorProfile::Ite { reader, temperatures, .. } => {
                read_temperatures(temperatures, |spec| ite_temperature(reader, spec))
            }
        }
    }

    /// Every mapped rail, decoded with this chip's own decode.
    pub fn read_rails(&self) -> BTreeMap<&'static str, f64> {
        match self {
            BoardSensorProfile::Nct668x { reader, rails, .. } => {
                read_rails(rails, |spec| nct668x_rail(reader, spec))
            }
            BoardSensorProfile::Nct679x { reader, rails, .. } => {
                read_rails(rails, |spec| nct679x_rail(reader, spec))
            }
            BoardSensorProfile::Ite { reader, rails, .. } => {
                read_rails(rails, |spec| ite_rail(reader, spec))
            }
        }
    }
}

/// Return the NCT679x reader and maps, or `None` when no such chip is here.
fn nct679x_profile() -> Option<BoardSensorProfile> {
    let reader = detected_nct679x().ok()??;
    Some(BoardSensorProfile::Nct679x {
        reader,
        temperatures: NCT6798D_TEMPERATURES,
        rails: NCT6798D_RAILS,
    })
}

/// Return the NCT668x reader and maps, or `None` when no such chip is here.
fn nct668x_profile() -> Option<BoardSensorProfile> {
    let reader = detected_nct668x().ok()??;
    Some(BoardSensorProfile::Nct668x {
        reader,
        temperatures: nct668x_temperatures(),
        rails: nct668x_rails(),
        fans: nct668x_fans(),
        fan_duties: nct668x_fan_duties(),
    })
}

/// Return the ITE reader and maps, or `None` when no ITE chip is here.
fn ite_profile() -> Option<BoardSensorProfile> {
    let mut reader = IteSuperIoReader::new();
    if !reader.detect() {
        return None;
    }
    Some(BoardSensorProfile::Ite {
        reader: Arc::new(reader),
        temperatures: ITE_TEMPERATURES,
        rails: ITE_RAILS,
    })
}

fn ite_temperature(reader: &IteSuperIoReader, spec: (&str, u8)) -> Option<f64> {
    let (chip, register) = spec;
    reader.read_temperature(chip, register)
}

fn ite_rail(reader: &IteSuperIoReader, spec: (&str, u8, f64)) -> Option<f64> {
    let (chip, register, divider) = spec;
    reader.read_voltage(chip, register, divider)
}

fn nct668x_temperature(reader: &SuperIoReader, address: u16) -> Option<f64> {
    reader.read_word(address).ok().map(|word| decode_temperature(word as u32))
}

fn nct668x_rail(reader: &SuperIoReader, spec: (u16, f64)) -> Option<f64> {
    let (address, step) = spec;
    reader.read_word(address).ok().map(|word| decode_sensor_volts(word as u32, step))
}

fn nct679x_temperature(reader: &Nct679xReader, address: u16) -> Option<f64> {
    reader
        .read_byte(address)
        .ok()
        .map(|raw| nct679x::decode_temperature(raw as u32) as f64)
}

fn nct679x_rail(reader: &Nct679xReader, spec: (u16, f64)) -> Option<f64> {
    let (address, multiplier) = spec;
    reader.read_byte(address).ok().map(|raw| nct679x::decode_volts(raw as u32, multiplier))
}

/// Tried in order. Each asks its own transport whether its chip is present, so
/// a board answers at most one of them; both decline on a chip neither knows,
/// which is the outcome that leaves the rows blank rather than invented.
const SENSOR_PROFILES: &[fn() -> Option<BoardSensorProfile>] =
    &[nct668x_profile, nct679x_profile, ite_profile];

/// Return this board's sensor profile, or `None`. Resolved once.
pub fn board_sensor_profile() -> Option<Arc<BoardSensorProfile>> {
    PROFILE.get_or_init(|| SENSOR_PROFILES.iter().find_map(|build| build()).map(Arc::new))
}

/// The Super I/O chip the board-sensor profile detected, as
/// `(reader module, chip name)` -- the module being the last component of
/// the Python reader's module ("superio_lpc", "nct679x" or "ite_superio"),
/// which `system_identity::lpcio_name` maps to a vendor. `None` when no
/// profile matched or its reader names no chip.
pub fn detected_lpcio_chip() -> Option<(&'static str, String)> {
    let profile = board_sensor_profile()?;
    let name = profile.chip_name().filter(|name| !name.is_empty())?;
    Some((profile.reader_module(), name))
}

/// Return `{sensor key: celsius}` for the confirmed board sensors.
///
/// The board's own chip decides which map and which decode apply.
pub fn read_board_temperatures() -> BTreeMap<&'static str, f64> {
    match board_sensor_profile() {
        Some(profile) => profile.read_temperatures(),
        None => BTreeMap::new(),
    }
}

/// The same, over an explicit NCT668x map: a caller that supplies the map is
/// supplying the transport too, and keeps the NCT668x word protocol it always
/// had (Python's `read_board_temperatures(sensors=...)`).
pub fn read_board_temperatures_with(sensors: AddressMap) -> BTreeMap<&'static str, f64> {
    if sensors.is_empty() {
        return BTreeMap::new();
    }
    let Ok(Some(reader)) = detected_nct668x() else {
        return BTreeMap::new();
    };
    read_temperatures(sensors, |address| nct668x_temperature(&reader, address))
}

/// Decode each mapped sensor, dropping anything that cannot be that row.
fn read_temperatures<S: Copy>(
    sensors: &[(&'static str, S)],
    read_one: impl Fn(S) -> Option<f64>,
) -> BTreeMap<&'static str, f64> {
    let mut values = BTreeMap::new();
    for &(key, spec) in sensors {
        let Some(celsius) = read_one(spec) else {
            continue;
        };
        if let Some(checked) = validate_temperature(key, celsius) {
            values.insert(key, checked);
        }
    }
    values
}

/// Format one board temperature for a table row, or `None` when unavailable.
pub fn temperature_text(key: &str) -> Option<String> {
    read_board_temperatures().get(key).map(|celsius| format!("{celsius:.1} °C"))
}

/// Return the reading, or `None` when it cannot be that rail.
pub fn validate_rail(key: &str, volts: f64) -> Option<f64> {
    let (_label, minimum, maximum) = lookup(INTEL_RAILS, key)?;
    (minimum <= volts && volts <= maximum).then_some(volts)
}

/// Return `{rail key: volts}` for the confirmed Intel rails.
///
/// Fail-closed like the other privileged transports: a rail that cannot be
/// read, or that decodes outside its band, is left out of the result rather
/// than reported as a number.
pub fn read_board_rails() -> BTreeMap<&'static str, f64> {
    match board_sensor_profile() {
        Some(profile) => profile.read_rails(),
        None => BTreeMap::new(),
    }
}

/// The same, over an explicit NCT668x map (Python's
/// `read_board_rails(sensors=...)`).
pub fn read_board_rails_with(sensors: RailMap) -> BTreeMap<&'static str, f64> {
    if sensors.is_empty() {
        // Nothing confirmed on this board, so nothing is claimed. Opening the
        // Super I/O machinery here would also open the ISA path for no reason.
        return BTreeMap::new();
    }
    let Ok(Some(reader)) = detected_nct668x() else {
        return BTreeMap::new();
    };
    read_rails(sensors, |spec| nct668x_rail(&reader, spec))
}

/// Decode each mapped rail, dropping anything outside its band.
fn read_rails<S: Copy>(
    sensors: &[(&'static str, S)],
    read_one: impl Fn(S) -> Option<f64>,
) -> BTreeMap<&'static str, f64> {
    let mut values = BTreeMap::new();
    for &(key, spec) in sensors {
        let Some(volts) = read_one(spec) else {
            continue;
        };
        if let Some(checked) = validate_rail(key, volts) {
            values.insert(key, checked);
        }
    }
    values
}

/// Format one rail for a table row, or `None` when it is unavailable.
pub fn rail_text(key: &str) -> Option<String> {
    read_board_rails().get(key).map(|volts| format!("{volts:.3} V"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sensors::superio_lpc::test_support::FakeIo;
    use crate::wmi_util::{FakeWmi, WmiObject};

    fn assert_close(actual: f64, expected: f64, places: i32) {
        assert!(
            (actual - expected).abs() < 0.5 * 10f64.powi(-places),
            "{actual} != {expected} to {places} places"
        );
    }

    fn captured(address: u16) -> u16 {
        lookup_address(STEP_REFERENCE_CAPTURE, address)
    }

    fn lookup_address(table: &[(u16, u16)], address: u16) -> u16 {
        table.iter().find(|(a, _)| *a == address).unwrap().1
    }

    fn rail(map: RailMap, key: &str) -> (u16, f64) {
        lookup(map, key).unwrap()
    }

    /// The board whose name `board_model_tag` reads.
    fn on_board(product: &str) {
        FakeWmi::new()
            .class(
                "Win32_BaseBoard",
                vec![WmiObject::new()
                    .with("Manufacturer", "Micro-Star International Co., Ltd.")
                    .with("Product", product)],
            )
            .install();
    }

    /// An NCT6687D answering with `words`; any other sensor fails to read,
    /// the way the Python tests' fake reader raised for it. `detects: false`
    /// is a board where no Super I/O answers, so every transport declines.
    fn nct668x(words: &[(u16, u16)], detects: bool) -> Arc<FakeIo> {
        let io = FakeIo::with_words(words);
        if !detects {
            io.state.lock().unwrap().answer_on = None;
        }
        io.install()
    }

    /// The Python fake reader's `reads`, as the sensor bytes addressed: each
    /// word read is its two bytes.
    fn word_reads(addresses: &[u16]) -> Vec<u16> {
        addresses.iter().flat_map(|a| [*a, *a + 1]).collect()
    }

    // The 0x100 block as swept on the Z790 target, against HWiNFO reading the
    // same chip: CPU, System, MOS, PCH, CPU Socket, T0, T1.
    const TEMPERATURE_CAPTURE: &[(u16, u16)] = &[
        (0x100, 0x3600), // 54.0
        (0x102, 0x2300), // 35.0
        (0x104, 0x2380), // 35.5
        (0x106, 0x2800), // 40.0
        (0x108, 0x2200), // 34.0
        (0x10A, 0x0B00), // 11.0  - the T0 anchor
        (0x10C, 0x1F00), // 31.0
    ];

    // -- ConfirmedMappingTest: the mapping must keep decoding to the values
    // it was confirmed against.

    #[test]
    fn every_confirmed_rail_is_a_known_rail() {
        for (key, _) in CONFIRMED_RAILS {
            assert!(lookup(INTEL_RAILS, key).is_some(), "{key}");
        }
    }

    #[test]
    fn dram_decodes_to_the_value_hwinfo_reported() {
        let (address, step) = rail(CONFIRMED_RAILS, "vdimm");
        assert_close(captured(address) as f64 * step, 1.572, 3);
    }

    #[test]
    fn cpu_aux_decodes_to_the_value_hwinfo_reported() {
        let (address, step) = rail(CONFIRMED_RAILS, "cpu_aux");
        assert_close(captured(address) as f64 * step, 1.816, 3);
    }

    #[test]
    fn every_confirmed_rail_decodes_inside_its_band() {
        for &(key, (address, step)) in CONFIRMED_RAILS {
            assert!(validate_rail(key, captured(address) as f64 * step).is_some(), "{key}");
        }
    }

    #[test]
    fn an_empty_map_reports_nothing_and_opens_no_path() {
        let io = nct668x(&[(0x128, 0x3120)], true);
        assert!(read_board_rails_with(&[]).is_empty());
        assert!(io.byte_reads().is_empty());
        assert!(io.writes().is_empty(), "the ISA path was opened");
    }

    // -- DetectionRefusalTest: a board whose chip declines must report
    // nothing, and only ask once.

    #[test]
    fn a_declining_chip_reports_no_temperatures() {
        let io = nct668x(TEMPERATURE_CAPTURE, false);
        assert!(read_board_temperatures_with(CONFIRMED_TEMPERATURES).is_empty());
        assert!(io.byte_reads().is_empty());
    }

    #[test]
    fn a_declining_chip_reports_no_rails() {
        let io = nct668x(&[(0x128, 0x3120)], false);
        assert!(read_board_rails_with(CONFIRMED_RAILS).is_empty());
        assert!(io.byte_reads().is_empty());
    }

    #[test]
    fn the_refusal_is_cached_so_the_isa_path_is_not_reprobed() {
        // Summary re-reads every tick. Without a cached refusal each tick takes
        // the monitoring mutex to re-ask a chip that cannot change.
        let io = nct668x(TEMPERATURE_CAPTURE, false);
        let unlocks = |io: &FakeIo| io.writes().iter().filter(|(_, v)| *v == 0x87).count();
        read_board_temperatures_with(CONFIRMED_TEMPERATURES);
        let first = unlocks(&io);
        assert!(first > 0);
        for _ in 0..4 {
            read_board_temperatures_with(CONFIRMED_TEMPERATURES);
        }
        assert_eq!(unlocks(&io), first);
    }

    // -- RailValidationTest ------------------------------------------------

    #[test]
    fn voltage_text_has_a_space_before_the_unit() {
        // 0x2EB0 counts at the full step on the DRAM channel is 1.494 V.
        on_board("PRO Z790-P WIFI DDR4 (MS-7E06)");
        nct668x(&[(0x128, 0x2EB0)], true);
        assert_eq!(rail_text("vdimm").as_deref(), Some("1.494 V"));
    }

    #[test]
    fn reading_inside_the_band_is_kept() {
        assert_eq!(validate_rail("vdimm", 1.568), Some(1.568));
    }

    #[test]
    fn reading_outside_the_band_is_dropped() {
        assert_eq!(validate_rail("vdimm", 16.032), None);
        assert_eq!(validate_rail("vdimm", 0.0), None);
    }

    #[test]
    fn band_edges_are_inclusive() {
        let (_label, minimum, maximum) = lookup(INTEL_RAILS, "vdimm").unwrap();
        assert_eq!(validate_rail("vdimm", minimum), Some(minimum));
        assert_eq!(validate_rail("vdimm", maximum), Some(maximum));
    }

    #[test]
    fn unknown_rail_is_dropped() {
        assert_eq!(validate_rail("nonexistent", 1.2), None);
    }

    #[test]
    fn unreadable_value_is_dropped() {
        // The Python passed None and a string; a Rust caller cannot, and the
        // one unreadable float there is is NaN.
        assert_eq!(validate_rail("vdimm", f64::NAN), None);
    }

    // -- ConfirmedRailReadTest: behaviour once a rail is confirmed, driven
    // through the explicit map.

    #[test]
    fn a_confirmed_rail_is_decoded_and_returned() {
        // 0x3100 counts at 0.125 mV each is 1.568 V.
        let io = nct668x(&[(0x126, 0x3100)], true);
        let rails = read_board_rails_with(&[("vdimm", (0x126, 0.000125))]);
        assert_close(rails["vdimm"], 1.568, 3);
        assert_eq!(io.byte_reads(), word_reads(&[0x126]));
    }

    #[test]
    fn a_decode_outside_the_band_is_left_out() {
        // 0xD080 at the full step is 6.672 V, which no DRAM rail can be.
        nct668x(&[(0x130, 0xD080)], true);
        assert!(read_board_rails_with(&[("vdimm", (0x130, 0.000125))]).is_empty());
    }

    #[test]
    fn a_sensor_that_does_not_answer_is_left_out_of_the_rails() {
        nct668x(&[], true);
        assert!(read_board_rails_with(&[("vdimm", (0x126, 0.000125))]).is_empty());
    }

    #[test]
    fn failed_detection_yields_no_rails() {
        let io = nct668x(&[(0x126, 0x3100)], false);
        assert!(read_board_rails_with(&[("vdimm", (0x126, 0.000125))]).is_empty());
        assert!(io.byte_reads().is_empty());
    }

    #[test]
    fn detection_is_reused_across_reads() {
        let io = nct668x(&[(0x126, 0x3100)], true);
        let sensors: RailMap = &[("vdimm", (0x126, 0.000125))];
        read_board_rails_with(sensors);
        read_board_rails_with(sensors);
        assert_eq!(io.byte_reads(), word_reads(&[0x126, 0x126]));
        let detections = io.writes().iter().filter(|(_, v)| *v == 0x87).count();
        assert_eq!(detections, 4, "two unlock bytes per port, one detection");
    }

    // -- MovingRailTest: Vcore and CPU SA, the two rails a single capture
    // could not pin.

    #[test]
    fn vcore_decodes_to_the_idle_value_that_was_measured() {
        let (address, step) = rail(CONFIRMED_RAILS, "vcore");
        assert_close(captured(address) as f64 * step, 1.306, 3);
    }

    #[test]
    fn cpu_sa_decodes_to_the_value_two_instruments_agreed_on() {
        let (address, step) = rail(CONFIRMED_RAILS, "cpu_sa");
        assert_close(captured(address) as f64 * step, 1.292, 3);
    }

    #[test]
    fn both_moving_rails_use_the_halved_step() {
        let full_step = rail(CONFIRMED_RAILS, "vdimm").1;
        for key in ["vcore", "cpu_sa"] {
            assert_close(rail(CONFIRMED_RAILS, key).1, full_step / 2.0, 7);
        }
    }

    #[test]
    fn the_droop_measured_under_load_stays_inside_the_band() {
        // 0x0124 fell to 2.5240 raw under an all-core load.
        let (_address, step) = rail(CONFIRMED_RAILS, "vcore");
        assert!(validate_rail("vcore", 0x51A0 as f64 * step).is_some());
        assert!(validate_rail("vcore", 20192.0 * step).is_some());
    }

    // -- TemperatureDecodeTest ---------------------------------------------

    #[test]
    fn the_high_byte_is_whole_degrees() {
        assert_eq!(decode_temperature(0x2800), 40.0);
    }

    #[test]
    fn the_low_byte_is_a_fraction_of_a_degree() {
        assert_eq!(decode_temperature(0x2380), 35.5);
    }

    #[test]
    fn a_negative_reading_decodes() {
        assert_eq!(decode_temperature(0xFB00), -5.0);
    }

    #[test]
    fn zero_decodes() {
        assert_eq!(decode_temperature(0x0000), 0.0);
    }

    #[test]
    fn every_captured_sensor_decodes_to_what_hwinfo_listed() {
        let expected = [
            (0x100, 54.0), (0x102, 35.0), (0x104, 35.5), (0x106, 40.0),
            (0x108, 34.0), (0x10A, 11.0), (0x10C, 31.0),
        ];
        for &(address, raw) in TEMPERATURE_CAPTURE {
            let want = expected.iter().find(|(a, _)| *a == address).unwrap().1;
            assert_eq!(decode_temperature(raw as u32), want, "0x{address:X}");
        }
    }

    // -- TemperatureValidationTest -----------------------------------------

    #[test]
    fn a_reading_inside_the_band_is_kept() {
        assert_eq!(validate_temperature("vrm", 35.5), Some(35.5));
    }

    #[test]
    fn a_reading_outside_the_band_is_dropped() {
        assert_eq!(validate_temperature("cpu", 300.0), None);
        assert_eq!(validate_temperature("cpu", -100.0), None);
    }

    #[test]
    fn an_unknown_sensor_is_dropped() {
        assert_eq!(validate_temperature("nonexistent", 40.0), None);
    }

    #[test]
    fn an_unreadable_value_is_dropped() {
        // The Python passed None and a string; NaN is the Rust counterpart.
        assert_eq!(validate_temperature("cpu", f64::NAN), None);
    }

    #[test]
    fn the_ceiling_sits_under_what_the_high_byte_can_hold() {
        // A signed high byte tops out at 127, so a bound above that could
        // never reject anything.
        for (key, (_label, _minimum, maximum)) in INTEL_TEMPERATURES {
            assert!(*maximum < 127.0, "{key}");
        }
    }

    // -- BoardTemperatureReadTest ------------------------------------------

    #[test]
    fn every_confirmed_sensor_is_a_known_sensor() {
        for (key, _) in CONFIRMED_TEMPERATURES {
            assert!(lookup(INTEL_TEMPERATURES, key).is_some(), "{key}");
        }
    }

    #[test]
    fn the_thermistor_headers_are_not_displayed() {
        // T0 reads 11 C with nothing attached, so neither header is a row.
        assert!(!CONFIRMED_TEMPERATURES.iter().any(|(_, a)| *a == 0x10A));
        assert!(!CONFIRMED_TEMPERATURES.iter().any(|(_, a)| *a == 0x10C));
    }

    #[test]
    fn the_captured_block_reads_back_as_the_board_sensors() {
        nct668x(TEMPERATURE_CAPTURE, true);
        let values = read_board_temperatures_with(CONFIRMED_TEMPERATURES);
        assert_eq!(values["cpu"], 54.0);
        assert_eq!(values["system"], 35.0);
        assert_eq!(values["vrm"], 35.5);
        assert_eq!(values["pch"], 40.0);
        assert_eq!(values["socket"], 34.0);
    }

    #[test]
    fn a_sensor_that_does_not_answer_is_left_out() {
        nct668x(&[(0x104, 0x2380)], true);
        let values = read_board_temperatures_with(CONFIRMED_TEMPERATURES);
        assert_eq!(values, BTreeMap::from([("vrm", 35.5)]));
    }

    #[test]
    fn an_implausible_reading_is_left_out() {
        nct668x(&[(0x104, 0x7F00)], true);
        assert!(read_board_temperatures_with(CONFIRMED_TEMPERATURES).is_empty());
    }

    #[test]
    fn failed_detection_yields_no_sensors() {
        let io = nct668x(TEMPERATURE_CAPTURE, false);
        assert!(read_board_temperatures_with(CONFIRMED_TEMPERATURES).is_empty());
        assert!(io.byte_reads().is_empty());
    }

    #[test]
    fn an_empty_map_reads_nothing() {
        let io = nct668x(TEMPERATURE_CAPTURE, true);
        assert!(read_board_temperatures_with(&[]).is_empty());
        assert!(io.byte_reads().is_empty());
    }

    #[test]
    fn formatting_carries_one_decimal_and_a_unit() {
        // Through the board's own profile: an NCT6687D on a board with no
        // map of its own reads the confirmed five.
        on_board("PRO Z790-P WIFI DDR4 (MS-7E06)");
        nct668x(TEMPERATURE_CAPTURE, true);
        assert_eq!(temperature_text("vrm").as_deref(), Some("35.5 °C"));
    }

    #[test]
    fn formatting_an_absent_sensor_reports_nothing() {
        on_board("PRO Z790-P WIFI DDR4 (MS-7E06)");
        nct668x(&[], true);
        assert_eq!(temperature_text("vrm"), None);
    }

    // -- The contract system_identity builds on ----------------------------

    #[test]
    fn the_detected_chip_is_named_with_its_transport() {
        on_board("PRO Z790-P WIFI DDR4 (MS-7E06)");
        nct668x(&[], true);
        assert_eq!(detected_lpcio_chip(), Some(("superio_lpc", "NCT6687D".to_string())));
    }

    #[test]
    fn no_chip_names_nothing() {
        on_board("PRO Z790-P WIFI DDR4 (MS-7E06)");
        nct668x(&[], false);
        assert_eq!(detected_lpcio_chip(), None);
    }

    // == tests/test_nct6687d_board_map.py: the MSI Z790MPOWER's NCT6687D,
    // every input HWiNFO lists, and only there.

    // Raw words read off the MS-7E01 window, with what HWiNFO showed for each.
    const MS_7E01_WINDOW: &[(&str, (u16, f64))] = &[
        ("plus12v", (0x3EA0, 12.024)),
        ("plus5v", (0x3F40, 5.060)),
        ("vcore", (0x4FA0, 1.274)),
        ("vin3", (0x3480, 0.840)),
        ("vdd2", (0x2B20, 1.380)),
        ("cpu_sa", (0x4A00, 1.184)),
        ("cpu_aux", (0x3820, 1.796)),
        ("vin7", (0x5EE0, 1.518)),
        ("plus3v3", (0xD100, 3.344)),
    ];
    const MS_7E01_TEMPERATURES: &[(&str, (u16, f64))] =
        &[("t0", (0x1080, 16.5)), ("t1", (0x2000, 32.0))];

    // -- Ms7e01RailsTest ---------------------------------------------------

    #[test]
    fn every_rail_decodes_to_the_hwinfo_reading() {
        on_board("Z790MPOWER (MS-7E01)");
        let rails = nct668x_rails();
        let mut keys: Vec<&str> = rails.iter().map(|(k, _)| *k).collect();
        let mut want: Vec<&str> = MS_7E01_WINDOW.iter().map(|(k, _)| *k).collect();
        keys.sort();
        want.sort();
        assert_eq!(keys, want);
        for &(key, (raw, expected)) in MS_7E01_WINDOW {
            let volts = decode_sensor_volts(raw as u32, rail(rails, key).1);
            assert_close(volts, expected, 3);
            assert!(validate_rail(key, volts).is_some(), "{key}");
        }
    }

    #[test]
    fn t1_is_shown_and_the_empty_t0_is_not() {
        // T0 reads 16.5 C, under the room it is in: an empty header.
        on_board("Z790MPOWER (MS-7E01)");
        let sensors = nct668x_temperatures();
        assert_eq!(lookup(sensors, "t0"), None);
        assert_eq!(lookup(sensors, "t1"), Some(0x10C));
        for &(key, (raw, expected)) in MS_7E01_TEMPERATURES {
            assert_eq!(decode_temperature(raw as u32), expected, "{key}");
        }
    }

    // -- Ms7e01FansTest ----------------------------------------------------

    #[test]
    fn the_three_headers_hwinfo_lists() {
        on_board("Z790MPOWER (MS-7E01)");
        assert_eq!(nct668x_fans(), &[("cpu_fan", 0x140), ("pump1", 0x142), ("system1", 0x144)]);
    }

    #[test]
    fn rpm_is_read_whole_and_written_without_a_separator() {
        // The telemetry statistics take the first plain number in the text;
        // "1,010 RPM" would have read as 1. (The parse_reading half of this
        // test belongs to the telemetry window and is ported with it.)
        on_board("Z790MPOWER (MS-7E01)");
        nct668x(&[(0x140, 0x03F2), (0x142, 0x0B36), (0x144, 0x05BC)], true);
        assert_eq!(
            read_board_fans(),
            BTreeMap::from([("cpu_fan", 1010), ("pump1", 2870), ("system1", 1468)])
        );
        assert_eq!(fan_text("cpu_fan").as_deref(), Some("1010 RPM"));
    }

    // -- Ms7e01FanDutyTest -------------------------------------------------

    /// The MS-7E01 with its three connected headers' duty bytes.
    fn duty_board() -> Arc<FakeIo> {
        on_board("Z790MPOWER (MS-7E01)");
        let io = FakeIo::new(&[(0x160, 0x66), (0x161, 0xFF), (0x162, 0x99)], Some(0x4E));
        io.state.lock().unwrap().missing_fails = true;
        io.install()
    }

    #[test]
    fn the_three_connected_headers() {
        on_board("Z790MPOWER (MS-7E01)");
        assert_eq!(
            nct668x_fan_duties(),
            &[("cpu_fan", 0x160), ("pump1", 0x161), ("system1", 0x162)]
        );
    }

    #[test]
    fn the_bench_bytes_decode_to_whole_percentages() {
        // (The parse_reading half belongs to the telemetry window.)
        duty_board();
        assert_eq!(
            read_board_fan_duties(),
            BTreeMap::from([("cpu_fan", 40), ("pump1", 100), ("system1", 60)])
        );
        assert_eq!(fan_duty_text("pump1", true).as_deref(), Some("100 %"));
    }

    #[test]
    fn one_read_serves_the_rows_of_a_tick() {
        // The first duty row refreshes every header; the rest take that read.
        let io = duty_board();
        let shown = [
            fan_duty_text("cpu_fan", true),
            fan_duty_text("pump1", false),
            fan_duty_text("system1", false),
        ];
        assert_eq!(
            shown,
            [Some("40 %".to_string()), Some("100 %".to_string()), Some("60 %".to_string())]
        );
        assert_eq!(io.byte_reads(), vec![0x160, 0x161, 0x162]);
    }

    #[test]
    fn off_and_full_scale() {
        assert_eq!(decode_fan_duty(0x00), 0);
        assert_eq!(decode_fan_duty(0xFF), 100);
    }

    // -- OtherBoardTest ----------------------------------------------------

    #[test]
    fn ms_7e06_keeps_its_own_map() {
        // Same chip, different wiring: T0 there is an unconnected header.
        on_board("PRO Z790-P WIFI DDR4 (MS-7E06)");
        assert_eq!(lookup(nct668x_temperatures(), "t0"), None);
        assert_eq!(lookup(nct668x_rails(), "plus12v"), None);
        assert!(nct668x_fans().is_empty());
        assert!(nct668x_fan_duties().is_empty());
    }
}
