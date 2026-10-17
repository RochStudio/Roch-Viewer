//! Everything read out of the memory controller: clocks, timings computed
//! from several fields, the mode-register tables, and the decodes that turn
//! them into text.
//!
//! Every reader returns `None` (or the Python's own "N/A"/"Unknown"/"Error"
//! text, where that is what it returned) when a read does not work; nothing
//! here panics on missing hardware.

use crate::hardware::read::{DynamicParams, FieldRead, read_field, read_physical_memory_int, read_timing};
use crate::memo::{Memo, MemoMap};
use crate::memory::ddr_generation::{DdrGeneration, detect_ddr_generation};
use crate::platform_profiles::Platform;
use crate::pyfmt::{float_str, round_half_even, round_int};
use crate::row::ReadSpec;

use super::formulas::{self, Table, lookup};
use super::identity::{get_cpu_name, get_motherboard_display, parse_first_number};
use super::{MCHBAR, MCHBAR2};

/// Python's `read_timing(address, bit_start=..., bit_length=...,
/// read_type="standard")`.
pub(crate) fn rt(address: u64, bit_start: u32, bit_length: i32) -> Option<u64> {
    read_field(&FieldRead::new(address, bit_start, bit_length))
}

/// The controller base a reader was asked for, MCHBAR when none was.
fn mc(base: Option<u64>) -> u64 {
    base.unwrap_or(MCHBAR)
}

fn ddr5_rows(platform: Platform) -> bool {
    platform == Platform::Lga1700Ddr5
}

// The clock rows are read down one column, so they have to be written the same
// way. Three of them were built by pasting the unit straight onto a rounded
// float, which put "5000.0Mhz" next to "2000 MHz": no space, and a decimal
// point on a whole number. Rounding to a fixed number of places and then
// dropping what it added keeps a real fraction like 4987.5 and loses the ".0".
pub const MHZ_DECIMALS: usize = 3;

/// Python's `round(x, ndigits)` for a float and `ndigits >= 1`.
///
/// Python rounds the float's exact binary value half to even, which is what
/// Rust's `{:.N}` does when it formats; scaling first, as
/// `pyfmt::round_half_even` does, turns 99.65 (stored a hair above) into an
/// exact tie at 996.5 and rounds it down, where Python gives 99.7.
fn py_round(value: f64, ndigits: usize) -> f64 {
    format!("{value:.ndigits$}").parse().unwrap_or(value)
}

/// A megahertz reading, written the way the rest of the column is.
pub fn mhz(value: f64, decimals: usize) -> String {
    let rounded = py_round(value, decimals);
    let text = format!("{rounded:.decimals$}");
    let text = text.trim_end_matches('0');
    format!("{} MHz", text.trim_end_matches('.'))
}

/// IMC frequency: BCLK times the ratio times the QCLK multiplier.
pub fn get_imc_freq() -> String {
    let bclk = get_bclk();
    let ratio = rt(MCHBAR + 0x5E04, 0, 8);
    let raw_multiplier = rt(MCHBAR + 0x5E04, 8, 4);
    let multiplier = match raw_multiplier {
        Some(0) => Some(4.0 / 3.0),
        Some(1) => Some(1.0),
        _ => None,
    };
    match (bclk, ratio, multiplier) {
        (Some(bclk), Some(ratio), Some(multiplier)) => mhz(bclk * ratio as f64 * multiplier, MHZ_DECIMALS),
        _ => "Unknown".into(),
    }
}

/// The measured BCLK in MHz, or `None` where the Python returned "Error".
pub fn get_bclk() -> Option<f64> {
    rt(MCHBAR + 0x5F60, 0, 32).map(|khz| khz as f64 / 1000.0)
}

// BCLK is measured, and its last places wander from one start to the next:
// trimmed to whatever it happened to read, the row said "100 MHz" on one run
// and "99.976 MHz" on another. On DDR5 it is always written to two places.
pub const BCLK_DECIMALS: usize = 2;

pub fn get_bclk_rd(platform: Platform) -> String {
    let Some(bclk) = get_bclk() else {
        return "Error".into();
    };
    if ddr5_rows(platform) {
        return format!("{bclk:.BCLK_DECIMALS$} MHz");
    }
    mhz(bclk, MHZ_DECIMALS)
}

/// Fastest current logical-core clock from Windows' CPU counters.
fn core_clock_mhz() -> Option<f64> {
    crate::sensors::cpu_clocks::read_clocks().get("core_max").copied().filter(|value| *value > 0.0)
}

/// Current fastest core clock, without claiming an unreadable MSR.
pub fn get_core_clock() -> Option<String> {
    core_clock_mhz().map(|value| mhz(value, MHZ_DECIMALS))
}

/// Current fastest core clock divided by the measured bus clock.
pub fn get_core_ratio() -> Option<String> {
    let clock = core_clock_mhz()?;
    let bclk = get_bclk().filter(|bclk| *bclk > 0.0)?;
    Some(format!("{:.1} x", clock / bclk))
}

pub fn get_tx() -> String {
    match rt(MCHBAR + 0x5E00, 17, 10) {
        Some(raw) => format!("{:.3} V", raw as f64 / 200.0),
        None => "Error".into(),
    }
}

pub fn get_sa() -> String {
    match rt(MCHBAR + 0x591C, 8, 23) {
        Some(raw) => format!("{:.3} V", raw as f64 / 8192.0),
        None => "Error".into(),
    }
}

// Ring ratio sits at MCHBAR 0x5918[31:24] on Alder/Raptor Lake.  Core Ultra
// 200S keeps the identical field but relocates the block by 0x10000, so the
// legacy offset reads back 0 there.  Confirmed by diffing a full MCHBAR
// snapshot across a BIOS change: 0x15918[31:24] tracked 42 -> 39 exactly.
pub const LEGACY_RING_RATIO: (u64, u32, i32) = (0x5918, 24, 8);
pub const ARROW_LAKE_RING_RATIO: (u64, u32, i32) = (0x15918, 24, 8);

/// The confirmed ring/uncore ratio field for the active platform.
pub(crate) fn ring_ratio_value() -> Option<u64> {
    let (offset, bit_start, bit_length) =
        if is_arrow_lake_platform() { ARROW_LAKE_RING_RATIO } else { LEGACY_RING_RATIO };
    rt(MCHBAR + offset, bit_start, bit_length).filter(|ratio| *ratio > 0)
}

/// Ring/uncore multiplier read directly from the controller field.
pub fn get_uncore_ratio(platform: Platform) -> Option<String> {
    uncore_ratio_text(ring_ratio_value(), platform)
}

pub(crate) fn uncore_ratio_text(ratio: Option<u64>, platform: Platform) -> Option<String> {
    let ratio = ratio?;
    // An eight-bit whole number, so the ".0" said nothing. DDR5 drops it.
    if ddr5_rows(platform) {
        return Some(format!("{ratio} x"));
    }
    Some(format!("{:.1} x", ratio as f64))
}

/// Ring/uncore clock: the ring ratio multiplied by BCLK.
pub fn get_ring_freq(platform: Platform) -> String {
    let ratio = ring_ratio_value();
    let bclk_khz = rt(MCHBAR + 0x5F60, 0, 32);
    let (Some(ratio), Some(bclk_khz)) = (ratio, bclk_khz.filter(|khz| *khz != 0)) else {
        return "N/A".into();
    };
    let megahertz = (ratio * bclk_khz) as f64 / 1000.0;
    // The ratio times the measured BCLK, so it carries the same wander --
    // 5000 on one start, 4998.8 on the next. Whole MHz on DDR5.
    if ddr5_rows(platform) {
        return format!("{} MHz", round_int(megahertz));
    }
    mhz(megahertz, MHZ_DECIMALS)
}

/// Read the platform-native CMD_STRETCH encoding from SC_GS_CFG.
fn cmd_stretch_raw(base: Option<u64>) -> Option<u64> {
    // Alder/Raptor Lake expose a two-bit 0..3 encoding. Arrow Lake changed
    // this to a single bit: 0 = 1N and 1 = 2N.
    let bit_length = if is_arrow_lake_platform() { 1 } else { 2 };
    rt(mc(base) + 0xE088, 3, bit_length)
}

/// Read the extended N:1 ratio used by CMD_STRETCH encoding 3.
fn n_to_1_ratio(base: Option<u64>) -> Option<u64> {
    rt(mc(base) + 0xE088, 5, 3)
}

/// Decode CMD Stretch instead of displaying its zero-based register value.
pub fn get_cmd_stretch(base: Option<u64>) -> String {
    let Some(raw) = cmd_stretch_raw(base) else {
        return "Unknown".into();
    };
    if is_arrow_lake_platform() || raw < 3 {
        return (raw + 1).to_string();
    }
    match n_to_1_ratio(base) {
        None => "N:Unknown".into(),
        Some(ratio) => format!("N:{ratio}"),
    }
}

/// Return the active command rate from the native CMD_STRETCH encoding.
pub fn get_command_rate(base: Option<u64>) -> String {
    let Some(raw) = cmd_stretch_raw(base) else {
        return "Unknown".into();
    };
    if is_arrow_lake_platform() {
        return if raw == 0 { "1N".into() } else { "2N".into() };
    }
    match raw {
        0 => "1N".into(),
        1 => "2N".into(),
        2 => "3N".into(),
        3 => match n_to_1_ratio(base) {
            None => "N:Unknown".into(),
            Some(ratio) => format!("N:{ratio}"),
        },
        _ => "Unknown".into(),
    }
}

/// Return tRC as the active tRAS + tRP cycle total.
pub fn get_trc_value(base: Option<u64>) -> String {
    let mc = mc(base);
    let tras_bit_start = if is_arrow_lake_platform() { 13 } else { 10 };
    let tras = rt(mc + 0xE004, tras_bit_start, 9);
    let trp = rt(mc + 0xE000, 0, 8);
    match (tras, trp) {
        (Some(tras), Some(trp)) => (tras + trp).to_string(),
        _ => "Unknown".into(),
    }
}

fn twtr_value(bit_start: u32, base: Option<u64>) -> String {
    let mc = mc(base);
    let turnaround = rt(mc + 0xE014, bit_start, 7);
    let tcwl = rt(mc + 0xE070, 24, 7);
    // DDR5 uses the same 10-clock controller-to-JEDEC conversion on both
    // Raptor Lake/Z790 and Arrow Lake/Z890. DDR4 uses six clocks.
    let adjustment = if is_arrow_lake_platform() || detect_ddr_generation() == DdrGeneration::Ddr5 { 10 } else { 6 };
    match (turnaround, tcwl) {
        (Some(turnaround), Some(tcwl)) => (turnaround as i64 - tcwl as i64 - adjustment).to_string(),
        _ => "Error".into(),
    }
}

/// Return tWTR_L from the write-to-read same-bank-group timing.
pub fn get_twtr_l(base: Option<u64>) -> String {
    twtr_value(0, base)
}

/// Return tWTR_S from the write-to-read different-bank-group timing.
pub fn get_twtr_s(base: Option<u64>) -> String {
    twtr_value(9, base)
}

static ARROW_LAKE: Memo<bool> = Memo::new();

