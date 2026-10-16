//! The Intel table: LGA1700 (DDR4 and DDR5) and LGA1851.
//!
//! Port of `rochviewer/intel/intel_timings.py`. The Python built `TIMINGS` as
//! its module body ran -- a literal of row dicts, then some sixty passes that
//! re-point, rename, re-file, promote and reword them -- so importing the
//! module *was* building the table, against whatever platform the dispatcher
//! had already detected. Here [`build_timings`] runs the same passes, in the
//! same order, on the same data, for the platform it is given; see `build`.
//!
//! Laid out by what each part is:
//!
//! * `dict` -- the row dict the passes work on, and the Python's identity
//!   and ordering idioms over a list of them;
//! * `formulas` -- every decode table;
//! * `identity` -- System Info's machine facts (CPU, board, OS, card,
//!   modules, channel layout);
//! * `registers` -- everything read out of the memory controller, and its
//!   decodes;
//! * `table` -- the declared literal;
//! * `passes`, `sensors`, `layout` -- the passes, in module order;
//! * `build` -- the sequence, and the conversion to [`Row`].

mod build;
mod dict;
pub mod formulas;
pub mod identity;
mod layout;
mod passes;
pub mod registers;
pub mod sensors;
mod table;

#[cfg(test)]
mod tests;

use std::sync::Arc;

use crate::hardware::read::{FieldRead, read_field};
use crate::memo::MemoMap;
use crate::platform_profiles::Platform;
use crate::row::Row;

pub use formulas::clock_offset;
pub use identity::{
    CHANNEL_LAYOUT_NAMES, DDR5_GPU_ROWS, GPU_ROWS, INTEL_CODE_NAMES, PCH_DEVICE_NAMES, PLATFORM_LABELS,
    SMBIOS_MEMORY_TYPES, channel_layout_name, clear_identity_caches, core_types_text, detect_dual_channel_memory,
    dimm_field, get_bios_date, get_bios_version, get_board_manufacturer, get_board_model, get_chipset_name,
    get_core_types, get_cpu_codename, get_cpu_cores_threads, get_cpu_name, get_cpu_package, get_cpu_signature,
    get_cpu_technology, get_gpu_driver_date, get_gpu_name, get_lpcio_name, get_memory_type, get_microcode,
    get_motherboard_display, get_motherboard_name, get_motherboard_version, get_os_edition, get_os_name,
    get_os_version, get_platform_name, get_ram_manufacturer, get_slots_used, get_southbridge_name,
    get_total_physical_memory, get_windows_display_version, get_windows_update_revision, parse_first_number,
    reported_channel_count,
};
pub use layout::{
    DDR5_DISPLAY_NAMES, DDR5_SYSTEM_INFO_REMOVED, DDR5_TRAINING_REMOVED, IMC_TAB, MISC_TAB, SETTINGS_TAB,
    SYSTEM_INFO_TAB, brief_ddr5_value, imc_switch_text, misc_rows_for_generation, resistance_first_rzq_text,
    system_info_order,
};
pub use passes::{
    ARROW_LAKE_POWER_DOWN, ARROW_LAKE_POWER_DOWN_UNKNOWN, ARROW_LAKE_POWER_DOWN_WIDE, CCD_CATEGORY, CCD_ROWS,
    DDR4_ABSENT_VREF_ROWS,
};
pub use registers::{
    arrow_lake_gear, decode_ddr4_dq_vref, get_bclk, get_bclk_rd, get_ccd_timing, get_cmd_stretch, get_command_rate,
    get_core_clock, get_core_max_temp, get_core_ratio, get_ddr4_refresh_mode_value, get_ddr4_ron, get_ddr4_rtt_nom,
    get_ddr4_rtt_park, get_ddr4_rtt_wr, get_dfe_bias, get_dfe_enable, get_dllk_timing, get_dram_frequency,
    get_dram_ratio_value, get_ecc_error_correction_value, get_gear_mode_value, get_imc_freq, get_mclk,
    get_memory_scrambler_value, get_power_down_mode_value, get_psf0_pll, get_qclk_ratio, get_ring_freq,
    get_row_hammer_value, get_sa, get_self_refresh_value, get_speed, get_tmod_value, get_trc_value, get_twr_value,
    get_twtr_l, get_twtr_s, get_tx, get_uclk, get_uncore_ratio, is_arrow_lake_platform, read_mode_register,
    refresh_is_normal, scheduler_gear_mode, SCHEDULER_CONFIG_OFFSET, SCHEDULER_GEAR2_BIT, SCHEDULER_GEAR4_BIT,
};
pub use sensors::{absent_sensor_rows, board_absent_sensor_rows, dimm_slots_by_channel, sensor_row_label};

pub const MCHBAR: u64 = 0xFEDC_0000;
pub const MCHBAR2: u64 = 0xFEDD_0000;

