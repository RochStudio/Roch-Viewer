//! What goes where: the module-level tables and pure layout functions of the
//! Python `main.py`, kept apart from the drawing so they can be tested
//! without a window.
//!
//! Every function here takes the platform's rows and answers in names or row
//! indices; `summary.rs`, `tabs.rs` and `spd_tab.rs` draw what it decides.
//!
//! The Python module's whole surface is kept, constants included, so the two
//! can be read side by side; a few of its tables only the tests (and, after
//! the merge, the Advanced window) read, hence the `allow` below.

#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};

use rochviewer::memory::dimm_inventory::{HasSlot, slots_by_channel};
use rochviewer::platform_profiles::Platform;
use rochviewer::row::{Extra, Row};


// Termination and drive rows: their own Summary panel, never a timing column.
pub const SIGNAL_CATEGORIES: &[&str] = &["RTT", "ODT", "RON", "Drive Strength"];
pub const SUMMARY_EXCLUDED_SIGNAL_NAMES: &[&str] = &["RTT LOOPBACK", "RTT Loopback"];
pub const SUMMARY_SIGNAL_GROUPS: &[&[&str]] = &[&["RTT"], &["ODT"], &["RON"]];

/// Select the RTT/ODT/RON/Drive Strength rows for the Summary signal panel.
pub fn summary_signal_timings(rows: &[Row]) -> Vec<usize> {
    rows.iter()
        .enumerate()
        .filter(|(_, row)| {
            SIGNAL_CATEGORIES.contains(&row.category.as_str())
                && !SUMMARY_EXCLUDED_SIGNAL_NAMES.contains(&row.name.as_str())
        })
        .map(|(index, _)| index)
        .collect()
}

/// Name the two channel columns after the slot each one holds.
///
/// "A2/B2" says which stick a column is; "ChA/ChB" only says which memory
/// controller. The slot comes from the board's own DeviceLocator, so it is
/// the same name printed on the board and shown by every other tool.
///
/// Only a channel holding exactly one module is renamed. With two sticks on a
/// channel the column still covers both, and neither slot name would be
/// right, so that channel keeps its generic label.
pub fn channel_slot_labels<M: HasSlot>(modules: &[M]) -> BTreeMap<String, String> {
    slots_by_channel(modules)
        .into_iter()
        .filter(|(_, slots)| slots.len() == 1)
        .map(|(channel, slots)| (channel.to_lowercase(), slots[0].clone()))
        .collect()
}

/// Label/value pairs the Summary system panel lays out per row. The panel
/// configures exactly this many column pairs plus one filler, so the layout
/// and the grid have to agree.
pub const SUMMARY_PAIRS_PER_ROW: usize = 3;

pub const SUMMARY_BASE_COLUMNS: usize = 3;

/// One Summary About row: its names (a hole is `None`) and whether it is laid
/// out on the Summary columns.
pub type AboutBlock = (Vec<Option<&'static str>>, bool);

/// Return `(names, aligned)` per Summary About row.
///
/// `aligned` puts one entry per Summary column, on those columns, so the
/// entry starts at the x the timing section below it starts at. An aligned
/// row keeps a hole for a name the platform does not report, because dropping
/// it would slide everything after it into the wrong column; the renderer
/// leaves that cell empty. A row that is not aligned packs tight, each entry
/// starting where the last value ended.
pub fn summary_system_memory_blocks(available: &BTreeSet<&str>) -> Vec<AboutBlock> {
    let mut blocks: Vec<AboutBlock> = Vec::new();

    let add = |blocks: &mut Vec<AboutBlock>, names: &[&'static str]| {
        let row: Vec<&'static str> = names.iter().copied().filter(|n| available.contains(n)).collect();
        // The panel configures columns for SUMMARY_PAIRS_PER_ROW label/value
        // pairs. A longer row wraps rather than spilling past the last one
        // into the filler, which is what pushed a fourth entry against the
        // right edge while the rest of the row stayed bunched left.
        for chunk in row.chunks(SUMMARY_PAIRS_PER_ROW) {
            blocks.push((chunk.iter().map(|n| Some(*n)).collect(), false));
        }
    };
    let add_aligned = |blocks: &mut Vec<AboutBlock>, names: &[&'static str]| {
        let row: Vec<Option<&'static str>> =
            names.iter().map(|n| available.contains(n).then_some(*n)).collect();
        if row.iter().any(Option::is_some) {
            blocks.push((row, true));
        }
    };

    add(&mut blocks, &["CPU", "Cores / Threads", "Microcode"]);

    if available.contains("AGESA") {
        // AGESA is the AM5 marker: Intel reports Microcode in its place. This
        // used to also accept MbVendor, which stopped meaning anything when
        // the board rows were renamed and left the test looking satisfied.
        //
        // Identity first, packed tight so a long board name stays readable,
        // then a block that reads down the columns:
        //
        //   the kit and how it     the firmware and the    the clocks that
        //   is being driven        policy applied to it    come out
        //   DRAM Frequency         AGESA                   MCLK
        //   Memory Capacity        BCLK                    FCLK
        //   UCLK:MCLK              Refresh Mode            UCLK
        //   Power Down Mode        Gear Down Mode          Nitro
        //
        // Each column starts where the timing section under it does, so the
        // memory picture sits over tCL, the firmware over tREFI, and the
        // clocks over RTT WR.
        //
        // Four rows of three, every cell filled. The block used to run five
        // rows with Gear Down Mode alone on the last one, because DRAM Ratio
        // took a row in the first column that the other two had nothing to
        // put beside. Gear Down Mode moved up next to Refresh Mode, which is
        // the other controller policy on the strip, and the ragged row went
        // with it.
        //
        // The effective DRAM Ratio sits below BCLK, followed by Nitro.
        //
        // Processor identity occupies the first row above. The second keeps
        // the board model and BIOS; its manufacturer remains on System Info.
        //
        // Every aligned row holds at most three entries, one per Summary
        // column. A fourth made the row too long to align, so it was packed
        // tight instead, and Power Down Mode and Gear Down Mode ran 170px
        // past the window's right edge.
        //
        // DRAM Ratio was asked off the strip; it stays on System Info. Nitro
        // and Gear Down Mode moved up a row each to take its place, so the
        // only empty cells are at the end of the last row.
        add(&mut blocks, &["Model", "BIOS"]);
        add(&mut blocks, &["AGESA"]);
        add_aligned(&mut blocks, &["DRAM Frequency", "BCLK", "MCLK"]);
        add_aligned(&mut blocks, &["Memory Capacity", "Nitro Rx/Tx/Ctrl", "FCLK"]);
        add_aligned(&mut blocks, &["UCLK:MCLK", "Gear Down Mode", "UCLK"]);
        // Refresh Mode under Gear Down, the other controller policy.
        add_aligned(&mut blocks, &["Power Down Mode", "Refresh Mode"]);
        return blocks;
    }

    // Intel's fixed-width Summary keeps three aligned columns:
    //
    //   what the kit is        what it is clocked from   the clocks it yields
    //   DRAM Frequency         BCLK                      MCLK
    //   Memory Capacity        QCLK Ratio                Ring
    //   Gear Mode              Power Down                UCLK
    //
    // The first column describes the memory itself, the second the references
    // and policy the controller applies to it, and the third keeps the clock
    // chain.
    //
    // Aligned like the AM5 block, so each column starts where the timing
    // section under it does: DRAM Frequency over tCL, BCLK over tREFI, MCLK
    // over RTT WR. Packing tight instead let each row pick its own column
    // positions, so the three rows stepped in and out against the grid below
    // them.
    //
    // Uncore is the row's internal name; Summary relabels it to Ring, which is
    // why the layout names one thing and the panel shows another.
    //
    // Power Down is the Misc tab's row, read from the controller's own bit.
    // AM5 above still places Power Down Mode, which is its own reading from
    // its own profile -- the two names are not interchangeable.
    //
    // FCLK is not in these rows because the Intel backend does not report one
    // -- it is an AM5 clock, and the AM5 block above places it. Adding a name
    // here that never resolves would cost a permanent hole in the row.
    add(&mut blocks, &["Model", "BIOS"]);
    // Three compact columns keep the strip readable in the fixed-width window.
    add_aligned(&mut blocks, &["DRAM Frequency", "BCLK", "MCLK"]);
    add_aligned(&mut blocks, &["Memory Capacity", "QCLK Ratio", "Uncore"]);
    add_aligned(&mut blocks, &["Gear Mode", "Power Down", "UCLK"]);
    if available.contains("Gear Down Mode") {
        add(&mut blocks, &["Gear Down Mode", "Nitro Rx/Tx/Ctrl"]);
    }
    blocks
}

/// Return the Summary About layout as plain rows of names.
pub fn summary_system_memory_layout(available: &BTreeSet<&str>) -> Vec<Vec<&'static str>> {
    summary_system_memory_blocks(available)
        .into_iter()
        .map(|(names, _)| names.into_iter().flatten().collect())
        .collect()
}

/// Snapshot rails that remain on the Voltages tab but are too secondary for
/// the compact Summary. The per-channel supply rails stay; input and regulator
/// output diagnostics belong in the dedicated voltage view.
pub const SUMMARY_HIDDEN_SNAPSHOT_NAMES: &[&str] = &[
    "VTT snapshot",
    // An auxiliary CPU rail rather than one a memory tuner sets. It stays on
    // the Voltages tab and in Telemetry.
    "CPU AUX snapshot",
    "CHA VIN",
    "CHA 1.8V output",
    "CHA 1.0V output",
    "CHB VIN",
    "CHB 1.8V output",
    "CHB 1.0V output",
];

/// The per-DIMM PMIC rails' categories on the Voltages tab.
pub const SUMMARY_DIMM_RAIL_CATEGORIES: &[&str] = &["CHA memory", "CHB memory"];