/// Detect Core Ultra 200S/Z890-style memory-controller register layout.
pub fn is_arrow_lake_platform() -> bool {
    ARROW_LAKE.get_or_init(|| {
        let cpu_name = get_cpu_name().to_uppercase();
        let board_name = get_motherboard_display().to_uppercase();
        let qclk_ratio = rt(MCHBAR + 0x13D10, 0, 8);
        let register_is_valid = qclk_ratio.is_some_and(|ratio| (1..=254).contains(&ratio));
        register_is_valid && (board_name.contains("Z890") || cpu_name.contains("CORE ULTRA"))
    })
}

pub fn get_dram_ratio_value() -> String {
    let ratio = if is_arrow_lake_platform() { rt(MCHBAR + 0x13D10, 0, 8) } else { rt(MCHBAR + 0x5E04, 0, 8) };
    match ratio {
        Some(ratio) => ratio.to_string(),
        None => "Unknown".into(),
    }
}

// 0x13D10 carries the DRAM ratio in its low byte and the gear immediately
// above it. Confirmed on a Z890 bench across a Gear 2 to Gear 4 BIOS change:
// the register went 0x00000084 -> 0x00000142, so bit 8 rose while the ratio
// byte halved from 132 to 66. That also settles what the bit is: had the
// ratio been a nine-bit field, bit 8 would be its top bit rather than a flag
// of its own, and at ratio 132 nothing distinguished the two readings.
// The memory PLL reference the Arrow Lake ratio counts in, and the one number
// the DRAM Frequency and QCLK Ratio rows share. It was written twice --
// 33.334 in get_speed and "133.33 MHz" in get_qclk_ratio -- for one fact: the
// ratio at 0x13D10 advances in quarter-reference steps, so 133.334 / 4 is the
// multiplier. Pinned by two gears against SMBIOS: 132 x 33.334 x 2 and
// 66 x 33.334 x 4 both land on 8800. Derived, since the selector register that
// carries it on earlier platforms reads zero here; see get_qclk_ratio.
pub const ARROW_LAKE_QCLK_REFERENCE_MHZ: f64 = 133.334;
pub const ARROW_LAKE_RATIO_STEPS_PER_REFERENCE: f64 = 4.0;

pub const ARROW_LAKE_GEAR_REGISTER: u64 = 0x13D10;
pub const ARROW_LAKE_GEAR_BIT: u32 = 8;

/// Gear as the Arrow Lake memory-controller register states it.
///
/// Two states, because that is what the encoding holds: DDR5 client parts
/// offer Gear 2 and Gear 4, and a single bit cannot say anything else. None
/// when the register will not answer, so a caller can tell "no reading" from
/// a reading it can use.
pub fn arrow_lake_gear() -> Option<u32> {
    let raw = rt(MCHBAR + ARROW_LAKE_GEAR_REGISTER, ARROW_LAKE_GEAR_BIT, 1)?;
    Some(if raw != 0 { 4 } else { 2 })
}

pub fn get_gear_mode_value() -> String {
    if is_arrow_lake_platform() {
        return match arrow_lake_gear() {
            None => "Unknown".into(),
            Some(gear) => gear.to_string(),
        };
    }
    match rt(MCHBAR + 0x5E04, 12, 2) {
        Some(raw) => lookup(formulas::GEAR_MODE_FORMULA, raw).unwrap_or("Unknown").to_string(),
        None => "Unknown".into(),
    }
}

// DDR5 transfers twice per clock, and the number this row shows is the
// transfer rate: 8000 on a kit whose clock (the MCLK row) is 4000 MHz. So it
// is MT/s, as the AM5 profile already says.
fn dram_rate_unit(platform: Platform) -> &'static str {
    if ddr5_rows(platform) { "MT/s" } else { "MHz" }
}

pub fn get_dram_frequency(platform: Platform) -> String {
    match parse_first_number(Some(&get_speed())) {
        Some(freq) => format!("{freq:.0} {}", dram_rate_unit(platform)),
        None => "Unknown".into(),
    }
}

pub fn get_mclk() -> String {
    match parse_first_number(Some(&get_speed())) {
        Some(freq) => format!("{:.0} MHz", freq / 2.0),
        None => "Unknown".into(),
    }
}

// The memory controller clock runs at MCLK divided by the active gear ratio,
// so Gear 2 at 4400 MCLK gives a 2200 UCLK.  Returning MCLK unchanged made
// every geared configuration report UCLK too high.
// Keyed by the row's display value, which is the bare gear number: the label
// beside it already says "Gear Mode", and "Gear Mode  Gear Mode 2" read the
// name twice.
pub const GEAR_DIVIDER: &[(&str, u32)] = &[("1", 1), ("2", 2), ("4", 4)];

pub fn get_uclk() -> String {
    let mclk = parse_first_number(Some(&get_mclk()));
    let gear = get_gear_mode_value();
    let divider = GEAR_DIVIDER.iter().find(|(key, _)| *key == gear).map(|(_, d)| *d);
    match (mclk, divider) {
        (Some(mclk), Some(divider)) => format!("{:.0} MHz", mclk / divider as f64),
        _ => "Unknown".into(),
    }
}

/// Report the memory PLL reference the QCLK ratio counts in.
///
/// This row has two states everywhere else in the tool -- 133.33 MHz or
/// 100.00 MHz -- selected by a bit. Arrow Lake used to return "33.33 MHz"
/// here, which is not one of them: 33.334 is the internal multiplier used by
/// get_speed(), and the reference is four times that. The row was reporting a
/// quarter of its own quantity.
///
/// It is derived rather than read, and says so. The selector this reads on
/// earlier platforms, 0x5E04[8:11], is dead silicon on Arrow Lake -- the whole
/// register reads zero, as does 0x5918, and neither has been found at a
/// relocated address. Deriving beats guessing here only because the arithmetic
/// is already pinned: get_speed() computes ratio x 33.334 x gear, and that
/// lands on exactly 8800 at two different gears with two different ratios
/// (132 in Gear 2, 66 in Gear 4), against an independent SMBIOS 8800 at both.
/// A 100 MHz reference cannot produce those ratios from 8800 at all.
///
/// Note that a reference tool reads this row as 133 MHz on this board by
/// taking bit 10 of the dead 0x5918 and mapping the resulting zero through
/// "0 means 133 MHz". Same number, no measurement behind it -- which is worth
/// knowing before treating its agreement as confirmation.
pub fn get_qclk_ratio() -> String {
    if is_arrow_lake_platform() {
        return format!("{ARROW_LAKE_QCLK_REFERENCE_MHZ:.2} MHz");
    }
    match rt(MCHBAR + 0x5E04, 8, 4) {
        Some(0) => "133.33 MHz".into(),
        Some(1) => "100.00 MHz".into(),
        Some(other) => other.to_string(),
        // str(None): what the Python printed for an unread selector.
        None => "None".into(),
    }
}

pub fn get_psf0_pll() -> String {
    match get_bclk() {
        Some(bclk) => format!("{:.2} MHz", bclk * 10.6666),
        None => "Unknown".into(),
    }
}

/// Derive active tWR from the platform-specific write-precharge and tCWL fields.
fn twr_from_controller(ddr_generation: DdrGeneration, base: Option<u64>) -> Option<i64> {
    let mc = mc(base);
    if is_arrow_lake_platform() {
        // Core Ultra 200S: tWRPRE is TC_PRE[42:33].
        let twr_pre = rt(mc + 0xE004, 1, 10)?;
        let tcwl = rt(mc + 0xE070, 24, 8)?;
        // DDR5 BL16 occupies 8 memory clocks. TC_PRE.tWRPRE starts at
        // the WRITE command, while tWR begins after the burst.
        let value = twr_pre as i64 - tcwl as i64 - 8;
        return (value > 0).then_some(value);
    }
    let twr_pre = rt(mc + 0xE004, 0, 10)?;
    let tcwl = rt(mc + 0xE070, 24, 7)?;

    // On Intel DDR4, TC_PRE.tWRPRE is the full WR-command-to-PRE delay.
    // DDR4 tWR begins after the BL8 write burst, so remove tCWL plus
    // BL/2 (8 transfers / 2 = 4 clocks) to obtain the value shown by
    // runtime timing tools such as HWiNFO.
    let value = match ddr_generation {
        DdrGeneration::Ddr4 => twr_pre as i64 - tcwl as i64 - 4,
        DdrGeneration::Ddr5 => twr_pre as i64 - tcwl as i64 - 8,
        DdrGeneration::Unknown => twr_pre as i64 - tcwl as i64,
    };
    (value > 0).then_some(value)
}

/// Fallback for DDR5 systems whose controller-derived tWR is unavailable.
fn twr_ddr5_mr_fallback(base: Option<u64>) -> Option<String> {
    for offset_start in [0xE600u64, 0xE000] {
        for command in 0..4u64 {
            let raw = read_timing(&ReadSpec::Dynamic(DynamicParams {
                offset_start,
                value_to_find: 0x06,
                offset_base: 0xE200,
                bit_start_dynamic: 0,
                bit_length_dynamic: 4,
                mchbar: mc(base),
                command,
                offset2: 0,
            }));
            if let Some(raw) = raw
                && let Some(mapped) = lookup(formulas::TWR_FORMULA, raw & 0xF) {
                    return Some(mapped.to_string());
                }
        }
    }
    None
}

/// Auto-detect DDR4/DDR5 and return the active tWR timing.
///
/// The controller value wins over the mode register, and on DDR5 the two can
/// legitimately disagree -- so this row will not always match a tool that
/// reads MR0, and that is not a defect to correct.
///
/// DDR5 encodes nWR in MR0 in steps of four: 48, 52, 56 and so on. A
/// configured tWR that is not on that grid cannot be stated there, so the
/// DRAM is programmed to the nearest encodable value while the controller
/// enforces the real one through TC_PRE.tWRPRE.
///
/// Measured on a Z890 bench at DDR5-8800, all four readings from the same
/// boot: tWRPRE 92, tCWL 34, giving 92 - 34 - 8 = 50, which is what BIOS was
/// set to. MR0 read 48, being the nearest step below, and the reference timing tool --
/// which reports the mode register here -- showed 48 as well. Reading 50 as
/// two clocks of error and "fixing" the burst constant to 10 would reproduce
/// the DRAM-side number and lose the configured one.
///
/// tRTP is the useful contrast: it is not quantized this way, and its
/// controller and mode-register readings agree exactly on the same bench.
pub fn get_twr_value(base: Option<u64>) -> String {
    let ddr_generation = detect_ddr_generation();
    let controller_value = twr_from_controller(ddr_generation, base);

    // The controller-derived value is the configured timing and works for both DDR4 and DDR5.
    if let Some(value) = controller_value {
        let in_range = match ddr_generation {
            DdrGeneration::Ddr4 => (4..=64).contains(&value),
            DdrGeneration::Ddr5 => (16..=192).contains(&value),
            DdrGeneration::Unknown => (4..=192).contains(&value),
        };
        if in_range {
            return value.to_string();
        }
    }

    // Only use the existing MR-table decoder on DDR5. It is not a DDR4 decoder.
    if ddr_generation == DdrGeneration::Ddr5
        && let Some(value) = twr_ddr5_mr_fallback(base) {
            return value;
        }
    "Unknown".into()
}

// MCHBAR 0xE440[31:24] is the Alder/Raptor Lake tMOD field and reads back 0
// on Core Ultra 200S.  Three BIOS values (8, 31, 20) were captured as full
// MCHBAR snapshots: no field anywhere in 0x0-0x20000 tracks the setting, as
// a raw value, with a constant offset, or scaled.  Report N/A rather than a
// coincidental match -- 0x0E430 and 0x079C both matched one transition and
// failed the next.
pub const LEGACY_TMOD: (u64, u32, i32) = (0xE440, 24, 8);

