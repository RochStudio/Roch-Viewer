//! The passes after the Sensors tab: the Timings sections, System Info, the
//! Misc tab and where its rows finally land, the per-module/shared split of
//! Training and IMC, and the presentation passes that run last.

use std::collections::BTreeMap;
use std::sync::{Arc, LazyLock};

use regex::Regex;

use crate::memory::ddr_generation::{DdrGeneration, detect_ddr_generation};
use crate::platform_profiles::Platform;
use crate::row::BaseReaderFn;

use super::build::Build;
use super::dict::{DUAL_KEYS, T, V, bits, is_dual_timing, lazy, partition};
use super::formulas::Table;
use super::identity::{
    DDR5_GPU_ROWS, GPU_ROWS, dimm_field, get_bios_date, get_chipset_name, get_core_types, get_cpu_codename,
    get_cpu_package, get_cpu_signature, get_cpu_technology, get_gpu_driver_date, get_gpu_name, get_lpcio_name,
    get_memory_type, get_motherboard_version, get_os_edition, get_os_version, get_platform_name, get_slots_used,
    get_southbridge_name, gpu_field,
};
use super::passes::base_reader;
use super::registers::{
    self, DDR4_ADVANCED_MODE_REGISTER_FIELDS, DDR4_MISC_MODE_REGISTER_FIELDS, MISC_BURST_LENGTH_FIELD,
    MISC_CKE_CONFIG_FIELDS, MISC_CKE_CONFIG_OFFSET, MISC_DQS_TIMER_REGISTER, MISC_FEATURE_FIELDS,
    MISC_GS_CONFIG_FIELDS, MISC_GS_CONFIG_OFFSET, MISC_MODE_REGISTER_COMMAND, MISC_MODE_REGISTER_ECS,
    MISC_MODE_REGISTER_FIELDS, MISC_MODE_REGISTER_STATE, ModeRegisterField, is_arrow_lake_platform,
    refresh_is_normal,
};
use super::{CHANNEL_B_CONTROLLER_OFFSET, MCHBAR, ddr5_timing_label};

// --- Refresh mode is policy rather than a timing interval.
//
// On DDR4 it comes from DDR_PTM_CTL in the global MCHBAR region; on DDR5 it is
// decoded from the mode-register shadow. It therefore lives with settings,
// with its source scope decided below: global on DDR4, per-DIMM on DDR5.
pub const REFRESH_MODE_SOURCE: (&str, &str) = ("REFRESH", "REFRESH MODE");
pub const REFRESH_MODE_NAME: &str = "Refresh Mode";
pub const REFRESH_POLICY_CATEGORY: &str = "Refresh";

// --- Rows the active refresh mode is not using.
//
// tRFCpb is the controller's per-bank refresh field. DDR4 normally leaves it
// at zero, but the reference viewer exposes the raw field, so keep it visible
// and dim it while the active policy is the normal all-bank tRFC schedule.
// main.py dims a row whose "dim" callable returns True, which is what that
// hook exists for.
//
// Attached here rather than on the row literal because refresh_is_normal is
// defined further down the file: the literal would capture the name before it
// is bound.
pub const REFRESH_MODE_DIMMED_ROWS: [&str; 1] = ["tRFCpb"];

// --- Timings tab column balance.
//
// Sections were assigned to columns row by row as the table grew, and the split
// had drifted to 43 lines on the left against 30 on the right: the left column
// ran past the bottom of the window and scrolled while the right half of the
// tab sat empty.
//
// Assigning whole sections here rather than per row keeps a section in one
// piece and puts the layout in one place where it can be checked. The two
// columns are semantic and balanced by row count: primary, secondary, command,
// and power-down timings on the left; refresh, tertiary, CAS-to-CAS, and other
// timings on the right.
pub const TIMINGS_TAB_COLUMNS: &[(&str, &str)] = &[
    ("Primary", "Left"),
    ("Secondary", "Left"),
    ("Command", "Left"),
    ("Power down", "Left"),
    ("Refresh timings", "Right"),
    ("Tertiary", "Right"),
    ("CAS to CAS", "Right"),
    ("Other Timings", "Right"),
];

// Power down had become the tab's catch-all: 25 rows holding the actual
// power-down entry and exit timings alongside chip-select spacing, mode
// register command timings, preamble and precharge, refresh management and
// the DLL lock time. Each of those belongs with what it describes.
//
// Applied by name after the rows are built, rather than by editing each
// definition, so the grouping is legible in one place and a row that moves
// again does not need its declaration hunted down.
pub const TIMINGS_SECTION_MOVES: &[(&str, &str)] = &[
    // Refresh management and the refresh interval read with the intervals.
    ("tRFM", "Refresh timings"),
    ("tREFSBRD", "Refresh timings"),
    ("Rank Idle", "Refresh timings"),
    // Precharge, preamble and the DLL lock are ordinary bus timings.
    ("tRDPRE", "Other Timings"),
    ("tWRPRE", "Other Timings"),
    ("tDLLK", "Other Timings"),
    // Chip select spacing and the mode-register command timings.
    ("tCSH", "Command"),
    ("tCSL", "Command"),
    ("tCA2CS", "Command"),
    ("tOSCO", "Command"),
    ("tPREMRR", "Command"),
    ("tMRR", "Command"),
    // The mode-register write timing sits with the read it pairs with.
    ("tMRRMRW", "Command"),
    // A mode-register command delay belongs with the command-bus timings.
    // The shared LPDDR back-to-back policy is moved to IMC later.
    ("tMOD", "Command"),
    // Keep the complete column-to-column delay family together.
    ("tCCD", "CAS to CAS"),
    ("tCCD_L", "CAS to CAS"),
    ("tCCD_L_WR", "CAS to CAS"),
    ("tCCD_L_WR2", "CAS to CAS"),
];

// Primary intentionally is not listed: its source order is already the desired
// display order and must stay untouched. Every other section is pinned here so
// rows moved between sections land beside related operations instead of at the
// position of their old category.
pub const TIMINGS_SECTION_ROW_ORDER: &[(&str, &[&str])] = &[
    (
        "Secondary",
        &["tWR", "tWR_MR", "tRRD_L", "tRRD_S", "tWTR_L", "tWTR_S", "tRTP", "tRTP_MR", "tFAW", "tCWL", "tCWL_MR"],
    ),
    ("Command", &["tCSH", "tCSL", "tCA2CS", "tOSCO", "tPREMRR", "tMRRMRW", "tMRR", "tMOD"]),
    (
        "Refresh timings",
        &[
            "Refresh Mode", "tREFI", "tREFIns", "tREFIx9", "tRFCns", "tRFC", "tRFC2", "tRFCpb", "Rank Idle", "tRFM",
            "tREFSBRD",
        ],
    ),
    (
        "Tertiary",
        &[
            "tRDRD_sg", "tRDRD_dg", "tRDRD_dr", "tRDRD_dd", "tRDWR_sg", "tRDWR_dg", "tRDWR_dr", "tRDWR_dd",
            "tWRRD_sg", "tWRRD_dg", "tWRRD_dr", "tWRRD_dd", "tWRWR_sg", "tWRWR_dg", "tWRWR_dr", "tWRWR_dd",
        ],
    ),
    ("CAS to CAS", &["tCCD", "tCCD_L", "tCCD_L_MR", "tCCD_L_WR", "tCCD_L_WR2"]),
    (
        "Power down",
        &[
            "tWRPDEN", "tRDPDEN", "tPRPDEN", "tAONPD", "tCPDED", "tCKE", "tXP", "tXPDLL", "tXSDLL", "tXSR",
            "tCKCKEH", "tPPD", "tSR",
        ],
    ),
    (
        "Other Timings",
        &["tZQCAL", "tZQCS", "ZQCS period", "tZQoper", "tCAL", "TRPab_EXT", "tRDPRE", "tWRPRE", "tDLLK"],
    ),
];

pub const SYSTEM_INFO_TAB: &str = "System Info";

// Controller policy bits rather than anything read or tuned from this tab.
//
// Power Down Mode is here because the Misc tab reads the same thing closer to
// the source: this row reported who owns the policy, decoded from DDR_PTM_CTL
// bit 6, while Misc reads the controller's own power-down bit. Summary now
// places that Misc row instead, so nothing is lost by dropping this one.
pub const SYSTEM_INFO_REMOVED: &[&str] = &[
    "CMD Stretch",
    "ECC / Error Correction",
    "Self Refresh",
    "Memory Scrambler",
    "Row Hammer",
    "Power Down Mode",
    // Per-module identity belongs in the SPD tab.  Keep System Info focused on
    // the machine-wide memory configuration instead of repeating it here.
    "RAM Manufacturer",
    "DRAM Manufacturer",
    "DRAM Die",
    "Part Number",
    "Serial Number",
    "Manufactured",
    "DRAM Technology",
    "Slots Used",
    "DIMM Size",
    "Rank",
    // Variable CPU clocks belong in Telemetry. System Info keeps the bus and
    // memory/controller ratios, which describe the configured platform.
    "Core Clock",
];

// And on LGA1700 DDR5. Core Ratio is the current core clock over BCLK, a
// reading that moves -- 53.8 x -- in a section of configured values; the live
// clock is in Telemetry. Package was SMBIOS's socket designation, which MSI
// fills with the board's reference designator (U3E1) rather than a package
// name, and Platform above it already says LGA1700.
pub const DDR5_SYSTEM_INFO_REMOVED: &[&str] = &["Core Ratio", "CPU Package"];

// Summary's compact clock chain still includes Ring.  Keep its existing
// hardware reader available to that purpose without exposing the changing
// value as a System Info row; Telemetry remains the detailed live-clock view.
pub const SYSTEM_INFO_SUMMARY_ONLY: &[&str] = &["Uncore"];

// Keep machine identity on the left. Configured clocks and graphics share the
// right column, with Graphics directly below Clocks. Memory identity and the
// two system-wide memory totals now live in the SPD page.
pub const SYSTEM_INFO_SECTIONS: &[(&str, &str, &[&str])] = &[
    ("System", "Left", &["OS", "OS Version", "Platform"]),
    (
        "Processor",
        "Left",
        &["CPU", "CPU Package", "CPU Signature", "Code Name", "Technology", "Cores / Threads", "Core Types", "Microcode"],
    ),
    (
        "Motherboard",
        "Left",
        &["Manufacturer", "Model", "Board Revision", "BIOS", "BIOS Date", "Chipset", "Southbridge", "LPCIO"],
    ),
    // Configured memory clocks and ratios only. Variable CPU core/ring clocks
    // are sensor readings and remain in Telemetry.
    (
        "Clocks",
        "Right",
        &["DRAM Frequency", "DRAM Ratio", "QCLK Ratio", "BCLK", "Core Ratio", "Uncore Ratio", "MCLK", "UCLK", "PSF0 PLL", "Gear Mode"],
    ),
    (
        "Graphics",
        "Right",
        &[
            "GPU", "Board Manufacturer", "GPU Code Name", "GPU Revision", "Cores", "GPU Technology", "Memory Size",
            "Memory Type", "Memory Vendor", "Bus Width", "Resizable BAR", "PCIe Link", "VBIOS Version",
            "Driver Version", "Driver Date",
        ],
    ),
];

pub const SPD_SYSTEM_ROWS: &[&str] = &["Channels", "Memory Capacity"];

// --- System Info reading order.
//
// The rows had accumulated in whatever order the table happened to build them,
// interleaving identity, clocks and module facts. They now read in groups: what
// the machine is, the board, the clock chain from BCLK down to UCLK, how the
// controller is configured, then what is installed in it.
//
// Uncore and PSF0 PLL are not in the requested order but were not asked to be
// removed either, so they keep a place: Uncore among the clocks it belongs
// with, PSF0 PLL after the rest.
//
// Derived from the sections rather than listed again beside them. Held
// separately, every new row had to be added in two places and the two had
// already drifted -- Memory listed its rows in one order here and another
// there -- with a grouping pass quietly repairing the difference. One list
// cannot disagree with itself, and rows sorted by it are contiguous within a
// section by construction, so no grouping pass is needed.
pub fn system_info_order() -> Vec<&'static str> {
    SYSTEM_INFO_SECTIONS.iter().flat_map(|(_, _, names)| names.iter().copied()).collect()
}