/// A row's `show_on_summary`, which defaults to true.
fn shows_on_summary(row: &Row) -> bool {
    !matches!(row.extra.get("show_on_summary"), Some(Extra::Bool(false)))
}

/// Return only the voltage snapshots intended for the Summary column.
pub fn summary_snapshot_voltage_rows(rows: &[Row]) -> Vec<usize> {
    rows.iter()
        .enumerate()
        .filter(|(_, row)| {
            row.tab == "Voltages"
                && row.category != "Snapshot"
                && shows_on_summary(row)
                && !SUMMARY_HIDDEN_SNAPSHOT_NAMES.contains(&row.name.as_str())
        })
        .map(|(index, _)| index)
        .collect()
}

/// Tabs whose generic sections get the Summary's zebra shading. System Info
/// reads as one long list with a wide gap between each name and its value,
/// which is exactly the case the banding was added for. The timing tabs put
/// their values close to their names and are already split into short
/// sections, so shading them would be noise.
pub const SHADED_TABS: &[&str] = &["System Info", "Timings", "RTL", "Training", "IMC", "Voltages"];

/// Tabs drawn as one continuous table per column rather than a stack of
/// blocks: the A1/B1 header once at the top, the section names as banded
/// rows, and no padding between sections. Every row is then the same pitch
/// from the same origin, so the two columns line up and take the same shade
/// at the same row without anything having to force them to.
pub const CONTINUOUS_SECTION_TABS: &[&str] = &["System Info", "Timings", "RTL", "Training", "IMC", "Voltages"];

/// These short reading tabs leave part of the viewport below their last row.
/// Their columns (and the panels drawn behind them) continue through that
/// space when the tab is shown.
pub const VIEWPORT_SHADED_TABS: &[&str] = &["System Info", "Timings", "Training", "IMC", "RTL", "Voltages"];

/// Tabs whose two halves are instead padded to a shared row grid so a band
/// runs unbroken across both. That padding is dead space, so it is only worth
/// it where the facing sections are within a row or two of each other.
///
/// Derived rather than listed: a continuous tab already stacks and shades
/// each column independently, so padding one would add blank rows to a
/// layout that does not need them.
pub fn paired_section_tabs() -> Vec<&'static str> {
    SHADED_TABS.iter().copied().filter(|tab| !CONTINUOUS_SECTION_TABS.contains(tab)).collect()
}

/// The Timings tab's section order: the timing groups down the left, read in
/// the order a training result is read, and the signal and mode groups down
/// the right. Section order otherwise follows the order rows happen to appear
/// in the profile, which is not an order anyone chose.
///
/// Which column a section lands in comes from its rows' Column field, not
/// from here; this only orders what is already in a column. An unlisted
/// section sorts to the foot of its column, which is why Tertiary and Other
/// Timings are named: without them the Intel right-hand column read
/// CAS-to-CAS before Refresh, and the left-hand one only reached Power down
/// by accident. One list, sorted into each column separately, so only the
/// order of sections *within* a column matters and the list as a whole need
/// not read as a layout.
///
/// It cannot read as one, because both platforms share it and they do not
/// agree on which column a section belongs to. Power down is the case that
/// forces the issue: AM5 puts it on the left under CAS to CAS, Intel on the
/// right under Tertiary. So this is one sequence satisfying four column
/// orders at once:
///
/// ```text
///   AM5 left     Primary, Secondary, CAS to CAS, Power down, Stagger,
///                Preamble / postamble, Mode register
///   AM5 right    Refresh timings, Turnaround, Read to read, Write to write,
///                PHY
///   Intel left   Primary, Secondary, Command, Power down
///   Intel right  Refresh timings, Tertiary, CAS to CAS, Other Timings
/// ```
///
/// Power down follows CAS to CAS for AM5 and precedes Other Timings for
/// Intel; the categories in between belong to different columns, so this one
/// sequence satisfies both layouts. Moving a name means checking every
/// column, and the tests do.
pub const TIMINGS_SECTION_ORDER: &[&str] = &[
    "Primary",
    "Secondary",
    "Command",
    "Refresh timings",
    "Tertiary",
    "CAS to CAS",
    "Power down",
    "Other Timings",
    "Turnaround",
    "Read to read",
    "Write to write",
    "Stagger",
    "Preamble / postamble",
    "Mode register",
    // Last: on AM5 it closes the middle column, under the mode registers.
    // Intel's Timings tab has none of the four sections above it here.
    "PHY",
    // Training tab sections. This order is only applied to the Timings tab
    // and they no longer appear there, so their position is inert -- but a
    // name dropped from the list is easy to mistake for a section
    // deliberately removed.
    "RTT",
    "ODT",
    "Drive Strength",
];

/// Timings is wide enough for three columns. Keep whole sections together
/// and use the third column to make the full table fit vertically without a
/// scrollbar. Intel and AM5 expose different section sets, so each platform
/// gets a layout that follows its own natural reading groups.
pub const INTEL_TIMINGS_THREE_COLUMN_LAYOUT: &[(&str, &str)] = &[
    ("Primary", "Left"),
    ("Secondary", "Left"),
    ("Command", "Left"),
    ("Refresh timings", "Middle"),
    ("Tertiary", "Middle"),
    ("CAS to CAS", "Right"),
    ("Power down", "Right"),
    ("Other Timings", "Right"),
];

pub const AM5_TIMINGS_THREE_COLUMN_LAYOUT: &[(&str, &str)] = &[
    ("Primary", "Left"),
    ("Secondary", "Left"),
    ("CAS to CAS", "Left"),
    ("Power down", "Middle"),
    ("Stagger", "Middle"),
    ("Mode register", "Middle"),
    ("Preamble / postamble", "Middle"),
    ("Refresh timings", "Right"),
    ("Turnaround", "Right"),
    ("Read to read", "Right"),
    ("Write to write", "Right"),
    // Under the mode registers: the right column ran eight rows longer than
    // the middle with it there, and the two are twenty rows each without it.
    ("PHY", "Middle"),
];

/// Return the balanced Timings layout for the active platform.
pub fn timings_three_column_layout<'a>(categories: impl IntoIterator<Item = &'a str>) -> &'static [(&'static str, &'static str)] {
    let intel = categories.into_iter().any(|c| ["Command", "Tertiary", "Other Timings"].contains(&c));
    if intel { INTEL_TIMINGS_THREE_COLUMN_LAYOUT } else { AM5_TIMINGS_THREE_COLUMN_LAYOUT }
}

/// The module-aware Training page uses taller columns. Keep related groups
/// together and make their order explicit.
pub const SKEW_SECTION_ORDER: &[&str] = &[
    // The termination stack stays intact in the first column.
    "RTT", "ODT", "RON", "ODT DELAY", "VREF",
    // Latency and data controls follow the termination/reference stack.
    "DLL / LATENCY", "DATA CONTROL", "DFE", "ODTL", "Command",
    "MPR / ACCESS",
    // Command, integrity, refresh, and preamble state share the second column.
    "PARITY / CRC", "REFRESH / POWER", "PREAMBLE / PPR",
    // Legacy section names remain as fallbacks for other memory generations.
    "MR0 / MR1", "MR2 / MR3", "MR4", "MR5 / MR6",
    "Mode Registers", "DQS", "Preamble", "ECS",
    // The per-channel controller registers, each at the foot of its column.
    "Features", "Power Down", "Refresh",
];

/// Shared memory-controller and PHY values use explicit columns. CMD is the
/// signal-compensation group; Command is the controller configuration group,
/// so both names are intentionally present.
pub const IMC_SECTION_ORDER: &[&str] = &[
    "VREF", "Command", "Refresh", "Power Down",
    "DATA", "CMD", "CLK", "CTL", "SComp",
    "ODTL", "PHY Control", "Features",
    "MR0 / MR1", "MR2 / MR3", "MR4", "MR5 / MR6",
];

/// Sort (category, rows) pairs into `order`.
///
/// A category that is not listed keeps its position relative to the other
/// unlisted ones and follows the listed ones, so a section added to the
/// profile later still appears rather than vanishing from the tab.
pub fn ordered_sections<T>(mut sections: Vec<(String, T)>, order: &[&str]) -> Vec<(String, T)> {
    let rank = |name: &str| order.iter().position(|o| *o == name).unwrap_or(order.len());
    // A stable sort, as Python's sorted() is.
    sections.sort_by_key(|(name, _)| rank(name));
    sections
}

/// Sensor Telemetry group order: what the silicon is doing first, then what
/// it is drawing, then the rails feeding it. The rails are one section,
/// ordered core outward within it. The per-DIMM panels follow, each with its
/// own PMIC.
pub const SENSOR_GROUP_ORDER: &[&str] = &[
    "Clocks",
    "Thermal & Power",
    "Voltages",
    "Fans",
    "Graphics",
    // Last, because it is the one section that should stay empty of news.
    "Errors",
];

// The Summary is one continuous table. Gaps between groups were tried and
// removed: every column broke at a different row, which put the columns out
// of step by the height of a gap and made a shaded row impossible to carry
// across the width. The shading does the grouping instead.

/// Keep Summary at three columns; snapshots stack in the third one.
pub fn summary_column_count(_rows: &[Row]) -> usize {
    SUMMARY_BASE_COLUMNS
}

/// The SPD timings table's last grid column: after the names and the five
/// profile columns, a filler that takes the rest of each row.
pub const SPD_TABLE_FILLER_COLUMN: usize = 6;

/// Summary rows read once when the window opens rather than on the live
/// refresh. DRAM Frequency is derived from the measured BCLK, which wanders
/// by a few hundredths of a MHz, so the live copy flickers between 8000 and
/// 7998 for a setting that has not changed. The Telemetry window still reads
/// it live.
pub const SUMMARY_STATIC_ROWS: &[&str] = &["DRAM Frequency"];