/// Return the live tMOD value from the memory controller.
pub fn get_tmod_value() -> String {
    let (offset, bit_start, bit_length) = LEGACY_TMOD;
    match rt(MCHBAR + offset, bit_start, bit_length) {
        None | Some(0) => "N/A".into(),
        Some(value) => value.to_string(),
    }
}

/// Return a live column-to-column timing in DRAM tCK cycles, or `None` where
/// the Python returned "Unknown".
fn bank_group_timing(register_offset: u64, bit_start: u32, base: Option<u64>) -> Option<u64> {
    let value = rt(mc(base) + register_offset, bit_start, 7)?;
    (1..=127).contains(&value).then_some(value)
}

// timing name -> (register offset, bit start).
//
// Empty pending hardware confirmation.
//
// TC_RDRD.sg and TC_WRWR.sg were used here, on the strength of the controller
// reading 8 and 4 while the BIOS reported tCCD_L 8 and tCCD 4 with both on
// Auto. A controlled change refuted that: setting tCCD 6 and tCCD_L 10 and
// rebooting left TC_RDRD at 0x08080488 and TC_WRWR at 0x08080408, byte for
// byte, on both controllers. Two defaults had coincided with two register
// fields, which is not the same as the fields carrying those settings.
//
// The scheduler fields are still read and still correct as what they are: they
// appear on the Timings tab as tRDRD_sg, tRDRD_dg and tWRWR_sg, which is the delay
// the controller actually applies. Only the claim that they *are* tCCD and
// tCCD_L was wrong, so only that claim was withdrawn.
pub const CCD_CONFIRMED_FIELDS: &[(&str, (u64, u32))] = &[];

// No candidate either, and the search is finished rather than merely stalled.
//
// 0xE08C looked convincing at tCCD 6 / tCCD_L 10: it read 0x04008A06 on both
// controllers, putting 6 at bits 0-6 and 10 at bits 8-14, and it was the only
// register in the swept window holding both values. A second controlled change
// to tCCD 8 / tCCD_L 14 left it at 0x04008A06, byte for byte. It had simply
// always held those numbers.
//
// That second change also settled the wider question. Diffing full snapshots of
// both controllers across the reboot, 7 registers of 768 differed: 0xE034 and
// 0xE040 drift between back-to-back reads at fixed settings, so they are live
// counters rather than configuration, and 0xE474 and 0xE4B0 are stable within a
// boot but carry no field moving 6->8 or 10->14, which is what per-boot
// training variance looks like. Sweeping the whole 0x0000-0x10000 window on
// both controllers found no register anywhere holding 8 and 14 in byte-aligned
// fields.
//
// The physical argument agrees: TC_RDRD.sg is the delay the controller applies
// between same-bank-group reads, and it stayed at 8 across both changes. It
// could not, had the DRAM actually been running tCCD_L 14.
//
// So this board's BIOS accepts the setting without programming it into the
// memory controller, and there is nothing to read. These rows report nothing
// because nothing carries the value, not because the search was abandoned.
pub const CCD_CANDIDATE_FIELDS: &[(&str, (u64, u32))] = &[];

// --- DDR5 mode-register shadow.
//
// The controller keeps the mode registers it programmed into the DIMMs in a
// table here, one dword each: the MR number in bits 8-15 and the value it
// wrote in bits 0-7. Confirmed on the Z790 bench, where the table runs MR0
// upward from 0xE600 and both controllers hold identical copies.
//
// This is where tCCD_L lives, and why the earlier sweep for it failed. That
// search swept both controllers for a field holding the cycle count across a
// controlled BIOS change. The DRAM does not store a cycle count: MR13 holds a
// 4-bit JEDEC code. A search for the count could never have found the code,
// so the conclusion that nothing carried the value came from searching for
// the wrong representation rather than from the value being absent.
pub const MODE_REGISTER_TABLE_START: u64 = 0xE600;
pub const MODE_REGISTER_TABLE_END: u64 = 0xE800;
pub const MODE_REGISTER_ENTRY_PREFIX: u64 = 0x8000;
pub const MODE_REGISTER_PAYLOAD_BASE: u64 = 0xE200;

/// Return one DDR5 mode register's value, or `None` when it is not listed.
///
/// An entry does not hold the value. Its data byte names the mode register
/// and its index byte is an offset into the payload array at 0xE200, which
/// is where the contents are. This read matched on the index instead and
/// returned the data byte -- for MR13 that gave 2 where the register holds
/// 8, and tCCD_L came out 10 rather than 16.
///
/// Two independent checks say this is the right way round. MR0 resolves to
/// 0x1C, which decodes to BL16 and CL 36, and 36 is exactly the tCL read
/// from the timing registers. And the reference map spells the lookup out:
/// the tCCD block is written as search_value "0D" with base_address
/// (mchbar)E200, which is a search on the data byte followed by the index.
pub fn read_mode_register(number: u64, base: Option<u64>) -> Option<u64> {
    let start = mc(base);
    let mut offset = MODE_REGISTER_TABLE_START;
    while offset < MODE_REGISTER_TABLE_END {
        let address = start + offset;
        offset += 4;
        let Some(entry) = rt(address, 0, 32) else {
            continue;
        };
        let entry = entry & 0xFFFF_FFFF;
        if (entry >> 16) & 0xFFFF != MODE_REGISTER_ENTRY_PREFIX {
            continue;
        }
        if entry & 0xFF != number & 0xFF {
            continue;
        }
        let payload = rt(start + MODE_REGISTER_PAYLOAD_BASE + ((entry >> 8) & 0xFF), 0, 8);
        return payload.map(|payload| payload & 0xFF);
    }
    None
}

// MR13 carries tCCD_L in its low nibble, and the write and second-write
// variants are fixed multiples of it. JESD79-5 encodes them as:
//
//   tCCD_L      = 8  + code   (8 to 22 nCK)
//   tCCD_L_WR   = 32 + 4*code (four times tCCD_L; the 20 ns write-to-write)
//   tCCD_L_WR2  = 16 + 2*code (twice tCCD_L; the 10 ns second write)
//
// The two write rows were once the other way round, which a bench reading of
// 0x02 could not show: 10 / 20 / 40 names 20 and 40 either way. At DDR5-8000
// on the Z790 MPOWER tCCD_L is 20, and the reference tool reads tCCD_L_WR 80
// and tCCD_L_WR2 40 on the same boot -- four and two times, as above. tCCD_L
// itself agrees with the controller's applied delay: TC_RDRD.sg has to be at
// least tCCD_L.
// Code 15 is reserved rather than a timing, so it reports nothing.
// --- DFE, from the same mode-register table.
//
// The table's entries carry a data byte, a pointer and a command code. A DFE
// tap is found by its data byte -- 0x81 through 0x84 for taps 1 to 4 -- and
// the entry then points at a byte holding all three of its settings: enable at
// bit 7, sign at bit 6, magnitude in bits 0-5.
//
// Read together rather than as three lookups. A tap's bias is a signed number
// that happens to be stored as two fields, and searching the table three times
// to reassemble one value would also let the sign and the magnitude come from
// different reads.
pub const DFE_TAP_DATA_BASE: u64 = 0x80;
pub const DFE_TAP_COUNT: u64 = 4;
pub const DFE_ENABLE_BIT: u32 = 7;
pub const DFE_SIGN_BIT: u32 = 6;
pub const DFE_BIAS_BITS: u32 = 6;

static MODE_REGISTER_TABLE_POPULATED: MemoMap<u64, bool> = MemoMap::new();

/// True when the table has been filled in at all.
///
/// A platform that never programs it -- DDR4 here -- leaves all 128 entries
/// reading zero, and a zero entry is a perfect match for register 0 with
/// command 0 pointing at payload index 0. So a search for MR0 succeeds
/// against a table that holds nothing, and Burst Length reported BL16: a
/// code DDR4 does not have, read out of an empty register. Every other Misc
/// row asks for a non-zero register and correctly found nothing, which is
/// why that one row was the only one that lied.
///
/// Cached, but only once something could actually be read -- called before
/// the driver is up it would otherwise cache "empty" for the session.
fn mode_register_table_populated(start: u64) -> bool {
    if let Some(cached) = MODE_REGISTER_TABLE_POPULATED.get(&start) {
        return cached;
    }
    let mut readable = false;
    let mut offset = MODE_REGISTER_TABLE_START;
    while offset < MODE_REGISTER_TABLE_END {
        let value = rt(start + offset, 0, 32);
        offset += 4;
        let Some(value) = value else {
            continue;
        };
        readable = true;
        if value & 0xFFFF_FFFF != 0 {
            MODE_REGISTER_TABLE_POPULATED.insert(start, true);
            return true;
        }
    }
    if readable {
        MODE_REGISTER_TABLE_POPULATED.insert(start, false);
    }
    false
}

/// Find a table entry by its data byte and return where it points.
pub(crate) fn mode_register_pointer(data_byte: u64, command: u64, offset_base: u64, base: Option<u64>) -> Option<u64> {
    let start = mc(base);
    if !mode_register_table_populated(start) {
        return None;
    }
    let mut offset = MODE_REGISTER_TABLE_START;
    while offset < MODE_REGISTER_TABLE_END {
        let value = rt(start + offset, 0, 32);
        offset += 4;
        let Some(value) = value else {
            continue;
        };
        let value = value & 0xFFFF_FFFF;
        if value & 0xFF != (data_byte & 0xFF) {
            continue;
        }
        if (value >> 22) & 0x3 != command {
            continue;
        }
        return Some(start + offset_base + ((value >> 8) & 0xFF));
    }
    None
}

/// Return `(enabled, bias)` for one DFE tap, or `None`.
fn read_dfe_tap(tap: u64, base: Option<u64>) -> Option<(bool, i64)> {
    let address = mode_register_pointer(DFE_TAP_DATA_BASE + tap, 0, MODE_REGISTER_PAYLOAD_BASE, base)?;
    let raw = rt(address, 0, 8)? & 0xFF;
    let magnitude = (raw & ((1 << DFE_BIAS_BITS) - 1)) as i64;
    let bias = if (raw >> DFE_SIGN_BIT) & 1 != 0 { -magnitude } else { magnitude };
    Some(((raw >> DFE_ENABLE_BIT) & 1 != 0, bias))
}

/// "Enable" or "Disable" for one tap, or `None` when the table has no entry.
pub fn get_dfe_enable(tap: u64, base: Option<u64>) -> Option<String> {
    read_dfe_tap(tap, base).map(|(enabled, _)| if enabled { "Enable".into() } else { "Disable".into() })
}

/// The tap's signed bias, written the way a sign-and-magnitude field reads.
pub fn get_dfe_bias(tap: u64, base: Option<u64>) -> Option<String> {
    read_dfe_tap(tap, base).map(|(_, bias)| format!("{bias:+}"))
}

