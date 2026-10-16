//! The decode tables: what a register code is called on screen.
//!
//! Kept as data, one table per Python dict, with the notes that say where
//! each came from. A row holds its table as a shared map (see [`Formulas`]),
//! because one presentation pass rewrites ten of them in place and every row
//! holding one of those has to see the rewrite -- which is what mutating a
//! shared dict did in the Python.

use std::collections::BTreeMap;
use std::sync::Arc;

/// A decode table: register code to display text.
pub type Table = &'static [(u64, &'static str)];

/// A table as the map a row's formula holds.
pub fn map_of(table: Table) -> Arc<BTreeMap<u64, String>> {
    Arc::new(table.iter().map(|(code, text)| (*code, text.to_string())).collect())
}

/// An owned map as the shared form.
pub fn arc(map: BTreeMap<u64, String>) -> Arc<BTreeMap<u64, String>> {
    Arc::new(map)
}

/// Look a code up in a table: Python's `table.get(code)`.
pub fn lookup(table: Table, code: u64) -> Option<&'static str> {
    table.iter().find(|(key, _)| *key == code).map(|(_, text)| *text)
}

pub const MULTIPLIER_FORMULA: Table = &[(0, "133.33"), (1, "100")];

pub const DFE_ENABLE_FORMULA: Table = &[(0, "ON"), (1, "OFF")];
pub const DFE_TAP_ENABLE_FORMULA: Table = &[(0, "OFF"), (1, "ON")];
pub const DFE_GAIN_FORMULA: Table = &[
    (0, "0"),
    (1, "1"),
    (2, "2"),
    (3, "3"),
    (4, "RFU"),
    (5, "RFU"),
    (6, "RFU"),
    (7, "RFU"),
    (8, "-0"),
    (9, "-1"),
    (10, "-2"),
    (11, "-3"),
    (12, "RFU"),
    (13, "RFU"),
    (14, "RFU"),
    (15, "RFU"),
];

pub const RON_FORMULA: Table = &[(0, "RZQ/7 (34)"), (1, "RZQ/6 (40)"), (2, "RZQ/5 (48)"), (3, "RFU")];
pub const GEAR_MODE_FORMULA: Table = &[(0, "1"), (1, "2"), (2, "4")];
pub const REFRESH_MODE_FORMULA: Table = &[(0, "Normal Refresh (tRFC)"), (1, "FGR")];
pub const TCCD_L_FORMULA: Table = &[
    (0, "8"),
    (1, "9"),
    (2, "10"),
    (3, "11"),
    (4, "12"),
    (5, "13"),
    (6, "14"),
    (7, "15"),
    (8, "16"),
    (9, "Reserved"),
    (10, "Reserved"),
    (11, "Reserved"),
    (12, "Reserved"),
    (13, "Reserved"),
    (14, "Reserved"),
    (15, "Reserved"),
];
pub const TCCD_L_WR_FORMULA: Table = &[
    (0, "32"),
    (1, "36"),
    (2, "40"),
    (3, "44"),
    (4, "48"),
    (5, "52"),
    (6, "56"),
    (7, "60"),
    (8, "64"),
    (9, "Reserved"),
    (10, "Reserved"),
    (11, "Reserved"),
    (12, "Reserved"),
    (13, "Reserved"),
    (14, "Reserved"),
    (15, "Reserved"),
];

/// The four Jedec-tab DFE tap value tables. Each is a 7-bit sign-and-
/// magnitude code: the positive run from code 0, the negative one from code
/// 64 ("0", "-1", ...), and every code past a tap's range "RFU". The tables
/// were written out code by code; they differ only in how far each tap's
/// range reaches -- 40, 15, 12 and 9 -- which is what this takes.
pub fn dfe_tap_table(reach: u64) -> BTreeMap<u64, String> {
    (0..128u64)
        .map(|code| {
            let text = if code <= reach {
                code.to_string()
            } else if (64..=64 + reach).contains(&code) {
                if code == 64 { "0".to_string() } else { format!("-{}", code - 64) }
            } else {
                "RFU".to_string()
            };
            (code, text)
        })
        .collect()
}

pub const DFE_TAP1_REACH: u64 = 40;
pub const DFE_TAP2_REACH: u64 = 15;
pub const DFE_TAP3_REACH: u64 = 12;
pub const DFE_TAP4_REACH: u64 = 9;