/// Return the rows eligible for the full-width Summary system panel.
///
/// Eligibility, not placement: summary_system_memory_layout decides which of
/// these actually appear and where. Detailed controller state remains on
/// Timings, Training and IMC rather than the compact Summary.
pub fn summary_system_memory_names() -> Vec<&'static str> {
    vec![
        "CPU", "Cores / Threads", "Microcode",
        "Model", "BIOS", "AGESA",
        "BCLK", "QCLK Ratio", "Uncore", "FCLK", "MCLK", "UCLK",
        "DRAM Frequency",
        "UCLK:MCLK", "Gear Mode",
        "Memory Capacity",
        "Power Down Mode", "Power Down", "Gear Down Mode", "Nitro Rx/Tx/Ctrl",
        // AM5 only: the Intel layout does not place it, so there it is
        // eligible and unused.
        "Refresh Mode",
    ]
}

/// The Summary About strip's own short labels; Training keeps the full names.
pub const SUMMARY_ABOUT_LABELS: &[(&str, &str)] = &[
    ("Uncore", "Ring"),
    ("Memory Scrambler", "Mem Scrambler"),
    // Shorter on the strip; Training keeps the full names.
    ("Gear Down Mode", "Gear Down"),
    ("Power Down Mode", "Power Down"),
];

/// DDR5 mode-register VREF rows. Summary leaves these out when the platform
/// also reports the up/down levels: they read as a percentage and a pair of
/// cross-references ("50.0% (fixed)", "Uses CA VREF") rather than a level
/// that can be compared with the rows around them.
///
/// Arrow Lake is the exception and the reason this is a filter rather than a
/// fixed list: it drops the up/down block entirely, so there these three are
/// the only VREF readings there are and they stay.
pub const DDR5_MODE_REGISTER_VREF: &[&str] = &["DQ VREF", "CA VREF", "CS VREF"];

/// The analog up/down block, matched by name so its presence can be
/// detected. These rows were called DQ VREFUP, CMD VREFDN and so on until
/// they were renamed to the reference tools' names for the same bits -- WrDS
/// for write drive strength, RdODT for read ODT. The names changed; the
/// registers and the bit fields did not.
pub const VREF_LEVEL_PREFIXES: &[&str] = &[
    "WrDS", "RdODT", // Compatibility with older/profile-provided rows.
    "Dq Vref", "Dq Odt Vref", "Cmd Vref", "Ctl Vref", "Clk Vref",
    "CkeCsVref", "CkeCs Vref",
];

/// VREF rows the Summary leaves to the Training tab.
///
/// DQ VREF is per DRAM device, so it is four rows rather than one, and four
/// device levels say nothing a Summary reader wants: the up/down pair above
/// already gives the level, and which device differs by half a percent is a
/// tuning detail. RX VREF is a receiver reference on a different scale from
/// every other row in the panel, and QXCOUNT is a comparator count rather
/// than a voltage at all -- it sits with the VREF rows on Training because
/// that is what it belongs to, not because it is one.
pub const SUMMARY_EXCLUDED_VREF_PREFIXES: &[&str] =
    &["DQ VREF D", "DQ VREF ", "RX VREF", "QXCOUNT", "QX Count", "CkeCs Vref Up"];

/// Primary/Secondary rows Summary leaves to the Timings tab. The whole tCCD
/// group is here: it is four rows off one mode-register nibble, which is a
/// lot of Summary space for one setting. Named rather than written inline
/// because an earlier version of this list said "tCCDL" and "tCCDL WR",
/// which no row has ever been called, so the exclusion matched nothing and
/// every one of them showed anyway.
pub const SUMMARY_EXCLUDED_TIMING_NAMES: &[&str] = &[
    "tCCD", "tCCD_L", "tCCD_L_WR", "tCCD_L_WR2", "tREFI",
    "tCPDED",
    // One spelling on both platforms now; AM5 always called it CR.
    "CR",
    // A back-to-back allowance rather than a timing anyone reads at a glance,
    // and it landed in the Tertiary column between tRDRD_sg and tRDRD_dg
    // where it broke up the group. Still on the Timings tab.
    "Allow 2cyc B2B LPDDR",
    // Each restates the row above it on the Timings tab, which is where the
    // pairing is the point. On the Summary the row they restate is already
    // listed, so these only lengthen the column. They stay on Timings,
    // directly under what they restate.
    //
    // tCCD_L_MR would be dropped by the tCCD_L entry above in any case; it is
    // named here as well so the group reads as one decision rather than two
    // rows excluded for unrelated reasons.
    "tCL_MR", "tWR_MR", "tRTP_MR", "tCWL_MR", "tCCD_L_MR",
];

/// The SPD page's module fields, in reading order: the first half is the
/// MODULE panel, the second IDENTITY.
pub const SPD_FIELDS: &[(&str, &str)] = &[
    ("Memory Type", "memory_type"),
    ("Module Type", "module_type"),
    ("Channels", "system_channels"),
    ("Capacity", "system_capacity"),
    ("Module Size", "capacity"),
    ("Max Bandwidth", "max_bandwidth"),
    ("Module Manuf.", "module_manufacturer"),
    ("DRAM Manuf.", "dram_manufacturer"),
    ("DRAM Die", "dram_die"),
    ("SPD Extension", "extension"),
    ("Ranks", "rank"),
    ("Part Number", "part_number"),
    ("Serial Number", "serial_number"),
    ("Manufactured", "manufacture_date"),
    ("SPD Address", "address"),
];

/// On LGA1700 DDR5 the module's makeup reads down MODULE -- size, the dies
/// it is built from, its ranks -- and IDENTITY opens with the DRAM maker
/// beside the die, then the module's own PMIC and SPD hub.
pub const SPD_DDR5_FIELDS: &[(&str, &str)] = &[
    ("Memory Type", "memory_type"),
    ("Module Type", "module_type"),
    ("Channels", "system_channels"),
    ("Capacity", "system_capacity"),
    ("Module Size", "capacity"),
    ("DRAM Organization", "organization"),
    ("Ranks", "rank"),
    // The JEDEC reference PCB the module is built on, which is what tuners
    // mean by an A-die kit being "on an A0 board".
    ("Raw Card", "raw_card"),
    ("Max Bandwidth", "max_bandwidth"),
    ("Module Manuf.", "module_manufacturer"),
    ("DRAM Manuf.", "dram_manufacturer"),
    ("DRAM Die", "dram_die"),
    ("PMIC", "pmic"),
    ("SPD Hub", "spd_hub"),
    ("SPD Extension", "extension"),
    ("Part Number", "part_number"),
    ("Serial Number", "serial_number"),
    ("Manufactured", "manufacture_date"),
    ("SPD Address", "address"),
];

/// The SPD timings table's rows.
pub const SPD_PROFILE_FIELDS: &[(&str, &str)] = &[
    ("Frequency", "frequency"),
    ("CAS Latency", "cl"),
    ("tRCD", "trcd"),
    ("tRP", "trp"),
    ("tRAS", "tras"),
    ("tRC", "trc"),
    ("Voltage", "voltage"),
];

/// DDR5 profiles also carry tWR, the three refresh cycle times, tRRD_L,
/// tCCD_L, tCCD_L_WR, tFAW, tRTP, tWTR_L and tWTR_S, and a voltage for each
/// of VDD, VDDQ and VPP. EXPO stores only four of the floored timings, and
/// XMP's are not read, so those columns show a dash for the rest.
pub const SPD_DDR5_PROFILE_FIELDS: &[(&str, &str)] = &[
    ("Frequency", "frequency"),
    ("CAS Latency", "cl"),
    ("tRCD", "trcd"),
    ("tRP", "trp"),
    ("tRAS", "tras"),
    ("tRC", "trc"),
    ("tWR", "twr"),
    ("tRFC1", "trfc1"),
    ("tRFC2", "trfc2"),
    ("tRFCsb", "trfcsb"),
    ("tRRD_L", "trrd_l"),
    ("tCCD_L", "tccd_l"),
    ("tCCD_L_WR", "tccd_l_wr"),
    ("tFAW", "tfaw"),
    ("tRTP", "trtp"),
    ("tWTR_L", "twtr_l"),
    ("tWTR_S", "twtr_s"),
    ("VDD", "vdd"),
    ("VDDQ", "vddq"),
    ("VPP", "vpp"),
];

/// What this machine's SPD page shows, and so what Advanced lists for it.
/// AM5 is DDR5-only, so it takes the DDR5 page too: it had been getting the
/// DDR4-shaped one, which leaves out the organization, the PMIC and SPD hub,
/// and every profile timing past tRC.
pub const DDR5_SPD_PLATFORMS: &[Platform] = &[Platform::Lga1700Ddr5, Platform::Am5];

pub fn spd_tab_fields(platform: Platform) -> &'static [(&'static str, &'static str)] {
    if DDR5_SPD_PLATFORMS.contains(&platform) { SPD_DDR5_FIELDS } else { SPD_FIELDS }
}

pub fn spd_tab_profile_fields(platform: Platform) -> &'static [(&'static str, &'static str)] {
    if DDR5_SPD_PLATFORMS.contains(&platform) { SPD_DDR5_PROFILE_FIELDS } else { SPD_PROFILE_FIELDS }
}

/// Carried at the foot of the Summary signal panel, under the VREF levels.
/// Added at the call site rather than inside summary_vref_row_names, which
/// takes its rows straight from the Training tab's VREF category so the two
/// displays cannot drift: DLL BWSEL is not a VREF row and putting it there
/// would mean the function no longer described what it returns.
pub const SUMMARY_SIGNAL_TAIL_ROWS: &[&str] = &["DLL BWSEL"];
pub const SUMMARY_SIGNAL_TAIL_ANCHOR: &str = "Clk Vref Dn";