// --- DDR4 mode-register shadow.
//
// DDR4 never fills the 0xE600 table, so every row that reads a mode register
// through it reports nothing. The registers are not missing, though: the
// controller keeps what it last wrote to the DRAM at MCHBAR + 0xE5A0, eight
// registers packed two to a dword, low half first.
//
// Found by following 0xE5AC -- the register the DDR4 DQ VREF row already read
// -- and asking whether its neighbours decode as MR0 through MR7. Four checks
// say they do, each against a number that comes from somewhere else entirely:
//
//   MR0 CL code 8       -> CL 18, and the controller's tCL register reads 18
//   MR0 WR/RTP code 6   -> WR 24, and the tWR row reads 24
//   MR3 FGR code 0      -> normal refresh, which is what Refresh Mode shows
//   MR6 tCCD_L code 4   -> 8, which is what the BIOS shows for tCCD_L
//
// and the two controllers agree on every register except MR6, where they read
// 0x101C against 0x101B: a per-DIMM trained VrefDQ. That is the one field that
// should differ between two sticks, and nothing a static copy could produce.
//
// MR4 is deliberately not read here. Only bit 3 is set on this bench, which
// is not enough to tell which field it is, so the three preamble rows keep
// reporting nothing until a controlled BIOS change says which bit moves.
pub const DDR4_MODE_REGISTER_BASE: u64 = 0xE5A0;
pub const DDR4_MODE_REGISTER_COUNT: u64 = 8;

/// One DDR4 mode register out of the shadow, or `None`.
pub(crate) fn ddr4_mode_register(number: u64, base: Option<u64>) -> Option<u64> {
    if number >= DDR4_MODE_REGISTER_COUNT {
        return None;
    }
    let start = mc(base);
    let offset = DDR4_MODE_REGISTER_BASE + (number / 2) * 4;
    let raw = read_physical_memory_int(start + offset, 4)? & 0xFFFF_FFFF;
    if raw == 0xFFFF_FFFF {
        return None;
    }
    Some((raw >> (16 * (number % 2))) & 0xFFFF)
}

/// One field out of the DDR4 shadow, or `None` when it cannot be read.
pub(crate) fn ddr4_mode_register_field(number: u64, bit_start: u32, bit_length: u32, base: Option<u64>) -> Option<u64> {
    let value = ddr4_mode_register(number, base)?;
    Some((value >> bit_start) & ((1 << bit_length) - 1))
}

// MR6 A[12:10]. Codes above 4 are not defined.
pub const DDR4_TCCD_L: &[(u64, u64)] = &[(0, 4), (1, 5), (2, 6), (3, 7), (4, 8)];
pub const DDR4_TCCD_L_MODE_REGISTER: u64 = 0x06;
pub const DDR4_TCCD_L_BITS: (u32, u32) = (10, 3);

// tCCD_S, the different-bank-group delay, keyed on the burst length in MR0.
//
// JESD79-4 fixes it at 4 nCK and gives no way to program it, which is why an
// earlier controlled experiment found nothing: tCCD was set to 6 and then 8 in
// the BIOS and no register in the swept window moved, because on DDR4 the DRAM
// ignores the setting. There was never a field to find. The BIOS shows 4 with
// the setting on Auto, and 4 is what every burst mode gives.
//
// So this is a constant, and it is written as a table keyed on a real register
// read rather than as a bare 4. That is the whole difference: the row appears
// only when MR0 can actually be read and names a burst mode this generation
// defines. A shadow that cannot be read, or a code that is not DDR4's, leaves
// the row empty instead of asserting a number about a module nobody reached.
pub const DDR4_TCCD_S: &[(u64, u64)] = &[
    (0, 4), // BL8 fixed
    (1, 4), // BC4 or BL8 on the fly
    (2, 4), // BC4 fixed
];
pub const DDR4_TCCD_S_MODE_REGISTER: u64 = 0x00;
pub const DDR4_TCCD_S_BITS: (u32, u32) = (0, 2);

fn pair_lookup(table: &[(u64, u64)], code: u64) -> Option<u64> {
    table.iter().find(|(key, _)| *key == code).map(|(_, value)| *value)
}

/// tCCD_L out of the DDR4 MR6 shadow, or `None`.
pub(crate) fn ddr4_ccd_l(base: Option<u64>) -> Option<u64> {
    let code = ddr4_mode_register_field(DDR4_TCCD_L_MODE_REGISTER, DDR4_TCCD_L_BITS.0, DDR4_TCCD_L_BITS.1, base)?;
    pair_lookup(DDR4_TCCD_L, code)
}

/// tCCD_S, from the burst length MR0 reports. See DDR4_TCCD_S.
pub(crate) fn ddr4_ccd_s(base: Option<u64>) -> Option<u64> {
    ddr4_ccd_s_from(ddr4_mode_register_field(DDR4_TCCD_S_MODE_REGISTER, DDR4_TCCD_S_BITS.0, DDR4_TCCD_S_BITS.1, base))
}

pub(crate) fn ddr4_ccd_s_from(code: Option<u64>) -> Option<u64> {
    pair_lookup(DDR4_TCCD_S, code?)
}

// DDR5's tCCD_S, the same way: JESD79-5 fixes it at half the burst length and
// gives no field to program, so it is keyed on the burst mode MR0 reports --
// 8 nCK for BL16 and BC8, 16 for BL32. BL32 on the fly chooses per command, so
// no one number describes it and the row stays empty.
pub const DDR5_TCCD_S: &[(u64, u64)] = &[
    (0, 8),  // BL16
    (1, 8),  // BC8 OTF
    (2, 16), // BL32
];
pub const DDR5_BURST_LENGTH_MODE_REGISTER: u64 = 0x00;

/// tCCD_S, from the burst length DDR5 MR0 reports. See DDR5_TCCD_S.
pub(crate) fn ddr5_ccd_s(base: Option<u64>) -> Option<u64> {
    ddr5_ccd_s_from(read_mode_register(DDR5_BURST_LENGTH_MODE_REGISTER, base))
}

pub(crate) fn ddr5_ccd_s_from(mr0: Option<u64>) -> Option<u64> {
    pair_lookup(DDR5_TCCD_S, mr0? & 0x03)
}

pub const CCD_MODE_REGISTER: u64 = 0x0D;
pub const CCD_CODE_RESERVED: u64 = 0x0F;
pub const CCD_FROM_MR13: &[(&str, (u64, u64))] =
    &[("tCCD_L", (8, 1)), ("tCCD_L_WR", (32, 4)), ("tCCD_L_WR2", (16, 2))];

// tDLLK comes from the same MR13 nibble tCCD_L does, under a different
// formula: the lock time steps every second code rather than every code, so
// 8 and 9 both give 2048. Taken from the reference tools' table -- 1024,
// 1024, 1280, 1280 ... 2816 -- which is 1024 + 256 per pair, with the
// reserved code at the top shared with tCCD_L.
pub const DLLK_BASE_CYCLES: u64 = 1024;
pub const DLLK_STEP_CYCLES: u64 = 256;

/// DLL lock time in cycles, or `None` when MR13 holds the reserved code.
pub fn get_dllk_timing(base: Option<u64>) -> Option<u64> {
    let raw = read_mode_register(CCD_MODE_REGISTER, base)?;
    let code = raw & 0x0F;
    if code == CCD_CODE_RESERVED {
        return None;
    }
    Some(DLLK_BASE_CYCLES + DLLK_STEP_CYCLES * (code / 2))
}

/// Return a column-to-column timing, or `None` when nothing carries it.
pub fn get_ccd_timing(name: &str, base: Option<u64>) -> Option<u64> {
    ccd_timing_with(name, base, CCD_CONFIRMED_FIELDS)
}

/// [`get_ccd_timing`] against a given confirmed-field map, which is how the
/// tests drive the path a confirmed field would take.
pub(crate) fn ccd_timing_with(name: &str, base: Option<u64>, confirmed: &[(&str, (u64, u32))]) -> Option<u64> {
    // DDR4 carries tCCD_L in MR6 rather than MR13, and has no register for
    // the write variants at all -- those two stay blank rather than being
    // derived from a DDR5 formula that has nothing behind it here.
    if detect_ddr_generation() == DdrGeneration::Ddr4 {
        return match name {
            "tCCD_L" => ddr4_ccd_l(base),
            "tCCD" => ddr4_ccd_s(base),
            _ => None,
        };
    }

    if let Some((_, (offset, step))) = CCD_FROM_MR13.iter().find(|(n, _)| *n == name) {
        let raw = read_mode_register(CCD_MODE_REGISTER, base)?;
        let code = raw & 0x0F;
        if code == CCD_CODE_RESERVED {
            return None;
        }
        return Some(offset + step * code);
    }

    match confirmed.iter().find(|(n, _)| *n == name) {
        None => {
            if name == "tCCD" {
                ddr5_ccd_s(base)
            } else {
                None
            }
        }
        Some((_, (offset, bit_start))) => bank_group_timing(*offset, *bit_start, base),
    }
}

/// Read the DDR4 MR6 VREFDQ shadow and return its programmed percentage.
///
/// Raptor Lake DDR4 exposes MR6 in the channel MR shadow at MCHBAR + 0xE5AC.
/// The trained value is bits [5:0] and the range selector is bit 7.
///
/// Bit 12 was read as the range selector for a while, which put this row at
/// 63.20% instead of 78.20%. Bit 12 is the top of tCCD_L: bits [12:10] read
/// 4 here, and 4 decodes to tCCD_L 8, which is exactly what the BIOS shows.
/// A range bit at 12 would have left tCCD_L at [11:10] = 0, meaning 4, and
/// the BIOS says 8 -- so bit 12 belongs to tCCD_L and the range selector is
/// the JEDEC one. Both bit 6 and bit 7 read 0 here, so the value is the same
/// whichever of the two conventions MR6 is written under.
pub fn decode_ddr4_dq_vref() -> String {
    for channel_base in [MCHBAR, MCHBAR2] {
        let Some(raw) = read_physical_memory_int(channel_base + 0xE5AC, 4) else {
            continue;
        };
        if raw == 0 {
            continue;
        }
        let value_code = raw & 0x3F;
        let range_two = (raw >> 7) & 0x1;
        if value_code > 50 {
            continue;
        }
        let base_percent = if range_two != 0 { 45.0 } else { 60.0 };
        let percent = base_percent + (value_code as f64 * 0.65);
        if (45.0..=92.5).contains(&percent) {
            return format!("{percent:.2}%");
        }
    }
    "N/A".into()
}

/// Report who owns the CKE power-down policy, from DDR_PTM_CTL[6].
///
/// The register is MCHBAR + 0x5880. Bit set means BIOS owns the policy; bit
/// clear means P-code manages it dynamically, which is reported as Automatic.
/// An unreadable register or an all-ones read gives N/A rather than a default.
///
/// TC_PWRDN/tPPD is not the place to look for this: it carries exit and
/// command timing values, not an enable flag. That is worth stating because it
/// is the register the name suggests.
///
/// Two things this does not know, kept here so the row is not read as more
/// than it is. Reporting bit 6 set as "Disabled" treats "BIOS owns the policy"
/// as "BIOS turned it off", which is an inference rather than something the
/// bit encodes -- BIOS owning the policy and BIOS enabling power-down are not
/// contradictory. And no BIOS change has been observed against this row on any
/// bench here, so neither state has been watched to follow a setting.
///
/// An earlier version of this docstring recorded what the calibrated Z690,
/// Z790 and Z890 configurations were expected to read. That has been removed:
/// naming the answer for the boards under test makes a wrong reading look
/// confirmed, which is the failure this row is most exposed to.
pub fn get_power_down_mode_value() -> String {
    match read_ddr_ptm_control() {
        None => "N/A".into(),
        Some(raw) => {
            let bios_controls_power_down = raw & (1 << 6) != 0;
            if bios_controls_power_down { "Disabled".into() } else { "Automatic".into() }
        }
    }
}