// RZQ divided by 0.5, 1, 2, 3, 4, 5 and 6, and code 6 is the rung that was
// missing. The table read 480, 240, 120, 80, 60, RFU, 40 -- a ladder with a
// hole in it, where every neighbour is present and only the 48 ohm step is
// absent. The RTT tables beside it carry the same progression with no gap.
//
// It surfaced as a Group B fault on MSI Z790MPOWER, where all three Group B
// rows read code 6 and printed "RFU" against a reference tool reading 48 for
// each. Group A was unaffected only because none of its codes lands on 6.
//
// Recorded because the first diagnosis was wrong: the byte's high nibble tags
// the register and the low nibble holds the value, so Group B's 0x2E, 0x3E and
// 0x4E look like a value of 14 if the field is read four bits wide. Widening it
// made 14 visible and 14 is not a termination, which read as firmware leaving
// the group unprogrammed -- plausible on a board with one DIMM per channel, and
// wrong. The field is three bits, bit 3 belongs to something else, and the
// reference tool's own map states bit_length 3 for all six rows.
pub const CA_ODT_FORMULA: Table = &[
    (0, "RTT_OFF"),
    (1, "RZQ/0.5 (480)"),
    (2, "RZQ/1 (240)"),
    (3, "RZQ/2 (120)"),
    (4, "RZQ/3 (80)"),
    (5, "RZQ/4 (60)"),
    (6, "RZQ/5 (48)"),
    (7, "RZQ/6 (40)"),
];
pub const CS_ODT_FORMULA: Table = &[
    (0, "RTT_OFF"),
    (1, "RZQ/0.5 (480)"),
    (2, "RZQ/1 (240)"),
    (3, "RZQ/2 (120)"),
    (4, "RZQ/3 (80)"),
    (5, "RZQ/4 (60)"),
    (6, "RZQ/5 (48)"),
    (7, "RZQ/6 (40)"),
];
pub const CK_ODT_FORMULA: Table = &[
    (0, "RTT_OFF"),
    (1, "RZQ/0.5 (480)"),
    (2, "RZQ/1 (240)"),
    (3, "RZQ/2 (120)"),
    (4, "RZQ/3 (80)"),
    (5, "RZQ/4 (60)"),
    (6, "RZQ/5 (48)"),
    (7, "RZQ/6 (40)"),
];
pub const DQS_RTT_PARK_FORMULA: Table = &[
    (0, "RTT_OFF"),
    (1, "RZQ (240)"),
    (2, "RZQ/2 (120)"),
    (3, "RZQ/3 (80)"),
    (4, "RZQ/4 (60)"),
    (5, "RZQ/5 (48)"),
    (6, "RZQ/6 (40)"),
    (7, "RZQ/7 (34)"),
];
pub const RTT_PARK_FORMULA: Table = &[
    (0, "RTT_OFF"),
    (1, "RZQ (240)"),
    (2, "RZQ/2 (120)"),
    (3, "RZQ/3 (80)"),
    (4, "RZQ/4 (60)"),
    (5, "RZQ/5 (48)"),
    (6, "RZQ/6 (40)"),
    (7, "RZQ/7 (34)"),
];
pub const RTT_WR_FORMULA: Table = &[
    (0, "RTT_OFF"),
    (1, "RZQ (240)"),
    (2, "RZQ/2 (120)"),
    (3, "RZQ/3 (80)"),
    (4, "RZQ/4 (60)"),
    (5, "RZQ/5 (48)"),
    (6, "RZQ/6 (40)"),
    (7, "RZQ/7 (34)"),
];
pub const RTT_NOM_WR_FORMULA: Table = &[
    (0, "RTT_OFF"),
    (1, "RZQ (240)"),
    (2, "RZQ/2 (120)"),
    (3, "RZQ/3 (80)"),
    (4, "RZQ/4 (60)"),
    (5, "RZQ/5 (48)"),
    (6, "RZQ/6 (40)"),
    (7, "RZQ/7 (34)"),
];
/// tWR's MR table. The Python held these as numbers; a formula's result is
/// shown through `str()`, so the text is the same.
pub const TWR_FORMULA: Table = &[
    (0, "48"),
    (1, "54"),
    (2, "60"),
    (3, "66"),
    (4, "72"),
    (5, "78"),
    (6, "84"),
    (7, "90"),
    (8, "96"),
];
pub const RTT_NOM_RD_FORMULA: Table = &[
    (0, "RTT_OFF"),
    (1, "RZQ (240)"),
    (2, "RZQ/2 (120)"),
    (3, "RZQ/3 (80)"),
    (4, "RZQ/4 (60)"),
    (5, "RZQ/5 (48)"),
    (6, "RZQ/6 (40)"),
    (7, "RZQ/7 (34)"),
];
pub const RTT_LOOPBACK_FORMULA: Table = &[
    (0, "RTT_OFF"),
    (1, "RFU"),
    (2, "RFU"),
    (3, "RFU"),
    (4, "RFU"),
    (5, "RZQ/5 (48)"),
    (6, "RFU"),
    (7, "RFU"),
];