/// Platforms whose modules refresh the DDR5 way.
pub const DDR5_TIMING_PLATFORMS: &[Platform] = &[Platform::Lga1700Ddr5, Platform::Lga1851];

// The two columns the tabs draw are the two installed modules. On Raptor Lake
// each module has a controller of its own, whatever the generation: the second
// module is on MC1, one 0x10000 window up.
//
// DDR5 splits a module into two 32-bit sub-channels, 0x800 apart inside its
// controller, so there are four trained contexts for two modules:
//
//   MCHBAR          MC0 sub-channel 0   A1
//   MCHBAR + 0x800  MC0 sub-channel 1   A1
//   MCHBAR2         MC1 sub-channel 0   B1
//   MCHBAR2 + 0x800 MC1 sub-channel 1   B1
//
// This used to read B1 at MCHBAR + 0x800, which is A1's second half. Three
// independent readings on the Z790 MPOWER, same boot, say where B1 is:
//
//   SMBIOS names the sockets Controller0-DIMMA1 and Controller1-DIMMB1.
//   The reference tool labels its four columns DIMM0 CH0 MC0, DIMM0 CH1
//   MC0, DIMM1 CH0 MC1 and DIMM1 CH1 MC1, with serials 4997 and 4996 --
//   A1's and B1's SPD serials.
//   The trained values follow the controllers, not the 0x800 step: DFE tap 1
//   reads -11, -11, -15, -11 across the four contexts above and DQ VREF 3
//   66.5, 66.0, 68.5, 68.0 %, and the reference tool's DIMM1 columns are
//   the MC1 ones.
//
// DDR4 has no sub-channels and the 0x800 block is a channel that was never
// trained. Measured on the Z790-P with a module in each channel, the RTL rows
// -- a per-DIMM trained latency, reading 25 where a slot is empty -- report:
//
//   MC0 CHA  77     MC1 CHA  79      the two installed modules
//   MC0 CHB  25     MC1 CHB  25      both sub-channels empty
//
// and the timing block at +0x800 reads tCL 5 against the real 18, with a tCWL
// of 6 against 17 -- low enough to drive the computed tWTR_L and tWTR_S rows
// negative, which is how it showed.
//
// Either way the ordinary timing registers hold the same values in both halves
// of a matched kit, so only a per-DIMM trained result tells the candidates
// apart: DFE taps and DQ VREF on DDR5, RTL on DDR4.
//
// Core Ultra 200S keeps the sub-channel step. It relocates several MCHBAR
// blocks by 0x10000, which is where MC1 sits on Raptor Lake, and no bench
// reading has yet said where its second module is.
pub const CHANNEL_B_SUBCHANNEL_OFFSET: u64 = 0x800;
pub const CHANNEL_B_CONTROLLER_OFFSET: u64 = MCHBAR2 - MCHBAR;
pub const SUBCHANNEL_B_PLATFORMS: &[Platform] = &[Platform::Lga1851];

/// The two controllers' windows: what a channel-A address must lie in to
/// have a twin.
pub const MC_WINDOW: u64 = MCHBAR2 - MCHBAR;

/// Distance from a channel-A register to its channel-B twin.
pub fn channel_b_offset(platform: Platform) -> u64 {
    if SUBCHANNEL_B_PLATFORMS.contains(&platform) {
        CHANNEL_B_SUBCHANNEL_OFFSET
    } else {
        CHANNEL_B_CONTROLLER_OFFSET
    }
}

/// `CHANNEL_B`: where channel B's registers start on this platform.
pub fn channel_b(platform: Platform) -> u64 {
    MCHBAR + channel_b_offset(platform)
}

// Timing rows whose name differs on DDR5: {declared name: DDR5 label}.
//
// DDR5 splits refresh into an all-bank interval and a same-bank one, named
// tRFC2 and tRFCpb; the register this row reads is the all-bank one, so tRFC2
// is what it is. DDR4 has a single tRFC and no per-bank refresh at all, which
// is why this is a rename per platform rather than a correction everywhere.
//
// The two derived nanosecond rows used to be renamed here as well: they were
// declared "tRFC (ns)" and "tREFI (ns)" and drawn as tRFCns and tREFIns on
// DDR5 only. They are not any more. A derived row restating a raw one in
// nanoseconds is the same row on both generations, so it carries the same
// compact name on both, matching the AM5 profile's rows of those names. Only
// tRFC, which really is a different register on DDR5, is still renamed.
//
// Applied last, by _install_ddr5_timing_labels at the foot of this module.
// Everything that matches rows by name -- the Arrow Lake field map, the
// secondary reorder, the tRFCns installer, the dual-channel promotion -- runs
// during import against the declared names, so the rename has to come after
// all of them. Only get_trfc_ns reads a row by name at render time, and it
// resolves the label the same way this does.
pub const DDR5_TIMING_LABELS: &[(&str, &str)] = &[("tRFC", "tRFC2")];