// --- Gear mode has a second witness.
//
// get_gear_mode_value() above reads MCHBAR + 0x5E04 bits 12-13. The scheduler
// register SC_GS_CFG at 0xE088 carries the same fact as two separate flags,
// and those two bits were placed by measurement on the LGA1700 DDR5 bench --
// a Gear 2 -> Gear 4 -> Gear 2 round trip, snapshotting the whole of MCHBAR
// at each step:
//
//     Gear 2   0x8FC00029     bit 15 = 0   bit 31 = 1
//     Gear 4   0x0FC08029     bit 15 = 1   bit 31 = 0
//     Gear 2   0x8FC00029     bit 15 = 0   bit 31 = 1
//
// Both transitions moved exactly those two bits and nothing else in the
// register, and it came back to its original value while 928 registers
// elsewhere did not survive the round trip unchanged. That is what places
// them rather than fits them. Worth recording that the obvious candidate
// beforehand was wrong: bit 0 is the only set bit below the known fields and
// it did not move.
//
// This is a check on the gear row, not a second way to show it. A duplicate
// row would print the same wrong answer twice if the field at 0x5E04 ever
// moved; two witnesses disagree instead, and the test says so.
pub const SCHEDULER_CONFIG_OFFSET: u64 = 0xE088;
pub const SCHEDULER_GEAR4_BIT: u32 = 15;
pub const SCHEDULER_GEAR2_BIT: u32 = 31;

// Arrow Lake states the gear in SC_GS_CFG differently: one flag rather than
// two, set for Gear 4 and clear for Gear 2. Measured on a Z890 bench across a
// Gear 2 to Gear 4 BIOS change with nothing else altered -- 0xE088 went
// 0x00000009 to 0x80000009, so bit 31 rose while bit 15 stayed clear.
//
// Reading it through the Raptor Lake pair was worse than useless there. In
// Gear 2 both flags read 0, so the witness returned None and the cross-check
// skipped. In Gear 4 bit 31 alone rose, which the pair reads as "gear2 set,
// gear4 clear" -- so the witness reported Gear 2 against a board in Gear 4,
// contradicting a row that was right.
pub const ARROW_LAKE_SCHEDULER_GEAR4_BIT: u32 = 31;

/// The gear as SC_GS_CFG reports it, or `None` when it cannot say.
///
/// None rather than a guess when neither flag is set or both are: the whole
/// value of a second witness is that it stays silent instead of agreeing by
/// construction.
pub fn scheduler_gear_mode(base: Option<u64>) -> Option<u32> {
    scheduler_gear_from(read_physical_memory_int(mc(base) + SCHEDULER_CONFIG_OFFSET, 4), is_arrow_lake_platform())
}

pub(crate) fn scheduler_gear_from(raw: Option<u64>, arrow_lake: bool) -> Option<u32> {
    let raw = raw?;
    if raw == 0xFFFF_FFFF {
        return None;
    }
    if arrow_lake {
        return Some(if (raw >> ARROW_LAKE_SCHEDULER_GEAR4_BIT) & 1 != 0 { 4 } else { 2 });
    }
    let gear2 = (raw >> SCHEDULER_GEAR2_BIT) & 1;
    let gear4 = (raw >> SCHEDULER_GEAR4_BIT) & 1;
    if gear2 == gear4 {
        return None;
    }
    Some(if gear4 != 0 { 4 } else { 2 })
}

/// Read Intel DDR power/thermal-management control when exposed.
fn read_ddr_ptm_control() -> Option<u64> {
    let raw = read_physical_memory_int(MCHBAR + 0x5880, 4)?;
    if raw == 0xFFFF_FFFF { None } else { Some(raw) }
}

// Refresh policy, DDR_PTM_CTL[3:2].
pub const REFRESH_MODE_NORMAL: u64 = 0;

// Labels are kept short on purpose. The row sits in a section whose other rows
// are per-channel, so its text lands in the narrow A1 column, and
// _align_dual_columns widens that column to its longest entry across every
// section on the tab. "Normal" costs the same width as the "144 ns" already
// there; the wordier original cost three times that. The 2x labels are longer,
// but they only appear when refresh is not on its default schedule, which is
// exactly when the row should draw the eye.
pub const REFRESH_MODE_LABELS: Table = &[
    (REFRESH_MODE_NORMAL, "Normal"),
    (1, "2x (Warm/Hot)"),
    (2, "2x (Hot)"),
    (3, "Invalid"),
];

/// Return the DDR_PTM_CTL[3:2] refresh policy code, or `None`.
fn ddr4_refresh_mode_code() -> Option<u64> {
    if detect_ddr_generation() != DdrGeneration::Ddr4 {
        return None;
    }
    let raw = read_ddr_ptm_control()?;
    Some((raw >> 2) & 0x3)
}

/// Decode the live DDR4 refresh policy.
pub fn get_ddr4_refresh_mode_value() -> String {
    match ddr4_refresh_mode_code() {
        None => "N/A".into(),
        Some(code) => lookup(REFRESH_MODE_LABELS, code).unwrap_or("N/A").to_string(),
    }
}

/// True when refresh is running its normal all-bank tRFC schedule.
///
/// Read from the same register the Refresh Mode row displays, so a dimmed row
/// and the mode above it can never disagree. Unknown counts as not normal:
/// dimming a row the hardware might well be using would be the worse mistake.
pub fn refresh_is_normal() -> bool {
    ddr4_refresh_mode_code() == Some(REFRESH_MODE_NORMAL)
}

/// Report system-memory error correction from SMBIOS, with DIMM-width fallback.
pub fn get_ecc_error_correction_value() -> String {
    use crate::wmi_util::{WmiObject, WmiValue, static_class};
    const CORRECTION_NAMES: Table =
        &[(3, "Disabled"), (4, "Parity"), (5, "Single-bit ECC"), (6, "Multi-bit ECC"), (7, "CRC")];
    // `int(getattr(obj, field, 0) or 0)`, or `None` where int() would raise.
    let int_field = |object: &WmiObject, field: &str| -> Option<i64> {
        let truthy = match object.get(field) {
            None | Some(WmiValue::Null) => false,
            Some(WmiValue::Str(text)) => !text.is_empty(),
            Some(WmiValue::Int(0)) | Some(WmiValue::UInt(0)) | Some(WmiValue::Bool(false)) => false,
            Some(_) => true,
        };
        if !truthy {
            return Some(0);
        }
        object.int(field)
    };
    for array in static_class("Win32_PhysicalMemoryArray").iter() {
        let Some(code) = int_field(array, "MemoryErrorCorrection") else {
            continue;
        };
        if let Some(name) = u64::try_from(code).ok().and_then(|code| lookup(CORRECTION_NAMES, code)) {
            return name.into();
        }
    }
    let mut saw_width = false;
    for memory in static_class("Win32_PhysicalMemory").iter() {
        let (Some(total_width), Some(data_width)) = (int_field(memory, "TotalWidth"), int_field(memory, "DataWidth"))
        else {
            continue;
        };
        if total_width > 0 && data_width > 0 {
            saw_width = true;
            if total_width > data_width {
                return "Enabled".into();
            }
        }
    }
    if saw_width {
        return "Disabled".into();
    }
    "N/A".into()
}

/// Report whether a valid self-refresh timing block is programmed.
pub fn get_self_refresh_value() -> String {
    // Raptor/Alder Lake TC_SRFTP is at MCHBAR + E440. A valid non-zero
    // timing block means the controller has self-refresh operation configured.
    if is_arrow_lake_platform() {
        return "N/A".into();
    }
    match read_physical_memory_int(MCHBAR + 0xE440, 4) {
        None | Some(0xFFFF_FFFF) => "N/A".into(),
        Some(0) => "Disabled".into(),
        Some(_) => "Enabled".into(),
    }
}

/// Do not claim a scrambler state without a verified live status source.
pub fn get_memory_scrambler_value() -> String {
    "Not Exposed".into()
}

/// Do not label DDR refresh policy as a verified row-hammer state.
pub fn get_row_hammer_value() -> String {
    "Not Exposed".into()
}

/// The DRAM transfer rate as the controller is set up, "8000.0 MHz".
pub fn get_speed() -> String {
    if is_arrow_lake_platform() {
        let Some(ratio) = rt(MCHBAR + 0x13D10, 0, 8) else {
            return "Unknown".into();
        };
        // Core Ultra 200S MemSS PMA reports a 33.334 MHz QCLK reference,
        // and the ratio counts QCLK steps -- so the gear multiplies it,
        // exactly as gear_divider does on the pre-Arrow-Lake path below.
        //
        // Measured on a Z890 bench running DDR5-8800 through a Gear 2 to
        // Gear 4 BIOS change, with nothing else altered:
        //
        //   Gear 2   0x13D10 = 0x00000084   ratio 132   132 x 33.334 x 2
        //   Gear 4   0x13D10 = 0x00000142   ratio  66    66 x 33.334 x 4
        //
        // Both give 8800, which is what SMBIOS reports at both settings.
        // The ratio halves because the gear doubles; a fixed multiplier
        // of 2.0 was the Gear 2 case written in, and it reported every
        // memory clock at half its real value in Gear 4.
        let Some(gear) = arrow_lake_gear() else {
            return "Unknown".into();
        };
        let effective_rate =
            ratio as f64 * ARROW_LAKE_QCLK_REFERENCE_MHZ / ARROW_LAKE_RATIO_STEPS_PER_REFERENCE * gear as f64;
        return format!("{} MHz", float_str(round_half_even(effective_rate, 0)));
    }

    let Some(bclk) = get_bclk() else {
        return "Unknown".into();
    };
    let ratio = rt(MCHBAR + 0x5E04, 0, 8);
    let raw_multiplier = rt(MCHBAR + 0x5E04, 8, 4);
    let raw_gear = rt(MCHBAR + 0x5E04, 12, 2);
    match (ratio, raw_multiplier, raw_gear) {
        (Some(ratio), Some(raw_multiplier), Some(raw_gear)) => speed_from(bclk, ratio, raw_multiplier, raw_gear),
        _ => "Unknown".into(),
    }
}

/// The pre-Arrow-Lake speed arithmetic over a measured BCLK and the three
/// 0x5E04 fields.
pub(crate) fn speed_from(bclk: f64, ratio: u64, raw_multiplier: u64, raw_gear: u64) -> String {
    // The measured BCLK wanders by a few hundredths of a MHz (99.98 to
    // 100.00 on one bench, read seconds apart), and at a ratio of 80 that
    // is DDR5-8000 reading as 7998 one moment and 8000 the next. Rounded
    // to 0.1 MHz the wander goes and a real BCLK change -- 102.5 MHz, say
    // -- still shows in full.
    let bclk = py_round(bclk, 1);
    let qclk_ratio = match raw_multiplier {
        0 => 133.333333,
        1 => 100.0,
        _ => 100.0,
    };
    let gear_divider = match raw_gear {
        0 => 1.0,
        1 => 2.0,
        2 => 4.0,
        _ => 1.0,
    };
    let speed = ratio as f64 * (qclk_ratio / 100.0) * bclk * gear_divider;
    format!("{} MHz", float_str(round_half_even(speed, 0)))
}