// --- ODT latency offsets.
//
// Each field is a 3-bit code for a signed offset in memory clocks. Written as
// offsets and formatted by one function rather than as six literal tables:
// spelled out by hand they drifted, and ODTL WR OFF and ODTL WR NT OFF ended
// up printing "2 Clocks" where the other four printed "+2 Clocks" for the same
// kind of value. The reference tool signs all of them.
//
// A code a field does not define reads RFU, which is what the reference map
// calls the reserved ones -- not every field uses all eight.

/// A signed offset in clocks: "+2 Clocks", "-1 Clock", "0 Clocks".
pub fn clock_offset(clocks: i64) -> String {
    let unit = if clocks.abs() == 1 { "Clock" } else { "Clocks" };
    if clocks == 0 { format!("0 {unit}") } else { format!("{clocks:+} {unit}") }
}

fn odtl_table(offsets: &[(u64, i64)]) -> BTreeMap<u64, String> {
    (0..8u64)
        .map(|code| {
            let text = match offsets.iter().find(|(key, _)| *key == code) {
                Some((_, clocks)) => clock_offset(*clocks),
                None => "RFU".to_string(),
            };
            (code, text)
        })
        .collect()
}

// ON moves the termination earlier as the code rises, OFF later; the two RD NT
// fields reserve the ends of the range where the others do not.
pub fn odtl_on_wr() -> BTreeMap<u64, String> {
    odtl_table(&[(1, -4), (2, -3), (3, -2), (4, -1), (5, 0), (6, 1), (7, 2)])
}
pub fn odtl_off_wr() -> BTreeMap<u64, String> {
    odtl_table(&[(1, 4), (2, 3), (3, 2), (4, 1), (5, 0), (6, -1), (7, -2)])
}
pub fn odtl_on_wr_nt() -> BTreeMap<u64, String> {
    odtl_table(&[(1, -4), (2, -3), (3, -2), (4, -1), (5, 0), (6, 1), (7, 2)])
}
pub fn odtl_off_wr_nt() -> BTreeMap<u64, String> {
    odtl_table(&[(1, 4), (2, 3), (3, 2), (4, 1), (5, 0), (6, -1), (7, -2)])
}
pub fn odtl_on_rd_nt() -> BTreeMap<u64, String> {
    odtl_table(&[(2, -3), (3, -2), (4, -1), (5, 0), (6, 1)])
}
pub fn odtl_off_rd_nt() -> BTreeMap<u64, String> {
    odtl_table(&[(2, 3), (3, 2), (4, 1), (5, 0), (6, -1)])
}

pub const EN_DIS_FORMULA: Table = &[(0, "Disabled"), (1, "Enabled")];

/// The inline `{0: "Disabled", 1: "Enabled"}` several literal rows carry,
/// each its own dict.
pub const DISABLED_ENABLED: Table = &[(0, "Disabled"), (1, "Enabled")];