/// The label a timing row carries on this platform. Pure, so it can be tested.
pub fn ddr5_timing_label(platform: Platform, name: &str) -> &str {
    if DDR5_TIMING_PLATFORMS.contains(&platform)
        && let Some((_, label)) = DDR5_TIMING_LABELS.iter().find(|(declared, _)| *declared == name) {
            return label;
        }
    name
}

/// The finished table's register fields, by row name, in table order.
pub type FinalTable = Vec<(String, FieldRead)>;

/// The last table built for each platform, for the public nanosecond
/// readers (the Python read its module global at call time).
static FINALIZED: MemoMap<Platform, Arc<FinalTable>> = MemoMap::new();

/// The full Intel table for `platform`.
pub fn build_timings(platform: Platform) -> Vec<Row> {
    build::Build::new(platform).run()
}

/// Read a timing row by name after the platform installers set its address.
///
/// A row's stored address always names the first memory controller, so `base`
/// reads the same offset on another one. An address that does not sit inside
/// MC0's window has no known twin, so that request returns None rather than a
/// register that was never confirmed to hold this timing.
pub(crate) fn read_finalized_timing(table: &FinalTable, name: &str, base: Option<u64>) -> Option<u64> {
    let (_, field) = table.iter().find(|(row, _)| row == name)?;
    let mut address = field.address;
    if let Some(base) = base {
        if !(MCHBAR..MCHBAR2).contains(&address) {
            return None;
        }
        address = base + (address - MCHBAR);
    }
    read_field(&FieldRead::new(address, field.bit_start, field.bit_length))
}

/// Convert the live refresh intervals into nanoseconds using the MCLK.
///
/// On DDR5 the row carries both: the all-bank interval the controller
/// refreshes on, then the same-bank one, which is the pair a tuner compares
/// and which no single figure can show. DDR4 has no per-bank refresh, so it
/// stays one number there.
///
/// MCLK is a controller-wide clock, so only the interval reads are
/// per-channel.
pub fn get_trfc_ns(platform: Platform, base: Option<u64>) -> Option<String> {
    let table = FINALIZED.get(&platform)?;
    get_trfc_ns_with(platform, base, &table)
}

pub(crate) fn get_trfc_ns_with(platform: Platform, base: Option<u64>, table: &FinalTable) -> Option<String> {
    trfc_ns_from(platform, Some(registers::get_mclk()), |name| read_finalized_timing(table, name, base))
}

/// [`get_trfc_ns`] over a given MCLK reading and interval reader.
pub(crate) fn trfc_ns_from(platform: Platform, mclk: Option<String>, read: impl Fn(&str) -> Option<u64>) -> Option<String> {
    let mclk = parse_first_number(mclk.as_deref()).filter(|mclk| *mclk != 0.0)?;
    let all_bank = read(ddr5_timing_label(platform, "tRFC")).filter(|cycles| *cycles != 0)?;
    let text = format!("{:.0} ns", all_bank as f64 * 1000.0 / mclk);
    if !DDR5_TIMING_PLATFORMS.contains(&platform) {
        return Some(text);
    }
    let Some(per_bank) = read("tRFCpb").filter(|cycles| *cycles != 0) else {
        // The all-bank interval on its own beats nothing, and a board that
        // does not report the per-bank one is not an error.
        return Some(text);
    };
    // The all-bank interval then the per-bank one, with the unit named
    // once at the end: "120/98 ns". Both values are in the same unit, so
    // saying it twice spends a third of the column repeating it.
    //
    // Bare rather than parenthesised, which is what the single-value
    // branch above already returns and what tREFIns beside it returns.
    // The pair was the only row on the tab still bracketing its unit.
    Some(format!("{:.0}/{:.0} ns", all_bank as f64 * 1000.0 / mclk, per_bank as f64 * 1000.0 / mclk))
}

/// tREFI restated as the time it actually is.
///
/// tREFI is how long the controller may go between refreshes, and a count of
/// memory clocks means nothing without the clock: 65535 cycles at 4000 MHz is
/// 16384 ns. Whole nanoseconds, matching the tRFCns beside it -- a refresh
/// window is not tuned to a picosecond, and the hundredths would come from
/// the division rather than from any precision the reading has.
pub fn get_trefi_ns(platform: Platform, base: Option<u64>) -> Option<String> {
    let table = FINALIZED.get(&platform)?;
    get_trefi_ns_with(base, &table)
}

pub(crate) fn get_trefi_ns_with(base: Option<u64>, table: &FinalTable) -> Option<String> {
    let mclk = parse_first_number(Some(&registers::get_mclk())).filter(|mclk| *mclk != 0.0)?;
    let cycles = read_finalized_timing(table, "tREFI", base).filter(|cycles| *cycles != 0)?;
    Some(format!("{:.0} ns", cycles as f64 * 1000.0 / mclk))
}