// --- Misc tab.
//
// The CKE/power-down control block, the two SC_GS_CFG command fields and the
// controller feature switches. Sources are the reference timing tool' register map, checked
// against live reads on the Z790 bench; the addresses below are the ones its
// map names, not guesses. That map has been wrong before (see the CLK Drv Dn
// note in the slew block), so anything not confirmed against a second source
// is flagged where it is decoded.
pub const MISC_TAB: &str = "Misc";
pub const SETTINGS_TAB: &str = "Settings";
pub const RTL_TAB: &str = "RTL";

// Misc rows naming a field DDR5 introduced, checked one at a time against
// JESD79-4 rather than dropped as a block. DDR4 does have preamble training
// and both preambles (MR4 bits 10-12), fine granularity refresh (MR3[8:6]),
// Qoff (MR1[12]), TDQS (MR1[11]), DM (MR5[10]), gear-down (MR3[3]) and the
// DRAM's own WR/RTP (MR0[13,11:9]) -- those rows name something the module
// really has, so they stay and read nothing only because this board never
// fills the controller's mode-register table.
//
// The ones below have no DDR4 counterpart at all: the postambles are fixed
// at 0.5 tCK with no register behind them, ECS is DDR5 on-die-ECC scrubbing
// on a part that has no on-die ECC, and the rest are DDR5 additions.
pub const DDR5_ONLY_MISC_ROWS: &[&str] = &[
    "tCL_MR",
    "Read Postamble",
    "Write Postamble",
    "Wide Range",
    "Package Output Driver Test Mode",
    "Read DQS Offset Timing",
    "DQS Interval Timer RT",
    "SRX/NOP Clock-Sync Support",
    "ECS Mode",
    "ECS Reset Counter",
    "ECS Counts",
    "ECS Error Register Index",
    // DDR4's MR2 and MR3 hold other things at these bits, and its MR4 has no
    // refresh-rate field; see MISC_MODE_REGISTER_STATE.
    "Write Leveling",
    "N-Mode",
    "MPSM",
    "CS Assertion Duration",
    "Device 15 MPSM",
    "Internal Write Timing",
    "Write Leveling LB",
    "Write Leveling UB",
    "Minimum Refresh Rate",
    "Refresh Interval Rate Indicator",
    "TUF",
];

// Rows DDR4 does have and this cannot reach. Kept apart from the list above
// so the two never read as the same statement: those fields do not exist on
// DDR4, these do and are not readable, which is the same distinction the
// sensor tab draws for VCCIO.
//
// JESD79-4 puts all three in MR4 -- read preamble training at A10, read
// preamble at A11, write preamble at A12, each selecting one clock or two.
// They are exactly the tRPRE and tWPRE a BIOS exposes, so the fields are as
// real here as on DDR5.
//
// What is missing is the bit positions, not the register: MR4 reads 0x0008 in
// the shadow, and bit 3 is the only one set. Every other mode register decoded
// against something independent -- MR0 against tCL and tWR, MR3 against the
// refresh mode, MR6 against the BIOS tCCD_L -- and MR4 has nothing to check
// against while all three of its preamble bits read zero. A layout that fits
// three zeroes is not a layout that has been confirmed.
//
// One controlled change settles it. Set Read Preamble to 2 tCK in the BIOS and
// reboot: if MR4 becomes 0x0808, A11 is confirmed and all three rows come back
// reading. Until then they say nothing, and a row that says nothing is better
// off the tab.
pub const DDR4_UNREACHABLE_MISC_ROWS: &[&str] = &["Read Preamble Training", "Read Preamble", "Write Preamble"];

// The mirror of the list above: rows DDR4 has a distinct register for and
// DDR5 does not, so on DDR5 they restate a row already on the Timings tab.
//
// Refresh tRFC Mode is the only one. On DDR4 it reads MR3[8:6] -- what the
// DRAM was commanded, five states including the on-the-fly ones -- while the
// Timings row reads DDR_PTM_CTL[3:2] at MCHBAR + 0x5880, what the controller
// decided. Those two can disagree, and seeing that they do is the point.
//
// DDR5 has no separate controller policy register. Both rows resolve to MR4
// bit 4 through the same shadow window: the Timings row searches 0xE600 for
// index 0x04 and reads bit 4 of the payload at 0xE200, which is what
// read_mode_register(0x04) does. Same bit, twice, under two names -- this
// bench prints "FGR" on Timings and "Fine Granularity Refresh"
// on Misc. The Timings row is the one that stays: it heads the Refresh
// timings section and names the interval that applies, directly above the
// tRFC/tRFC2/tRFCpb rows it applies to.
pub const DDR4_ONLY_MISC_ROWS: &[&str] = &["Refresh tRFC Mode"];

/// The Misc rows one memory generation shows, from the full set.
///
/// Pure, and the only place the three gating lists are applied, so what a
/// generation drops can be checked without building the tab.
pub fn misc_rows_for_generation<'a>(names: &[&'a str], generation: DdrGeneration) -> Vec<&'a str> {
    names.iter().copied().filter(|name| !misc_row_dropped(name, generation)).collect()
}

fn misc_row_dropped(name: &str, generation: DdrGeneration) -> bool {
    if generation == DdrGeneration::Ddr4 {
        DDR5_ONLY_MISC_ROWS.contains(&name) || DDR4_UNREACHABLE_MISC_ROWS.contains(&name)
    } else {
        DDR4_ONLY_MISC_ROWS.contains(&name)
    }
}

// Mode-register copies of a controller timing, and the row each sits under.
// tWR_MR and tRTP_MR come from the DDR5 map above and are re-pointed at the
// shadow on DDR4; the other two exist on DDR4 only -- DDR5 keeps CWL and
// tCCD_L in different registers and this project has no verified mapping for
// them there, so inventing the rows on that generation would mean reading a
// register nobody has checked.
pub const DDR4_ONLY_MODE_REGISTER_TIMINGS: [&str; 2] = ["tCWL_MR", "tCCD_L_MR"];

// --- tWR_MR and tRTP_MR belong with the timings they restate.
//
// Both are the mode register's own copy of a timing the controller also holds:
// tWR beside tWR, tRTP beside tRTP. They were built with the rest of the mode
// registers on Misc, which is where they are read from, not where they are
// read for.
//
// Moved after the fact rather than built elsewhere, so they keep the getters
// the Misc builder gave them -- the same decode, and the DDR4 re-point that
// follows it.
// Every mode-register copy of a controller timing, and nothing else. The row
// each sits under is its own name without the suffix, so the list is the only
// place a new one has to be named.
pub const MODE_REGISTER_TIMING_ROWS: &[&str] = &["tCL_MR", "tWR_MR", "tRTP_MR", "tCWL_MR", "tCCD_L_MR"];

// Dropped on Core Ultra 200S, where both decode to "Reserved".
//
// The mode register genuinely holds no value for either here: the field is
// present and its contents are outside the table the DDR5 spec defines for it,
// so the decode is correct and the row still says nothing. A row whose only
// possible reading is "Reserved" is not a blank waiting to be filled -- it is
// a row this platform has no answer for, sitting directly beneath the tWR and
// tRTP it claims to restate, where it reads as a failure of those.
//
// tCWL_MR and tCCD_L_MR are not listed: they are gated off on DDR5 already.
pub const ARROW_LAKE_ABSENT_MODE_REGISTER_ROWS: &[&str] = &["tWR_MR", "tRTP_MR"];

pub const MODE_REGISTER_TIMING_SUFFIX: &str = "_MR";
pub const MODE_REGISTER_TIMING_FALLBACK: (&str, &str) = ("Secondary", "Left");

// --- The refresh arbitration rows read as controller policy, not as timings.
//
// What the controller does about refresh -- when it may skip a per-bank one,
// how many it will bank up before it insists -- rather than how long any of
// them takes. They are read from the same two registers as Rank Idle and
// tRFCpb, which is why they were built beside them.
//
// The complete PBR control block moves together; splitting one register
// between Timings and IMC made neither page describe the policy completely.
pub const REFRESH_POLICY_ROWS: &[&str] = &[
    "Refresh Interval",
    "Refresh Stagger",
    "Refresh Stagger Mode",
    "Stolen Refresh",
    "Refresh Type Display",
    "tREFI Pulse Stagger",
    "Wake Up On HPM",
    "PBR Disable",
    "PBR OOO Disable",
    "PBR Disable On Hot",
    "PBR Exit On Idle",
    "Refresh ABR Release",
    "Refresh HP WM",
    "Refresh Panic WM",
    "Count tREFI While Ref Disabled",
    "HP Refresh On MRS",
    "Self Refresh Exit Debits",
    "Raise Block Wait",
];

// --- Misc section columns.
//
// Inventory and mode-register results read down the first column. Operational
// controls read down the second, so the long latency pair stays together and
// none of the settings blocks is split between sides.
pub const MISC_TAB_COLUMNS: &[(&str, &str)] = &[
    ("Latency CHA", "Left"),
    ("Latency CHB", "Left"),
    ("Mode Registers", "Left"),
    ("Refresh", "Right"),
    ("ECS", "Right"),
    ("Command", "Right"),
    ("Features", "Right"),
    ("Preamble", "Right"),
    ("Power Down", "Right"),
];

// --- Misc section order.
//
// The tab draws its sections in table order, so this is the table order. It is
// declared rather than inherited because the sections are built by several
// passes -- mode-register groups, feature switches, the latency block, the
// refresh policy -- and the order they happened to run in is not a reading
// order. The left-column sequence comes first, followed by the independent
// right-column sequence; the renderer preserves each sequence after splitting
// the rows into their assigned columns.
pub const MISC_SECTION_ORDER: &[&str] = &[
    "Latency CHA",
    "Latency CHB",
    "Mode Registers",
    "Command",
    "Preamble",
    "Refresh",
    "ECS",
    "Features",
    "Power Down",
];

pub const DDR4_INAPPLICABLE_TIMING_ROWS: &[&str] = &["tDLLK", "tCCD_L_WR", "tCCD_L_WR2", "Allow 2cyc B2B LPDDR"];

// --- Training drops the rows that read nothing.
//
// Training is a panel of measured levels, and a row of N/A among them is not
// informative the way a blank timing is: there is no "this is off" reading to
// distinguish, only a level the silicon either states or does not. On Core
// Ultra 200S that is 31 of 85 rows, and four whole categories -- DATA, CMD,
// CLK and CTL -- are entirely unreadable, so they go with them.
//
// Scoped to Core Ultra 200S, and that scope is load-bearing rather than
// cautious. Deleting a row because it reads nothing *now* deletes any row whose
// reader is working but whose register happens to be blank at that instant --
// which is exactly what happened on the DDR4 path, where the collapsed DQ VREF
// row carries the MR6 decoder and reads nothing until a DDR4 module answers.
// It disappeared, decoder and all. On this platform the blank rows are blank
// structurally: 0x2CE8 is unmapped, and the DATA/CMD/CLK/CTL rows have no
// source at all here. Elsewhere that is not established, so nothing is dropped.
pub const BLANK_READINGS: &[&str] = &["N/A", "None", "", "\u{2014}"];

// DDR5's DQ-device, CA/CS VREF and ODTL rows come from the selected module's
// mode-register window. A four-context hardware dump proves the VREF values are
// not shared PHY values, while the descriptor type places ODTL on that same
// context-sensitive path even when both modules happen to hold equal offsets.
// Expose the two module sources before the shared-row split below decides
// which tab owns each row.
pub const DDR5_MODULE_VREF_NAMES: &[&str] = &["DQ VREF D0", "DQ VREF D1", "DQ VREF D2", "DQ VREF D3", "CA VREF", "CS VREF"];

// --- Keep Training strictly per-module.
//
// A module selector only tells the truth when every row beneath it has two
// independent hardware sources.  Rows that expose one controller/PHY value
// used to remain mixed into Training beside genuine A1/B1 readings, which made a
// shared result look as though it belonged to the selected DIMM.  Preserve
// their existing three-column grouping on a separate PHY tab and leave Training
// with a simple invariant: every displayed row is dual-source.
pub const IMC_TAB: &str = "IMC";