/// Return the IMC VREF rows Summary keeps, in their table order.
pub fn summary_vref_row_names(rows: &[Row]) -> Vec<String> {
    let names: Vec<String> = rows
        .iter()
        .filter(|row| {
            row.tab == "IMC"
                && row.category == "VREF"
                && !SUMMARY_EXCLUDED_VREF_PREFIXES.iter().any(|p| row.name.starts_with(p))
        })
        .map(|row| row.name.clone())
        .collect();
    if names.iter().any(|name| VREF_LEVEL_PREFIXES.iter().any(|p| name.starts_with(p))) {
        return names.into_iter().filter(|name| !DDR5_MODE_REGISTER_VREF.contains(&name.as_str())).collect();
    }
    names
}

/// R0 only. R1 is the second rank's latency, and on a single-rank kit it
/// reads the untrained 25 -- a number beside every real one that says
/// nothing about the module. It is on the RTL tab with the rest.
pub const SUMMARY_RTL_ROWS: &[(&str, &str, Option<&str>)] = &[
    ("RTL MC0 CHA R0", "RTL MC0 CHA R0", None),
    ("RTL MC0 CHB R0", "RTL MC0 CHB R0", None),
    ("RTL MC1 CHA R0", "RTL MC1 CHA R0", None),
    ("RTL MC1 CHB R0", "RTL MC1 CHB R0", None),
];

/// Shown directly under the RTL pairs, and written as (label, name) rather
/// than the (label, first, second) above. An RTL entry slashes two separate
/// rows together; a DFE row already carries both channels, so the pair is
/// its own A and B. Summary reads both forms.
pub const SUMMARY_DFE_BIAS_ROWS: &[(&str, &str)] = &[];

/// One entry of a Summary timing column: a row by name, or one of the two
/// paired forms above.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SummaryEntry {
    Name(String),
    /// `(label, first, second)`: two rows slashed together, or the first
    /// alone when there is no second.
    Rows { label: &'static str, first: &'static str, second: Option<&'static str> },
    /// `(label, name)`: one dual row's own A and B.
    Sides { label: &'static str, name: &'static str },
}

impl SummaryEntry {
    pub fn name(name: &str) -> Self {
        SummaryEntry::Name(name.to_string())
    }

    /// The row name, for a plain entry.
    pub fn as_name(&self) -> Option<&str> {
        match self {
            SummaryEntry::Name(name) => Some(name),
            _ => None,
        }
    }

    /// True for either paired form.
    pub fn is_summary_pair(&self) -> bool {
        !matches!(self, SummaryEntry::Name(_))
    }

    /// The row names a paired entry reads.
    pub fn pair_names(&self) -> Vec<&'static str> {
        match self {
            SummaryEntry::Name(_) => Vec::new(),
            SummaryEntry::Rows { first, second, .. } => std::iter::once(*first).chain(*second).collect(),
            SummaryEntry::Sides { name, .. } => vec![*name],
        }
    }
}

impl From<&str> for SummaryEntry {
    fn from(name: &str) -> Self {
        SummaryEntry::name(name)
    }
}

pub fn summary_rtl_entries() -> Vec<SummaryEntry> {
    SUMMARY_RTL_ROWS
        .iter()
        .map(|&(label, first, second)| SummaryEntry::Rows { label, first, second })
        .collect()
}

pub fn summary_dfe_entries() -> Vec<SummaryEntry> {
    SUMMARY_DFE_BIAS_ROWS.iter().map(|&(label, name)| SummaryEntry::Sides { label, name }).collect()
}

/// Place extra rows directly below the anchor timing name.
pub fn insert_summary_rows_after(names: Vec<SummaryEntry>, anchor: &str, extra: Vec<SummaryEntry>) -> Vec<SummaryEntry> {
    // Anchor missing on this platform: fall back to the end of the column.
    let position = names.iter().position(|entry| entry.as_name() == Some(anchor)).map_or(names.len(), |p| p + 1);
    let mut rows = names;
    let tail = rows.split_off(position);
    rows.extend(extra);
    rows.extend(tail);
    rows
}

/// Place the paired RTL rows below the requested timing anchor.
pub fn insert_summary_rtl_after(names: Vec<SummaryEntry>, anchor: &str) -> Vec<SummaryEntry> {
    insert_summary_rows_after(names, anchor, summary_rtl_entries())
}

/// The Summary's left column, in this order. tRCDRD leads tRCDWR because
/// read is the one that gets tuned first, and the power-down pair tails the
/// column after tMOD rather than interrupting the precharge and
/// mode-register rows.
pub const AM5_SUMMARY_TIMING_PRIORITY: &[&str] = &[
    "tCL", "tRCDRD", "tRCDWR", "tRP", "tRAS", "tRC", "tWR",
    "tRFCns", "tRFC", "tRFC2", "tRFCsb", "tRRD_L", "tRRD_S", "tWTR_L",
    "tWTR_S", "tRTP", "tFAW", "tCWL", "tMOD", "tRDPRE", "tWRPRE",
    "tCKE", "tXP",
];

/// Which rows are held at the foot of the Summary's left column whatever the
/// category filters do. Membership only -- the order is the one above.
pub const SUMMARY_COLUMN_TAIL: &[&str] = &["tRDPRE", "tWRPRE", "tMOD", "tCKE", "tXP"];

/// Shown in the Summary system strip, not the timing columns.
pub const AM5_SUMMARY_SYSTEM_ONLY: &[&str] = &["Refresh Mode"];

/// Rows the Summary places itself, which must therefore not also arrive
/// through the generic columns. CR is a Timings row so that the Timings tab
/// can close its primary group with it, and the Summary puts it under tRC by
/// name -- and without this it came through both ways and appeared twice.
pub const AM5_SUMMARY_PLACED_NAMES: &[&str] = &["CR"];

/// Timings rows that stay on the Timings tab and never reach the Summary. Its
/// own set rather than a reuse of the two above, because it means neither of
/// their things: these are not placed elsewhere in the Summary and are not in
/// the system strip, they are simply not summarised.
///
/// tREFIns is tREFI restated in nanoseconds. On the Timings tab it earns its
/// place next to the raw interval it converts; in a Summary column it is a
/// second row saying what the row above it already said.
///
/// The stagger, mode-register, pre- and post-amble timings, the power-down
/// pair and two of the three PHY timings were asked off the Summary; they
/// remain on the Timings tab.
pub const AM5_SUMMARY_OMITTED: &[&str] = &[
    "tREFIns",
    "tSTAG", "tSTAGsb",
    "tMRD", "tMRDPDA", "tMOD", "tMODPDA",
    "tRDPRE", "tWRPRE",
    "tRDPOST", "tWRPOST",
    "tCKE", "tXP",
    "tPHYWRD", "tPHYWRL",
];

pub const AM5_SUMMARY_PHY_NAMES: &[&str] = &["tPHYWRD", "tPHYRDL", "tPHYWRL"];

/// Pinned to the top of the Summary middle column, in this order: the
/// refresh interval, then the same-direction read and write groups, each
/// complete, then the two turnarounds. Everything else follows in TIMINGS
/// order.
pub const AM5_SUMMARY_LEFTOVER_PRIORITY: &[&str] = &[
    "tREFI",
    "tRDRDSCL", "tRDRDSC", "tRDRDSD", "tRDRDDD",
    "tWRWRSCL", "tWRWRSC", "tWRWRSD", "tWRWRDD",
    "tWRRD", "tRDWR",
];

/// Rows the LGA1700 DDR5 Summary leaves to the Timings tab. The first three
/// each say again what another row already says: tRDPRE is tRTP under a
/// second name -- the same register field. tWRPRE is the write-to-precharge
/// sum of rows the Summary shows, tCWL + BL/2 + tWR (36 + 8 + 48 = 92).
/// tREFIx9 is a limit derived from tREFI, which is shown. tMOD is a setting
/// of its own, read live from 0xE440[31:24] on both controllers; it is simply
/// not one the Summary carries, and stays in the Timings tab's Command group.
pub const INTEL_DDR5_SUMMARY_OMITTED: &[&str] = &["tRDPRE", "tWRPRE", "tREFIx9", "tMOD"];
/// And the rows it adds, each after the row named: tXP, the power-down exit,
/// under tCKE, the power-down entry.
pub const INTEL_DDR5_SUMMARY_ADDED: &[(&str, &[&str])] = &[("tCKE", &["tXP"])];

fn names(list: &[&str]) -> Vec<SummaryEntry> {
    list.iter().map(|n| SummaryEntry::name(n)).collect()
}

/// Return the Intel Summary's (primary/secondary, tertiary) column names.
///
/// SUMMARY_EXCLUDED_TIMING_NAMES governs both columns. It used to be applied
/// to the first only, so a name added to it that happened to be a Tertiary
/// row -- Allow 2cyc B2B LPDDR was -- stayed on the Summary with nothing to
/// say why. `omitted` names rows to leave out on this platform alone, and
/// `added` rows to put in, as (row to follow, names) pairs; see
/// INTEL_DDR5_SUMMARY_OMITTED and INTEL_DDR5_SUMMARY_ADDED.
pub fn intel_summary_timing_columns(
    rows: &[Row],
    omitted: &[&str],
    added: &[(&str, &[&str])],
) -> (Vec<SummaryEntry>, Vec<SummaryEntry>) {
    intel_summary_timing_columns_excluding(rows, omitted, added, SUMMARY_EXCLUDED_TIMING_NAMES)
}