pub const VREF_FORMULA: Table = &[
    (0, "97.5%"), (1, "97.0%"), (2, "96.5%"), (3, "96.0%"), (4, "95.5%"), (5, "95.0%"), (6, "94.5%"), (7, "94.0%"), (8, "93.5%"), (9, "93.0%"),
    (10, "92.5%"), (11, "92.0%"), (12, "91.5%"), (13, "91.0%"), (14, "90.5%"), (15, "90.0%"), (16, "89.5%"), (17, "89.0%"), (18, "88.5%"), (19, "88.0%"),
    (20, "87.5%"), (21, "87.0%"), (22, "86.5%"), (23, "86.0%"), (24, "85.5%"), (25, "85.0%"), (26, "84.5%"), (27, "84.0%"), (28, "83.5%"), (29, "83.0%"),
    (30, "82.5%"), (31, "82.0%"), (32, "81.5%"), (33, "81.0%"), (34, "80.5%"), (35, "80.0%"), (36, "79.5%"), (37, "79.0%"), (38, "78.5%"), (39, "78.0%"),
    (40, "77.5%"), (41, "77.0%"), (42, "76.5%"), (43, "76.0%"), (44, "75.5%"), (45, "75.0%"), (46, "74.5%"), (47, "74.0%"), (48, "73.5%"), (49, "73.0%"),
    (50, "72.5%"), (51, "72.0%"), (52, "71.5%"), (53, "71.0%"), (54, "70.5%"), (55, "70.0%"), (56, "69.5%"), (57, "69.0%"), (58, "68.5%"), (59, "68.0%"),
    (60, "67.5%"), (61, "67.0%"), (62, "66.5%"), (63, "66.0%"), (64, "65.5%"), (65, "65.0%"), (66, "64.5%"), (67, "64.0%"), (68, "63.5%"), (69, "63.0%"),
    (70, "62.5%"), (71, "62.0%"), (72, "61.5%"), (73, "61.0%"), (74, "60.5%"), (75, "60.0%"), (76, "59.5%"), (77, "59.0%"), (78, "58.5%"), (79, "58.0%"),
    (80, "57.5%"), (81, "57.0%"), (82, "56.5%"), (83, "56.0%"), (84, "55.5%"), (85, "55.0%"), (86, "54.5%"), (87, "54.0%"), (88, "53.5%"), (89, "53.0%"),
    (90, "52.5%"), (91, "52.0%"), (92, "51.5%"), (93, "51.0%"), (94, "50.5%"), (95, "50.0%"), (96, "49.5%"), (97, "49.0%"), (98, "48.5%"), (99, "48.0%"),
    (100, "47.5%"), (101, "47.0%"), (102, "46.5%"), (103, "46.0%"), (104, "45.5%"), (105, "45.0%"), (106, "44.5%"), (107, "44.0%"), (108, "43.5%"), (109, "43.0%"),
    (110, "42.5%"), (111, "42.0%"), (112, "41.5%"), (113, "41.0%"), (114, "40.5%"), (115, "40.0%"), (116, "39.5%"), (117, "39.0%"), (118, "38.5%"), (119, "38.0%"),
    (120, "37.5%"), (121, "37.0%"), (122, "36.5%"), (123, "36.0%"), (124, "35.5%"), (125, "35.0%"),
];

// --- DDR4 RTT / RON controller-shadow decodes.
// RTT has BIOS comparison evidence. On 2026-09-18 the user confirmed that
// ASUS Z790-A D4 BIOS 3202 / 0x11F RON selections 34/48 track the viewer with
// DIMM RON Training and MRC Fast Boot disabled. Earlier dumps stayed at 48.
// This validates the readout response on that configuration, not measured
// resistance or the rank/slot mapping of each shadow. See docs/ddr4-ron-validation.md.
// DDR5 keeps the project's original Training-tab readers until its register mapping is verified.
pub const DDR4_RTT_NOM_PARK_FORMULA: Table = &[
    (0b000, "0 RZQ OFF"),
    (0b001, "60 RZQ/4"),
    (0b010, "120 RZQ/2"),
    (0b011, "40 RZQ/6"),
    (0b100, "240 RZQ"),
    (0b101, "48 RZQ/5"),
    (0b110, "80 RZQ/3"),
    (0b111, "34 RZQ/7"),
];

pub const DDR4_RTT_WR_LIVE_FORMULA: Table = &[
    (0b000, "0 RZQ OFF"),
    (0b001, "120 RZQ/2"),
    (0b010, "240 RZQ"),
    (0b011, "High-Z"),
    (0b100, "80 RZQ/3"),
    (0b101, "Reserved (101b)"),
    (0b110, "Reserved (110b)"),
    (0b111, "Reserved (111b)"),
];

pub const DDR4_RON_LIVE_FORMULA: Table = &[
    (0b00, "34 RZQ/7"),
    (0b01, "48 RZQ/5"),
    (0b10, "Reserved (10b)"),
    (0b11, "Reserved (11b)"),
];

// --- Misc tab decodes.
//
// The decode tables below are the reference tools' own rather than anything
// derived from the JEDEC encoding: read_pre names two different two-clock
// patterns at codes 1 and 2, and write_pre has no entry for code 0 at all.
// Neither is something a reading of the standard would have produced, so a
// code the tables do not name is shown as a bare number instead of guessed.
pub const MISC_READ_PREAMBLE: Table = &[
    (0, "1 tCK - 10 Pattern"),
    (1, "2 tCK - 0010 Pattern"),
    (2, "2 tCK - 1110 Pattern"),
    (3, "3 tCK - 000010 Pattern"),
    (4, "4 tCK - 00001010 Pattern"),
];
pub const MISC_WRITE_PREAMBLE: Table = &[
    (1, "2 tCK - 0010 Pattern"),
    (2, "3 tCK - 000010 Pattern"),
    (3, "4 tCK - 00001010 Pattern"),
];
pub const MISC_POSTAMBLE: Table = &[(0, "0.5 tCK - 0 Pattern"), (1, "1.5 tCK - 010 Pattern")];
// Named states rather than a switch: the cleared bit is a mode the DRAM is
// in, not a feature that is off.
pub const MISC_READ_PREAMBLE_TRAINING: Table = &[(0, "Normal Mode"), (1, "Read Preamble Training")];