// --- DDR4 RTT / RON controller-shadow readers.

fn read_live_field(base: u64, offset: u64, bit_start: u32, bit_length: i32) -> Option<u64> {
    rt(base + offset, bit_start, bit_length)
}

/// Read both controller shadows. Show one value if they match, otherwise show both.
fn decode_live_rank_pair(base: u64, offsets: [u64; 2], bit_start: u32, bit_length: i32, formula: Table) -> String {
    let decoded: Vec<String> = offsets
        .iter()
        .map(|offset| match read_live_field(base, *offset, bit_start, bit_length) {
            None => "N/A".to_string(),
            Some(raw) => lookup(formula, raw).map(str::to_string).unwrap_or_else(|| format!("Unknown ({raw:#x})")),
        })
        .collect();
    if decoded[0] == decoded[1] {
        return decoded[0].clone();
    }
    if decoded[0] == "N/A" {
        return decoded[1].clone();
    }
    if decoded[1] == "N/A" {
        return decoded[0].clone();
    }
    format!("R0 {} / R1 {}", decoded[0], decoded[1])
}

pub fn get_ddr4_rtt_nom(base: u64) -> String {
    decode_live_rank_pair(base, [0xE5A0, 0xF5A0], 24, 3, formulas::DDR4_RTT_NOM_PARK_FORMULA)
}

pub fn get_ddr4_rtt_wr(base: u64) -> String {
    decode_live_rank_pair(base, [0xE5A4, 0xF5A4], 9, 3, formulas::DDR4_RTT_WR_LIVE_FORMULA)
}

pub fn get_ddr4_rtt_park(base: u64) -> String {
    decode_live_rank_pair(base, [0xE5A8, 0xF5A8], 22, 3, formulas::DDR4_RTT_NOM_PARK_FORMULA)
}

pub const DDR4_RON_SHADOW_OFFSETS: [u64; 2] = [0xE5A0, 0xF5A0];

/// Read a packed MR0/MR1 shadow; reject empty/unmapped windows.
fn read_ddr4_ron_shadow(base: u64, offset: u64) -> Option<u64> {
    let raw = read_physical_memory_int(base + offset, 4)?;
    (0 < raw && raw < 0xFFFF_FFFF).then_some(raw)
}

fn ddr4_ron_from_shadow(raw: Option<u64>) -> String {
    // MR1 occupies the upper halfword. Its A2:A1 field is 00=34, 01=48;
    // 10/11 are reserved. ASUS Setup's Auto/34/48 enum (0/1/2) is unrelated.
    // Source: ISSI IS43/46QR81024A, Mode Register 1 (MR1), page 19.
    let Some(raw) = raw else {
        return "N/A".into();
    };
    let mr1 = (raw >> 16) & 0xFFFF;
    lookup(formulas::DDR4_RON_LIVE_FORMULA, (mr1 >> 1) & 0b11).expect("two bits cover the table").to_string()
}

/// Nominal DRAM RON decoded from controller shadows, not measured ohms.
///
/// Keep differing or unavailable windows visible. Their mapping to physical
/// ranks/DIMMs has not been established, so do not label them R0/R1.
pub fn get_ddr4_ron(base: u64) -> String {
    let values: Vec<String> = DDR4_RON_SHADOW_OFFSETS
        .iter()
        .map(|offset| ddr4_ron_from_shadow(read_ddr4_ron_shadow(base, *offset)))
        .collect();
    if values[0] == values[1] {
        return values[0].clone();
    }
    format!("W0 {} / W1 {}", values[0], values[1])
}

// --- Alder/Raptor Lake memory I/O slew-rate and compensation results.
// These are trained PHY/compensation results exposed through the client MCHBAR.
// Arrow Lake uses a different register layout; all-ones/unsupported registers are
// reported as N/A rather than displaying misleading 63/255 values.
pub(crate) fn read_slew_rate_field(offset: u64, bit_start: u32, bit_length: u32) -> String {
    read_slew_rate_field_from_base(MCHBAR, offset, bit_start, bit_length)
}

// Training contains readings only. DDR4 has no VREFCA or VREFCS register, so those
// rows are removed by _install_platform_vref_additional instead of filling the
// value column with explanatory constants. DDR5 reads both from MR11/MR12.
// tests/test_skew_live.py checks this rule on every supported Intel profile.

// The five categories the slew-rate rows are filed under -- the four signal
// classes and the common SComp block. _install_slew_rate_rows assigns them by
// row; this is the list the tests check that assignment against.
pub const SLEW_RATE_GROUPS: [&str; 5] = ["DATA", "CMD", "CLK", "CTL", "SComp"];

// Named rather than prefixed: this row starts with "CMD" but belongs to the
// common SComp block, and grouping it by its prefix split that block in two.
pub const SLEW_RATE_SCOMP_ROWS: [&str; 1] = ["CMD SlewStatlegen"];

/// Which slew-rate group a row belongs to, from the name it already has.
///
/// Matched without regard to case. The rows carry the reference tools' own
/// names, and theirs is "Data Drv Up" against a heading of DATA; an exact
/// prefix test dropped every DATA row into the SComp group the moment the
/// names were aligned.
pub fn slew_rate_category(name: &str) -> &'static str {
    if SLEW_RATE_SCOMP_ROWS.contains(&name) {
        return "SComp";
    }
    for group in ["DATA", "CMD", "CLK", "CTL"] {
        if name.to_uppercase().starts_with(&format!("{group} ")) {
            return group;
        }
    }
    "SComp"
}

/// Read one slew-rate field from an explicitly selected controller base.
pub(crate) fn read_slew_rate_field_from_base(base: u64, offset: u64, bit_start: u32, bit_length: u32) -> String {
    match read_physical_memory_int(base + offset, 4) {
        None | Some(0xFFFF_FFFF) => "N/A".into(),
        Some(dword) => ((dword >> bit_start) & ((1u64 << bit_length) - 1)).to_string(),
    }
}

/// Read Z690/Z790 DDR4 clock compensation from the live FEDC block.
///
/// The Z790 DDR4 CLK result is not laid out like DATA/CMD/CTL at 0x2CE0.
/// The verified dump stores the trained clock codes in the split clock
/// compensation registers at 0x2CFC/0x2CF4.
pub(crate) fn read_ddr4_clk_slew_field(field_name: &str) -> String {
    let fields: [(&str, (u64, u32, u32)); 4] = if detect_ddr_generation() != DdrGeneration::Ddr4 {
        // CLK shares a compensation domain with CTL, in 0x2CE4. Every one
        // of its fields comes from that register, the pull-down included.
        //
        // This read 0x2CE0 before, on the reasoning that DATA/CMD/CLK/CTL
        // ran 0x2CD8/0x2CDC/0x2CE0/0x2CE4 and CLK therefore had to be the
        // third. Position was never evidence. CMD and CTL hold an
        // identical low word -- 0x04D9, giving 25 and 19 -- because the
        // command-group signals share their drive compensation, while
        // 0x2CE0 holds 0x3C7D, a different shape whose drive-up sits
        // pinned at 61 and VssHiFF at 63, the six-bit maximum, in every
        // sample. That is not a register of this family.
        //
        // Widths follow the reference tools. Their CLK block is a 64-bit
        // read at 0x2CE0 with starts of 32, 48, 44 and 52; that upper word
        // is 0x2CE4, so the same fields are 0, 16, 12 and 20 read here.
        //
        // The pull-down deliberately does not follow them. Both tools put
        // it at 48, which is bits 16-21 of this register, and that window
        // is not a field: bits 16-19 are the top nibble of SComp and bits
        // 20-21 are the low end of VssHiFF, so it reads 2 or 18 depending
        // on where VssHiFF happens to be. The five CTL fields already
        // account for all 32 bits -- 6 + 6 + 8 + 6 + 6 -- so there is no
        // room for a separate CLK pull-down, and sweeping 0x2C00-0x2D00
        // for another comp-shaped register turns up only 0x2CDC (CMD).
        //
        // Writing both blocks in the same 64-bit frame shows what the 48
        // is. CLK is 32, 48, 44, 52. CTL, plus 32, is 32, 38, 44, 52, 58.
        // Drive-up, SComp and VssHiFF are identical; only the pull-down
        // differs, 48 against 38, which reads as 38 with a digit wrong.
        // Bit 6 would give 19: steady, and the same value CMD and CTL
        // trained to, which is what a shared domain should produce.
        //
        // Bit 16 is used anyway, by explicit decision: agreeing with the
        // reference tools matters more here than the argument above, and
        // this row is compared against them directly. It reads 18, and it
        // moves to 2 or 50 when SComp or VssHiFF do. Bit 6 is the change
        // if that is ever revisited.
        [
            ("up", (0x2CE4, 0, 6)),
            ("dn", (0x2CE4, 16, 6)),
            ("scomp", (0x2CE4, 12, 8)),
            ("vsshiff", (0x2CE4, 20, 6)),
        ]
    } else {
        [
            // Verified against the supplied FEDC0000 dump:
            //   0x2CFC = 0x0320A0FF -> 63 / 50 / 32
            //   0x2CF4 = 0x00A2407F -> VssHiFF 63
            ("up", (0x2CFC, 0, 6)),
            ("dn", (0x2CFC, 20, 6)),
            ("scomp", (0x2CFC, 8, 6)),
            ("vsshiff", (0x2CF4, 0, 6)),
        ]
    };
    match fields.iter().find(|(name, _)| *name == field_name) {
        Some((_, (offset, bit_start, bit_length))) => {
            read_slew_rate_field_from_base(MCHBAR, *offset, *bit_start, *bit_length)
        }
        None => {
            eprintln!("Error reading DDR4 CLK slew field {field_name}: '{field_name}'");
            "N/A".into()
        }
    }
}

/// One DDR4 ODT latency, derived from the live tCL/tCWL pair.
///
/// Derived on each call rather than once while the table is built. The two
/// timings it depends on are themselves read from the controller, and a row
/// that quotes them has to move when they do.
pub(crate) fn ddr4_odtl_clocks(kind: &str) -> String {
    let tcl = rt(MCHBAR + 0xE070, 16, 7);
    let tcwl = rt(MCHBAR + 0xE070, 24, 7);
    let (Some(tcl), Some(tcwl)) = (tcl, tcwl) else {
        return "N/A".into();
    };
    let write = (tcwl as i64 - 2).max(0);
    let read_off = (tcl as i64 - 2).max(0);
    let clocks = match kind {
        "write_on" | "write_off" => write,
        "read_on" => read_off + 6,
        "read_off" => read_off,
        _ => {
            eprintln!("Error calculating DDR4 ODTL values: '{kind}'");
            return "N/A".into();
        }
    };
    format!("{clocks} Clocks")
}