// --- Keep Misc strictly per-module too.
//
// The reference row table distinguishes mode-register fields (type 5) from
// fixed controller-register fields (type 6). The former are evaluated in the
// selected DIMM context and therefore have independent A1/B1 sources. The
// latter are one controller/PHY setting; the four-column dump repeats the
// same fixed register result for every DIMM context. Misc retains only the
// mode-register rows and its explicitly paired MC0/MC1 latency rows. Shared
// refresh, command, feature and power-down controls move to Settings.
pub const SETTINGS_TAB_COLUMNS: &[(&str, &str)] =
    &[("Refresh", "Left"), ("Features", "Left"), ("Command", "Right"), ("Power Down", "Right")];

// --- Final Intel tab composition.
//
// RTL remains an exhaustive register view: MC0 and MC1 are both visible at
// once, stacked under the two physical channel headings. Per-DIMM
// mode-register data joins Training,
// whose selector already represents A1/B1. Shared controller settings join
// PHY, which deliberately has no module selector. The four-column PHY layout
// keeps all 85 shared rows visible without a scroll canvas.
pub const SKEW_MISC_COLUMNS: &[(&str, &str)] = &[
    ("RTT", "Left"),
    ("ODT", "Left"),
    ("RON", "Left"),
    // Above Command: the first column ends with Power Down, and the middle
    // one with Features and Refresh.
    ("DLL / LATENCY", "Right"),
    ("DATA CONTROL", "Left"),
    ("MR0 / MR1", "Left"),
    ("MR5 / MR6", "Left"),
    // The ODT timing groups read straight on from RTT, ODT and RON; VREF
    // heads the middle column, above the DFE taps it is trained with.
    ("ODT DELAY", "Left"),
    ("DFE", "Middle"),
    ("VREF", "Middle"),
    ("ODTL", "Left"),
    ("MPR / ACCESS", "Right"),
    ("MR2 / MR3", "Middle"),
    ("Command", "Right"),
    ("PARITY / CRC", "Right"),
    ("REFRESH / POWER", "Right"),
    ("PREAMBLE / PPR", "Right"),
    ("Mode Registers", "Right"),
    ("DQS", "Right"),
    ("Preamble", "Right"),
    ("ECS", "Right"),
    ("MR4", "Right"),
];

pub const PHY_SETTINGS_COLUMNS: &[(&str, &str)] = &[
    ("VREF", "Left"),
    ("Command", "Left"),
    // The added refresh controls make the left half taller; ODTL balances the
    // fixed-height two-column page without splitting any section.
    ("ODTL", "Right"),
    ("Refresh", "Left"),
    // Keep the signal-drive groups in one readable sequence: DATA above CMD,
    // then CLK, CTL, and finally SComp.
    ("DATA", "Right"),
    ("SComp", "Right"),
    ("CMD", "Right"),
    ("CLK", "Right"),
    ("CTL", "Right"),
    ("PHY Control", "Right"),
    ("Features", "Right"),
    ("Power Down", "Left"),
    ("MR0 / MR1", "Middle"),
    ("MR2 / MR3", "Middle"),
    ("MR4", "Right"),
    ("MR5 / MR6", "Left"),
];

// --- Additional Raptor Lake DDR4 controller fields.
//
// These descriptors were recovered from the supplied reference executable
// and checked against the matching live dump on the ASUS Z790-A D4. A direct
// read on the same boot reproduced every value, including the 64-bit fields
// above bit 31. Raptor Lake DDR5 carries them at the same places; see
// DDR5_CONFIRMED_ADDITIONAL_GROUPS for how that was established.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdditionalFormula {
    Enabled,
    StaggerMode,
    InvertedDisable,
}

pub const DDR4_ADDITIONAL_REFRESH_FIELDS: &[(&str, u64, u32, i32, Option<AdditionalFormula>)] = &[
    ("Refresh Interval", 0xE444, 0, 13, None),
    ("Refresh Stagger", 0xE444, 15, 1, Some(AdditionalFormula::Enabled)),
    ("Refresh Stagger Mode", 0xE444, 16, 1, Some(AdditionalFormula::StaggerMode)),
    ("Stolen Refresh", 0xE444, 13, 1, Some(AdditionalFormula::InvertedDisable)),
    ("Refresh Type Display", 0xE444, 14, 1, Some(AdditionalFormula::Enabled)),
    ("tREFI Pulse Stagger", 0xE444, 17, 1, Some(AdditionalFormula::InvertedDisable)),
    ("Wake Up On HPM", 0xE444, 19, 13, None),
];

pub const DDR4_ADDITIONAL_COMMAND_FIELDS: &[(&str, u64, u32, i32, &str)] = &[
    // SC_GS_CFG is 64 bits; both fields are in its upper dword.
    ("Write 0", 0xE088, 49, 1, "wide"),
    ("Multi-Cycle Command", 0xE088, 51, 1, "wide"),
];

// DDR5 names Multi-Cycle the way the reference tool does; Write 0 stays a
// count.
pub const DDR5_ADDITIONAL_COMMAND_FORMULAS: &[(&str, AdditionalFormula)] =
    &[("Multi-Cycle Command", AdditionalFormula::Enabled)];

pub const DDR4_ADDITIONAL_POWER_DOWN_FIELDS: &[(&str, u64, u32, i32)] = &[
    // MC_INIT_STATE_G: the reference executable descriptor is 0x4278[12].
    // On Raptor Lake DDR4 that maps to MCHBAR 0xE278 bit 12 and reads 0 on
    // both live controllers, matching the supplied DDR4 reference dump.
    ("Add 1 QCLK Delay", 0xE278, 12, 1),
];

// DDR5 takes Add 1 QCLK Delay from the reference executable's Raptor Lake
// table, which puts it at 0xE478[12] -- the register Dec tCWL is confirmed in,
// by the same 0x4278 -> 0xE478 step. Whether DDR4's 0xE278 above should follow
// is for a DDR4 bench to settle; both read 0 there.
pub const DDR5_ADDITIONAL_POWER_DOWN_FIELDS: &[(&str, u64, u32, i32)] = &[("Add 1 QCLK Delay", 0xE478, 12, 1)];

pub const DDR4_ADDITIONAL_CWL_FIELDS: &[(&str, u64, u32, i32)] = &[("Add tCWL", 0xE478, 6, 6), ("Dec tCWL", 0xE478, 0, 6)];

pub const DDR4_ADDITIONAL_PHY_FIELDS: &[(&str, u64, u32, i32)] = &[
    ("Weak Lock End Delay", 0x01AC, 8, 5),
    ("SCR DLL Enable Timer", 0x2D1C, 13, 10),
    ("SCR PI Enable Timer", 0x2D20, 0, 11),
];

// Raptor Lake DDR5 carries all of them at the same places. The reference
// executable's own Raptor Lake field table -- read out of the binary, whose
// 0xE4xx records match every field this project already had confirmed, tREFIx9
// at 0xE438[31:24] and tRFCpb at 0xE488[20:10] among them -- names
// 0xE444[12:0], [15], [16], [13], [14], [17] and [31:19] for the seven refresh
// rows, 0xE088[49] and [51] for Write 0 and Multi-Cycle, and 0xE478[5:0],
// [11:6] and [12] for Dec tCWL, Add tCWL and Add 1 QCLK Delay. The "DDR5 table"
// that moves refresh bits is its other one, with 0x42xx offsets, for another
// platform.
//
// Read on the Z790 MPOWER on the reference dump's boot, every one decodes to
// what that tool shows: Dec tCWL 3, the PHY fields 2, 200 and 1200, and the
// refresh, command and power-down fields their zeroes and Enabled/Per DIMM.
pub const DDR5_CONFIRMED_ADDITIONAL_GROUPS: &[&str] = &["refresh", "command", "power_down", "cwl", "phy"];

// --- A register each module has its own copy of belongs on Training.
//
// The 0xE000 scheduler block has a copy in each module's controller. The
// Timings tab already reads it at CHANNEL_B -- tRDRD from 0xE00C, tREFIx9 and
// Rank Idle from 0xE438, tRFCpb from 0xE488 -- and every IMC field in the block
// reads back from B1's controller as well. Showing those fields as one value
// from A1 hid the second module's reading, so they get both reads and join
// Training, where every row is dual-source.
//
// So does the 0xD800 block where B1 is a whole controller away: Error
// Correction (0xD804) and Self Refresh (0xD860) read 0x100 and 0x10200 in both
// controllers on the Z790 MPOWER. A sub-channel step from there lands on the
// scheduler block instead -- 0xD804 + 0x800 is tRCD's 0xE004 -- so on such a
// platform they stay single.
//
// Nothing else. The DDRIO/PHY block below 0x4000, 0x3E00 and 0x5E00 read 0 or
// 0xFFFFFFFF in MC1's window: one PHY and one global block for both modules,
// which stay on IMC as the one value they are.
pub const CHANNEL_REGISTER_BLOCK: std::ops::Range<u64> = 0xE000..0xE800;
pub const CONTROLLER_REGISTER_BLOCK: std::ops::Range<u64> = 0xD800..0xD900;

// Kept apart from the mode-register groups, which stay where they were. Power
// Down closes the first column under the ODT groups; Features and Refresh
// follow the DFE taps in the middle. That leaves 43 lines in each of the
// three, the third being full of mode registers since MR2, MR3 and MR4.
pub const TRAINING_CHANNEL_REGISTER_COLUMNS: &[(&str, &str)] =
    &[("Command", "Right"), ("Refresh", "Middle"), ("Features", "Middle"), ("Power Down", "Left")];

// DDR5's IMC, once those rows have left it: VREF alone would be a third the
// height of the signal groups beside it, so PHY Control and Features join it.
pub const DDR5_PHY_SETTINGS_COLUMNS: &[(&str, &str)] = &[("PHY Control", "Left"), ("Features", "Left")];

// --- Intel signal presentation.
//
// Keep the register reads and lookup codes above as the source of every value;
// this final presentation pass only gives the decoded settings the same names
// and resistance-first notation used by the reference viewer. Mutating the shared
// formula dictionaries also keeps Summary and Training on one decoder.
pub const REFERENCE_SIGNAL_NAMES: &[(&str, &str)] = &[
    ("RTT WR", "RTT Wr"),
    ("RTT NOM", "RTT Nom"),
    ("RTT PARK", "RTT Park"),
    ("RTT NOM WR", "RTT Nom Wr"),
    ("RTT NOM RD", "RTT Nom Rd"),
    ("RTT PARK DQS", "RTT Park Dqs"),
    ("RTT LOOPBACK", "RTT Loopback"),
    ("CK ODT GROUP A", "CK ODT Group A"),
    ("CS ODT GROUP A", "CS ODT Group A"),
    ("CA ODT GROUP A", "CA ODT Group A"),
    ("CK ODT GROUP B", "CK ODT Group B"),
    ("CS ODT GROUP B", "CS ODT Group B"),
    ("CA ODT GROUP B", "CA ODT Group B"),
    ("PULL UP", "Pull Up Drv"),
    ("PULL DN", "Pull Down Drv"),
];

pub const REFERENCE_VREF_NAMES: &[(&str, &str)] = &[
    ("WrDS Up", "Dq Vref Up"),
    ("WrDS Dn", "Dq Vref Dn"),
    ("RdODT Up", "Dq Odt Vref Up"),
    ("RdODT Dn", "Dq Odt Vref Dn"),
    ("WrDSCmd Up", "Cmd Vref Up"),
    ("WrDSCmd Dn", "Cmd Vref Dn"),
    ("WrDSCtl Up", "Ctl Vref Up"),
    ("WrDSCtl Dn", "Ctl Vref Dn"),
    ("WrDSClk Up", "Clk Vref Up"),
    ("WrDSClk Dn", "Clk Vref Dn"),
    ("WrDSCke CS Up", "CkeCs Vref Up"),
    ("QXCOUNT", "QX Count"),
    ("DQ VREF D0", "DQ VREF 0"),
    ("DQ VREF D1", "DQ VREF 1"),
    ("DQ VREF D2", "DQ VREF 2"),
    ("DQ VREF D3", "DQ VREF 3"),
];

pub const REFERENCE_SIGNAL_ORDER: &[(&str, &[&str])] = &[
    ("RTT", &["RTT Wr", "RTT Nom Rd", "RTT Nom Wr", "RTT Park", "RTT Park Dqs", "RTT Loopback"]),
    (
        "ODT",
        &["CA ODT Group A", "CS ODT Group A", "CK ODT Group A", "CA ODT Group B", "CS ODT Group B", "CK ODT Group B"],
    ),
    ("RON", &["Pull Up Drv", "Pull Down Drv"]),
];