// Burst Length, MR0[1:0]. Names from the reference tools' table rather than
// from the JEDEC encoding: code 1 is the on-the-fly eight-burst and code 2 is
// the optional thirty-two, which is not the order a decode from memory would
// have produced.
//
// This read N/A for a while and was pulled. The fault was on our side: the
// lookup matched the table entry by its index and returned the data byte,
// which finds nothing for MR0. Matching on the data byte and following the
// index into the payload array is what the reference tools do, and it lands
// on 0x1C -- BL16 with CL 36, and 36 is exactly the tCL read from the timing
// registers, which is what makes the path trustworthy rather than plausible.
pub const MISC_BURST_LENGTHS: Table = &[
    (0, "BL16"),
    (1, "BC8 OTF"),
    (2, "BL32 (Optional)"),
    (3, "BL32 OTF (Optional)"),
];

// The rest of what the mode-register table carries. 79 registers are
// reachable and these are the fields the reference map names in them that no
// other row already shows. All decode tables are the map's own.
pub const MR_REFRESH_MODE: Table = &[(0, "Normal Refresh Mode"), (1, "Fine Granularity Refresh")];
pub const MR_SUPPORTED: Table = &[(0, "Not Supported"), (1, "Supported")];
pub const MR_ENABLED: Table = &[(0, "Disable"), (1, "Enable")];
pub const MR_DATA_OUTPUT: Table = &[(0, "Normal Operation"), (1, "Disabled")];
pub const MR_GEARDOWN: Table = &[(0, "Disabled"), (1, "Enabled")];
pub const MR_CLOCK_SYNC: Table = &[(0, "Not supported"), (1, "Supported")];
pub const MR_ECS_MODE: Table = &[(0, "Manual ECS Mode Disabled"), (1, "Manual ECS Mode Enabled")];
pub const MR_ECS_RESET: Table = &[(0, "Normal"), (1, "Reset ECC Counter")];
pub const MR_ECS_COUNTS: Table = &[
    (0, "ECS counts Rows with errors"),
    (1, "ECS counts Code words with errors"),
];
pub const MR_READ_DQS_OFFSET: Table = &[(0, "0 Clock"), (1, "1 Clock"), (2, "2 Clocks"), (3, "3 Clocks")];
pub const MR_WRITE_LEVELING: Table = &[(0, "Normal Mode"), (1, "Write Leveling Mode")];
// MR2 OP[2] is set for 1N: clear, the device runs 2N, which is the default.
pub const MR_N_MODE: Table = &[(0, "2N Mode"), (1, "1N Mode")];
pub const MR_DISABLED_ENABLED: Table = &[(0, "Disabled"), (1, "Enabled")];
pub const MR_CS_ASSERTION: Table = &[(0, "Multiple Cycles"), (1, "Single Cycle")];
// MR3's two nibbles each pull write leveling in by whole clocks.
pub const MR_WRITE_LEVELING_CYCLES: Table = &[
    (0, "0 tCK"), (1, "-1 tCK"), (2, "-2 tCK"), (3, "-3 tCK"), (4, "-4 tCK"), (5, "-5 tCK"),
    (6, "-6 tCK"), (7, "-7 tCK"), (8, "-8 tCK"), (9, "-9 tCK"), (10, "-10 tCK"), (11, "-11 tCK"),
    (12, "-12 tCK"), (13, "-13 tCK"), (14, "-14 tCK"), (15, "-15 tCK"),
];
// MR4 OP[2:0]. Only the code this bench returns is named -- 2, beside the
// reference tool's "tREFI x1" -- because the published orderings of this
// field disagree with each other; any other code prints as itself.
pub const MR_MINIMUM_REFRESH_RATE: Table = &[(2, "tREFI x1")];
pub const MR_RATE_INDICATOR: Table = &[(0, "Not implemented"), (1, "Implemented")];
// MR4 OP[7], the temperature update flag: set when OP[2:0] has changed since
// the controller last read MR4.
pub const MR_TUF: Table = &[(0, "No Change"), (1, "Changed")];
// DDR5 MR0 encodes CAS latency in A6:A2. The codes are the even CL values
// from 22 through 84; code 8 is the reference machine's CL38 setting.
pub const MR_TCL: Table = &[
    (0, "22"), (1, "24"), (2, "26"), (3, "28"), (4, "30"), (5, "32"), (6, "34"), (7, "36"),
    (8, "38"), (9, "40"), (10, "42"), (11, "44"), (12, "46"), (13, "48"), (14, "50"), (15, "52"),
    (16, "54"), (17, "56"), (18, "58"), (19, "60"), (20, "62"), (21, "64"), (22, "66"), (23, "68"),
    (24, "70"), (25, "72"), (26, "74"), (27, "76"), (28, "78"), (29, "80"), (30, "82"), (31, "84"),
];