// --- The hottest core, from the die rather than from the board.
//
// CPU Temp above is the Super I/O's CPU channel: a board sensor, steady at
// 33 C here while the die spikes. This is the die's own reading, and it is
// what a tuner watches against TjMax.
//
// Located by sweeping the whole of MCHBAR across a controlled load rather
// than assumed. Idle -> 16-process load -> idle, twice, keeping only registers
// holding a plausible Celsius value that rose by at least 8 and came back:
//
//   0x2914, 0x2994, 0x8C14   31 idle, 62 loaded, flat and slow
//   0x5978, 0x597C           33 idle, 77 loaded, spiky and immediate
//
// The second group is the die. It jitters 33 -> 44 -> 34 at idle the way DTS
// does when a core briefly wakes, where the first group holds 31 without
// moving; and it returns to idle the moment load stops, which a sensor with
// more mass behind it does not. RAPL power, for contrast, lags a whole sample
// window on the way down, so this is not a power counter either.
//
// The two offsets mirror one value: 198 of 200 back-to-back reads at idle
// agreed exactly, and 60 of 60 under a single-core load, where two separate
// sensors would have parted. So this is one reading available twice, not the
// package and the hottest core separately -- which is also why there is one
// row here and not two. On Intel the package sensor is the hottest core in
// any case, which is why HWiNFO's CPU Package and Core Max track each other.
//
// What is NOT here: per-core temperatures. The sweep covered all 131072 bytes
// of MCHBAR and found five temperature-shaped registers, not the twenty-four
// a 14900KS would need. Per-core DTS is in IA32_THERM_STATUS, one MSR per
// logical processor, and this driver has no RDMSR.
pub const CORE_MAX_TEMP_OFFSET: u64 = 0x5978;
pub const CORE_MAX_TEMP_MIRROR: u64 = 0x597C;

// Nothing outside this is a temperature. Zero is the register unread rather
// than a die at freezing, and TjMax is 100 C on this part.
pub const CORE_MAX_TEMP_RANGE: (u64, u64) = (5, 125);

/// The hottest core in degrees C, or `None`.
pub fn get_core_max_temp() -> Option<String> {
    let raw = read_physical_memory_int(MCHBAR + CORE_MAX_TEMP_OFFSET, 4)?;
    let (low, high) = CORE_MAX_TEMP_RANGE;
    if !(low..=high).contains(&raw) {
        return None;
    }
    Some(format!("{:.1} °C", raw as f64))
}

// --- Misc tab registers.
//
// One register holds all ten CKE fields. the reference timing tool lists them in this order
// and shows them as plain numbers, so they are reported the same way rather
// than being decoded into cycles -- the units are not documented anywhere
// reachable, and a wrong unit is worse than a raw count.
pub const MISC_CKE_CONFIG_OFFSET: u64 = 0xE0B8;
pub const MISC_CKE_CONFIG_FIELDS: &[(&str, u32, u32)] = &[
    ("Idle Length", 1, 4),
    ("Power Down Latency", 5, 5),
    ("Power Down Length", 10, 4),
    ("Self Refresh Latency", 14, 6),
    ("Self Refresh Length", 20, 4),
    ("CKE Valid Length", 24, 4),
    ("CKE Valid Enable", 28, 1),
    ("Idle Enable", 29, 1),
    ("Power Down Enable", 30, 1),
    ("Self Refresh Enable", 31, 1),
];

// SC_GS_CFG -- the register get_cmd_stretch() already decodes. Reported raw
// here because that is what the reference tool shows; the decoded command
// rate stays on System Info.
pub const MISC_GS_CONFIG_OFFSET: u64 = 0xE088;
pub const MISC_GS_CONFIG_FIELDS: &[(&str, u32, u32)] = &[("CMD Stretch", 3, 2), ("N:1 Ratio", 5, 3)];

// Feature switches, each in its own register, in display order. The final
// flag inverts the bit: the map calls that field "dis_pt_it", so the bit
// disables the timeout and a clear bit means it is running. Only that row
// inverts, which is why the flag is carried per row rather than assumed.
pub const MISC_FEATURE_FIELDS: &[(&str, u64, u32, u32, bool)] = &[
    // The reference tools call this one runbusy.
    ("Realtime Memory", 0x5E00, 31, 1, false),
    // Bit 40 of the 64-bit register at 0xE040, which is bit 8 of 0xE044.
    ("Power Down", 0xE044, 8, 1, false),
    ("Error Correction", 0xD804, 12, 2, false),
    ("Self Refresh", 0xD860, 16, 1, false),
    ("Memory Scrambler", 0x3E00, 0, 1, false),
    ("Row Hammer", 0xE0F8, 0, 1, false),
    ("Page Close Idle Timeout", 0xE028, 6, 1, true),
];

// What these read on Core Ultra 200S, since the whole group installs there.
//
// Power Down is the one with a reading behind it. Its register is live at
// 0x00000600, it sits in the 0xE0xx block this platform demonstrably does not
// relocate -- 0xE000, 0xE050 and 0xE4C0 are all confirmed here -- and bit 8
// reads 0 against a BIOS set to disabled. That is one agreeing state and not a
// proof: a bit stuck at zero reads the same. Toggling power down in BIOS would
// settle it the way the 90-to-120 pair settled tWRPDEN.
//
// Realtime Memory is the opposite case and the one to distrust. It reads
// 0x5E00, and the whole 0x5Exx block is dead silicon here -- zero to every
// query, as is 0x5918 beside it. A zero from a dead register decodes to a
// confident "Disabled" that means nothing, which is worse than N/A because it
// does not look like an absence. Kept visible on request rather than hidden;
// this note is beside the row above so it is not mistaken for a reading. It
// was also a constant naming that row, which nothing consulted -- an unread
// list of untrusted rows enforces nothing and reads as though it does.

// 2N Mode is MR2 bit 2, reached through the pointer path rather than by
// indexing the table directly: an entry's data byte names the mode register
// and its index byte is an offset into the payload array at 0xE200. That is
// the path the DFE rows already use, and it checks out here -- MR0 resolves
// to 0x1C, which decodes to BL16 and CL 36, and 36 is exactly the tCL this
// tool reads from the timing registers.