static RZQ_LABEL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(RZQ(?:/[0-9.]+)?) \(([0-9]+)\)$").expect("valid pattern"));

/// Convert a decoded RZQ label to resistance-first notation.
pub fn resistance_first_rzq_text(value: &str) -> String {
    if value == "RTT_OFF" {
        return "0 RZQ OFF".into();
    }
    match RZQ_LABEL.captures(value) {
        Some(found) => format!("{} {}", &found[2], &found[1]),
        None => value.to_string(),
    }
}

pub const DDR4_TRAINING_SECTION_ROWS: &[(&str, &[&str])] = &[
    (
        "DLL / LATENCY",
        &["Add tCWL", "Dec tCWL", "CAS Latency", "DLL Reset", "DLL Enable", "Additive Latency", "Write Leveling"],
    ),
    ("DATA CONTROL", &["Data Output Disable", "TDQS Enable", "DM Enable", "ODT Buffer (PD)", "Write DBI", "Read DBI"]),
    ("VREF", &["VrefDQ Train Value", "VrefDQ Train Range", "VrefDQ Train Enable"]),
    (
        "Command",
        &["Burst Length", "Read Burst Type", "Test Mode", "Write CMD Latency", "CS Geardown", "CS to CMD Latency"],
    ),
    ("MPR / ACCESS", &["MPR Page Select", "MPR Operation", "MPR Read Format", "Per DRAM Addr"]),
    (
        "PARITY / CRC",
        &["Write CRC", "CA Parity Latency", "CRC Error Clear", "CA Parity Err Status", "CA Parity Persist Err"],
    ),
    (
        "REFRESH / POWER",
        &[
            "Refresh tRFC Mode", "Low Power ASR", "Max Power Down", "Temp Refresh Range", "Temp Ctrl Refresh",
            "Internal Vref Mon", "Self Refresh Abort", "Temp Sensor Readout",
        ],
    ),
    ("PREAMBLE / PPR", &["Soft PPR", "Read Preamble Train", "Read Preamble", "Write Preamble", "Hard PPR"]),
];

pub fn ddr4_training_category_by_name(name: &str) -> Option<&'static str> {
    // A later section claiming the same name wins, as a dict comprehension
    // over the sections does.
    DDR4_TRAINING_SECTION_ROWS
        .iter()
        .rev()
        .find(|(_, names)| names.contains(&name))
        .map(|(category, _)| *category)
}

pub const DDR4_TRAINING_SECTION_ORDER: &[&str] = &[
    "RTT", "ODT", "RON", "ODT DELAY", "VREF", "DLL / LATENCY", "DATA CONTROL", "DFE", "ODTL", "Command",
    "MPR / ACCESS", "PARITY / CRC", "REFRESH / POWER", "PREAMBLE / PPR",
];

pub const DDR4_TRAINING_TWO_COLUMN_COLUMNS: &[(&str, &str)] = &[
    ("RTT", "Left"),
    ("ODT", "Left"),
    ("RON", "Left"),
    ("DLL / LATENCY", "Left"),
    ("DATA CONTROL", "Left"),
    ("ODT DELAY", "Left"),
    ("DFE", "Left"),
    ("VREF", "Left"),
    ("ODTL", "Left"),
    ("Command", "Right"),
    ("MPR / ACCESS", "Right"),
    ("PARITY / CRC", "Right"),
    ("REFRESH / POWER", "Right"),
    ("PREAMBLE / PPR", "Right"),
];

pub const DDR4_IMC_SECTION_ORDER: &[&str] = &[
    "VREF", "Command", "Refresh", "Power Down", "DATA", "CMD", "CLK", "CTL", "SComp", "ODTL", "PHY Control", "Features",
];

// --- LGA1700 DDR5, read at a glance.
//
// Everything below is display only and LGA1700 DDR5 only: rows dropped, values
// reworded and rows relabelled, per row rather than in the shared decode
// tables, which DDR4 and the other platforms read too. It runs once, last,
// after every pass that installs, moves or regroups a row, so no row it names
// can be rebuilt behind it.

// Training rows that only say what the DRAM is able to do: none can be tuned,
// none changes, and on the bench they read Not Supported, Not implemented and
// Not supported. Their names were also the widest on the tab, which is what
// held Training at 1087px.
pub const DDR5_TRAINING_REMOVED: &[&str] = &[
    "Wide Range",
    "Package Output Driver Test Mode",
    "Refresh Interval Rate Indicator",
    "SRX/NOP Clock-Sync Support",
];

// Labels, the rows keeping their own names for everything keyed on them:
//
// QCLK Ratio reads 100.00 or 133.33 MHz, the memory PLL reference the ratio
//   counts in, not the ratio, which is DRAM Ratio.
// DDR5 calls the short column-to-column delay tCCD_S, beside the tCCD_L,
//   tCCD_L_WR and tCCD_L_WR2 rows under it.
// MR14 bit 7 switches manual ECS, so "ECS Mode: Disabled" read as ECS off.
// The slew row carried the register field's squeezed name, CmdSlewStatLegEn.
// Realtime Memory is Intel's Realtime Memory Timing, as the BIOS names it.
pub const DDR5_DISPLAY_NAMES: &[(&str, &str)] = &[
    ("QCLK Ratio", "QCLK Reference"),
    ("tCCD", "tCCD_S"),
    ("ECS Mode", "Manual ECS"),
    ("CMD SlewStatlegen", "CMD Slew Static Leg"),
    ("Realtime Memory", "Realtime Memory Timing"),
];

// The same readings in fewer words, on Training. "0 RZQ OFF" puts a
// resistance on a termination that is off. The preambles keep their bit
// pattern only where two share a length -- the read preamble's two 2 tCK
// settings.
pub const DDR5_BRIEF_VALUES: &[(&str, &str)] = &[
    ("0 RZQ OFF", "Off"),
    ("1 tCK - 10 Pattern", "1 tCK"),
    ("2 tCK - 0010 Pattern", "2 tCK (0010)"),
    ("2 tCK - 1110 Pattern", "2 tCK (1110)"),
    ("3 tCK - 000010 Pattern", "3 tCK"),
    ("4 tCK - 00001010 Pattern", "4 tCK"),
    ("0.5 tCK - 0 Pattern", "0.5 tCK"),
    ("1.5 tCK - 010 Pattern", "1.5 tCK"),
    ("Manual ECS Mode Disabled", "Disabled"),
    ("Manual ECS Mode Enabled", "Enabled"),
    ("ECS counts Rows with errors", "Rows"),
    ("ECS counts Code words with errors", "Code Words"),
    ("Timer Stops via MPC Command", "MPC Command"),
];
// Row by row, where a value reads differently on one row. The write preamble
// has one setting per length, so no pattern is needed to tell its 2 tCK from
// another. The slew row is a one-bit switch that read as a bare 1.
pub const IMC_SWITCH_WORDS: &[(&str, &str)] = &[("0", "Disabled"), ("1", "Enabled")];
pub const DDR5_BRIEF_VALUES_BY_ROW: &[(&str, &[(&str, &str)])] =
    &[("Write Preamble", &[("2 tCK - 0010 Pattern", "2 tCK")]), ("CMD SlewStatlegen", IMC_SWITCH_WORDS)];

static DQS_TIMER_STOP: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^Timer Stops at (\d+)(?:st|nd|rd|th) clocks$").expect("valid pattern"));

/// A one-bit reading as Enabled or Disabled; anything else as it came.
pub fn imc_switch_text(value: &str) -> String {
    IMC_SWITCH_WORDS
        .iter()
        .find(|(k, _)| *k == value.trim())
        .map(|(_, v)| v.to_string())
        .unwrap_or_else(|| value.to_string())
}

/// `value` as LGA1700 DDR5 words it, on row `row`.
pub fn brief_ddr5_value(value: &str, row: Option<&str>) -> String {
    // A row's own words first, matched on the text a number shows as too:
    // the slew bit reads back as 1, not "1".
    if let Some((_, own)) = DDR5_BRIEF_VALUES_BY_ROW.iter().find(|(name, _)| Some(*name) == row)
        && let Some((_, brief)) = own.iter().find(|(k, _)| *k == value.trim()) {
            return brief.to_string();
        }
    if let Some(found) = DQS_TIMER_STOP.captures(value) {
        return format!("{} clocks", &found[1]);
    }
    DDR5_BRIEF_VALUES
        .iter()
        .find(|(k, _)| *k == value)
        .map(|(_, v)| v.to_string())
        .unwrap_or_else(|| value.to_string())
}

/// A decode table as the rows share it.
type SharedTable = Arc<BTreeMap<u64, String>>;

fn lookup_str(table: &[(&str, &str)], key: Option<&str>) -> Option<String> {
    let key = key?;
    table.iter().find(|(k, _)| *k == key).map(|(_, v)| v.to_string())
}

impl Build {
    pub(crate) fn install_refresh_mode_row(&mut self) {
        let Some(index) = self
            .rows
            .iter()
            .position(|t| t.is("name", REFRESH_MODE_SOURCE.0) && t.is("Category", REFRESH_MODE_SOURCE.1))
        else {
            return;
        };
        let mut row = self.rows.remove(index);
        row.set_str("name", REFRESH_MODE_NAME);
        row.set_str("Category", REFRESH_POLICY_CATEGORY);
        row.set_str("Tab", "Misc");
        row.set_str("Column", "Left");
        // DDR5 Refresh Mode is a type-5 mode-register field. DDR4's
        // DDR_PTM_CTL form is instead one fixed controller-policy source.
        row.set_str(
            "source_scope",
            if detect_ddr_generation() == DdrGeneration::Ddr5 { "module" } else { "controller" },
        );
        self.rows.push(row);
    }

    pub(crate) fn install_refresh_mode_dimming(&mut self) {
        for timing in self.rows.iter_mut() {
            if timing.name().is_some_and(|name| REFRESH_MODE_DIMMED_ROWS.contains(&name)) {
                timing.set("dim", V::Pred(Arc::new(refresh_is_normal)));
            }
        }
    }

    /// Move rows out of Power down into the section that describes them.
    pub(crate) fn install_timings_sections(&mut self) {
        for timing in self.rows.iter_mut() {
            if !timing.is("Tab", "Timings") {
                continue;
            }
            if let Some(category) = lookup_str(TIMINGS_SECTION_MOVES, timing.name()) {
                timing.set_str("Category", &category);
            }
        }
    }

    /// Keep each section's rows together so it draws as one block.
    ///
    /// The tab is built by walking its rows in order and starting a new block
    /// every time the category changes, so a category whose rows are not
    /// contiguous renders as two headings with the same name -- Refresh timings
    /// appeared three times, and Power down and Other Timings twice each, after
    /// the pass above re-categorised rows without moving them.
    ///
    /// Categories keep the order they first appear in and rows keep their order
    /// within a category; only the gaps close. Section order on the page is
    /// decided later, by TIMINGS_SECTION_ORDER.
    pub(crate) fn group_timings_sections(&mut self) {
        let positions: Vec<usize> =
            self.rows.iter().enumerate().filter(|(_, t)| t.is("Tab", "Timings")).map(|(i, _)| i).collect();
        if positions.is_empty() {
            return;
        }
        let mut order: Vec<Option<String>> = Vec::new();
        for &index in &positions {
            let category = self.rows[index].category().map(str::to_string);
            if !order.contains(&category) {
                order.push(category);
            }
        }
        let category_rank = |t: &T| order.iter().position(|c| c.as_deref() == t.category()).expect("seen above");
        let row_rank = |t: &T| {
            let names = TIMINGS_SECTION_ROW_ORDER.iter().find(|(c, _)| Some(*c) == t.category()).map(|(_, n)| *n);
            match names {
                Some(names) => names.iter().position(|n| Some(*n) == t.name()).unwrap_or(names.len()),
                None => 0,
            }
        };
        super::dict::sort_in_place(&mut self.rows, &positions, |t| (category_rank(t), row_rank(t)));
    }

    pub(crate) fn install_timings_column_layout(&mut self) {
        for timing in self.rows.iter_mut() {
            if !timing.is("Tab", "Timings") {
                continue;
            }
            if let Some(column) = lookup_str(TIMINGS_TAB_COLUMNS, timing.category()) {
                timing.set_str("Column", &column);
            }
        }
    }