// MR6 keeps the DRAM's own write recovery and read-to-precharge. These and
// tCL_MR are the module-programmed copies of controller timings displayed on
// the Timings tab.
pub const MR_TWR: Table = &[
    (0, "48"), (1, "54"), (2, "60"), (3, "66"), (4, "72"), (5, "78"), (6, "84"), (7, "90"),
    (8, "96"), (9, "102"), (10, "108"), (11, "114"), (12, "120"), (13, "126"),
    (14, "132"), (15, "Reserved"),
];
pub const MR_TRTP: Table = &[
    (0, "12"), (1, "14"), (2, "15"), (3, "17"), (4, "18"), (5, "20"), (6, "21"), (7, "23"),
    (8, "24"), (9, "Reserved"), (10, "Reserved"), (11, "Reserved"), (12, "Reserved"),
    (13, "Reserved"), (14, "Reserved"), (15, "Reserved"),
];

// The rows DDR4 does have, read out of the mode-register shadow instead of
// the 0xE600 table DDR4 never fills. Same fields, different registers and
// different encodings -- DDR4 tops out at BL8 and its write recovery counts
// from 10, not 48, so reusing the DDR5 tables here would print DDR5 numbers.
pub const DDR4_MR_BURST_LENGTHS: Table = &[
    (0, "BL8 (Fixed)"),
    (1, "BC4 or BL8 (On The Fly)"),
    (2, "BC4 (Fixed)"),
];
pub const DDR4_MR_REFRESH_MODE: Table = &[
    (0, "Normal Refresh Mode"),
    (1, "Fixed 2x"),
    (2, "Fixed 4x"),
    (5, "On The Fly 2x"),
    (6, "On The Fly 4x"),
];
// MR0 pairs write recovery with read-to-precharge in one code, split across
// A13 and A[11:9]. Code 6 reads WR 24 here, which is what the tWR row shows.
pub const DDR4_MR_WR_RTP: &[(u64, (u32, u32))] = &[
    (0, (10, 5)),
    (1, (12, 6)),
    (2, (14, 7)),
    (3, (16, 8)),
    (4, (18, 9)),
    (5, (20, 10)),
    (6, (24, 12)),
    (7, (22, 11)),
    (8, (26, 13)),
    (9, (28, 14)),
];

// The DRAM's own CAS write latency, MR2 A5:A3. DDR4's codes are not
// contiguous -- 12 steps to 14, skipping 13 -- so this is the JEDEC table
// rather than an offset from the code.
pub const DDR4_MR_CWL: Table = &[
    (0, "9"), (1, "10"), (2, "11"), (3, "12"), (4, "14"), (5, "16"), (6, "18"), (7, "20"),
];