/// [`intel_summary_timing_columns`] with the exclusion list as a parameter,
/// which is how the tests stand in for patching the module constant.
pub fn intel_summary_timing_columns_excluding(
    rows: &[Row],
    omitted: &[&str],
    added: &[(&str, &[&str])],
    excluded: &[&str],
) -> (Vec<SummaryEntry>, Vec<SummaryEntry>) {
    let wanted = |category: &str| -> Vec<SummaryEntry> {
        rows.iter()
            .filter(|row| row.category == category && !excluded.contains(&row.name.as_str()))
            .map(|row| SummaryEntry::name(&row.name))
            .collect()
    };

    let mut primary_secondary = wanted("Primary");
    primary_secondary.extend(wanted("Secondary"));
    // The refresh cycle times follow the write recovery, which is where they
    // were asked for. tRFC is named both ways because the two generations
    // spell it differently -- DDR5 renames it to tRFC2 -- so on either
    // platform exactly one of that pair exists and the other resolves to
    // nothing. tRFCns is not in that position: it is the same derived row
    // under the same name on both. The raw DDR4 tRFCpb field stays on the
    // full Timings page; only DDR5's active per-bank interval is summarized
    // here.
    let present: BTreeSet<&str> = rows.iter().map(|row| row.name.as_str()).collect();
    let mut refresh_rows: Vec<SummaryEntry> =
        ["tRFCns", "tRFC2", "tRFC"].iter().filter(|n| present.contains(*n)).map(|n| SummaryEntry::name(n)).collect();
    if present.contains("tRFC2") && present.contains("tRFCpb") {
        refresh_rows.push(SummaryEntry::name("tRFCpb"));
    }
    primary_secondary = insert_summary_rows_after(primary_secondary, "tWR", refresh_rows);
    // The refresh interval heads the column, ahead of the turnarounds.
    let mut tertiary = names(&["tREFI", "tREFIx9", "tCKE"]);
    tertiary.extend(
        wanted("Tertiary")
            .into_iter()
            .filter(|entry| !["tREFI", "tREFIx9", "tCKE", "tXP"].contains(&entry.as_name().unwrap_or(""))),
    );
    // tMOD follows the final write-to-write turnaround, and the two precharge
    // timings follow tMOD: the middle column is the shorter one, and they sit
    // with the command timings there rather than lengthening the first.
    tertiary = insert_summary_rows_after(tertiary, "tWRWR_dd", names(&["tMOD"]));
    tertiary = insert_summary_rows_after(tertiary, "tMOD", names(&["tRDPRE", "tWRPRE"]));
    // The full per-rank RTL table remains on its dedicated tab; the Summary's
    // four rows close the first column.
    primary_secondary.extend(summary_rtl_entries());
    let keep = |entry: &SummaryEntry| !entry.as_name().is_some_and(|n| omitted.contains(&n));
    primary_secondary.retain(keep);
    tertiary.retain(keep);
    for (anchor, extra) in added {
        let extra: Vec<SummaryEntry> = extra.iter().filter(|n| present.contains(*n)).map(|n| SummaryEntry::name(n)).collect();
        if primary_secondary.iter().any(|e| e.as_name() == Some(anchor)) {
            primary_secondary = insert_summary_rows_after(primary_secondary, anchor, extra);
        } else if tertiary.iter().any(|e| e.as_name() == Some(anchor)) {
            tertiary = insert_summary_rows_after(tertiary, anchor, extra);
        }
    }
    (primary_secondary, tertiary)
}

/// Return requested AM5 priority rows, followed by stable leftovers.
///
/// Leftovers keep TIMINGS order except for AM5_SUMMARY_LEFTOVER_PRIORITY,
/// which is pinned to the top: the same-direction read and write groups,
/// then the two turnarounds, then everything else. Dual ChA/ChB PHY rows are
/// excluded here and rendered in their own section.
pub fn am5_summary_timing_columns(rows: &[Row]) -> (Vec<String>, Vec<String>) {
    // Every Timings-tab row that is not a termination row is a timing, rather
    // than a fixed list of section names. The tertiary block is split into
    // several named sections, and naming them here again would mean a
    // renamed or added section silently vanishing from the Summary.
    let mut available: Vec<&str> = Vec::new();
    for row in rows {
        let name = row.name.as_str();
        if row.tab == "Timings"
            && !SIGNAL_CATEGORIES.contains(&row.category.as_str())
            && !name.is_empty()
            && !available.contains(&name)
            && !AM5_SUMMARY_PHY_NAMES.contains(&name)
        {
            available.push(name);
        }
    }
    let elsewhere =
        |name: &str| AM5_SUMMARY_SYSTEM_ONLY.contains(&name) || AM5_SUMMARY_PLACED_NAMES.contains(&name) || AM5_SUMMARY_OMITTED.contains(&name);
    let first: Vec<String> = AM5_SUMMARY_TIMING_PRIORITY
        .iter()
        .filter(|n| available.contains(n) && !elsewhere(n))
        .map(|n| n.to_string())
        .collect();
    let mut leftover: Vec<String> = available
        .iter()
        .filter(|n| !first.iter().any(|f| f == *n) && !elsewhere(n))
        .map(|n| n.to_string())
        .collect();
    let pinned: Vec<String> =
        AM5_SUMMARY_LEFTOVER_PRIORITY.iter().filter(|n| leftover.iter().any(|l| l == *n)).map(|n| n.to_string()).collect();
    if !pinned.is_empty() {
        let rest: Vec<String> = leftover.into_iter().filter(|n| !pinned.contains(n)).collect();
        leftover = pinned;
        leftover.extend(rest);
    }
    (first, leftover)
}

/// Rows the generic section drawer resolves a name to: the first row with
/// that name (case-insensitively) in that category, anywhere in the table.
pub fn section_row(rows: &[Row], section: &str, name: &str) -> Option<usize> {
    let lower = name.to_lowercase();
    rows.iter().position(|row| row.category == section && row.name.to_lowercase() == lower)
}