    fn system_info_row(name: &str, value: V, display_name: Option<&str>) -> T {
        let mut row = T::new()
            .s("name", name)
            .with("value", value)
            .s("Category", "General")
            .s("Tab", SYSTEM_INFO_TAB)
            .s("Column", "Left")
            .s("read_type", "standard");
        if let Some(display) = display_name {
            row.set_str("display_name", display);
        }
        row
    }

    /// Insert rows beside an existing System Info row.
    ///
    /// Position is taken from a named neighbour rather than an index, so an
    /// earlier pass reordering the tab carries these rows with it. If the
    /// neighbour is gone the rows still appear, at the end.
    fn place_system_info_rows(&mut self, anchor: &str, rows: Vec<T>, after: bool) -> bool {
        if let Some(index) = self.rows.iter().position(|t| t.is("name", anchor) && t.is("Tab", SYSTEM_INFO_TAB)) {
            let at = if after { index + 1 } else { index };
            self.rows.splice(at..at, rows);
            return true;
        }
        self.rows.extend(rows);
        false
    }

    pub(crate) fn install_system_info_identity_rows(&mut self) {
        let platform = self.platform;
        let row = Self::system_info_row;
        // What the machine is, above the CPU it is built on.
        self.place_system_info_rows(
            "CPU",
            vec![
                row("Platform", lazy(move || get_platform_name(platform)), None),
                row("OS", lazy(get_os_edition), None),
                row("OS Version", lazy(get_os_version), None),
            ],
            false,
        );
        self.place_system_info_rows("Cores / Threads", vec![row("GPU", lazy(get_gpu_name), None)], true);
        // What the silicon is, beside the name it is sold under.
        self.place_system_info_rows(
            "CPU",
            vec![
                row("CPU Package", lazy(get_cpu_package), Some("Package")),
                row("CPU Signature", lazy(get_cpu_signature), Some("Family / Model / Step")),
                row("Code Name", lazy(get_cpu_codename), None),
                row("Technology", lazy(get_cpu_technology), None),
            ],
            true,
        );
        // The board's own silicon, under the board it sits on.
        self.place_system_info_rows(
            "BIOS Date",
            vec![
                row("Chipset", lazy(get_chipset_name), None),
                row("Southbridge", lazy(get_southbridge_name), None),
                row("LPCIO", lazy(get_lpcio_name), None),
            ],
            true,
        );
        self.place_system_info_rows("Channels", vec![row("DRAM Technology", lazy(get_memory_type), None)], false);
        let mut gpu_rows: Vec<T> = GPU_ROWS
            .iter()
            .map(|(label, field)| {
                let field: &'static str = field;
                row(label, lazy(move || gpu_field(field)), None)
            })
            .collect();
        gpu_rows.push(row("Driver Date", lazy(get_gpu_driver_date), None));
        self.place_system_info_rows("GPU", gpu_rows, true);
        if platform == Platform::Lga1700Ddr5 {
            // Enabled cores by type, under the count they make up. Placed here;
            // SYSTEM_INFO_SECTIONS puts every row in its final order.
            self.place_system_info_rows("Cores / Threads", vec![row("Core Types", lazy(get_core_types), None)], true);
            let ddr5_gpu: Vec<T> = DDR5_GPU_ROWS
                .iter()
                .map(|(label, field)| {
                    let field: &'static str = field;
                    row(label, lazy(move || gpu_field(field)), None)
                })
                .collect();
            self.place_system_info_rows("GPU", ddr5_gpu, true);
        }
        // Beside the firmware version it dates, and the socket count beside the
        // modules filling them.
        self.place_system_info_rows("BIOS", vec![row("BIOS Date", lazy(get_bios_date), None)], true);
        self.place_system_info_rows("Model", vec![row("Board Revision", lazy(get_motherboard_version), None)], true);
        self.place_system_info_rows("Memory Capacity", vec![row("Slots Used", lazy(get_slots_used), None)], true);
        // What is installed, next to the total it adds up to.
        self.place_system_info_rows(
            "Memory Capacity",
            vec![row("DIMM Size", lazy(|| dimm_field("size")), None), row("Rank", lazy(|| dimm_field("rank")), None)],
            true,
        );
    }

    pub(crate) fn install_system_info_order(&mut self) {
        for timing in self.rows.iter_mut() {
            if timing.is("Tab", SYSTEM_INFO_TAB) && timing.name().is_some_and(|n| SYSTEM_INFO_SUMMARY_ONLY.contains(&n)) {
                timing.set_str("Tab", "Summary");
            }
        }

        let ddr5 = self.platform == Platform::Lga1700Ddr5;
        let removed = |name: &str| SYSTEM_INFO_REMOVED.contains(&name) || (ddr5 && DDR5_SYSTEM_INFO_REMOVED.contains(&name));
        self.rows.retain(|t| !(t.is("Tab", SYSTEM_INFO_TAB) && t.name().is_some_and(removed)));

        if !self.rows.iter().any(|t| t.is("Tab", SYSTEM_INFO_TAB)) {
            return;
        }
        let rows = std::mem::take(&mut self.rows);
        let mut rebuilt: Vec<Option<T>> = Vec::new();
        // {name: [rows]}, in the order each name first appears.
        let mut remaining: Vec<(Option<String>, Vec<T>)> = Vec::new();
        let mut placed = false;
        for timing in rows {
            if !timing.is("Tab", SYSTEM_INFO_TAB) {
                rebuilt.push(Some(timing));
                continue;
            }
            if !placed {
                rebuilt.push(None);
                placed = true;
            }
            let name = timing.name().map(str::to_string);
            match remaining.iter_mut().find(|(n, _)| *n == name) {
                Some((_, group)) => group.push(timing),
                None => remaining.push((name, vec![timing])),
            }
        }
        let mut ordered: Vec<T> = Vec::new();
        for name in system_info_order() {
            if let Some(index) = remaining.iter().position(|(n, _)| n.as_deref() == Some(name)) {
                ordered.extend(remaining.remove(index).1);
            }
        }
        // A row the order does not mention keeps a place at the end rather than
        // vanishing, so one added later shows up somewhere visible instead of
        // being dropped without trace.
        for (_, leftover) in remaining {
            ordered.extend(leftover);
        }
        let mut ordered = Some(ordered);
        for slot in rebuilt {
            match slot {
                Some(timing) => self.rows.push(timing),
                None => self.rows.extend(ordered.take().expect("placed once")),
            }
        }
    }

    /// Give each System Info row its section and column.
    pub(crate) fn install_system_info_sections(&mut self) {
        for timing in self.rows.iter_mut() {
            if !timing.is("Tab", SYSTEM_INFO_TAB) {
                continue;
            }
            if timing.name().is_some_and(|n| SPD_SYSTEM_ROWS.contains(&n)) {
                timing.set_str("Tab", "SPD");
                timing.set_str("Category", "System");
                timing.set_str("Column", "FullWidth");
                continue;
            }
            let section =
                SYSTEM_INFO_SECTIONS.iter().find(|(_, _, names)| timing.name().is_some_and(|n| names.contains(&n)));
            // A row nobody placed keeps the old heading rather than vanishing,
            // which is how it stays visible long enough to be placed properly.
            if let Some((title, column, _)) = section {
                timing.set_str("Category", title);
                timing.set_str("Column", column);
            }
        }
    }

    fn misc_row(name: &str, category: &str, column: &str, value: V, source_scope: &str) -> T {
        // Source scope is intentionally explicit. The reference descriptor table uses
        // type 5 for a mode-register field evaluated in the selected DIMM context
        // and type 6 for a fixed controller-register offset. Equal live values do
        // not turn two mode-register reads into one source, and repeated dump
        // columns do not turn one controller register into four sources.
        T::new()
            .s("name", name)
            .s("Category", category)
            .s("Tab", MISC_TAB)
            .s("Column", column)
            .with("value", value)
            .s("read_type", "standard")
            .s("source_scope", source_scope)
    }

    /// Bind a base-taking reader to channel A.
    fn channel_a(read: &BaseReaderFn) -> V {
        let read = read.clone();
        lazy(move || read(MCHBAR))
    }

    /// Add the Misc tab: CKE/power-down control, command config, features.
    ///
    /// The map was built on Alder/Raptor Lake, and Arrow Lake moved several of
    /// these registers -- CMD_STRETCH alone went from two bits to one -- so the
    /// same offsets there read plausible numbers that mean something else. That
    /// hazard belongs to the source, not to the tab, so the two sources are
    /// split rather than the whole tab being withheld.
    ///
    /// Rows read through the mode-register pointer path are installed on every
    /// platform. That path resolves the payload at 0xE200 instead of naming a
    /// fixed offset, it is the one the DFE rows already use on Arrow Lake, and
    /// on a Z890 bench all five of its display rows matched the reference timing tool on the
    /// same boot: read and write preamble 4 tCK, read postamble 0.5 tCK, write
    /// postamble 1.5 tCK, burst length BC8 OTF.
    ///
    /// Rows naming an MCHBAR offset directly -- the CKE/power-down block, the
    /// two SC_GS_CFG command fields and the feature switches -- stay on the
    /// platforms the map was built for. On that same bench SC_GS_CFG shows why:
    /// read with the Alder Lake two-bit field it reports CMD_STRETCH 1 while the
    /// command rate is 2N. The feature switches are no loss there either, since
    /// System Info already carries those from offsets confirmed for it.
    pub(crate) fn install_misc_tab(&mut self) {
        let _arrow_lake = is_arrow_lake_platform();

        // Every row carries a getter, not a reading. These registers change while
        // the tool is open -- the CKE block and the feature switches follow what
        // the controller is doing -- so a value resolved here would be a snapshot
        // of startup that never moved again.
        //
        // Rows start with a channel-A-compatible getter so older consumers remain
        // valid. The source split below promotes type-5 mode-register rows to
        // independent A1/B1 getters and moves fixed type-6 controller registers
        // into Settings; the ones in the per-channel block go on to Training.
        let cke = |bit_start: u32, bit_length: u32| {
            base_reader(move |base| Some(registers::misc_number(MISC_CKE_CONFIG_OFFSET, bit_start, bit_length, Some(base))))
        };
        let gs = |bit_start: u32, bit_length: u32| {
            base_reader(move |base| Some(registers::misc_number(MISC_GS_CONFIG_OFFSET, bit_start, bit_length, Some(base))))
        };
        let switch = |offset: u64, bit_start: u32, bit_length: u32, inverted: bool| {
            base_reader(move |base| Some(registers::misc_switch(offset, bit_start, bit_length, inverted, Some(base))))
        };
        // Build one type-5 row with independent A1/B1 readers.
        let module_row = |name: &str, category: &str, column: &str, read: BaseReaderFn| {
            Self::misc_row(name, category, column, Self::channel_a(&read), "module").with("base_reader", V::Base(read))
        };
        // Build one fixed-offset row, keeping what a channel-B read needs.
        //
        // Shown from channel A until _move_channel_registers_to_training looks
        // at the offset: only the per-channel block has a channel-B copy.
        let register_row = |name: &str, category: &str, column: &str, offset: u64, read: BaseReaderFn| {
            Self::misc_row(name, category, column, Self::channel_a(&read), "controller")
                .with("base_reader", V::Base(read))
                .with("register_offset", V::Int(offset))
        };
        let mode_register = |number: u64, bit_start: u32, bit_length: u32, decode: Option<Table>| {
            base_reader(move |base| {
                Some(registers::misc_mode_register_value(number, bit_start, bit_length, decode, Some(base)))
            })
        };
        let mode_rows = |fields: &[ModeRegisterField], category: &str, column: &str| -> Vec<T> {
            fields
                .iter()
                .map(|(name, number, bit_start, bit_length, decode)| {
                    module_row(name, category, column, mode_register(*number, *bit_start, *bit_length, *decode))
                })
                .collect()
        };

        // Every Training and Misc row the LGA1700 DDR5 table carries is installed on
        // Core Ultra 200S too, by request, so the two platforms show the same rows
        // and a row that reads nothing says so rather than being absent. A missing
        // row and an N/A row look identical on screen but mean opposite things --
        // "this platform has no such setting" against "this setting was not read".
        //
        // The guard these groups used to sit behind is gone rather than inverted:
        // the registers are read, and whatever they answer is what shows.
        let mut rows: Vec<T> = Vec::new();
        rows.extend(MISC_CKE_CONFIG_FIELDS.iter().map(|(name, bit_start, bit_length)| {
            register_row(name, "Power Down", "Right", MISC_CKE_CONFIG_OFFSET, cke(*bit_start, *bit_length))
        }));
        rows.extend(MISC_GS_CONFIG_FIELDS.iter().map(|(name, bit_start, bit_length)| {
            register_row(name, "Command", "Right", MISC_GS_CONFIG_OFFSET, gs(*bit_start, *bit_length))
        }));
        // Burst Length sits with the command configuration, where the reference
        // tools list it, even though it comes from a mode register.
        let (number, bit_start, bit_length, decode) = MISC_BURST_LENGTH_FIELD;
        rows.push(module_row("Burst Length", "Command", "Right", mode_register(number, bit_start, bit_length, Some(decode))));
        rows.extend(mode_rows(MISC_MODE_REGISTER_COMMAND, "Command", "Right"));
        rows.extend(mode_rows(MISC_MODE_REGISTER_ECS, "ECS", "Left"));
        rows.extend(mode_rows(MISC_MODE_REGISTER_FIELDS, "Preamble", "Left"));
        rows.extend(mode_rows(MISC_MODE_REGISTER_STATE, "Mode Registers", "Left"));
        let dqs_timer = base_reader(|base| {
            Some(registers::misc_mode_register_code(MISC_DQS_TIMER_REGISTER, 0, 8, registers::dqs_interval_timer, Some(base)))
        });
        rows.push(module_row("DQS Interval Timer RT", "Mode Registers", "Left", dqs_timer));

        rows.extend(MISC_FEATURE_FIELDS.iter().map(|(name, offset, bit_start, bit_length, inverted)| {
            register_row(name, "Features", "Right", *offset, switch(*offset, *bit_start, *bit_length, *inverted))
        }));

        let generation = detect_ddr_generation();
        rows.retain(|row| !misc_row_dropped(row.name().unwrap_or_default(), generation));

        if generation == DdrGeneration::Ddr4 {
            // Re-point what DDR4 does carry at the shadow. The three preamble
            // rows are not in here: they live in MR4, where only one bit is set
            // on this bench and one bit is not enough to say which field it is.
            // They keep reporting nothing until a BIOS change says which moves.
            let mut ddr4: Vec<(&str, BaseReaderFn)> = DDR4_MISC_MODE_REGISTER_FIELDS
                .iter()
                .map(|(name, number, start, length, table)| {
                    let (number, start, length, table) = (*number, *start, *length, *table);
                    (*name, base_reader(move |base| Some(registers::ddr4_misc_value(number, start, length, table, Some(base)))))
                })
                .collect();
            ddr4.push(("tWR_MR", base_reader(|base| Some(registers::ddr4_wr_rtp(0, Some(base))))));
            ddr4.push(("tRTP_MR", base_reader(|base| Some(registers::ddr4_wr_rtp(1, Some(base))))));
            for row in rows.iter_mut() {
                if let Some((_, read)) = ddr4.iter().find(|(name, _)| Some(*name) == row.name()) {
                    row.set("value", Self::channel_a(read));
                    row.set("base_reader", V::Base(read.clone()));
                }
            }

            // The two the DDR5 map has no entry for, so there is no row here to
            // re-point and they are built instead. Same shape as the rest: bound
            // to channel A for Misc, with the unbound reader kept so the move onto
            // the Timings tab can give them both controllers.
            for name in DDR4_ONLY_MODE_REGISTER_TIMINGS {
                let read = match name {
                    "tCWL_MR" => base_reader(|base| Some(registers::ddr4_cwl_mr(Some(base)))),
                    _ => base_reader(|base| Some(registers::ddr4_ccd_l_mr(Some(base)))),
                };
                rows.push(
                    Self::misc_row(name, "Mode Registers", "Left", Self::channel_a(&read), "module")
                        .with("base_reader", V::Base(read)),
                );
            }
        }
        self.rows.extend(rows);
    }