// Additional DDR4 MR0-MR6 state shown in Advanced. The ordinary Training and
// Timings tabs already expose the useful duplicates: burst length, RON/RTT,
// TDQS/Qoff, CWL, FGR, gear-down, data mask, VrefDQ, tCCD_L and WR/RTP. These
// are the remaining fields that describe power management, training and DRAM
// operating modes. Bit positions and encodings follow JESD79-4; the captured
// Z790-A D4 values agree with the reference capture field for field.
pub const DDR4_MR_SWITCH: Table = &[(0, "Disabled"), (1, "Enabled")];
pub const DDR4_MR_READ_BURST: Table = &[(0, "Sequential"), (1, "Interleaved")];
pub const DDR4_MR_TEST_MODE: Table = &[(0, "Normal"), (1, "Test")];
pub const DDR4_MR_DLL_RESET: Table = &[(0, "No"), (1, "Yes")];
pub const DDR4_MR_ADDITIVE_LATENCY: Table = &[(0, "0 (AL disabled)"), (1, "CL-1"), (2, "CL-2"), (3, "Reserved")];
pub const DDR4_MR_LP_ASR: Table = &[
    (0, "Manual (Normal)"),
    (1, "Manual (Reduced)"),
    (2, "Manual (Extended)"),
    (3, "Auto Self Refresh"),
];
pub const DDR4_MR_MPR_PAGE: Table = &[(0, "Page 0"), (1, "Page 1"), (2, "Page 2"), (3, "Page 3")];
pub const DDR4_MR_MPR_OPERATION: Table = &[(0, "Normal"), (1, "Dataflow to/from MPR")];
pub const DDR4_MR_MPR_FORMAT: Table = &[(0, "Serial"), (1, "Parallel"), (2, "Staggered"), (3, "Reserved")];
pub const DDR4_MR_WRITE_COMMAND_LATENCY: Table = &[(0, "4 nCK"), (1, "5 nCK"), (2, "6 nCK"), (3, "Reserved")];
pub const DDR4_MR_TEMP_REFRESH_RANGE: Table = &[(0, "Normal"), (1, "Extended")];
pub const DDR4_MR_CS_COMMAND_LATENCY: Table = &[
    (0, "Disabled"), (1, "3 nCK"), (2, "4 nCK"), (3, "5 nCK"),
    (4, "6 nCK"), (5, "8 nCK"), (6, "Reserved"), (7, "Reserved"),
];
pub const DDR4_MR_PREAMBLE: Table = &[(0, "1 nCK"), (1, "2 nCK")];
pub const DDR4_MR_CA_PARITY_LATENCY: Table = &[
    (0, "Disabled"), (1, "4 nCK"), (2, "5 nCK"), (3, "6 nCK (RFU)"),
    (4, "8 nCK (RFU)"), (5, "Reserved"), (6, "Reserved"), (7, "Reserved"),
];
pub const DDR4_MR_CLEAR_ERROR: Table = &[(0, "Clear"), (1, "Error")];
pub const DDR4_MR_ODT_BUFFER_PD: Table = &[(0, "Activated"), (1, "Deactivated")];
pub const DDR4_MR_VREF_RANGE: Table = &[(0, "Range 1"), (1, "Range 2")];

pub const DDR4_MR_CAS_LATENCY: Table = &[
    (0x00, "9"), (0x01, "10"), (0x02, "11"), (0x03, "12"),
    (0x04, "13"), (0x05, "14"), (0x06, "15"), (0x07, "16"),
    (0x08, "18"), (0x09, "20"), (0x0A, "22"), (0x0B, "24"),
    (0x0C, "23"), (0x0D, "17"), (0x0E, "19"), (0x0F, "21"),
    (0x10, "25"), (0x11, "26"), (0x12, "27"), (0x13, "28"),
    (0x14, "Reserved (29)"), (0x15, "30"), (0x16, "Reserved (31)"),
    (0x17, "32"),
];

// --- Additional Raptor Lake DDR4 controller fields.
pub const DDR4_REFRESH_STAGGER_MODE: Table = &[(0, "Per DIMM"), (1, "Per Channel")];
pub const DDR4_ENABLED: Table = &[(0, "Disabled"), (1, "Enabled")];
pub const DDR4_INVERTED_DISABLE: Table = &[(0, "Enabled"), (1, "Disabled")];

/// The shared tables a row can hold, each built once per table so the rows
/// that hold one hold the same map.
pub struct Formulas {
    pub multiplier: Arc<BTreeMap<u64, String>>,
    pub gear_mode: Arc<BTreeMap<u64, String>>,
    pub twr: Arc<BTreeMap<u64, String>>,
    pub tccd_l: Arc<BTreeMap<u64, String>>,
    pub tccd_l_wr: Arc<BTreeMap<u64, String>>,
    pub dfe_enable: Arc<BTreeMap<u64, String>>,
    pub dfe_tap_enable: Arc<BTreeMap<u64, String>>,
    pub dfe_gain: Arc<BTreeMap<u64, String>>,
    pub dfe_tap1: Arc<BTreeMap<u64, String>>,
    pub dfe_tap2: Arc<BTreeMap<u64, String>>,
    pub dfe_tap3: Arc<BTreeMap<u64, String>>,
    pub dfe_tap4: Arc<BTreeMap<u64, String>>,
    pub ron: Arc<BTreeMap<u64, String>>,
    pub refresh_mode: Arc<BTreeMap<u64, String>>,
    pub ca_odt: Arc<BTreeMap<u64, String>>,
    pub cs_odt: Arc<BTreeMap<u64, String>>,
    pub ck_odt: Arc<BTreeMap<u64, String>>,
    pub dqs_rtt_park: Arc<BTreeMap<u64, String>>,
    pub rtt_park: Arc<BTreeMap<u64, String>>,
    pub rtt_wr: Arc<BTreeMap<u64, String>>,
    pub rtt_nom_wr: Arc<BTreeMap<u64, String>>,
    pub rtt_nom_rd: Arc<BTreeMap<u64, String>>,
    pub rtt_loopback: Arc<BTreeMap<u64, String>>,
    pub odtl_on_wr: Arc<BTreeMap<u64, String>>,
    pub odtl_off_wr: Arc<BTreeMap<u64, String>>,
    pub odtl_on_wr_nt: Arc<BTreeMap<u64, String>>,
    pub odtl_off_wr_nt: Arc<BTreeMap<u64, String>>,
    pub odtl_on_rd_nt: Arc<BTreeMap<u64, String>>,
    pub odtl_off_rd_nt: Arc<BTreeMap<u64, String>>,
    pub en_dis: Arc<BTreeMap<u64, String>>,
    pub vref: Arc<BTreeMap<u64, String>>,
    pub ddr4_enabled: Arc<BTreeMap<u64, String>>,
    pub ddr4_refresh_stagger_mode: Arc<BTreeMap<u64, String>>,
    pub ddr4_inverted_disable: Arc<BTreeMap<u64, String>>,
}