/// (row, mode register, bit start, bit length, decode table or None)
pub type ModeRegisterField = (&'static str, u64, u32, u32, Option<Table>);

pub const MISC_MODE_REGISTER_FIELDS: &[ModeRegisterField] = &[
    ("Read Preamble Training", 0x02, 0, 1, Some(formulas::MISC_READ_PREAMBLE_TRAINING)),
    ("Read Preamble", 0x08, 0, 3, Some(formulas::MISC_READ_PREAMBLE)),
    ("Write Preamble", 0x08, 3, 2, Some(formulas::MISC_WRITE_PREAMBLE)),
    ("Read Postamble", 0x08, 6, 1, Some(formulas::MISC_POSTAMBLE)),
    ("Write Postamble", 0x08, 7, 1, Some(formulas::MISC_POSTAMBLE)),
];

pub const MISC_BURST_LENGTH_FIELD: (u64, u32, u32, Table) = (0x00, 0, 2, formulas::MISC_BURST_LENGTHS);

/// Read one Misc field, or `None` when the register is not readable.
pub(crate) fn read_misc_field(offset: u64, bit_start: u32, bit_length: u32, base: Option<u64>) -> Option<u64> {
    let raw = read_physical_memory_int(mc(base) + offset, 4)?;
    if raw == 0xFFFF_FFFF {
        return None;
    }
    Some((raw >> bit_start) & ((1u64 << bit_length) - 1))
}

pub(crate) fn misc_number(offset: u64, bit_start: u32, bit_length: u32, base: Option<u64>) -> String {
    match read_misc_field(offset, bit_start, bit_length, base) {
        None => "N/A".into(),
        Some(value) => value.to_string(),
    }
}

pub(crate) fn misc_switch(offset: u64, bit_start: u32, bit_length: u32, inverted: bool, base: Option<u64>) -> String {
    match read_misc_field(offset, bit_start, bit_length, base) {
        None => "N/A".into(),
        Some(value) => {
            if (value != 0) != inverted {
                "Enabled".into()
            } else {
                "Disabled".into()
            }
        }
    }
}

// MR45's table is 256 entries, but it is four runs stepping 16 clocks that
// restart at codes 64, 128 and 192 on 2048, 4096 and 8192. Written as the
// rule rather than the table: 256 transcribed lines is 256 chances to fumble
// one, and the rule reproduces every entry exactly.
pub const MISC_DQS_TIMER_BLOCKS: [(u64, u64); 4] = [(192, 8192), (128, 4096), (64, 2048), (1, 0)];
pub const MISC_DQS_TIMER_REGISTER: u64 = 0x2D;

/// 1st, 2nd, 3rd, 4th -- and 12th rather than 12nd.
pub fn ordinal(number: u64) -> String {
    if (10..=20).contains(&(number % 100)) {
        return format!("{number}th");
    }
    let suffix = match number % 10 {
        1 => "st",
        2 => "nd",
        3 => "rd",
        _ => "th",
    };
    format!("{number}{suffix}")
}

/// MR45 read the way the reference table reads it.
pub fn dqs_interval_timer(code: u64) -> String {
    if code == 0 {
        return "Timer Stops via MPC Command".into();
    }
    for (start, base) in MISC_DQS_TIMER_BLOCKS {
        if code >= start {
            return format!("Timer Stops at {} clocks", ordinal(base + 16 * (code - start)));
        }
    }
    code.to_string()
}

pub const MISC_MODE_REGISTER_STATE: &[ModeRegisterField] = &[
    ("tCL_MR", 0x00, 2, 5, Some(formulas::MR_TCL)),
    ("Refresh tRFC Mode", 0x04, 4, 1, Some(formulas::MR_REFRESH_MODE)),
    ("Wide Range", 0x04, 5, 1, Some(formulas::MR_SUPPORTED)),
    ("Data Output Disable", 0x05, 0, 1, Some(formulas::MR_DATA_OUTPUT)),
    ("Package Output Driver Test Mode", 0x05, 3, 1, Some(formulas::MR_SUPPORTED)),
    ("TDQS Enable", 0x05, 4, 1, Some(formulas::MR_ENABLED)),
    ("DM Enable", 0x05, 5, 1, Some(formulas::MR_ENABLED)),
    ("tWR_MR", 0x06, 0, 4, Some(formulas::MR_TWR)),
    ("tRTP_MR", 0x06, 4, 4, Some(formulas::MR_TRTP)),
    // MR2, MR3 and MR4 as JESD79-5 lays them out. Checked against the reference
    // tool on the Z790 MPOWER, same boot: MR2 reads 0x90 -- CS assertion
    // duration and internal write timing set, the rest clear -- MR3 reads 0x04,
    // and MR4 0x12, and every row below decodes to what that tool shows.
    ("Write Leveling", 0x02, 1, 1, Some(formulas::MR_WRITE_LEVELING)),
    ("N-Mode", 0x02, 2, 1, Some(formulas::MR_N_MODE)),
    ("MPSM", 0x02, 3, 1, Some(formulas::MR_DISABLED_ENABLED)),
    ("CS Assertion Duration", 0x02, 4, 1, Some(formulas::MR_CS_ASSERTION)),
    ("Device 15 MPSM", 0x02, 5, 1, Some(formulas::MR_DISABLED_ENABLED)),
    ("Internal Write Timing", 0x02, 7, 1, Some(formulas::MR_DISABLED_ENABLED)),
    ("Write Leveling LB", 0x03, 0, 4, Some(formulas::MR_WRITE_LEVELING_CYCLES)),
    ("Write Leveling UB", 0x03, 4, 4, Some(formulas::MR_WRITE_LEVELING_CYCLES)),
    ("Minimum Refresh Rate", 0x04, 0, 3, Some(formulas::MR_MINIMUM_REFRESH_RATE)),
    ("Refresh Interval Rate Indicator", 0x04, 3, 1, Some(formulas::MR_RATE_INDICATOR)),
    ("TUF", 0x04, 7, 1, Some(formulas::MR_TUF)),
    ("Read DQS Offset Timing", 0x28, 0, 3, Some(formulas::MR_READ_DQS_OFFSET)),
];

// Command-bus behaviour, from the register tCCD_L comes out of.
pub const MISC_MODE_REGISTER_COMMAND: &[ModeRegisterField] = &[
    ("CS Geardown", 0x0D, 5, 1, Some(formulas::MR_GEARDOWN)),
    ("SRX/NOP Clock-Sync Support", 0x0D, 6, 1, Some(formulas::MR_CLOCK_SYNC)),
];

// Error check and scrub, all of MR14.
pub const MISC_MODE_REGISTER_ECS: &[ModeRegisterField] = &[
    ("ECS Mode", 0x0E, 7, 1, Some(formulas::MR_ECS_MODE)),
    ("ECS Reset Counter", 0x0E, 6, 1, Some(formulas::MR_ECS_RESET)),
    ("ECS Counts", 0x0E, 5, 1, Some(formulas::MR_ECS_COUNTS)),
    ("ECS Error Register Index", 0x0E, 0, 4, None),
];

/// Read one mode-register field, or `None` when the table has no entry.
fn read_misc_mode_register_field(number: u64, bit_start: u32, bit_length: u32, base: Option<u64>) -> Option<u64> {
    let address = mode_register_pointer(number, 0, MODE_REGISTER_PAYLOAD_BASE, base)?;
    let raw = read_physical_memory_int(address, 1)?;
    Some((raw >> bit_start) & ((1u64 << bit_length) - 1))
}

/// A mode-register field decoded by a function rather than a table.
pub(crate) fn misc_mode_register_code(
    number: u64,
    bit_start: u32,
    bit_length: u32,
    decode: fn(u64) -> String,
    base: Option<u64>,
) -> String {
    match read_misc_mode_register_field(number, bit_start, bit_length, base) {
        None => "N/A".into(),
        Some(value) => decode(value),
    }
}

/// One mode-register field, named by its table or shown as a number.
///
/// No table means the field is a count or an index, not a switch. It read
/// as Enabled/Disabled once, which turned the ECS error register index --
/// four bits selecting which record to read -- into "Disabled" at index 0.
/// Every field that is a switch carries the table that says so.
pub(crate) fn misc_mode_register_value(
    number: u64,
    bit_start: u32,
    bit_length: u32,
    decode: Option<Table>,
    base: Option<u64>,
) -> String {
    match read_misc_mode_register_field(number, bit_start, bit_length, base) {
        None => "N/A".into(),
        Some(value) => match decode {
            None => value.to_string(),
            Some(table) => lookup(table, value).map(str::to_string).unwrap_or_else(|| value.to_string()),
        },
    }
}

/// MR0's write-recovery code, whose two halves are not adjacent.
fn ddr4_wr_rtp_code(base: Option<u64>) -> Option<u64> {
    let value = ddr4_mode_register(0x00, base)?;
    Some((((value >> 13) & 1) << 3) | ((value >> 9) & 0x7))
}

pub(crate) fn ddr4_wr_rtp(half: usize, base: Option<u64>) -> String {
    let Some(code) = ddr4_wr_rtp_code(base) else {
        return "N/A".into();
    };
    match formulas::DDR4_MR_WR_RTP.iter().find(|(key, _)| *key == code) {
        None => "N/A".into(),
        Some((_, (wr, rtp))) => if half == 0 { wr } else { rtp }.to_string(),
    }
}

pub const DDR4_CWL_MODE_REGISTER: u64 = 0x02;
pub const DDR4_CWL_BITS: (u32, u32) = (3, 3);

/// The DRAM's own tCCD_L, MR6 A12:A10 -- the same field, and the same codes,
/// the tCCD_L row on the Timings tab already reads. Derived from that table
/// rather than retyped, so the two cannot drift into disagreeing about what a
/// code means; only the rendering differs, since a Misc row is text.
pub fn ddr4_mr_ccd_l() -> Vec<(u64, String)> {
    DDR4_TCCD_L.iter().map(|(code, value)| (*code, value.to_string())).collect()
}

/// CWL as the module was programmed with it, out of the MR2 shadow.
pub(crate) fn ddr4_cwl_mr(base: Option<u64>) -> String {
    ddr4_misc_value(DDR4_CWL_MODE_REGISTER, DDR4_CWL_BITS.0, DDR4_CWL_BITS.1, Some(formulas::DDR4_MR_CWL), base)
}

/// tCCD_L as the module was programmed with it, out of the MR6 shadow.
pub(crate) fn ddr4_ccd_l_mr(base: Option<u64>) -> String {
    let value = ddr4_mode_register_field(DDR4_TCCD_L_MODE_REGISTER, DDR4_TCCD_L_BITS.0, DDR4_TCCD_L_BITS.1, base);
    match value {
        None => "N/A".into(),
        Some(code) => ddr4_mr_ccd_l()
            .into_iter()
            .find(|(key, _)| *key == code)
            .map(|(_, text)| text)
            .unwrap_or_else(|| code.to_string()),
    }
}

/// (row, mode register, bit start, bit length, decode table)
pub const DDR4_MISC_MODE_REGISTER_FIELDS: &[ModeRegisterField] = &[
    ("Burst Length", 0x00, 0, 2, Some(formulas::DDR4_MR_BURST_LENGTHS)),
    ("TDQS Enable", 0x01, 11, 1, Some(formulas::MR_ENABLED)),
    ("Data Output Disable", 0x01, 12, 1, Some(formulas::MR_DATA_OUTPUT)),
    ("CS Geardown", 0x03, 3, 1, Some(formulas::MR_GEARDOWN)),
    ("Refresh tRFC Mode", 0x03, 6, 3, Some(formulas::DDR4_MR_REFRESH_MODE)),
    ("DM Enable", 0x05, 10, 1, Some(formulas::MR_ENABLED)),
];

/// (row, mode register, bit start, bit length, decode table or None). A None
/// table means a numeric field. CAS latency is the sole split-bit field and is
/// decoded separately below.
pub const DDR4_ADVANCED_MODE_REGISTER_FIELDS: &[ModeRegisterField] = &[
    ("Read Burst Type", 0x00, 3, 1, Some(formulas::DDR4_MR_READ_BURST)),
    ("Test Mode", 0x00, 7, 1, Some(formulas::DDR4_MR_TEST_MODE)),
    ("DLL Reset", 0x00, 8, 1, Some(formulas::DDR4_MR_DLL_RESET)),
    ("DLL Enable", 0x01, 0, 1, Some(formulas::DDR4_MR_SWITCH)),
    ("Additive Latency", 0x01, 3, 2, Some(formulas::DDR4_MR_ADDITIVE_LATENCY)),
    ("Write Leveling", 0x01, 7, 1, Some(formulas::DDR4_MR_SWITCH)),
    ("Low Power ASR", 0x02, 6, 2, Some(formulas::DDR4_MR_LP_ASR)),
    ("Write CRC", 0x02, 12, 1, Some(formulas::DDR4_MR_SWITCH)),
    ("MPR Page Select", 0x03, 0, 2, Some(formulas::DDR4_MR_MPR_PAGE)),
    ("MPR Operation", 0x03, 2, 1, Some(formulas::DDR4_MR_MPR_OPERATION)),
    ("Per DRAM Addr", 0x03, 4, 1, Some(formulas::DDR4_MR_SWITCH)),
    ("Temp Sensor Readout", 0x03, 5, 1, Some(formulas::DDR4_MR_SWITCH)),
    ("Write CMD Latency", 0x03, 9, 2, Some(formulas::DDR4_MR_WRITE_COMMAND_LATENCY)),
    ("MPR Read Format", 0x03, 11, 2, Some(formulas::DDR4_MR_MPR_FORMAT)),
    ("Max Power Down", 0x04, 1, 1, Some(formulas::DDR4_MR_SWITCH)),
    ("Temp Refresh Range", 0x04, 2, 1, Some(formulas::DDR4_MR_TEMP_REFRESH_RANGE)),
    ("Temp Ctrl Refresh", 0x04, 3, 1, Some(formulas::DDR4_MR_SWITCH)),
    ("Internal Vref Mon", 0x04, 4, 1, Some(formulas::DDR4_MR_SWITCH)),
    ("Soft PPR", 0x04, 5, 1, Some(formulas::DDR4_MR_SWITCH)),
    ("CS to CMD Latency", 0x04, 6, 3, Some(formulas::DDR4_MR_CS_COMMAND_LATENCY)),
    ("Self Refresh Abort", 0x04, 9, 1, Some(formulas::DDR4_MR_SWITCH)),
    ("Read Preamble Train", 0x04, 10, 1, Some(formulas::DDR4_MR_SWITCH)),
    ("Read Preamble", 0x04, 11, 1, Some(formulas::DDR4_MR_PREAMBLE)),
    ("Write Preamble", 0x04, 12, 1, Some(formulas::DDR4_MR_PREAMBLE)),
    ("Hard PPR", 0x04, 13, 1, Some(formulas::DDR4_MR_SWITCH)),
    ("CA Parity Latency", 0x05, 0, 3, Some(formulas::DDR4_MR_CA_PARITY_LATENCY)),
    ("CRC Error Clear", 0x05, 3, 1, Some(formulas::DDR4_MR_CLEAR_ERROR)),
    ("CA Parity Err Status", 0x05, 4, 1, Some(formulas::DDR4_MR_CLEAR_ERROR)),
    ("ODT Buffer (PD)", 0x05, 5, 1, Some(formulas::DDR4_MR_ODT_BUFFER_PD)),
    ("CA Parity Persist Err", 0x05, 9, 1, Some(formulas::DDR4_MR_SWITCH)),
    ("Write DBI", 0x05, 11, 1, Some(formulas::DDR4_MR_SWITCH)),
    ("Read DBI", 0x05, 12, 1, Some(formulas::DDR4_MR_SWITCH)),
    ("VrefDQ Train Value", 0x06, 0, 6, None),
    ("VrefDQ Train Range", 0x06, 6, 1, Some(formulas::DDR4_MR_VREF_RANGE)),
    ("VrefDQ Train Enable", 0x06, 7, 1, Some(formulas::DDR4_MR_SWITCH)),
];

/// CAS latency from MR0's split A12,A6:A4,A2 encoding.
pub(crate) fn ddr4_mr_cas_latency(base: Option<u64>) -> String {
    let Some(value) = ddr4_mode_register(0x00, base) else {
        return "N/A".into();
    };
    let code = (((value >> 12) & 1) << 4) | (((value >> 4) & 0x7) << 1) | ((value >> 2) & 1);
    lookup(formulas::DDR4_MR_CAS_LATENCY, code).unwrap_or("Reserved").to_string()
}

/// One Misc row read out of the DDR4 shadow.
pub(crate) fn ddr4_misc_value(number: u64, bit_start: u32, bit_length: u32, decode: Option<Table>, base: Option<u64>) -> String {
    match ddr4_mode_register_field(number, bit_start, bit_length, base) {
        None => "N/A".into(),
        Some(value) => match decode {
            None => value.to_string(),
            Some(table) => lookup(table, value).map(str::to_string).unwrap_or_else(|| value.to_string()),
        },
    }
}