    /// Put the mode-register copies of the timings beside what they restate.
    pub(crate) fn move_mode_register_timings(&mut self) {
        let absent: &[&str] = if is_arrow_lake_platform() { ARROW_LAKE_ABSENT_MODE_REGISTER_ROWS } else { &[] };
        for name in MODE_REGISTER_TIMING_ROWS {
            let Some(id) = self.rows.iter().find(|t| t.is("name", name) && t.is("Tab", MISC_TAB)).map(|t| t.id) else {
                continue;
            };
            if absent.contains(name) {
                self.take_row(id);
                continue;
            }
            let under = &name[..name.len() - MODE_REGISTER_TIMING_SUFFIX.len()];
            let anchor = self.rows.iter().find(|t| t.is("name", under) && t.is("Tab", "Timings"));
            // The section comes from the row being restated rather than a fixed
            // name. tWR and tRTP are both Secondary, which is what this used to
            // assume; tCCD_L is in Other Timings, and a copy filed under Secondary
            // while sitting between two Other Timings rows would put a row in a
            // section its neighbours are not in.
            let (mut category, mut column) =
                (MODE_REGISTER_TIMING_FALLBACK.0.to_string(), MODE_REGISTER_TIMING_FALLBACK.1.to_string());
            let anchor_id = anchor.map(|anchor| {
                category = anchor.category().map(str::to_string).unwrap_or(category.clone());
                column = anchor.column().map(str::to_string).unwrap_or(column.clone());
                anchor.id
            });
            let mut row = self.take_row(id).expect("found above");
            row.set_str("Tab", "Timings");
            row.set_str("Category", &category);
            row.set_str("Column", &column);
            if let Some(V::Base(reader)) = row.pop("base_reader") {
                // Every row on this tab reads both controllers.
                self.promote_computed_row(&mut row, reader);
            }
            match anchor_id.and_then(|id| self.position_of(id)) {
                None => self.rows.push(row),
                Some(index) => self.rows.insert(index + 1, row),
            }
        }
    }

    /// Put the refresh arbitration controls on Misc, in their own section.
    pub(crate) fn move_refresh_policy_rows(&mut self) {
        let mut moved = Vec::new();
        for name in REFRESH_POLICY_ROWS {
            let Some(id) = self.rows.iter().find(|t| t.is("name", name) && t.is("Tab", "Timings")).map(|t| t.id) else {
                continue;
            };
            let mut row = self.take_row(id).expect("found above");
            // Left: the right column already carries Power Down, Command and
            // Features, and eleven more there would leave the columns 33 and 50.
            row.set_str("Tab", MISC_TAB);
            row.set_str("Category", "Refresh");
            row.set_str("Column", "Left");
            row.set_str("source_scope", "controller");
            // The reference descriptor names one fixed type-6 register for each
            // row. Collapse the earlier Timings mirroring back to that source; the
            // split below then files it under Settings.
            row.pop_all(DUAL_KEYS);
            moved.push(row);
        }
        self.rows.extend(moved);
    }

    pub(crate) fn install_misc_columns(&mut self) {
        for timing in self.rows.iter_mut() {
            if !timing.is("Tab", MISC_TAB) {
                continue;
            }
            if let Some(column) = lookup_str(MISC_TAB_COLUMNS, timing.category()) {
                timing.set_str("Column", &column);
            }
        }
    }

    pub(crate) fn install_misc_section_order(&mut self) {
        // A section this list does not name sorts to the foot rather than the
        // head. A new group appearing at the top of the tab reads as a
        // deliberate promotion; appearing at the bottom reads as unplaced,
        // which is what it is.
        let rank = |t: &T| {
            MISC_SECTION_ORDER.iter().position(|c| Some(*c) == t.category()).unwrap_or(MISC_SECTION_ORDER.len())
        };
        // Stable, so row order inside a section is whatever built it.
        self.sort_rows_where(|t| t.is("Tab", MISC_TAB), rank);
    }

    // --- DDR5 row names.
    //
    // Deliberately the last installer in this module. See DDR5_TIMING_LABELS:
    // every pass above matches rows by their declared name, so renaming any
    // earlier would make those passes miss the rows they are looking for.
    pub(crate) fn install_ddr5_timing_labels(&mut self) {
        let platform = self.platform;
        for timing in self.rows.iter_mut() {
            let Some(name) = timing.name().map(str::to_string) else {
                continue;
            };
            let label = ddr5_timing_label(platform, &name);
            if label != name {
                timing.set_str("name", label);
            }
        }
    }

    /// Drop DDR5 MR13 and LPDDR-only controls from the DDR4 table.
    pub(crate) fn remove_inapplicable_ddr4_timings(&mut self) {
        if detect_ddr_generation() != DdrGeneration::Ddr4 {
            return;
        }
        self.rows.retain(|t| !t.name().is_some_and(|n| DDR4_INAPPLICABLE_TIMING_ROWS.contains(&n)));
    }

    /// Lay out Training as signal, reference, and compensation columns.
    pub(crate) fn place_skew_columns(&mut self) {
        const COLUMNS: &[(&str, &str)] = &[
            ("RTT", "Left"),
            ("ODT", "Left"),
            ("RON", "Left"),
            ("ODTL", "Left"),
            ("ODT DELAY", "Left"),
            // DFE remains with the read/termination training values.
            ("DFE", "Left"),
            ("VREF", "Middle"),
            ("PHY Control", "Middle"),
            ("DATA", "Right"),
            ("CMD", "Right"),
            ("CLK", "Right"),
            ("CTL", "Right"),
            ("SComp", "Right"),
        ];
        for timing in self.rows.iter_mut() {
            if !timing.is("Tab", "Training") {
                continue;
            }
            if let Some(column) = lookup_str(COLUMNS, timing.category()) {
                timing.set_str("Column", &column);
            }
        }
    }

    pub(crate) fn install_skew_without_blank_rows(&mut self) {
        if !is_arrow_lake_platform() {
            return;
        }
        let skew: Vec<u64> = self.rows.iter().filter(|t| t.is("Tab", "Training")).map(|t| t.id).collect();
        if skew.is_empty() {
            return;
        }
        let blank: Vec<u64> = self.rows.iter().filter(|t| t.is("Tab", "Training") && row_reads_nothing(t)).map(|t| t.id).collect();

        // Every row blank means the reads are not working at all -- no driver, or
        // a platform whose Training block is somewhere else entirely -- and emptying
        // the tab would turn that into a silent success. Nothing is dropped then.
        if blank.len() == skew.len() {
            return;
        }
        self.rows.retain(|t| !blank.contains(&t.id));
    }

    pub(crate) fn promote_ddr5_module_rows(&mut self) {
        if detect_ddr_generation() != DdrGeneration::Ddr5 {
            return;
        }
        let channel_b = self.channel_b;
        let mut rows = std::mem::take(&mut self.rows);
        for timing in rows.iter_mut() {
            if !timing.is("Tab", "Training")
                || (!timing.name().is_some_and(|n| DDR5_MODULE_VREF_NAMES.contains(&n)) && !timing.is("Category", "ODTL"))
            {
                continue;
            }
            let Some(params) = timing.dynamic("dynamic_params") else {
                continue;
            };
            if !timing.is("read_type", "dynamic") {
                continue;
            }
            // Some original ODTL descriptors were rooted at MC1 because that was
            // the one value the former shared view displayed. Build both sides
            // explicitly instead of assuming the original always starts at MC0.
            let params_a = crate::hardware::read::DynamicParams { mchbar: MCHBAR, ..params };
            let params_b = crate::hardware::read::DynamicParams { mchbar: channel_b, ..params };
            timing.set("dynamic_params_a", V::Dyn(params_a));
            timing.set("dynamic_params_b", V::Dyn(params_b));
            timing.set_str("read_type_a", "dynamic");
            timing.set_str("read_type_b", "dynamic");
            self.dual_channel_headers(timing);
            timing.set_str("source_scope", "module");
        }
        self.rows = rows;
    }

    pub(crate) fn move_shared_skew_rows_to_phy(&mut self) {
        for timing in self.rows.iter_mut() {
            if !timing.is("Tab", "Training") || is_dual_timing(timing) {
                continue;
            }
            timing.set_str("Tab", IMC_TAB);
            // These are fixed controller/PHY fields. The reference dump
            // repeats the same register result in every DIMM context; that is not
            // four independent module sources.  Keep the scope explicit so a
            // later layout pass cannot accidentally put one beneath the A1/B1
            // selector just because the row originally lived on Training.
            timing.set_default_str("source_scope", "controller");
        }
    }