/// The first row with this exact name, as the Summary looks rows up.
pub fn row_named(rows: &[Row], name: &str) -> Option<usize> {
    rows.iter().position(|row| row.name == name)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn row(name: &str, category: &str, tab: &str) -> Row {
        Row::new(name, category, tab, "Left")
    }

    fn set<'a>(names: &[&'a str]) -> BTreeSet<&'a str> {
        names.iter().copied().collect()
    }

    fn plain(entries: &[SummaryEntry]) -> Vec<&str> {
        entries.iter().filter_map(SummaryEntry::as_name).collect()
    }

    // --- tests/test_main_dual_rows.py: DualTimingDefinitionTest

    #[test]
    fn summary_selects_rtt_odt_and_drive_strength_after_cs_odt_b() {
        let rows = vec![
            row("RTT WR", "RTT", "Training"),
            row("RTT LOOPBACK", "RTT", "Training"),
            row("CA ODT A", "ODT", "Training"),
            row("CS ODT B", "ODT", "Training"),
            row("Proc ODT Pu", "Drive Strength", "Training"),
            row("Proc CA DS", "Drive Strength", "Training"),
            row("VREF only", "VREF", "Training"),
        ];
        let names: Vec<&str> = summary_signal_timings(&rows).into_iter().map(|i| rows[i].name.as_str()).collect();
        assert_eq!(names, vec!["RTT WR", "CA ODT A", "CS ODT B", "Proc ODT Pu", "Proc CA DS"]);
        assert!(names.iter().position(|n| *n == "Proc ODT Pu") > names.iter().position(|n| *n == "CS ODT B"));
        assert!(!names.contains(&"RTT LOOPBACK"));
    }

    fn am5_names() -> BTreeSet<&'static str> {
        set(&[
            "CPU", "Cores / Threads", "Microcode", "Manufacturer", "Model", "BIOS",
            "AGESA", "BCLK", "MCLK", "UCLK", "FCLK", "DRAM Frequency",
            "UCLK:MCLK", "DRAM Ratio", "CR", "Refresh Mode",
            "Memory Capacity",
            "Gear Down Mode", "Power Down Mode", "Nitro Rx/Tx/Ctrl",
        ])
    }

    fn without(mut names: BTreeSet<&'static str>, name: &str) -> BTreeSet<&'static str> {
        names.remove(name);
        names
    }

    #[test]
    fn am5_summary_places_refresh_mode_under_gear_down() {
        assert_eq!(
            summary_system_memory_layout(&am5_names()),
            vec![
                vec!["CPU", "Cores / Threads", "Microcode"],
                vec!["Model", "BIOS"],
                vec!["AGESA"],
                vec!["DRAM Frequency", "BCLK", "MCLK"],
                vec!["Memory Capacity", "Nitro Rx/Tx/Ctrl", "FCLK"],
                vec!["UCLK:MCLK", "Gear Down Mode", "UCLK"],
                vec!["Power Down Mode", "Refresh Mode"],
            ]
        );
    }

    #[test]
    fn dram_ratio_is_not_on_the_summary_strip() {
        // Asked off the Summary; System Info still carries it.
        let placed: Vec<&str> = summary_system_memory_layout(&am5_names()).concat();
        assert!(!placed.contains(&"DRAM Ratio"));
    }

    #[test]
    fn the_memory_block_is_aligned_and_the_identity_rows_are_not() {
        // Only an aligned row lands on the Summary columns; identity packs
        // tight so a long board name is not chopped into a narrow cell.
        let blocks = summary_system_memory_blocks(&am5_names());
        let flag = |names: &[&str]| {
            blocks
                .iter()
                .find(|(row, _)| row.iter().flatten().copied().collect::<Vec<_>>() == names)
                .map(|(_, aligned)| *aligned)
                .unwrap()
        };
        assert!(!flag(&["CPU", "Cores / Threads", "Microcode"]));
        assert!(!flag(&["Model", "BIOS"]));
        assert!(flag(&["DRAM Frequency", "BCLK", "MCLK"]));
        assert!(flag(&["UCLK:MCLK", "Gear Down Mode", "UCLK"]));
        assert!(flag(&["Power Down Mode", "Refresh Mode"]));
    }

    #[test]
    fn an_aligned_row_keeps_a_hole_for_a_missing_name() {
        // Dropping it would slide Nitro and FCLK one column to the left,
        // under the wrong timing section.
        let blocks = summary_system_memory_blocks(&without(am5_names(), "Memory Capacity"));
        let row = &blocks.iter().find(|(names, _)| names.contains(&Some("FCLK"))).unwrap().0;
        assert_eq!(row, &vec![None, Some("Nitro Rx/Tx/Ctrl"), Some("FCLK")]);
        assert_eq!(row.iter().position(|n| *n == Some("Nitro Rx/Tx/Ctrl")), Some(1));
    }

    #[test]
    fn a_policy_row_missing_its_first_name_holds_its_column() {
        // Missing readings must not move the controller policy to the left.
        let blocks = summary_system_memory_blocks(&without(am5_names(), "UCLK:MCLK"));
        let (row, aligned) = blocks.iter().find(|(names, _)| names.contains(&Some("Gear Down Mode"))).unwrap();
        assert_eq!(row, &vec![None, Some("Gear Down Mode"), Some("UCLK")]);
        assert!(aligned);
    }

    fn slots(list: &[Option<&str>]) -> Vec<Option<String>> {
        list.iter().map(|s| s.map(str::to_string)).collect()
    }

    #[test]
    fn channel_columns_take_the_slot_name() {
        let labels = channel_slot_labels(&slots(&[Some("A2"), Some("B2")]));
        assert_eq!(labels, BTreeMap::from([("a".into(), "A2".into()), ("b".into(), "B2".into())]));
    }

    #[test]
    fn two_modules_on_one_channel_keep_the_generic_label() {
        // The column covers both sticks, so neither slot name would be right.
        let labels = channel_slot_labels(&slots(&[Some("A1"), Some("A2"), Some("B2")]));
        assert_eq!(labels, BTreeMap::from([("b".into(), "B2".into())]));
    }

    #[test]
    fn an_unreadable_locator_leaves_the_column_unnamed() {
        assert!(channel_slot_labels(&slots(&[None])).is_empty());
        assert!(channel_slot_labels::<Option<String>>(&[]).is_empty());
    }

    #[test]
    fn intel_summary_layout_does_not_gain_empty_am5_row() {
        let layout = summary_system_memory_layout(&set(&["CPU", "MCLK", "UCLK", "Gear Mode"]));
        assert!(!layout.contains(&vec!["Gear Down Mode", "Nitro Rx/Tx/Ctrl"]));
    }

    #[test]
    fn intel_summary_keeps_every_clock() {
        // The original MCLK, Uncore and UCLK clock column remains intact.
        let placed = summary_system_memory_layout(&set(&["Uncore", "MCLK", "UCLK"])).concat();
        for name in ["Uncore", "MCLK", "UCLK"] {
            assert!(placed.contains(&name), "{name}");
        }
    }

    fn intel_names() -> BTreeSet<&'static str> {
        set(&[
            "CPU", "Cores / Threads", "Microcode", "Manufacturer", "Model", "BIOS",
            "Microcode", "BCLK", "QCLK Ratio", "Uncore", "MCLK", "UCLK",
            "DRAM Frequency", "Gear Mode", "Power Down", "Memory Capacity",
            "Self Refresh", "Memory Scrambler", "Refresh Mode",
        ])
    }

    #[test]
    fn intel_summary_matches_the_requested_arrangement() {
        assert_eq!(
            summary_system_memory_layout(&intel_names()),
            vec![
                vec!["CPU", "Cores / Threads", "Microcode"],
                // System Info splits the board the way CPU-Z does, so Summary
                // names both halves -- the same shape the AM5 block already
                // used.
                vec!["Model", "BIOS"],
                // Down the columns: what the kit is, what clocks it, what it
                // yields. Every cell filled, so no column carries a hole.
                vec!["DRAM Frequency", "BCLK", "MCLK"],
                vec!["Memory Capacity", "QCLK Ratio", "Uncore"],
                vec!["Gear Mode", "Power Down", "UCLK"],
            ]
        );
    }

    #[test]
    fn intel_summary_clock_rows_sit_on_the_summary_columns() {
        // DRAM Frequency over tCL, BCLK over tREFI, then the clock chain in
        // the third column. IMC-state details remain on their own tabs.
        let aligned: BTreeSet<Vec<Option<&str>>> = summary_system_memory_blocks(&intel_names())
            .into_iter()
            .filter(|(_, aligned)| *aligned)
            .map(|(names, _)| names)
            .collect();
        let expected: BTreeSet<Vec<Option<&str>>> = [
            vec![Some("DRAM Frequency"), Some("BCLK"), Some("MCLK")],
            vec![Some("Memory Capacity"), Some("QCLK Ratio"), Some("Uncore")],
            vec![Some("Gear Mode"), Some("Power Down"), Some("UCLK")],
        ]
        .into_iter()
        .collect();
        assert_eq!(aligned, expected);
    }

    #[test]
    fn an_intel_row_missing_a_name_still_holds_its_column() {
        // No Intel row carries a hole now that every cell is filled, so the
        // behaviour that keeps UCLK over its own column is checked directly
        // rather than resting on one arrangement happening to have a gap.
        let aligned: Vec<Vec<Option<&str>>> = summary_system_memory_blocks(&set(&[
            "DRAM Frequency", "MCLK", "Memory Capacity", "Uncore", "Gear Mode", "UCLK",
        ]))
        .into_iter()
        .filter(|(_, aligned)| *aligned)
        .map(|(names, _)| names)
        .collect();
        assert!(aligned.contains(&vec![Some("DRAM Frequency"), None, Some("MCLK")]));
        assert!(aligned.contains(&vec![Some("Memory Capacity"), None, Some("Uncore")]));
        assert!(aligned.contains(&vec![Some("Gear Mode"), None, Some("UCLK")]));
    }

    #[test]
    fn no_summary_row_exceeds_the_configured_column_pairs() {
        // The Summary is three columns on every platform. A fourth entry
        // made an AM5 row too long to align, so it packed tight and ran
        // 170px past the window's right edge.
        let every: BTreeSet<&str> = summary_system_memory_names().into_iter().collect();
        for layout in [summary_system_memory_layout(&every), summary_system_memory_layout(&without(every.clone(), "AGESA"))] {
            for row in layout {
                assert!(row.len() <= SUMMARY_PAIRS_PER_ROW, "{row:?}");
            }
        }
    }

    #[test]
    fn summary_allowlist_includes_fclk() {
        assert!(summary_system_memory_names().contains(&"FCLK"));
    }

    #[test]
    fn summary_allowlist_excludes_detailed_memory_state() {
        let names = summary_system_memory_names();
        // Refresh Mode was on this list until it was asked on to the AM5
        // strip, under Gear Down.
        for name in ["Self Refresh", "Memory Scrambler", "Error Correction"] {
            assert!(!names.contains(&name), "{name}");
        }
    }

    fn vref_rows(names: &[&str]) -> Vec<Row> {
        names.iter().map(|n| row(n, "VREF", "IMC")).collect()
    }

    #[test]
    fn summary_vref_matches_the_skew_tab_minus_the_mode_registers() {
        let skew = [
            "Dq Vref Up", "Dq Vref Dn",
            "Dq Odt Vref Up", "Dq Odt Vref Dn",
            "Cmd Vref Up", "Cmd Vref Dn",
            "Ctl Vref Up", "Ctl Vref Dn",
            "Clk Vref Up", "Clk Vref Dn", "CkeCs Vref Up",
            "DQ VREF", "CA VREF", "CS VREF",
        ];
        let mut rows = vref_rows(&skew);
        rows.push(Row::new("RTT WR", "RTT", "", "Left"));
        let expected: Vec<String> = skew
            .iter()
            .filter(|n| !DDR5_MODE_REGISTER_VREF.contains(n) && **n != "CkeCs Vref Up")
            .map(|n| n.to_string())
            .collect();
        assert_eq!(summary_vref_row_names(&rows), expected);
    }

    #[test]
    fn summary_vref_keeps_table_order() {
        assert_eq!(summary_vref_row_names(&vref_rows(&["WrDSClk Up", "WrDS Up"])), vec!["WrDSClk Up", "WrDS Up"]);
    }

    #[test]
    fn summary_vref_keeps_the_mode_registers_when_they_are_all_there_is() {
        // Arrow Lake drops the up/down block, so these three are the only
        // VREF readings on that platform and must not be filtered away.
        assert_eq!(summary_vref_row_names(&vref_rows(DDR5_MODE_REGISTER_VREF)), DDR5_MODE_REGISTER_VREF.to_vec());
    }

    #[test]
    fn summary_vref_ignores_other_categories() {
        let rows = vec![row("RTT WR", "RTT", "IMC"), row("tCL", "Primary", "Timings"), row("Cmd Vref Up", "VREF", "Training")];
        assert!(summary_vref_row_names(&rows).is_empty());
    }

    fn timing(name: &str, category: &str) -> Row {
        row(name, category, "Timings")
    }

    #[test]
    fn am5_summary_uses_requested_timing_priority_then_leftovers() {
        // tRCDRD leads tRCDWR, and the power-down pair tails the column
        // after tMOD rather than interrupting precharge and mode register.
        let priority = [
            "tCL", "tRCDRD", "tRCDWR", "tRP", "tRAS", "tRC", "tWR",
            "tRFCns", "tRFC", "tRFC2", "tRFCsb", "tRRD_L", "tRRD_S", "tWTR_L",
            "tWTR_S", "tRTP", "tFAW", "tCWL",
            "tMOD", "tRDPRE", "tWRPRE", "tCKE", "tXP",
        ];
        let omitted = ["tMOD", "tRDPRE", "tWRPRE", "tCKE", "tXP"];
        let mut rows = vec![
            timing("tRCDRD", "Primary"),
            timing("tREFI", "Refresh timings"),
            timing("Refresh Mode", "Refresh timings"),
        ];
        rows.extend(priority.iter().rev().map(|n| timing(n, "Primary")));
        rows.push(timing("RTT WR", "RTT"));
        rows.push(timing("tXP", "Tertiary"));

        let (first, leftover) = am5_summary_timing_columns(&rows);
        // The precharge, mode-register and power-down rows were asked off
        // the Summary; they go to neither column rather than falling through
        // to the leftovers.
        let expected: Vec<String> = priority.iter().filter(|n| !omitted.contains(n)).map(|n| n.to_string()).collect();
        assert_eq!(first, expected);
        assert_eq!(leftover, vec!["tREFI"]);
        assert!(!first.contains(&"Refresh Mode".to_string()));
        assert!(!leftover.contains(&"Refresh Mode".to_string()));
    }

    #[test]
    fn am5_summary_places_trefi_above_the_pinned_groups() {
        let rows = vec![
            timing("tCL", "Primary"),
            timing("tRDRDSCL", "Tertiary"),
            timing("tRDRDSC", "Tertiary"),
            timing("tREFI", "Refresh timings"),
            timing("tRFCns", "Refresh timings"),
            timing("tRFC", "Refresh timings"),
            timing("Refresh Mode", "Refresh timings"),
        ];
        let (first, leftover) = am5_summary_timing_columns(&rows);
        assert_eq!(first, vec!["tCL", "tRFCns", "tRFC"]);
        // The refresh interval leads, then the pinned read group.
        assert_eq!(leftover, vec!["tREFI", "tRDRDSCL", "tRDRDSC"]);
    }

    #[test]
    fn am5_summary_places_the_turnarounds_after_the_groups() {
        let rows = vec![
            timing("tCL", "Primary"),
            timing("tRDRDSCL", "Tertiary"),
            timing("tWRWRSCL", "Tertiary"),
            timing("tRDWR", "Tertiary"),
            timing("tWRRD", "Tertiary"),
            timing("tCCD_L", "Tertiary"),
            timing("tREFI", "Refresh timings"),
        ];
        let (_, leftover) = am5_summary_timing_columns(&rows);
        assert_eq!(leftover, vec!["tREFI", "tRDRDSCL", "tWRWRSCL", "tWRRD", "tRDWR", "tCCD_L"]);
    }

    #[test]
    fn am5_summary_leftover_priority_tolerates_missing_names() {
        let rows = vec![timing("tRDRDSCL", "Tertiary"), timing("tRDWR", "Tertiary")];
        let (_, leftover) = am5_summary_timing_columns(&rows);
        assert_eq!(leftover, vec!["tRDRDSCL", "tRDWR"]); // tREFI absent here
    }

    #[test]
    fn the_board_row_contains_model_and_bios_without_manufacturer() {
        let layout = summary_system_memory_layout(&set(&["Manufacturer", "Model", "CPU"]));
        assert_eq!(layout.iter().find(|row| row.contains(&"Model")).unwrap(), &vec!["Model"]);
        assert!(!summary_system_memory_names().contains(&"Manufacturer"));
    }

    #[test]
    fn the_board_row_survives_a_missing_manufacturer() {
        // A platform that reports only half the board identity still gets a
        // row, rather than the pair collapsing to nothing.
        assert!(summary_system_memory_layout(&set(&["Model"])).contains(&vec!["Model"]));
        // AGESA marks the AM5 branch, which places the same two names, so
        // this is the same case on the other platform.
        assert!(summary_system_memory_layout(&set(&["AGESA", "Model"])).contains(&vec!["Model"]));
    }

    #[test]
    fn summary_is_three_columns_whether_or_not_rails_exist() {
        // The rails had a fourth column. They are readings, so they moved to
        // the Sensor Telemetry window; the timing columns keep their width.
        let mut rows = vec![timing("tCL", "Primary")];
        assert_eq!(summary_column_count(&rows), 3);
        rows.push(Row::new("VDDIO", "Voltages", "Sensors", "Left").with_extra("rail_key", Extra::Str("vddio_mem".into())));
        rows.push(Row::new("VSOC", "Voltages", "Sensors", "Left").with_extra("rail_key", Extra::Str("vddcr_soc".into())));
        assert_eq!(summary_column_count(&rows), 3);
    }

    #[test]
    fn am5_summary_leaves_the_column_tail_off() {
        let mut rows: Vec<Row> =
            ["tCKE", "tXP", "tMOD", "tRDPRE", "tWRPRE", "tRDRDSCL"].iter().map(|n| timing(n, "Tertiary")).collect();
        rows.push(timing("tCL", "Primary"));
        rows.extend(["tPHYRDL", "tPHYWRL", "tPHYWRD"].iter().map(|n| timing(n, "Tertiary")));
        let (first, leftover) = am5_summary_timing_columns(&rows);
        // The whole tail the column used to end on -- tMOD, the precharge
        // pair and the power-down pair -- is off the Summary, and none of it
        // falls through to the leftovers either.
        for name in ["tMOD", "tRDPRE", "tWRPRE", "tCKE", "tXP"] {
            assert!(!first.iter().chain(&leftover).any(|n| n == name), "{name}");
        }
        assert_eq!(first, vec!["tCL"]);
        for name in ["tPHYRDL", "tPHYWRL", "tPHYWRD"] {
            assert!(!first.iter().chain(&leftover).any(|n| n == name), "{name}");
        }
    }

    #[test]
    fn summary_allowlist_includes_agesa_and_bclk() {
        let names = summary_system_memory_names();
        for name in ["AGESA", "BCLK", "Refresh Mode"] {
            assert!(names.contains(&name), "{name}");
        }
    }

    // --- TimingsSectionOrderTest

    const AM5_LEFT: &[&str] =
        &["Primary", "Secondary", "CAS to CAS", "Power down", "Stagger", "Preamble / postamble", "Mode register"];
    const AM5_RIGHT: &[&str] = &["Refresh timings", "Turnaround", "Read to read", "Write to write", "PHY"];
    const INTEL_LEFT: &[&str] = &["Primary", "Secondary", "Command", "Power down"];
    const INTEL_RIGHT: &[&str] = &["Refresh timings", "Tertiary", "CAS to CAS", "Other Timings"];
    const SKEW_ONLY: &[&str] = &["RTT", "ODT", "Drive Strength"];

    fn order(names: &[&str]) -> Vec<String> {
        let sections: Vec<(String, ())> = names.iter().map(|n| (n.to_string(), ())).collect();
        ordered_sections(sections, TIMINGS_SECTION_ORDER).into_iter().map(|(n, _)| n).collect()
    }

    fn reversed(names: &[&str]) -> Vec<&'static str> {
        names.iter().rev().map(|n| *TIMINGS_SECTION_ORDER.iter().find(|o| *o == n).unwrap()).collect()
    }

    #[test]
    fn the_am5_left_column_reads_in_the_requested_order() {
        assert_eq!(order(&reversed(AM5_LEFT)), AM5_LEFT);
    }

    #[test]
    fn the_am5_right_column_reads_in_the_requested_order() {
        assert_eq!(order(&reversed(AM5_RIGHT)), AM5_RIGHT);
    }

    #[test]
    fn the_intel_left_column_keeps_its_order() {
        assert_eq!(order(&reversed(INTEL_LEFT)), INTEL_LEFT);
    }

    #[test]
    fn the_intel_right_column_keeps_its_order() {
        // The AM5 reshuffle must not drag Intel's Power down to the top of
        // its column, which is exactly what moving the name nearly did.
        assert_eq!(order(&reversed(INTEL_RIGHT)), INTEL_RIGHT);
    }

    #[test]
    fn every_named_section_belongs_to_a_column_somewhere() {
        let placed: BTreeSet<&str> =
            AM5_LEFT.iter().chain(AM5_RIGHT).chain(INTEL_LEFT).chain(INTEL_RIGHT).chain(SKEW_ONLY).copied().collect();
        assert_eq!(TIMINGS_SECTION_ORDER.iter().copied().collect::<BTreeSet<_>>(), placed);
    }

    #[test]
    fn a_section_added_later_still_appears() {
        // Unlisted categories keep their own order and follow the listed
        // ones rather than being dropped from the tab.
        assert_eq!(order(&["Stagger", "New Group", "Primary", "Another"]), vec!["Primary", "Stagger", "New Group", "Another"]);
    }

    #[test]
    fn refresh_leads_the_intel_right_column() {
        assert_eq!(order(&["Tertiary", "Refresh timings"]), vec!["Refresh timings", "Tertiary"]);
    }

    #[test]
    fn power_down_leads_other_timings_across_the_two_column_order() {
        assert_eq!(order(&["Other Timings", "Power down"]), vec!["Power down", "Other Timings"]);
    }

    #[test]
    fn the_timings_tab_is_shaded() {
        assert!(SHADED_TABS.contains(&"Timings"));
    }

    #[test]
    fn the_skew_tab_is_shaded() {
        // Training is a dense multi-section table like Timings, and was the
        // one tab of that shape still reading as an unbroken block.
        assert!(SHADED_TABS.contains(&"Training"));
    }

    #[test]
    fn the_dense_dual_channel_tabs_are_continuous() {
        let continuous: BTreeSet<&str> = CONTINUOUS_SECTION_TABS.iter().copied().collect();
        assert_eq!(continuous, set(&["System Info", "Timings", "Training", "IMC", "RTL", "Voltages"]));
    }

    #[test]
    fn the_signal_tail_follows_the_last_vref_level() {
        let tail: Vec<SummaryEntry> = SUMMARY_SIGNAL_TAIL_ROWS.iter().map(|n| SummaryEntry::name(n)).collect();
        let placed = insert_summary_rows_after(
            vec!["WrDSClk Dn".into(), SUMMARY_SIGNAL_TAIL_ANCHOR.into()],
            SUMMARY_SIGNAL_TAIL_ANCHOR,
            tail,
        );
        let mut expected = vec!["WrDSClk Dn", SUMMARY_SIGNAL_TAIL_ANCHOR];
        expected.extend(SUMMARY_SIGNAL_TAIL_ROWS);
        assert_eq!(plain(&placed), expected);
    }

    #[test]
    fn the_signal_tail_is_not_a_vref_row() {
        // It rides in the VREF name list but does not come from that
        // category. A tail row appearing there would mean it was added to
        // the wrong source group.
        let rows = vec![row("Clk Vref Dn", "VREF", "IMC"), row("DLL BWSEL", "PHY Control", "IMC")];
        let vref = summary_vref_row_names(&rows);
        for name in SUMMARY_SIGNAL_TAIL_ROWS {
            assert!(!vref.contains(&name.to_string()), "{name}");
        }
    }

    #[test]
    fn only_rtl_rows_follow_twrpre() {
        let placed = insert_summary_rtl_after(vec!["tRDPRE".into(), "tWRPRE".into(), "tMOD".into()], "tWRPRE");
        let mut expected: Vec<SummaryEntry> = vec!["tRDPRE".into(), "tWRPRE".into()];
        expected.extend(summary_rtl_entries());
        expected.push("tMOD".into());
        assert_eq!(placed, expected);
        assert!(SUMMARY_DFE_BIAS_ROWS.is_empty());
    }

    #[test]
    fn a_dfe_entry_pairs_one_row_rather_than_two() {
        // The RTL form names two rows; a DFE row already carries both
        // channels, so it names one and the pair comes from its own sides.
        for entry in summary_dfe_entries() {
            assert!(entry.is_summary_pair());
            assert!(matches!(entry, SummaryEntry::Sides { .. }));
        }
        for entry in summary_rtl_entries() {
            assert!(entry.is_summary_pair());
            assert!(matches!(entry, SummaryEntry::Rows { .. }));
        }
    }

    #[test]
    fn summary_rtl_labels_match_the_misc_row_names() {
        // R0 only: the label is the row's own name and there is no second.
        for (label, first, second) in SUMMARY_RTL_ROWS {
            assert_eq!(label, first);
            assert!(first.ends_with(" R0"));
            assert!(second.is_none());
        }
    }

    #[test]
    fn summary_does_not_include_dfe_taps() {
        assert!(SUMMARY_DFE_BIAS_ROWS.is_empty());
    }

    #[test]
    fn summary_leaves_imc_vref_to_imc_and_keeps_compact_rtl() {
        // The IMC VREF levels were carried under RON and, once every board
        // voltage was listed, sat below the window unseen. They are on IMC.
        assert_eq!(SUMMARY_SIGNAL_GROUPS, &[&["RTT"][..], &["ODT"][..], &["RON"][..]]);
        let rows = vec![timing("tCL", "Primary"), timing("tRDRD_sg", "Tertiary")];
        let (primary, tertiary) = intel_summary_timing_columns(&rows, &[], &[]);
        let pairs: Vec<SummaryEntry> = primary.into_iter().filter(SummaryEntry::is_summary_pair).collect();
        assert_eq!(pairs, summary_rtl_entries());
        assert!(!tertiary.iter().any(SummaryEntry::is_summary_pair));
    }

    #[test]
    fn summary_leaves_the_whole_ccd_group_to_the_timings_tab() {
        // All four come off one mode-register nibble. Listing part of the
        // group would read as a deliberate selection rather than what it is.
        for name in ["tCCD", "tCCD_L", "tCCD_L_WR", "tCCD_L_WR2"] {
            assert!(SUMMARY_EXCLUDED_TIMING_NAMES.contains(&name), "{name}");
        }
    }

    #[test]
    fn a_continuous_tab_is_never_also_padded() {
        // It already lines its columns up by construction, so padding would
        // add blank rows to a layout that does not need them.
        assert!(paired_section_tabs().iter().all(|t| !CONTINUOUS_SECTION_TABS.contains(t)));
    }

    #[test]
    fn padding_is_only_ever_asked_of_a_shaded_tab() {
        // The spacers carry band colours, so padding an unshaded tab would
        // add rows that shade nothing.
        assert!(paired_section_tabs().iter().all(|t| SHADED_TABS.contains(t)));
    }

    #[test]
    fn a_continuous_tab_is_shaded() {
        // The section headings take their colour from the band sequence, so
        // a continuous tab that was not shaded would band nothing.
        assert!(CONTINUOUS_SECTION_TABS.iter().all(|t| SHADED_TABS.contains(t)));
    }

    // --- SummaryColumnTailTest

    #[test]
    fn every_tail_row_is_in_the_priority_order() {
        assert!(SUMMARY_COLUMN_TAIL.iter().all(|n| AM5_SUMMARY_TIMING_PRIORITY.contains(n)));
    }

    #[test]
    fn the_tail_is_the_last_thing_in_the_priority_order() {
        let ordered: Vec<&str> = AM5_SUMMARY_TIMING_PRIORITY.iter().copied().filter(|n| SUMMARY_COLUMN_TAIL.contains(n)).collect();
        assert_eq!(ordered, vec!["tMOD", "tRDPRE", "tWRPRE", "tCKE", "tXP"]);
        assert_eq!(&AM5_SUMMARY_TIMING_PRIORITY[AM5_SUMMARY_TIMING_PRIORITY.len() - ordered.len()..], &ordered[..]);
    }

    // --- tests/test_ddr5_summary_labels.py: SummaryRowsTest

    fn summary_rows_fixture() -> Vec<Row> {
        let mut rows: Vec<Row> = ["tCL", "tRTP", "tWR", "tCWL"].iter().map(|n| timing(n, "Primary")).collect();
        rows.extend(["tREFI", "tREFIx9", "tCKE", "tWRWR_dd", "tMOD", "tRDPRE", "tWRPRE"].iter().map(|n| timing(n, "Tertiary")));
        rows.push(timing("tXP", "Power down"));
        rows
    }

    fn ddr5_columns() -> (Vec<SummaryEntry>, Vec<SummaryEntry>) {
        intel_summary_timing_columns(&summary_rows_fixture(), INTEL_DDR5_SUMMARY_OMITTED, INTEL_DDR5_SUMMARY_ADDED)
    }

    #[test]
    fn ddr5_leaves_these_rows_to_timings() {
        let (first, tertiary) = ddr5_columns();
        let shown: Vec<&str> = plain(&first).into_iter().chain(plain(&tertiary)).collect();
        for name in ["tRDPRE", "tWRPRE", "tREFIx9", "tMOD"] {
            assert!(!shown.contains(&name), "{name}");
        }
        assert!(plain(&first).contains(&"tRTP"));
    }

    #[test]
    fn ddr5_puts_txp_under_tcke() {
        let (_, tertiary) = ddr5_columns();
        let names = plain(&tertiary);
        let at = names.iter().position(|n| *n == "tCKE").unwrap();
        assert_eq!(names[at + 1], "tXP");
        assert_eq!(names.iter().filter(|n| **n == "tXP").count(), 1);
    }

    #[test]
    fn other_platforms_are_unchanged() {
        let (_, tertiary) = intel_summary_timing_columns(&summary_rows_fixture(), &[], &[]);
        let names = plain(&tertiary);
        for name in ["tRDPRE", "tWRPRE", "tREFIx9", "tMOD"] {
            assert!(names.contains(&name), "{name}");
        }
        assert!(!names.contains(&"tXP"));
    }

    #[test]
    fn a_tertiary_row_can_be_kept_off_the_summary() {
        // The case that failed: a Tertiary name in the exclusion list.
        let rows = vec![timing("tRDRD_sg", "Tertiary"), timing("tRDRD_dg", "Tertiary")];
        let (_, tertiary) = intel_summary_timing_columns_excluding(&rows, &[], &[], &["tRDRD_sg"]);
        assert!(!plain(&tertiary).contains(&"tRDRD_sg"));
        assert!(plain(&tertiary).contains(&"tRDRD_dg"));
    }

    // --- tests/test_voltage_snapshot.py (the window's half)

    #[test]
    fn voltage_tab_uses_continuous_row_shading() {
        assert!(SHADED_TABS.contains(&"Voltages"));
        assert!(CONTINUOUS_SECTION_TABS.contains(&"Voltages"));
    }

    #[test]
    fn summary_stays_three_columns_with_snapshot_rails() {
        let rows = vec![Row::new("VDDIO", "Voltages", "Voltages", "Left")];
        assert_eq!(summary_column_count(&rows), 3);
    }

    #[test]
    fn summary_keeps_diagnostic_voltage_rows_on_the_voltage_tab_only() {
        let mut rows: Vec<Row> = ["VDDIO", "VTT snapshot", "CHA VIN", "CHA 1.8V output", "CHB 1.0V output", "VSOC"]
            .iter()
            .map(|n| Row::new(*n, "Memory", "Voltages", "Left"))
            .collect();
        rows.push(Row::new("Reading mode", "Snapshot", "Voltages", "Left"));
        rows.push(Row::new("VDD hidden", "Memory", "Voltages", "Left").with_extra("show_on_summary", Extra::Bool(false)));
        let names: Vec<&str> = summary_snapshot_voltage_rows(&rows).into_iter().map(|i| rows[i].name.as_str()).collect();
        assert_eq!(names, vec!["VDDIO", "VSOC"]);
    }

    #[test]
    fn spd_fields_follow_the_platform() {
        assert_eq!(spd_tab_fields(Platform::Am5), SPD_DDR5_FIELDS);
        assert_eq!(spd_tab_fields(Platform::Lga1700Ddr5), SPD_DDR5_FIELDS);
        assert_eq!(spd_tab_fields(Platform::Lga1700Ddr4), SPD_FIELDS);
        assert_eq!(spd_tab_profile_fields(Platform::Lga1851), SPD_PROFILE_FIELDS);
        // DDR5 drops the single Voltage row for its three rails.
        assert!(!SPD_DDR5_PROFILE_FIELDS.iter().any(|(label, _)| *label == "Voltage"));
        assert_eq!(&SPD_DDR5_PROFILE_FIELDS[..6], &SPD_PROFILE_FIELDS[..6]);
    }

    #[test]
    fn spd_shows_the_source_backed_module_type() {
        // test_window_fit: both SPD pages name the module type.
        assert!(SPD_FIELDS.contains(&("Module Type", "module_type")));
        assert!(SPD_DDR5_FIELDS.contains(&("Module Type", "module_type")));
    }
}