impl Formulas {
    pub fn new() -> Self {
        Formulas {
            multiplier: map_of(MULTIPLIER_FORMULA),
            gear_mode: map_of(GEAR_MODE_FORMULA),
            twr: map_of(TWR_FORMULA),
            tccd_l: map_of(TCCD_L_FORMULA),
            tccd_l_wr: map_of(TCCD_L_WR_FORMULA),
            dfe_enable: map_of(DFE_ENABLE_FORMULA),
            dfe_tap_enable: map_of(DFE_TAP_ENABLE_FORMULA),
            dfe_gain: map_of(DFE_GAIN_FORMULA),
            dfe_tap1: arc(dfe_tap_table(DFE_TAP1_REACH)),
            dfe_tap2: arc(dfe_tap_table(DFE_TAP2_REACH)),
            dfe_tap3: arc(dfe_tap_table(DFE_TAP3_REACH)),
            dfe_tap4: arc(dfe_tap_table(DFE_TAP4_REACH)),
            ron: map_of(RON_FORMULA),
            refresh_mode: map_of(REFRESH_MODE_FORMULA),
            ca_odt: map_of(CA_ODT_FORMULA),
            cs_odt: map_of(CS_ODT_FORMULA),
            ck_odt: map_of(CK_ODT_FORMULA),
            dqs_rtt_park: map_of(DQS_RTT_PARK_FORMULA),
            rtt_park: map_of(RTT_PARK_FORMULA),
            rtt_wr: map_of(RTT_WR_FORMULA),
            rtt_nom_wr: map_of(RTT_NOM_WR_FORMULA),
            rtt_nom_rd: map_of(RTT_NOM_RD_FORMULA),
            rtt_loopback: map_of(RTT_LOOPBACK_FORMULA),
            odtl_on_wr: arc(odtl_on_wr()),
            odtl_off_wr: arc(odtl_off_wr()),
            odtl_on_wr_nt: arc(odtl_on_wr_nt()),
            odtl_off_wr_nt: arc(odtl_off_wr_nt()),
            odtl_on_rd_nt: arc(odtl_on_rd_nt()),
            odtl_off_rd_nt: arc(odtl_off_rd_nt()),
            en_dis: map_of(EN_DIS_FORMULA),
            vref: map_of(VREF_FORMULA),
            ddr4_enabled: map_of(DDR4_ENABLED),
            ddr4_refresh_stagger_mode: map_of(DDR4_REFRESH_STAGGER_MODE),
            ddr4_inverted_disable: map_of(DDR4_INVERTED_DISABLE),
        }
    }

    /// One of the literal rows' own `{0: "Disabled", 1: "Enabled"}` dicts.
    pub fn disabled_enabled(&self) -> Arc<BTreeMap<u64, String>> {
        map_of(DISABLED_ENABLED)
    }

    /// The ten tables the reference signal presentation rewrites, in the
    /// Python's order.
    pub fn presented_tables(&self) -> [&Arc<BTreeMap<u64, String>>; 10] {
        [
            &self.ron,
            &self.ca_odt,
            &self.cs_odt,
            &self.ck_odt,
            &self.dqs_rtt_park,
            &self.rtt_park,
            &self.rtt_wr,
            &self.rtt_nom_wr,
            &self.rtt_nom_rd,
            &self.rtt_loopback,
        ]
    }
}

impl Default for Formulas {
    fn default() -> Self {
        Self::new()
    }
}