    /// Whether a fixed MC latency row has the other physical MC source.
    fn misc_latency_has_peer(&self, timing: &T) -> bool {
        let name = timing.name().unwrap_or_default();
        let padded = format!(" {name} ");
        let peer_name = if padded.contains(" MC0 ") {
            name.replace(" MC0 ", " MC1 ")
        } else if padded.contains(" MC1 ") {
            name.replace(" MC1 ", " MC0 ")
        } else {
            return false;
        };
        self.rows.iter().any(|row| {
            row.id != timing.id
                && row.is("Tab", MISC_TAB)
                && row.category() == timing.category()
                && row.name() == Some(peer_name.as_str())
        })
    }

    pub(crate) fn split_misc_sources(&mut self) {
        // First expose both mode-register windows to the synchronized A1/B1
        // selector. A row's old channel-A getter remains only as a compatibility
        // fallback; the table reads value_a/value_b after promotion.
        let mut rows = std::mem::take(&mut self.rows);
        for timing in rows.iter_mut() {
            if !timing.is("Tab", MISC_TAB) || !timing.is("source_scope", "module") {
                continue;
            }
            if let Some(reader) = timing.base_fn("base_reader") {
                self.promote_computed_row(timing, reader);
            } else if timing.is("read_type", "dynamic") {
                self.promote_dynamic_row(timing);
            }
        }
        self.rows = rows;

        // Then move every remaining real single-source row. Empty latency spacer
        // rows are layout, while named latency rows have explicit MC0/MC1 peers
        // and are compacted into A1/B1 by the Misc renderer.
        let moving: Vec<u64> = self
            .rows
            .iter()
            .filter(|t| {
                t.is("Tab", MISC_TAB)
                    && !t.name().unwrap_or_default().trim().is_empty()
                    && !is_dual_timing(t)
                    && !self.misc_latency_has_peer(t)
            })
            .map(|t| t.id)
            .collect();
        for timing in self.rows.iter_mut() {
            if !moving.contains(&timing.id) {
                continue;
            }
            timing.set_str("Tab", SETTINGS_TAB);
            let column = lookup_str(SETTINGS_TAB_COLUMNS, timing.category())
                .unwrap_or_else(|| timing.column().unwrap_or("Left").to_string());
            timing.set_str("Column", &column);
        }
    }

    pub(crate) fn combine_intel_detail_tabs(&mut self) {
        for timing in self.rows.iter_mut() {
            let tab = timing.tab().unwrap_or_default().to_string();
            let mut category = timing.category().map(str::to_string);
            let name = timing.name().unwrap_or_default().to_string();

            if tab == "Timings" && name == "Allow 2cyc B2B LPDDR" {
                // A command-scheduler policy bit, not a trained result, despite
                // the LPDDR name. Filed under IMC's Command section here; its
                // register is per channel, so _move_channel_registers_to_training
                // takes it on to Training with both channels' reads.
                timing.set_str("Tab", IMC_TAB);
                timing.set_str("Category", "Command");
                timing.set_str("Column", &lookup_str(PHY_SETTINGS_COLUMNS, Some("Command")).expect("listed"));
                timing.set_str("source_scope", "controller");
                timing.pop_all(DUAL_KEYS);
                continue;
            }

            if tab == RTL_TAB {
                // The unnamed row already present between MC0 and MC1 is kept as
                // the visual break shown by the reference layout. Its original category identifies
                // which channel it belongs to when there is no row name to parse.
                let original_category = category.clone().unwrap_or_default();
                let subchannel = if name.contains("CHB") || original_category.contains("CHB") { "CHB" } else { "CHA" };
                timing.set_str("Category", &format!("RTL {subchannel}"));
                timing.set_str("Column", if subchannel == "CHB" { "Right" } else { "Left" });
                continue;
            }

            if tab == MISC_TAB {
                timing.set_str("Tab", "Training");
                if category.as_deref() == Some("Mode Registers")
                    && matches!(name.as_str(), "Read DQS Offset Timing" | "DQS Interval Timer RT")
                {
                    category = Some("DQS".into());
                    timing.set_str("Category", "DQS");
                }
                let column = lookup_str(SKEW_MISC_COLUMNS, category.as_deref())
                    .unwrap_or_else(|| timing.column().unwrap_or("Right").to_string());
                timing.set_str("Column", &column);
                continue;
            }

            if tab == SETTINGS_TAB {
                timing.set_str("Tab", IMC_TAB);
                let column = lookup_str(PHY_SETTINGS_COLUMNS, category.as_deref())
                    .unwrap_or_else(|| timing.column().unwrap_or("Right").to_string());
                timing.set_str("Column", &column);
            }
        }

        // Rebalance the original Training/PHY rows as part of the combined tables.
        for timing in self.rows.iter_mut() {
            if timing.is("Tab", "Training") {
                let column = lookup_str(SKEW_MISC_COLUMNS, timing.category())
                    .unwrap_or_else(|| timing.column().unwrap_or("Left").to_string());
                timing.set_str("Column", &column);
            } else if timing.is("Tab", IMC_TAB) {
                let column = lookup_str(PHY_SETTINGS_COLUMNS, timing.category())
                    .unwrap_or_else(|| timing.column().unwrap_or("Right").to_string());
                timing.set_str("Column", &column);
            }
        }
    }

    /// Expose the remaining verified fixed-register fields.
    ///
    /// Every group on LGA1700 DDR4; on LGA1700 DDR5 only the groups in
    /// DDR5_CONFIRMED_ADDITIONAL_GROUPS. The per-channel ones reach Training
    /// through _move_channel_registers_to_training.
    pub(crate) fn install_additional_ddr4_controller_fields(&mut self) {
        let platform = self.platform;
        let groups: &[&str] = match platform {
            Platform::Lga1700Ddr4 => &["refresh", "command", "power_down", "cwl", "phy"],
            Platform::Lga1700Ddr5 => DDR5_CONFIRMED_ADDITIONAL_GROUPS,
            _ => return,
        };
        let formula = |kind: AdditionalFormula| {
            V::Map(match kind {
                AdditionalFormula::Enabled => self.f.ddr4_enabled.clone(),
                AdditionalFormula::StaggerMode => self.f.ddr4_refresh_stagger_mode.clone(),
                AdditionalFormula::InvertedDisable => self.f.ddr4_inverted_disable.clone(),
            })
        };
        let field_row = |name: &str, offset: u64, start: u32, length: i32, category: &str, tab: &str, column: &str, read_type: &str, scope: &str| {
            T::new()
                .s("name", name)
                .with("address", V::Int(MCHBAR + offset))
                .with("parameters", bits(start, length))
                .s("Category", category)
                .s("Tab", tab)
                .s("Column", column)
                .s("read_type", read_type)
                .s("source_scope", scope)
        };
        let mut added: Vec<T> = Vec::new();

        if groups.contains(&"refresh") {
            for (name, offset, start, length, kind) in DDR4_ADDITIONAL_REFRESH_FIELDS {
                let mut row = field_row(name, *offset, *start, *length, "Refresh", IMC_TAB, "Left", "standard", "controller");
                if let Some(kind) = kind {
                    row.set("Formula", formula(*kind));
                }
                added.push(row);
            }
        }

        let ddr5 = platform == Platform::Lga1700Ddr5;
        if groups.contains(&"command") {
            for (name, offset, start, length, read_type) in DDR4_ADDITIONAL_COMMAND_FIELDS {
                let kind = if ddr5 {
                    DDR5_ADDITIONAL_COMMAND_FORMULAS.iter().find(|(n, _)| n == name).map(|(_, k)| *k)
                } else {
                    None
                };
                let mut row = field_row(name, *offset, *start, *length, "Command", IMC_TAB, "Left", read_type, "controller");
                if let Some(kind) = kind {
                    row.set("Formula", formula(kind));
                }
                added.push(row);
            }
        }

        let power_down_fields = if ddr5 { DDR5_ADDITIONAL_POWER_DOWN_FIELDS } else { DDR4_ADDITIONAL_POWER_DOWN_FIELDS };
        if groups.contains(&"power_down") {
            for (name, offset, start, length) in power_down_fields {
                added.push(field_row(name, *offset, *start, *length, "Power Down", IMC_TAB, "Left", "standard", "controller"));
            }
        }

        if groups.contains(&"cwl") {
            let column = lookup_str(SKEW_MISC_COLUMNS, Some("DLL / LATENCY")).expect("listed");
            for (name, offset, start, length) in DDR4_ADDITIONAL_CWL_FIELDS {
                let mut row =
                    field_row(name, *offset, *start, *length, "DLL / LATENCY", "Training", &column, "standard", "module");
                self.promote_standard_row(&mut row);
                added.push(row);
            }
        }

        if groups.contains(&"phy") {
            let column = lookup_str(PHY_SETTINGS_COLUMNS, Some("PHY Control")).expect("listed");
            for (name, offset, start, length) in DDR4_ADDITIONAL_PHY_FIELDS {
                added.push(field_row(name, *offset, *start, *length, "PHY Control", IMC_TAB, &column, "standard", "controller"));
            }
        }
        self.rows.extend(added);
    }

    /// Place the per-module DDR5 refresh policy beside its refresh timings.
    ///
    /// DDR4's row is one shared controller-policy source and therefore remains on
    /// IMC. DDR5 reads the setting independently through both module
    /// windows, so it satisfies the Timings tab's A1/B1 source rule.
    pub(crate) fn move_module_refresh_mode_to_timings(&mut self) {
        if let Some(row) = self
            .rows
            .iter_mut()
            .find(|t| t.is("name", REFRESH_MODE_NAME) && t.is("source_scope", "module") && is_dual_timing(t))
        {
            row.set_str("Tab", "Timings");
            row.set_str("Category", "Refresh timings");
            row.set_str("Column", "Middle");
        }
    }

    /// Whether a register at this MCHBAR offset has B1's own copy.
    fn has_channel_b_copy(&self, offset: u64) -> bool {
        if CHANNEL_REGISTER_BLOCK.contains(&offset) {
            return true;
        }
        self.channel_b_offset == CHANNEL_B_CONTROLLER_OFFSET && CONTROLLER_REGISTER_BLOCK.contains(&offset)
    }

    /// The MCHBAR offset a single-source row reads, if it names one.
    fn channel_register_offset(timing: &T) -> Option<Option<u64>> {
        if let Some(address) = timing.int("address") {
            // An address below MCHBAR is a negative offset, in no block.
            return Some(address.checked_sub(MCHBAR));
        }
        timing.int("register_offset").map(Some)
    }

    /// Keep each IMC section one block, in the order the sections first appear.
    ///
    /// The three PHY fields added for DDR5 arrive after Features, so PHY Control
    /// was drawn twice -- once with the DLL codes, once again at the foot of the
    /// column. DDR4 has its own ordering pass for this; see
    /// _organize_ddr4_imc_sections.
    fn group_imc_sections(&mut self) {
        let mut first: Vec<Option<String>> = Vec::new();
        for timing in self.rows.iter().filter(|t| t.is("Tab", IMC_TAB)) {
            let category = timing.category().map(str::to_string);
            if !first.contains(&category) {
                first.push(category);
            }
        }
        self.sort_rows_where(
            |t| t.is("Tab", IMC_TAB),
            |t| first.iter().position(|c| c.as_deref() == t.category()).expect("seen above"),
        );
    }

    /// LGA1700 DDR5 only, the platform these copies were read on.
    ///
    /// DDR4's IMC and Training keep the layout they were built with. Core Ultra
    /// 200S keeps its as well: its second module still reads a sub-channel step
    /// away, which on Raptor Lake is the first module's other half, so a moved
    /// row there could show A1 twice under two names.
    pub(crate) fn move_channel_registers_to_training(&mut self) {
        let platform = self.platform;
        if platform != Platform::Lga1700Ddr4 {
            // Display order only, and wanted wherever IMC is built this way: Core
            // Ultra 200S drew its Command section in two pieces as well. DDR4
            // orders its IMC in _organize_ddr4_imc_sections.
            self.group_imc_sections();
        }
        if platform != Platform::Lga1700Ddr5 {
            return;
        }
        for timing in self.rows.iter_mut() {
            if timing.is("Tab", IMC_TAB)
                && let Some(column) = lookup_str(DDR5_PHY_SETTINGS_COLUMNS, timing.category()) {
                    timing.set_str("Column", &column);
                }
        }
        let mut moved_ids: Vec<u64> = Vec::new();
        let mut rows = std::mem::take(&mut self.rows);
        for timing in rows.iter_mut() {
            if !timing.is("Tab", IMC_TAB) || is_dual_timing(timing) {
                continue;
            }
            let offset = match Self::channel_register_offset(timing) {
                Some(Some(offset)) => offset,
                _ => continue,
            };
            if !self.has_channel_b_copy(offset) {
                continue;
            }
            if let Some(reader) = timing.base_fn("base_reader") {
                self.promote_computed_row(timing, reader);
            } else if !self.promote_standard_row(timing) {
                continue;
            }
            let column = lookup_str(TRAINING_CHANNEL_REGISTER_COLUMNS, timing.category())
                .unwrap_or_else(|| timing.column().unwrap_or("Right").to_string());
            timing.set_str("Tab", "Training");
            timing.set_str("Column", &column);
            timing.set_str("source_scope", "module");
            moved_ids.push(timing.id);
        }

        // Re-filed as whole sections. The rows were built by separate passes --
        // DDR4's extra refresh fields arrive long after the refresh policy rows --
        // and a section drawn from rows scattered through the table is split in
        // two. Command joins the Command section Training already has.
        let (moved, rest) = partition(rows, |t| moved_ids.contains(&t.id));
        self.rows = rest;
        let mut categories: Vec<Option<String>> = Vec::new();
        for timing in &moved {
            let category = timing.category().map(str::to_string);
            if !categories.contains(&category) {
                categories.push(category);
            }
        }
        let mut moved: Vec<Option<T>> = moved.into_iter().map(Some).collect();
        for category in categories {
            let group: Vec<T> = moved
                .iter_mut()
                .filter(|slot| slot.as_ref().is_some_and(|t| t.category().map(str::to_string) == category))
                .map(|slot| slot.take().expect("filtered"))
                .collect();
            let last = self
                .rows
                .iter()
                .rposition(|t| t.is("Tab", "Training") && t.category().map(str::to_string) == category);
            match last {
                None => self.rows.extend(group),
                Some(last) => {
                    self.rows.splice(last + 1..last + 1, group);
                }
            }
        }
    }

    pub(crate) fn install_reference_signal_presentation(&mut self) {
        // Mutating the shared tables: every row holding one of the ten sees the
        // rewritten text, as every holder of the shared dict did.
        let mut rewritten: Vec<(SharedTable, SharedTable)> = Vec::new();
        for table in self.f.presented_tables() {
            let presented: BTreeMap<u64, String> =
                table.iter().map(|(code, decoded)| (*code, resistance_first_rzq_text(decoded))).collect();
            rewritten.push((table.clone(), Arc::new(presented)));
        }
        for timing in self.rows.iter_mut() {
            if let Some(V::Map(formula)) = timing.get("Formula")
                && let Some((_, presented)) = rewritten.iter().find(|(original, _)| Arc::ptr_eq(original, formula)) {
                    timing.set("Formula", V::Map(presented.clone()));
                }
        }

        for timing in self.rows.iter_mut() {
            let renamed = if matches!(timing.category(), Some("RTT") | Some("ODT") | Some("RON")) {
                lookup_str(REFERENCE_SIGNAL_NAMES, timing.name())
            } else if timing.is("Category", "VREF") {
                lookup_str(REFERENCE_VREF_NAMES, timing.name())
            } else {
                None
            };
            if let Some(name) = renamed {
                timing.set_str("name", &name);
            }
        }

        // Keep each category in the reference row order without moving it out of
        // its existing Training block. Stable sorting leaves platform-only rows in
        // their original relative order.
        for (category, names) in REFERENCE_SIGNAL_ORDER {
            self.sort_rows_where(
                |t| t.is("Tab", "Training") && t.is("Category", category),
                |t| names.iter().position(|n| Some(*n) == t.name()).unwrap_or(names.len()),
            );
        }
    }

    /// Place every independently read A2/B2 mode-register field on Training.
    pub(crate) fn install_ddr4_mode_register_rows(&mut self) {
        if self.platform != Platform::Lga1700Ddr4 {
            return;
        }
        let mut readers: Vec<(&str, BaseReaderFn)> =
            vec![("CAS Latency", base_reader(|base| Some(registers::ddr4_mr_cas_latency(Some(base)))))];
        for (name, number, start, length, decode) in DDR4_ADVANCED_MODE_REGISTER_FIELDS {
            let (number, start, length, decode) = (*number, *start, *length, *decode);
            readers.push((name, base_reader(move |base| Some(registers::ddr4_misc_value(number, start, length, decode, Some(base))))));
        }
        let mut rows = Vec::new();
        for (name, read) in readers {
            let category = ddr4_training_category_by_name(name).expect("every DDR4 mode register has a section");
            let mut row = self.make_dual_live_row(name, category, read);
            row.set_str("source_scope", "module");
            row.set_str("Column", &lookup_str(SKEW_MISC_COLUMNS, Some(category)).expect("listed"));
            rows.push(row);
        }
        // One category must be contiguous: the renderer starts a new section when
        // the category changes. VrefDQ is an MR6 field, so source order otherwise
        // splits MR5/MR6 into two blocks around the VREF row.
        rows.sort_by_key(|row| {
            DDR4_TRAINING_SECTION_ROWS.iter().position(|(c, _)| row.is("Category", c)).expect("a listed section")
        });
        self.rows.extend(rows);
    }

    /// Group DDR4 Training rows by function instead of MR number.
    pub(crate) fn organize_ddr4_training_sections(&mut self) {
        if self.platform != Platform::Lga1700Ddr4 {
            return;
        }
        for timing in self.rows.iter_mut() {
            if !timing.is("Tab", "Training") {
                continue;
            }
            if let Some(category) = timing.name().and_then(ddr4_training_category_by_name) {
                timing.set_str("Category", category);
            }
            let column = lookup_str(DDR4_TRAINING_TWO_COLUMN_COLUMNS, timing.category())
                .unwrap_or_else(|| timing.column().unwrap_or("Left").to_string());
            timing.set_str("Column", &column);
        }

        // Several original Misc rows occur earlier than the additional decoded
        // fields. Once both sets share functional categories, gather each section
        // into one contiguous block so the renderer does not repeat its heading.
        let mut row_rank: Vec<(&str, usize)> = Vec::new();
        for (_, names) in DDR4_TRAINING_SECTION_ROWS {
            for (index, name) in names.iter().enumerate() {
                match row_rank.iter_mut().find(|(n, _)| n == name) {
                    Some(entry) => entry.1 = index,
                    None => row_rank.push((name, index)),
                }
            }
        }
        let row_rank_len = row_rank.len();
        self.sort_rows_where(
            |t| t.is("Tab", "Training"),
            |t| {
                let rank = DDR4_TRAINING_SECTION_ORDER
                    .iter()
                    .position(|c| Some(*c) == t.category())
                    .unwrap_or(DDR4_TRAINING_SECTION_ORDER.len());
                let within = row_rank.iter().find(|(n, _)| Some(*n) == t.name()).map(|(_, i)| *i).unwrap_or(row_rank_len);
                (rank, within)
            },
        );
    }

    /// Keep each DDR4 controller group contiguous and in functional order.
    pub(crate) fn organize_ddr4_imc_sections(&mut self) {
        if self.platform != Platform::Lga1700Ddr4 {
            return;
        }
        self.sort_rows_where(
            |t| t.is("Tab", IMC_TAB),
            |t| DDR4_IMC_SECTION_ORDER.iter().position(|c| Some(*c) == t.category()).unwrap_or(DDR4_IMC_SECTION_ORDER.len()),
        );
    }

    /// Drop, reword and relabel LGA1700 DDR5's rows, once and last.
    ///
    /// Values are reworded on Training, where the long mode-register and
    /// termination text lives, and on the rows DDR5_BRIEF_VALUES_BY_ROW names
    /// wherever they are; Summary shows the same Training rows, so its RTT and
    /// ODT readings say Off as well. Each row gets its own shortened copy of its
    /// table and its readers are wrapped.
    pub(crate) fn present_ddr5_rows(&mut self) {
        if self.platform != Platform::Lga1700Ddr5 {
            return;
        }
        self.rows.retain(|t| !(t.is("Tab", "Training") && t.name().is_some_and(|n| DDR5_TRAINING_REMOVED.contains(&n))));
        for timing in self.rows.iter_mut() {
            let name = timing.name().map(str::to_string);
            if let Some(label) = lookup_str(DDR5_DISPLAY_NAMES, name.as_deref()) {
                timing.set_str("display_name", &label);
            }
            let by_row = name.as_deref().is_some_and(|n| DDR5_BRIEF_VALUES_BY_ROW.iter().any(|(r, _)| *r == n));
            if !timing.is("Tab", "Training") && !by_row {
                continue;
            }
            if let Some(V::Map(formula)) = timing.get("Formula")
                && formula.values().any(|text| brief_ddr5_value(text, name.as_deref()) != *text) {
                    let briefed: BTreeMap<u64, String> =
                        formula.iter().map(|(code, text)| (*code, brief_ddr5_value(text, name.as_deref()))).collect();
                    timing.set("Formula", V::Map(Arc::new(briefed)));
                }
            for key in ["value", "value_a", "value_b"] {
                if let Some(read) = timing.text_fn(key) {
                    let row = name.clone();
                    timing.set(key, lazy(move || read().map(|value| brief_ddr5_value(&value, row.as_deref()))));
                }
            }
            if let Some(read) = timing.base_fn("base_reader") {
                let row = name.clone();
                timing.set(
                    "base_reader",
                    V::Base(base_reader(move |base| read(base).map(|value| brief_ddr5_value(&value, row.as_deref())))),
                );
            }
        }
    }
}

/// Whether a row has no value to show, by the rules the panel uses.
pub(crate) fn row_reads_nothing(timing: &T) -> bool {
    use crate::hardware::read::{FieldRead, ReadKind, read_field, read_timing};
    use crate::row::ReadSpec;

    let read_at = |address_key: &str, parameters_key: &str, read_type_key: &str| -> Option<String> {
        let address = timing.int(address_key)?;
        let (bit_start, bit_length) = timing.params(parameters_key)?;
        let read_type = timing
            .str(read_type_key)
            .filter(|kind| !kind.is_empty())
            .or_else(|| timing.str("read_type").filter(|kind| !kind.is_empty()))
            .unwrap_or("standard");
        let kind = if read_type == "wide" { ReadKind::Wide } else { ReadKind::Standard };
        read_field(&FieldRead { address, bit_start, bit_length, kind }).map(|v| v.to_string())
    };
    let dynamic_at = |key: &str| -> Option<String> {
        timing.dynamic(key).and_then(|params| read_timing(&ReadSpec::Dynamic(params))).map(|v| v.to_string())
    };
    let value_of = |key: &str| -> Option<Option<String>> {
        match timing.get(key)? {
            V::None => None,
            V::Str(text) => Some(Some(text.clone())),
            V::Int(number) => Some(Some(number.to_string())),
            V::Bool(flag) => Some(Some(if *flag { "True".into() } else { "False".into() })),
            V::Text(read) => Some(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| read())).ok().flatten()),
            _ => Some(None),
        }
    };

    // Each candidate is a resolved reading: `Some(text)`, or `None` for one
    // that resolved to nothing (shown as the em dash).
    let mut candidates: Vec<Option<Option<String>>> = Vec::new();
    if is_dual_timing(timing) {
        for side in ["a", "b"] {
            candidates.push(value_of(&format!("value_{side}")));
            candidates.push(dynamic_at(&format!("dynamic_params_{side}")).map(Some));
            candidates.push(read_at(&format!("address_{side}"), &format!("parameters_{side}"), &format!("read_type_{side}")).map(Some));
        }
    } else {
        candidates.push(dynamic_at("dynamic_params").map(Some));
        candidates.push(read_at("address", "parameters", "read_type").map(Some));
        candidates.push(value_of("value"));
    }
    candidates.push(value_of("default_value"));

    for candidate in candidates.into_iter().flatten() {
        let resolved = candidate.unwrap_or_else(|| "\u{2014}".to_string());
        if !BLANK_READINGS.contains(&resolved.trim()) {
            return false;
        }
    }
    true
}
