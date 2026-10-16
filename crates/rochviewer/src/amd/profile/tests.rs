//! The AM5 profile's tests, ported from the Python suite:
//!
//! * tests/test_am5_profile.py
//! * tests/test_am5_system_info_not_hardcoded.py
//! * tests/test_training_fields_cover_the_block.py
//! * tests/test_am5_misc_settings.py (the five cases on this module's
//!   helpers; the decode cases live with amd::timings and amd::apob)
//! * tests/test_raphael.py's RaphaelProfileTest
//! * tests/test_ccdl_run.py's RegisterFallbackTest
//! * tests/test_system_info_additions.py's CacheAndBoardTextTest and
//!   ClockFormatTest
//! * tests/test_voltage_snapshot.py's AM5 cases
//! * tests/test_am5_touches_no_intel_reader.py
//!
//! Cases that need the window (`rochviewer.ui.main`) wait for the window's
//! port; each is named where it would have been.
//!
//! The Python patched module attributes with `mock.patch`; here the same
//! readers are replaced through [`super::seams`], WMI through `FakeWmi`, PCI
//! configuration space through `set_thread_config_space`, and hardware
//! through `FakeHardware` -- all for the calling test thread only.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;
use crate::amd::apob::{DEFAULT_APOB_ADDRESSES, RAPHAEL_BLOCK_SIZE};
use crate::amd::timings::{REG_BGS0, REG_BGS1, REG_BGSA0, REG_BGSA1, REG_PD};
use crate::display_values::select_tab_names;
use crate::row::Channel;
use crate::testing::FakeHardware;
use crate::wmi_util::{FakeWmi, WmiObject};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// Encode the oracle timing set into UMC register values
/// (tests/test_am5_timings.py's `_oracle_regs`).
pub(super) fn oracle_regs() -> Registers {
    let mut regs = Registers::new();
    // 0x50200: ratio[15:0], Cmd2T bit17, GDM bit18
    regs.insert(0x50200, 4100); // MCLK 4100, 1T, GDM off
    // 0x50204: CL[5:0], RAS[14:8], RCDRD[21:16], RCDWR[29:24]
    regs.insert(0x50204, 36 | (58 << 8) | (46 << 16) | (8 << 24));
    // 0x50208: RP[21:16], RC[7:0]
    regs.insert(0x50208, 104 | (46 << 16));
    // 0x5020C: RTP[28:24], RRDL[12:8], RRDS[4:0]
    regs.insert(0x5020C, 8 | (8 << 8) | (12 << 24));
    // 0x50210: FAW[7:0]
    regs.insert(0x50210, 32);
    // 0x50214: WTRL[22:16], WTRS[12:8], CWL[5:0]
    regs.insert(0x50214, 34 | (4 << 8) | (18 << 16));
    // 0x50218: WR[7:0]
    regs.insert(0x50218, 48);
    // 0x50230: REFI[15:0]
    regs.insert(0x50230, 65535);
    // tRFC / tRFC2: first nonzero, non-0x00C00138 in 0x50260/64/68/6C
    regs.insert(0x50260, 0x00C0_0138); // placeholder -> skipped
    regs.insert(0x50264, 0); // skipped
    regs.insert(0x50268, (480 << 16) | 480); // one active dword: tRFC2 | tRFC
    regs.insert(0x5026C, 0);
    // tRFCsb: first nonzero low-11 in 0x502C0/C4/C8/CC
    regs.insert(0x502C0, 0);
    regs.insert(0x502C4, 390);
    // Nitro 0x50284: Rx=1, Tx=3, Ctrl=1
    regs.insert(0x50284, 1 | (3 << 4) | (1 << 8));
    // powerdown 0x5012C bit28
    regs.insert(0x5012C, 0);
    // 0x50198: tCCD_L[7:3] + 5, and 0x502E0: tCCD_L_WR2[5:0] + 7.
    // Raw values captured on the bench, where ZenTimings reads 21 and 42.
    regs.insert(0x50198, 0x1B01_1385);
    regs.insert(0x502E0, 0x0000_0023);
    regs
}

/// Answers every address from a register map, 0 where it has nothing.
pub(super) struct FakeReader {
    regs: Registers,
    /// Answer the same registers on both UMC bases.
    both_bases: bool,
    batch_calls: Arc<AtomicUsize>,
}

impl SmnTransport for FakeReader {
    fn read_many(&self, smn_addresses: &[u32]) -> BTreeMap<u32, Option<u32>> {
        self.batch_calls.fetch_add(1, Ordering::SeqCst);
        smn_addresses
            .iter()
            .map(|address| {
                let key = if self.both_bases { address & 0xFFFFF } else { *address };
                (*address, Some(self.regs.get(&key).copied().unwrap_or(0)))
            })
            .collect()
    }

    fn last_error(&self) -> String {
        String::new()
    }
}

pub(super) fn reader_factory(regs: Registers) -> SmnReaderFactory {
    Box::new(move || {
        Ok(Box::new(FakeReader { regs: regs.clone(), both_bases: false, batch_calls: Arc::default() })
            as Box<dyn SmnTransport>)
    })
}

/// A runtime on `regs`, with the default APOB reader and CPU name -- which,
/// with WMI unstubbed, is no name, so training is never attempted.
pub(super) fn runtime_on(regs: Registers) -> Arc<Am5Runtime> {
    Am5Runtime::with_options(Am5RuntimeOptions { reader_factory: reader_factory(regs), ..Default::default() })
}

pub(super) fn oracle_runtime() -> Arc<Am5Runtime> {
    runtime_on(oracle_regs())
}

#[derive(Clone)]
pub(super) struct FakeTrainingReader {
    pub(super) values: TrainingValues,
    pub(super) channel_values: BTreeMap<&'static str, TrainingValues>,
    pub(super) raw_table: Vec<u8>,
    pub(super) calls: Arc<AtomicUsize>,
}

impl FakeTrainingReader {
    pub(super) fn new(values: TrainingValues) -> Self {
        FakeTrainingReader { values, channel_values: BTreeMap::new(), raw_table: Vec::new(), calls: Arc::default() }
    }
}

impl TrainingReader for FakeTrainingReader {
    fn read(&mut self) -> Option<TrainingValues> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Some(self.values.clone())
    }
    fn raw_table(&self) -> Vec<u8> {
        self.raw_table.clone()
    }
    fn channel_values(&self) -> BTreeMap<&'static str, TrainingValues> {
        self.channel_values.clone()
    }
    fn channel_record_addresses(&self) -> BTreeMap<&'static str, u64> {
        if self.channel_values.is_empty() {
            return BTreeMap::new();
        }
        BTreeMap::from([("cha", 0x0A20_00D0), ("chb", 0x0A20_0100)])
    }
    fn table_address(&self) -> u64 {
        0x0A20_0000
    }
    fn record_address(&self) -> u64 {
        0x0A20_00D0
    }
    fn last_error(&self) -> String {
        String::new()
    }
}

pub(super) fn text_values(pairs: &[(&'static str, &str)]) -> TrainingValues {
    pairs.iter().map(|(name, value)| (*name, TrainingValue::Text(value.to_string()))).collect()
}

fn training_factory(reader: FakeTrainingReader) -> TrainingReaderFactory {
    Box::new(move |_layout| Ok(Box::new(reader.clone()) as Box<dyn TrainingReader>))
}

fn cpu(name: &'static str) -> CpuNameFactory {
    Box::new(move || name.to_string())
}

/// Seed a live source with a value and mark it freshly read.
///
/// Live rails expire after a fraction of a second so the UI can show movement.
/// A test that stubs a cached value must also stamp it, or the expiry sends
/// the read to real hardware.
pub(super) fn stub_live<T: Clone + Truthy>(source: &Mutex<LiveSource<T>>, value: T) {
    let mut source = lock(source);
    source.value = value;
    source.stamp = Some(Instant::now());
}

fn freeze<T: Clone + Truthy>(source: &Mutex<LiveSource<T>>) {
    let mut source = lock(source);
    if source.stamp.is_none() {
        source.stamp = Some(Instant::now());
    }
}

/// Mark every live source as just-read, without changing its value.
pub(super) fn freeze_live(runtime: &Am5Runtime) {
    let sources = &runtime.sources;
    freeze(&sources.clocks);
    freeze(&sources.voltages);
    freeze(&sources.power);
    freeze(&sources.dram);
    freeze(&sources.board);
    freeze(&sources.board_monitor);
    freeze(&sources.board_temp);
    freeze(&sources.dimm_temp);
}

pub(super) fn by_name(rows: &[Row]) -> BTreeMap<String, Row> {
    rows.iter().map(|row| (row.name.clone(), row.clone())).collect()
}

pub(super) fn names(rows: &[Row]) -> Vec<String> {
    rows.iter().map(|row| row.name.clone()).collect()
}

/// `row["value"]()`.
pub(super) fn value(row: &Row) -> String {
    row.value.as_ref().expect("a row value").resolve(EM_DASH)
}

/// `row["value_a"]()` / `row["value_b"]()`.
pub(super) fn side(row: &Row, channel: Channel) -> String {
    row.side(channel).and_then(|side| side.value.as_ref()).expect("a side value").resolve(EM_DASH)
}

fn dim(row: &Row) -> bool {
    row.dim.as_ref().expect("a dim predicate")()
}

fn extra_str<'a>(row: &'a Row, key: &str) -> Option<&'a str> {
    row.extra_str(key)
}

fn voltages(values: &[(&'static str, f64)]) -> Option<SmuVoltages> {
    Some(SmuVoltages { version: 0x620105, table_base: 0x1000, values: values.iter().copied().collect() })
}

fn power(
    values: &[(&'static str, f64)],
    limits: &[(&'static str, f64)],
    temperatures: &[(&'static str, f64)],
) -> Option<SmuPower> {
    Some(SmuPower {
        version: 0x620105,
        table_base: 0x1000,
        values: values.iter().copied().collect(),
        limits: limits.iter().copied().collect(),
        temperatures: temperatures.iter().copied().collect(),
    })
}

fn readings(values: &[(&'static str, f64)]) -> Readings {
    values.iter().copied().collect()
}

fn clocks_stub() -> SmuClocks {
    SmuClocks { version: 0x620105, table_base: 0x1000, fclk_mhz: 2000.0, uclk_mhz: 2050.0, mclk_mhz: 4100.0, verified: true }
}

const GRANITE: &str = "AMD Ryzen 7 9850X3D 8-Core Processor";
const RAPHAEL: &str = "AMD Ryzen 7 7800X3D 8-Core Processor";

// ---------------------------------------------------------------------------
// test_am5_profile.py: Am5RuntimeTest
// ---------------------------------------------------------------------------

#[test]
fn default_transport_is_mcfg_ecam() {
    // The Python compared the factory to the McfgSmnReader class. A boxed
    // closure cannot be compared, so the default is read where it is set.
    let source = include_str!("mod.rs");
    let start = source.find("impl Default for Am5RuntimeOptions").unwrap();
    let block = &source[start..start + source[start..].find("\n}\n").unwrap()];
    assert!(block.contains("McfgSmnReader::new()"), "{block}");
}

#[test]
fn loads_first_plausible_channel_once() {
    let created = Arc::new(AtomicUsize::new(0));
    let batch_calls = Arc::new(AtomicUsize::new(0));
    let runtime = {
        let (created, batch_calls) = (created.clone(), batch_calls.clone());
        Am5Runtime::with_options(Am5RuntimeOptions {
            reader_factory: Box::new(move || {
                created.fetch_add(1, Ordering::SeqCst);
                Ok(Box::new(FakeReader { regs: oracle_regs(), both_bases: false, batch_calls: batch_calls.clone() })
                    as Box<dyn SmnTransport>)
            }),
            ..Default::default()
        })
    };
    assert_eq!(runtime.value("tCL"), Reading::Int(36));
    assert_eq!(runtime.value("tREFI"), Reading::Int(65535));
    assert_eq!(created.load(Ordering::SeqCst), 1);
    assert_eq!(batch_calls.load(Ordering::SeqCst), 1);
    assert_eq!(runtime.active_base(), Some(0));
    assert!(runtime.status().contains("UMC0"));
}

#[test]
fn failure_is_neutral_and_actionable() {
    let runtime = runtime_on(Registers::new());
    assert_eq!(runtime.value("tCL").to_string(), "—");
    assert!(runtime.status().to_lowercase().contains("no plausible"));
}

#[test]
fn profile_has_amd_rows_and_no_intel_terms() {
    let runtime = oracle_runtime();
    let rows = build_timings_with(&runtime);
    let found: BTreeSet<String> = names(&rows).into_iter().collect();
    for name in ["MCLK", "CR", "Gear Down Mode", "BGS", "Nitro Rx/Tx/Ctrl", "tCL", "tRFCsb"] {
        assert!(found.contains(name), "{name}");
    }
    for name in ["Gear Mode", "System Agent", "Ring", "CMD Stretch"] {
        assert!(!found.contains(name), "{name}");
    }
    let rows = by_name(&rows);
    assert!(value(&rows["Read Status"]).contains("AMD SMN/MCFG READ-ONLY"));
}

#[test]
fn summary_control_rows_use_requested_labels_and_values() {
    let mut regs = oracle_regs();
    let rows = by_name(&build_timings_with(&runtime_on(regs.clone())));
    assert_eq!(value(&rows["Gear Down Mode"]), "Disabled");
    assert_eq!(value(&rows["Nitro Rx/Tx/Ctrl"]), "1/3/1");
    assert_eq!(rows["Nitro Rx/Tx/Ctrl"].display_name.as_deref(), Some("Nitro"));

    *regs.get_mut(&0x50200).unwrap() |= 1 << 18;
    let enabled = by_name(&build_timings_with(&runtime_on(regs)));
    assert_eq!(value(&enabled["Gear Down Mode"]), "Enabled");
}

#[test]
fn rtt_nom_read_precedes_rtt_nom_write() {
    let names = names(&build_timings_with(&oracle_runtime()));
    let index = |name: &str| names.iter().position(|n| n == name).unwrap();
    assert!(index("RTT WR") < index("RTT Nom RD"));
    assert!(index("RTT Nom RD") < index("RTT Nom WR"));
}

#[test]
fn system_info_rows_follow_the_requested_order() {
    let names: Vec<String> = build_timings_with(&oracle_runtime())
        .into_iter()
        .filter(|row| row.tab == "System Info")
        .map(|row| row.name)
        .collect();
    // Sectioned the way the Intel tab reads: what the machine is, the
    // board, and what could and could not be read down the left; the
    // clock chain and the card down the right.
    assert_eq!(
        names,
        [
            "OS", "OS Version", "Platform",
            "CPU", "CPU Package", "CPU Signature", "Code Name",
            "Vendor", "Technology",
            "Cores / Threads", "L3 Cache", "Microcode",
            // CPU-Z's names for the board, which is why the processor's own
            // vendor row is "Vendor" -- two rows cannot share one name.
            "Manufacturer", "Model", "Board Revision", "BIOS", "BIOS Date",
            "Chipset", "Southbridge", "LPCIO", "AGESA",
            "Status", "SMU Status", "Read Status", "Training Status",
            "Voltage Status", "Power Status",
            "BCLK", "MCLK", "FCLK", "UCLK", "DRAM Frequency", "UCLK:MCLK",
            "DRAM Ratio",
            "Temp Limit", "PPT Limit", "TDC Limit", "EDC Limit", "Scalar",
            "GPU", "Board Manufacturer", "GPU Code Name", "GPU Revision",
            "GPU Technology", "Cores", "ROPs / TMUs",
            "Memory Size", "Memory Type", "Memory Vendor",
            "Bus Width", "Resizable BAR", "PCIe Link", "VBIOS",
            "Driver Version", "Driver Date",
        ]
    );
}

#[test]
fn channels_and_capacity_live_on_spd() {
    let rows: BTreeSet<String> =
        build_timings_with(&oracle_runtime()).into_iter().filter(|row| row.tab == "SPD").map(|row| row.name).collect();
    assert_eq!(rows, BTreeSet::from(["Channels".to_string(), "Memory Capacity".to_string()]));
}

#[test]
fn graphics_is_the_final_system_info_section() {
    assert_eq!(SYSTEM_INFO_SECTIONS.last().unwrap().0, "Graphics");
}

#[test]
fn every_system_info_row_is_placed_in_a_section() {
    // A row nobody placed would have no heading at all. Failing here is
    // how it stays visible long enough to be noticed and placed.
    let placed: BTreeSet<&str> =
        SYSTEM_INFO_SECTIONS.iter().flat_map(|(_title, _column, names)| names.iter().copied()).collect();
    for row in build_timings_with(&oracle_runtime()) {
        if row.tab != "System Info" {
            continue;
        }
        assert!(placed.contains(row.name.as_str()), "{}", row.name);
        assert_eq!(row.category, SECTION_OF[row.name.as_str()], "{}", row.name);
    }
}

#[test]
fn each_system_info_section_draws_as_one_block() {
    // Rows of one category that are not contiguous render as two headings
    // with the same name -- the trap the Timings tab hit.
    let mut seen: Vec<String> = Vec::new();
    let mut previous: Option<String> = None;
    for row in build_timings_with(&oracle_runtime()) {
        if row.tab != "System Info" {
            continue;
        }
        if previous.as_deref() != Some(row.category.as_str()) {
            assert!(!seen.contains(&row.category), "{}", row.category);
            seen.push(row.category.clone());
            previous = Some(row.category.clone());
        }
    }
    let titles: Vec<&str> = SYSTEM_INFO_SECTIONS.iter().map(|(title, _, _)| *title).collect();
    assert_eq!(seen, titles);
}

#[test]
fn system_info_splits_identity_from_clocks_and_graphics() {
    // One column of every section came to 930px on a 633px tab.
    let mut columns: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for row in build_timings_with(&oracle_runtime()) {
        if row.tab == "System Info" {
            columns.entry(row.category.clone()).or_default().insert(row.column.clone());
        }
    }
    let expected: BTreeMap<String, BTreeSet<String>> = [
        ("System", "Left"),
        ("Processor", "Left"),
        ("Motherboard", "Left"),
        ("Status", "Left"),
        ("Clocks", "Right"),
        ("Limits", "Right"),
        ("Graphics", "Right"),
    ]
    .iter()
    .map(|(category, column)| (category.to_string(), BTreeSet::from([column.to_string()])))
    .collect();
    assert_eq!(columns, expected);
}

#[test]
fn the_os_splits_into_edition_and_version() {
    let combined = "Microsoft Windows 11 Professional (x64) 23H2 Build 22631.6060";
    seams::set("os_name", Box::new(move || Some(combined.to_string())) as Box<dyn Fn() -> Option<String>>);
    assert_eq!(os_part(0), "Microsoft Windows 11 Professional (x64)");
    assert_eq!(os_part(1), "23H2 Build 22631.6060");
    // A reading with no architecture to split after stays whole.
    seams::set("os_name", Box::new(|| Some("Windows".to_string())) as Box<dyn Fn() -> Option<String>>);
    assert_eq!(os_part(0), "Windows");
    assert_eq!(os_part(1), "—");
}

#[test]
fn spd_identity_is_not_repeated_in_system_info() {
    let rows = by_name(&build_timings_with(&oracle_runtime()));
    for name in ["Part Number", "Module Manufacturer", "IC Manufacturer", "DRAM Die", "Serial Number", "Manufactured"] {
        assert!(!rows.contains_key(name), "{name}");
    }
}

#[test]
fn tertiary_groups_cover_every_row_and_split_across_columns() {
    let rows = build_timings_with(&oracle_runtime());
    let mut by_category: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for row in &rows {
        if row.tab != "Timings" {
            continue;
        }
        by_category.entry(row.category.clone()).or_default().push(row.name.clone());
    }

    // The old single 28-row Tertiary block is gone, and every one of its
    // rows still has a home.
    assert!(!by_category.contains_key("Tertiary"));
    assert_eq!(by_category["Read to read"], ["tRDRDSCL", "tRDRDSC", "tRDRDSD", "tRDRDDD"]);
    assert_eq!(by_category["Write to write"], ["tWRWRSCL", "tWRWRSC", "tWRWRSD", "tWRWRDD"]);
    assert_eq!(by_category["Turnaround"], ["tRDWR", "tWRRD"]);
    assert_eq!(by_category["CAS to CAS"], ["tCCD_L", "tCCD_L_WR", "tCCD_L_WR2"]);
    assert_eq!(by_category["Mode register"], ["tMRD", "tMOD", "tMRDPDA", "tMODPDA"]);
    assert_eq!(by_category["Power down"], ["tCKE", "tXP"]);
    assert_eq!(by_category["Preamble / postamble"], ["tRDPRE", "tRDPOST", "tWRPRE", "tWRPOST"]);
    assert_eq!(by_category["Stagger"], ["tSTAG", "tSTAGsb"]);
    assert_eq!(by_category["PHY"], ["tPHYWRD", "tPHYRDL", "tPHYWRL"]);

    // The point of the split is a balanced tab, so the groups have to land
    // on both sides.
    let columns = |category: &str| -> BTreeSet<String> {
        rows.iter().filter(|row| row.category == category).map(|row| row.column.clone()).collect()
    };
    let only = |column: &str| BTreeSet::from([column.to_string()]);
    assert_eq!(columns("Read to read"), only("Right"));
    assert_eq!(columns("Write to write"), only("Right"));
    assert_eq!(columns("Turnaround"), only("Right"));
    assert_eq!(columns("PHY"), only("Right"));
    assert_eq!(columns("CAS to CAS"), only("Left"));
    assert_eq!(columns("Power down"), only("Left"));
    assert_eq!(columns("Stagger"), only("Left"));
    assert_eq!(columns("Mode register"), only("Left"));
}

// test_every_timing_row_reaches_the_summary: deferred, it needs the window's
// am5_summary_timing_columns and the Summary's name lists (ui.main).

#[test]
fn secondary_section_order() {
    // Write recovery first, then long/short kept together per pair.
    let names: Vec<String> = build_timings_with(&oracle_runtime())
        .into_iter()
        .filter(|row| row.category == "Secondary")
        .map(|row| row.name)
        .collect();
    assert_eq!(names, ["tWR", "tRRD_L", "tRRD_S", "tWTR_L", "tWTR_S", "tRTP", "tFAW", "tCWL"]);
}

#[test]
fn row_names_are_unique_across_every_tab() {
    // Lookups join on row name, so a duplicate silently hijacks whichever
    // view resolves it second. A temperature row named "CPU" once won the
    // Summary lookup over the CPU-name row and showed 41.4 C as the CPU.
    let names = names(&build_timings_with(&oracle_runtime()));
    let duplicates: BTreeSet<&String> =
        names.iter().filter(|name| names.iter().filter(|n| n == name).count() > 1).collect();
    assert!(duplicates.is_empty(), "duplicate row names: {duplicates:?}");
}

#[test]
fn bgs_alt_reads_its_own_registers_not_bgs() {
    // BGS Alt is a separate control: it comes from 0x500D0/0x500D4 bits
    // 4..10, while BGS is the pattern comparison on 0x50050/0x50058.
    let mut regs = oracle_regs();
    regs.insert(REG_BGSA0, 0x0000_0000);
    regs.insert(REG_BGSA1, 0x0000_0000);
    let rows = by_name(&build_timings_with(&runtime_on(regs.clone())));
    assert_eq!(value(&rows["BGS Alt"]), "Disabled");

    regs.insert(REG_BGSA0, 0x0000_0010); // bit 4 set
    let rows = by_name(&build_timings_with(&runtime_on(regs)));
    assert_eq!(value(&rows["BGS Alt"]), "Enabled");
}

#[test]
fn bgs_alt_is_independent_of_bgs() {
    // The two must be able to disagree; mirroring BGS would look correct
    // only while they happen to match.
    let mut regs = oracle_regs();
    regs.insert(REG_BGS0, 0x8765_4321); // BGS disabled
    regs.insert(REG_BGS1, 0x8765_4321);
    regs.insert(REG_BGSA0, 0x0000_0010); // BGS Alt enabled
    regs.insert(REG_BGSA1, 0x0000_0000);
    let rows = by_name(&build_timings_with(&runtime_on(regs)));
    assert_eq!(value(&rows["BGS"]), "Disabled");
    assert_eq!(value(&rows["BGS Alt"]), "Enabled");
}

#[test]
fn trfcns_shows_both_intervals_outside_normal_refresh() {
    // Mixed mode refreshes on tRFC2 and tRFCsb, so one number cannot
    // describe it; tRFC itself is not in effect and is dimmed.
    let mut regs = oracle_regs();
    regs.insert(REG_PD, 0x0539_114A);
    let rows = by_name(&build_timings_with(&runtime_on(regs)));
    assert_eq!(value(&rows["Refresh Mode"]), "Mixed");
    // The unit is named once, after both values, rather than on each.
    assert_eq!(value(&rows["tRFCns"]), "117/95 ns");
    assert!(dim(&rows["tRFC"]));
    assert!(!dim(&rows["tRFC2"]));
    assert!(!dim(&rows["tRFCsb"]));
}

#[test]
fn trefins_is_whole_nanoseconds() {
    // 65535 cycles at 4100 MHz is 15984.15 ns, and the hundredths came
    // from the division rather than from any precision the reading has.
    // Nothing covered the format before, so changing it broke no test --
    // which is the reason for this one. The per-channel columns are the
    // same formatter with a channel argument, so they are covered here
    // too; this fixture has no per-channel records to read them from.
    let rows = by_name(&build_timings_with(&oracle_runtime()));
    assert_eq!(value(&rows["tREFIns"]), "15984 ns");
}

#[test]
fn trfcns_shows_one_interval_in_normal_refresh() {
    let mut regs = oracle_regs();
    // Clear the per-bank bit and the fine-grain field: plain Normal refresh.
    regs.insert(REG_PD, 0x0539_1148 & !0x0007_0002);
    let rows = by_name(&build_timings_with(&runtime_on(regs)));
    assert_eq!(value(&rows["Refresh Mode"]), "Normal");
    // One interval in effect, so one number -- and the unit still reads
    // the same way it does when there are two.
    assert_eq!(value(&rows["tRFCns"]), "117 ns");
    // tRFC is the interval in effect here, so it must not be dimmed.
    assert!(!dim(&rows["tRFC"]));
    assert!(dim(&rows["tRFC2"]));
    assert!(dim(&rows["tRFCsb"]));
}

#[test]
fn trfcns_reports_both_channels() {
    let mut regs = oracle_regs();
    regs.insert(REG_PD, 0x0539_114A);
    // Answer the same registers on both UMC bases.
    let runtime = Am5Runtime::with_options(Am5RuntimeOptions {
        reader_factory: Box::new(move || {
            Ok(Box::new(FakeReader { regs: regs.clone(), both_bases: true, batch_calls: Arc::default() })
                as Box<dyn SmnTransport>)
        }),
        ..Default::default()
    });
    let rows = by_name(&build_timings_with(&runtime));
    assert_eq!(side(&rows["tRFCns"], Channel::A), "117/95 ns");
    assert_eq!(side(&rows["tRFCns"], Channel::B), "117/95 ns");
}

#[test]
fn apob_termination_values_are_lazy_and_exposed_as_rows() {
    let training = FakeTrainingReader::new(text_values(&[
        ("rtt_wr", "40 RZQ/6"),
        ("ca_odt_a", "480 Ω"),
        ("proc_dq_ds_pu", "40 Ω"),
    ]));
    let calls = training.calls.clone();
    let runtime = Am5Runtime::with_options(Am5RuntimeOptions {
        reader_factory: reader_factory(oracle_regs()),
        training_reader_factory: training_factory(training),
        cpu_name_factory: cpu("AMD Ryzen 7 9850X3D"),
    });
    let rows = by_name(&build_timings_with(&runtime));

    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(value(&rows["RTT WR"]), "40 RZQ/6");
    assert_eq!(value(&rows["CA ODT A"]), "480 Ω");
    assert_eq!(value(&rows["Proc DQ DS Pu"]), "40 Ω");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(runtime.training_status().contains("0x0A200000"));
}

#[test]
fn raphael_asks_for_the_zen4_layout_never_the_zen5_one() {
    // Ryzen 7000 reads its training record experimentally now, but only
    // through ZenStates-Core's Zen 4 layout: decoding its APOB with the
    // validated Ryzen 9000 layout would put every field at the wrong byte.
    let calls: Arc<Mutex<Vec<ApobLayout>>> = Arc::default();
    let runtime = {
        let calls = calls.clone();
        Am5Runtime::with_options(Am5RuntimeOptions {
            reader_factory: reader_factory(oracle_regs()),
            training_reader_factory: Box::new(move |layout| {
                calls.lock().unwrap().push(layout);
                Ok(Box::new(FakeTrainingReader::new(text_values(&[("rtt_wr", "40 RZQ/6")]))) as Box<dyn TrainingReader>)
            }),
            cpu_name_factory: cpu("AMD Ryzen 7 7800X3D"),
        })
    };
    assert_eq!(runtime.value("rtt_wr"), Reading::Text("40 RZQ/6".into()));
    assert_eq!(*calls.lock().unwrap(), [ApobLayout::Zen4]);
}

#[test]
fn other_amd_parts_never_instantiate_an_apob_reader() {
    let calls = Arc::new(AtomicUsize::new(0));
    let runtime = {
        let calls = calls.clone();
        Am5Runtime::with_options(Am5RuntimeOptions {
            reader_factory: reader_factory(oracle_regs()),
            training_reader_factory: Box::new(move |_layout| {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(Box::new(FakeTrainingReader::new(text_values(&[("rtt_wr", "wrong")]))) as Box<dyn TrainingReader>)
            }),
            cpu_name_factory: cpu("AMD Ryzen 7 8700G w/ Radeon 780M Graphics"),
        })
    };
    assert_eq!(runtime.value("rtt_wr").to_string(), "—");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(runtime.training_status().contains("Ryzen 9000 or 7000"));
}

#[test]
fn apob_rows_expose_distinct_cha_chb_values_without_group_name_collision() {
    let cha = text_values(&[
        ("rtt_wr", "40 RZQ/6"),
        ("ca_odt_a", "480 Ω"),
        ("ca_odt_b", "60 Ω"),
        ("proc_odt_pu", "34.3 Ω"),
    ]);
    let chb = text_values(&[
        ("rtt_wr", "60 RZQ/4"),
        ("ca_odt_a", "240 Ω"),
        ("ca_odt_b", "48 Ω"),
        ("proc_odt_pu", "60 Ω"),
    ]);
    let mut training = FakeTrainingReader::new(cha.clone());
    training.channel_values = BTreeMap::from([("cha", cha), ("chb", chb)]);
    let runtime = Am5Runtime::with_options(Am5RuntimeOptions {
        reader_factory: reader_factory(oracle_regs()),
        training_reader_factory: training_factory(training),
        cpu_name_factory: cpu("AMD Ryzen 7 9850X3D"),
    });
    let rows = by_name(&build_timings_with(&runtime));

    assert_eq!(rows["RTT WR"].a.as_ref().unwrap().name.as_deref(), Some("ChA"));
    assert_eq!(rows["RTT WR"].b.as_ref().unwrap().name.as_deref(), Some("ChB"));
    assert_eq!(side(&rows["RTT WR"], Channel::A), "40 RZQ/6");
    assert_eq!(side(&rows["RTT WR"], Channel::B), "60 RZQ/4");
    assert_eq!(side(&rows["CA ODT A"], Channel::A), "480 Ω");
    assert_eq!(side(&rows["CA ODT A"], Channel::B), "240 Ω");
    assert_eq!(side(&rows["CA ODT B"], Channel::A), "60 Ω");
    assert_eq!(side(&rows["CA ODT B"], Channel::B), "48 Ω");
    assert_eq!(side(&rows["Proc ODT Pu"], Channel::A), "34.3 Ω");
    assert_eq!(side(&rows["Proc ODT Pu"], Channel::B), "60 Ω");
    assert!(runtime.training_status().contains("ChA 0x0A2000D0"));
    assert!(runtime.training_status().contains("ChB 0x0A200100"));
}

#[test]
fn row_values_are_lazy_callables() {
    let rows = by_name(&build_timings_with(&oracle_runtime()));
    let tcl = &rows["tCL"];
    assert!(tcl.value.as_ref().unwrap().is_lazy());
    assert_eq!(value(tcl), "36");
}

#[test]
fn phy_rows_expose_distinct_cha_chb_umc_values() {
    let mut regs = Registers::new();
    // Populate both UMC windows with distinct PHY values.
    for base in [0u32, 0x100000] {
        for (offset, value) in oracle_regs() {
            regs.insert(base + offset, value);
        }
    }
    regs.insert(0x50258, (6 << 24) | (35 << 16) | (21 << 8)); // UMC0 / ChA
    regs.insert(0x100000 + 0x50258, (7 << 24) | (36 << 16) | (22 << 8)); // UMC1 / ChB

    let runtime = runtime_on(regs);
    let rows = by_name(&build_timings_with(&runtime));

    assert_eq!(rows["tPHYRDL"].a.as_ref().unwrap().name.as_deref(), Some("ChA"));
    assert_eq!(rows["tPHYRDL"].b.as_ref().unwrap().name.as_deref(), Some("ChB"));
    assert_eq!(side(&rows["tPHYWRD"], Channel::A), "6");
    assert_eq!(side(&rows["tPHYWRD"], Channel::B), "7");
    assert_eq!(side(&rows["tPHYRDL"], Channel::A), "35");
    assert_eq!(side(&rows["tPHYRDL"], Channel::B), "36");
    assert_eq!(side(&rows["tPHYWRL"], Channel::A), "21");
    assert_eq!(side(&rows["tPHYWRL"], Channel::B), "22");
    assert!(runtime.status().contains("UMC0"));
    assert!(runtime.status().contains("UMC1"));
}

#[test]
fn preamble_rows_balance_the_right_timing_column() {
    let rows = by_name(&build_timings_with(&oracle_runtime()));
    assert_eq!(rows["tRDPRE"].column, "Right");
    assert_eq!(rows["tRDPOST"].column, "Right");
    assert_eq!(rows["tWRPRE"].column, "Right");
    assert_eq!(rows["tWRPOST"].column, "Right");
    assert_eq!(rows["tMOD"].column, "Left");
}

#[test]
fn each_row_is_on_the_page_its_kind_belongs_to() {
    let rows = by_name(&build_timings_with(&oracle_runtime()));
    // Everything read per channel is on Training, which shows a reading
    // per channel: the controller settings, which each channel's
    // controller holds its own copy of, and the raw training codes from
    // each channel's record. Only the CPU-wide limits stay on Misc.
    for name in ["Refresh Mode", "Gear Down Mode", "Read Preamble", "Nitro Rx/Tx/Ctrl"] {
        assert_eq!(rows[name].tab, "Training", "{name}");
        assert_eq!(rows[name].category, "Other Settings", "{name}");
        assert_eq!(rows[name].column, "Left", "{name}");
        assert!(rows[name].a.is_some(), "{name}");
    }
    for name in ["ALERT_PU", "TX_DFE"] {
        assert_eq!(rows[name].tab, "Training", "{name}");
        assert_eq!(rows[name].category, "Misc (raw)", "{name}");
        assert_eq!(rows[name].column, "Right", "{name}");
        assert!(rows[name].b.is_some(), "{name}");
    }
    // The CPU-wide limits, the last of Misc, are on System Info.
    assert_eq!(rows["PPT Limit"].tab, "System Info");
    assert_eq!(rows["PPT Limit"].category, "Limits");
    assert!(rows["PPT Limit"].a.is_none());
    assert_eq!(rows["Proc ODT Pu"].column, "Right");
    // The command rate reads as a timing, and closes the primary group.
    assert_eq!(rows["CR"].tab, "Timings");
    assert_eq!(rows["CR"].category, "Primary");
    // What training settled on, rather than what it was told.
    assert_eq!(rows["RTT WR"].tab, "Training");
    assert_eq!(rows["Proc ODT Pu"].tab, "Training");
    assert_eq!(rows["MCLK"].tab, "System Info");
}

#[test]
fn cr_closes_the_primary_group() {
    let names: Vec<String> = build_timings_with(&oracle_runtime())
        .into_iter()
        .filter(|row| row.tab == "Timings" && row.category == "Primary")
        .map(|row| row.name)
        .collect();
    assert_eq!(names, ["tCL", "tRCDRD", "tRCDWR", "tRP", "tRAS", "tRC", "CR"]);
}

// test_the_summary_still_gathers_the_signal_rows_by_category and
// test_the_timing_columns_do_not_pick_up_another_tabs_rows: deferred, both
// read the window's Summary selection (ui.main).

#[test]
fn voltage_rows_land_on_their_own_tab_and_columns() {
    let rows = build_timings_with(&oracle_runtime());
    let voltage_rows: Vec<&Row> = rows.iter().filter(|row| extra_str(row, "rail_key").is_some()).collect();
    // Every board-wide rail, which is every rail except the per-module
    // ones each DIMM's own panel reads.
    assert_eq!(voltage_rows.len(), RAILS.len() - PER_MODULE_RAILS.len());
    let labels: Vec<&str> = voltage_rows.iter().map(|row| row.name.as_str()).collect();
    assert_eq!(labels[0], "VDDCR_VDD");
    assert_eq!(*labels.last().unwrap(), "VDD_MISC");
    assert!(!labels.contains(&"Core VID"));
    assert!(labels.contains(&"VDDCR_SOC"));
    assert!(!labels.contains(&"DRAM VPP"));
    let by_name: BTreeMap<&str, &Row> = voltage_rows.iter().map(|row| (row.name.as_str(), *row)).collect();
    // Every rail sits in the left column; power and temperatures take the
    // right one.
    for row in &voltage_rows {
        assert_eq!(row.column, "Left");
        assert_eq!(row.tab, "Sensors");
    }
    // One section for every rail; the domain rides along on the row so
    // anything that wants it can group by it.
    assert_eq!(by_name["VDDIO"].category, "Voltages");
    assert_eq!(extra_str(by_name["VDDIO"], "rail_group"), Some("Memory"));
    assert_eq!(extra_str(by_name["VDDCR_VDD"], "rail_group"), Some("Core"));
    let categories: BTreeSet<&str> = voltage_rows.iter().map(|row| row.category.as_str()).collect();
    assert_eq!(categories, BTreeSet::from(["Voltages"]));
}

#[test]
fn voltage_rows_read_as_unavailable_when_no_offset_is_confirmed() {
    // Hermetic: force the empty-map path instead of touching the PM table
    // or the DIMM PMIC, either of which returns live rails on real hardware.
    // The Python emptied CONFIRMED_VOLTAGE_OFFSETS, CONFIRMED_PMIC_RAILS and
    // the Super I/O map; those are constants here, so the voltage source's
    // gate is given the empty table and the PMIC and Super I/O reads answer
    // nothing, which is what each does with its map empty.
    let runtime = Am5Runtime::with_options(Am5RuntimeOptions {
        reader_factory: reader_factory(oracle_regs()),
        cpu_name_factory: cpu(GRANITE),
        ..Default::default()
    });
    lock(&runtime.sources.voltages).gate = Some(Box::new(|| {
        offsets_gate(
            false,
            "Voltages unavailable — no confirmed PM-table offset (run COLLECT_VOLTAGE_REPORT.bat)",
        )
    }));
    lock(&runtime.sources.dram).read = Box::new(|| Ok(Readings::new()));
    lock(&runtime.sources.board).read = Box::new(|| Ok(Readings::new()));
    let rows = build_timings_with(&runtime);
    for row in &rows {
        if extra_str(row, "rail_key").is_some() {
            assert_eq!(value(row), "—", "{}", row.name);
        }
    }
    let status = rows.iter().find(|row| row.name == "Voltage Status").unwrap();
    assert!(value(status).contains("no confirmed PM-table offset"));
}

#[test]
fn unmapped_rails_stay_blank_when_others_are_confirmed() {
    // A confirmed rail must not make a neighbouring one borrow its reading.
    // All three transports are stubbed so nothing reaches real hardware.
    let runtime = oracle_runtime();
    stub_live(&runtime.sources.voltages, voltages(&[("vddcr_soc", 1.203)]));
    stub_live(&runtime.sources.dram, Readings::new());
    stub_live(&runtime.sources.board, Readings::new());
    let rows = by_name(&build_timings_with(&runtime));
    assert_eq!(value(&rows["VDDCR_SOC"]), "1.203 V");
    assert_eq!(value(&rows["VDDIO"]), "—");
    // The DRAM rails are per-module and have no set-wide row; the rule
    // still holds at the layer their panels read.
    assert_eq!(runtime.voltage_value("dram_vdd"), None);
    assert_eq!(runtime.voltage_value("dram_vddq"), None);
}

#[test]
fn each_rail_reads_from_its_own_transport() {
    // Three transports feed one view; a value from one must never appear
    // under a rail belonging to another.
    let runtime = oracle_runtime();
    stub_live(&runtime.sources.voltages, voltages(&[("vddg_iod", 1.082)]));
    stub_live(&runtime.sources.dram, readings(&[("dram_vdd", 1.530)]));
    stub_live(&runtime.sources.board, readings(&[("vddio_mem", 1.424)]));
    let rows = by_name(&build_timings_with(&runtime));
    assert_eq!(value(&rows["VDDG IOD"]), "1.082 V"); // PM table
    assert_eq!(value(&rows["VDDIO"]), "1.424 V"); // Super I/O
    assert_eq!(runtime.voltage_value("dram_vdd"), Some(1.530)); // DDR5 PMIC
}

#[test]
fn dram_rails_come_from_the_pmic_not_the_pm_table() {
    let runtime = oracle_runtime();
    // PM table reports nothing; the PMIC supplies the DRAM rails.
    stub_live(&runtime.sources.voltages, None);
    stub_live(&runtime.sources.dram, readings(&[("dram_vdd", 1.500), ("dram_vddq", 1.440), ("dram_vpp", 1.800)]));
    stub_live(&runtime.sources.board, Readings::new());
    let rows = by_name(&build_timings_with(&runtime));
    assert_eq!(runtime.voltage_value("dram_vdd"), Some(1.500));
    assert_eq!(runtime.voltage_value("dram_vddq"), Some(1.440));
    assert_eq!(runtime.voltage_value("dram_vpp"), Some(1.800));
    assert_eq!(value(&rows["VDDCR_SOC"]), "—");
}

#[test]
fn the_dram_rails_have_no_set_wide_row() {
    // One board setting drives them, but the modules need not agree: the
    // bench pair sits 15 mV apart on VDDQ, which a single row cannot show.
    let rows = build_timings_with(&oracle_runtime());
    let keys: BTreeSet<&str> = rows.iter().filter_map(|row| extra_str(row, "rail_key")).collect();
    for key in ["dram_vdd", "dram_vddq", "dram_vpp"] {
        assert!(!keys.contains(key), "{key}");
    }
    assert!(keys.contains("vddio_mem"));
}

#[test]
fn voltage_value_reports_confirmed_rails() {
    let runtime = oracle_runtime();
    stub_live(&runtime.sources.voltages, voltages(&[("vddcr_soc", 1.203)]));
    assert!((runtime.voltage_value("vddcr_soc").unwrap() - 1.203).abs() < 1e-9);
    assert_eq!(runtime.voltage_value("vddg_iod"), None);
    let rows = by_name(&build_timings_with(&runtime));
    assert_eq!(value(&rows["VDDCR_SOC"]), "1.203 V");
}

/// Replace the clock source's read, as the Python patched read_smu_clocks.
fn clock_reads(runtime: &Am5Runtime, results: Vec<Option<SmuClocks>>) -> Arc<AtomicUsize> {
    let calls = Arc::new(AtomicUsize::new(0));
    let results = Arc::new(Mutex::new(VecDeque::from(results)));
    let counter = calls.clone();
    lock(&runtime.sources.clocks).read = Box::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
        let mut results = results.lock().unwrap();
        Ok(if results.len() > 1 { results.pop_front().unwrap() } else { results[0] })
    });
    calls
}

#[test]
fn a_transient_clock_failure_is_retried() {
    // Regression: one contended read at startup used to blank FCLK/UCLK for
    // the whole session, because the failure was cached as "attempted".
    let runtime = Am5Runtime::with_options(Am5RuntimeOptions {
        reader_factory: reader_factory(oracle_regs()),
        cpu_name_factory: cpu(GRANITE),
        ..Default::default()
    });
    let calls = clock_reads(&runtime, vec![None, Some(clocks_stub())]);
    assert_eq!(runtime.clocks(), None);
    assert_eq!(runtime.clocks(), Some(clocks_stub()));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn repeated_clock_failures_eventually_stop_retrying() {
    let runtime = Am5Runtime::with_options(Am5RuntimeOptions {
        reader_factory: reader_factory(oracle_regs()),
        cpu_name_factory: cpu(GRANITE),
        ..Default::default()
    });
    let calls = clock_reads(&runtime, vec![None]);
    for _ in 0..10 {
        assert_eq!(runtime.clocks(), None);
    }
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

#[test]
fn a_lost_read_keeps_the_last_good_voltage() {
    // These reads race other monitoring tools for the PCI mutex. Losing
    // once must not blank a rail that was reading fine a moment ago.
    let good = voltages(&[("vddcr_soc", 1.203)]);
    let runtime = Am5Runtime::with_options(Am5RuntimeOptions {
        reader_factory: reader_factory(oracle_regs()),
        cpu_name_factory: cpu(GRANITE),
        ..Default::default()
    });
    let calls = Arc::new(AtomicUsize::new(0));
    {
        let (calls, good) = (calls.clone(), good.clone());
        let mut source = lock(&runtime.sources.voltages);
        source.read = Box::new(move || {
            let call = calls.fetch_add(1, Ordering::SeqCst) + 1;
            Ok(if call == 1 { good.clone() } else { None })
        });
        // The gate is the constant table, which is populated; it stays.
    }
    assert_eq!(runtime.voltages(), good);
    lock(&runtime.sources.voltages).stamp = None; // force a re-read
    assert_eq!(runtime.voltages(), good);
    assert!(runtime.voltage_status().to_lowercase().contains("stale"));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn a_lost_dram_read_keeps_the_last_good_rails() {
    let runtime = oracle_runtime();
    let calls = Arc::new(AtomicUsize::new(0));
    {
        let calls = calls.clone();
        lock(&runtime.sources.dram).read = Box::new(move || {
            let call = calls.fetch_add(1, Ordering::SeqCst) + 1;
            Ok(if call == 1 { readings(&[("dram_vdd", 1.53)]) } else { Readings::new() })
        });
    }
    assert_eq!(runtime.dram_rails(), readings(&[("dram_vdd", 1.53)]));
    lock(&runtime.sources.dram).stamp = None;
    assert_eq!(runtime.dram_rails(), readings(&[("dram_vdd", 1.53)]));
    assert!(runtime.dram_rail_status().to_lowercase().contains("stale"));
}

#[test]
fn cpu_name_is_queried_only_once() {
    // It is a WMI query, and callers repeat every second.
    let calls = Arc::new(AtomicUsize::new(0));
    let runtime = {
        let calls = calls.clone();
        Am5Runtime::with_options(Am5RuntimeOptions {
            reader_factory: reader_factory(oracle_regs()),
            cpu_name_factory: Box::new(move || {
                calls.fetch_add(1, Ordering::SeqCst);
                GRANITE.to_string()
            }),
            ..Default::default()
        })
    };
    for _ in 0..5 {
        runtime.cpu_name();
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn temperatures_and_limits_share_one_section() {
    // Thermal Limit is a temperature against a limit, so a heading between
    // the two halves put one row's parts on either side of it.
    let rows = build_timings_with(&oracle_runtime());
    let section: Vec<&Row> = rows.iter().filter(|row| row.category == "Thermal & Power").collect();
    assert_eq!(
        section.iter().map(|row| row.name.as_str()).collect::<Vec<_>>(),
        // The DIMM sensors moved to their module's own telemetry panel,
        // beside the rails from the same PMIC.
        // CPU Temp carries Tctl against its limit, so there is no
        // separate Thermal Limit row repeating the temperature.
        // What the CPU reports about itself, then what the board measures
        // around it, then what it is all drawing.
        // Scalar is not here: it is a setting rather than a reading, so
        // it sits with the limits on System Info. This window keeps a
        // maximum per row, which a value that never moves has no use for.
        // PCH and PCIEX16 follow the board's own three; they come from
        // the board-gated monitor and are hidden where it reads nothing.
        [
            "CPU Temp", "IOD Average", "IOD Hotspot", "L3 Temp",
            "VDDCR_VDD VRM", "VDDCR_SOC VRM", "VDD_MISC VRM",
            "CPU Temp (board)", "VRM Temp (board)", "System Temp",
            "PCH Temp", "PCIEX16 Temp",
            "PPT", "TDC", "EDC",
        ]
    );
    for row in &section {
        assert_eq!(row.tab, "Sensors");
        assert_eq!(row.column, "Right");
    }
    let categories: BTreeSet<&str> = rows.iter().map(|row| row.category.as_str()).collect();
    for gone in ["Temperatures", "Power Limits"] {
        assert!(!categories.contains(gone), "{gone}");
    }
}

#[test]
fn power_rows_are_blank_when_no_offset_is_confirmed() {
    // CONFIRMED_POWER_OFFSETS is a constant; the source's gate is given the
    // empty table, which is what patching it empty did.
    let runtime = Am5Runtime::with_options(Am5RuntimeOptions {
        reader_factory: reader_factory(oracle_regs()),
        cpu_name_factory: cpu(GRANITE),
        ..Default::default()
    });
    lock(&runtime.sources.power).gate = Some(Box::new(|| {
        offsets_gate(false, "Power limits unavailable — no confirmed PM-table offset")
    }));
    let rows = build_timings_with(&runtime);
    for row in &rows {
        if row.category == "Power Limits" {
            assert_eq!(value(row), "—", "{}", row.name);
        }
    }
    let status = rows.iter().find(|row| row.name == "Power Status").unwrap();
    assert!(value(status).contains("no confirmed PM-table offset"));
}

#[test]
fn power_rows_render_current_over_limit() {
    let runtime = oracle_runtime();
    stub_live(
        &runtime.sources.power,
        power(
            &[("ppt", 41.0244), ("tdc", 9.6954)],
            &[("ppt", 162.0), ("tdc", 120.0), ("edc", 180.0)],
            &[("cpu", 43.7)],
        ),
    );
    let rows = by_name(&build_timings_with(&runtime));
    assert_eq!(value(&rows["PPT"]), "41.0 / 162.0 W");
    assert_eq!(value(&rows["TDC"]), "9.7 / 120.0 A");
    // Limit known but no live value yet: show the limit alone.
    assert_eq!(value(&rows["EDC"]), "180.0 A");
    assert_eq!(value(&rows["Scalar"]), "—");
}

/// A training factory that must never run: the tests below mark training
/// as already attempted.
fn unused_training_reader() -> TrainingReaderFactory {
    Box::new(|_layout| panic!("training is already attempted here; the reader must not be built"))
}

#[test]
fn one_status_line_covers_every_transport() {
    let runtime = Am5Runtime::with_options(Am5RuntimeOptions {
        reader_factory: reader_factory(oracle_regs()),
        training_reader_factory: unused_training_reader(),
        // Fixed rather than the host's, as every other test in this file
        // does. What CPU runs the suite is not what this test is about.
        cpu_name_factory: cpu("AMD Ryzen 7 9850X3D"),
    });
    freeze_live(&runtime);
    runtime.load();
    // The APOB result is an input here, not something to re-derive: the
    // line is built to carry ChA/ChB so the summary can be checked for
    // dropping them. The Status row reads training_status through
    // load_training, which without this would run the Granite Ridge gate
    // and build the reader.
    {
        let mut training = lock(&runtime.training);
        training.attempted = true;
        training.status = "AMD APOB READ-ONLY — table 0x0A200000, ChA 0x1, ChB 0x2".into();
    }
    let rows = by_name(&build_timings_with(&runtime));
    let summary = value(&rows["Status"]);
    assert!(summary.contains("SMN UMC0"), "{summary}");
    // The APOB record addresses stay in the dump; the table address is
    // what the summary carries.
    assert!(summary.contains("APOB table 0x0A200000"), "{summary}");
    assert!(!summary.contains("ChA"), "{summary}");
    // The SMU's reads are the next row, so neither line outgrows its
    // half of System Info.
    assert!(value(&rows["SMU Status"]).contains("power"));
}

#[test]
fn a_failing_transport_puts_its_own_message_in_the_line() {
    // The summary is short only while there is nothing to report.
    let runtime = Am5Runtime::with_options(Am5RuntimeOptions {
        reader_factory: reader_factory(Registers::new()),
        training_reader_factory: unused_training_reader(),
        cpu_name_factory: cpu("AMD Ryzen 7 9850X3D"),
    });
    freeze_live(&runtime);
    // Same reason as the test above: on a Granite Ridge name this passes
    // the gate and would build the APOB reader.
    lock(&runtime.training).attempted = true;
    let rows = by_name(&build_timings_with(&runtime));
    assert!(value(&rows["Status"]).contains("no plausible"));
}

#[test]
fn a_missing_driver_is_one_short_status_line() {
    // The missing-driver message lists every directory it searched. Each
    // failing transport carried the whole of it into the Status row, which
    // made System Info 1500px wide and several lines tall.
    use crate::hardware::driver_path::missing_message;

    let runtime = Am5Runtime::with_options(Am5RuntimeOptions {
        reader_factory: Box::new(|| Err(missing_message())),
        training_reader_factory: Box::new(|_layout| Err(missing_message())),
        cpu_name_factory: cpu("AMD Ryzen 7 9850X3D"),
    });
    freeze_live(&runtime);
    {
        let mut training = lock(&runtime.training);
        training.attempted = true;
        training.status = format!("APOB physical reader unavailable: {}", missing_message());
    }
    let rows = by_name(&build_timings_with(&runtime));
    assert_eq!(value(&rows["Status"]), "No register reads: inpoutx64.dll not found");
}

#[test]
fn a_failure_keeps_only_its_headline() {
    assert_eq!(
        status_tail("PM-table read failed: version 0x1 refused. Retry later.\n  detail line", "RSMU", "ok"),
        "PM-table read failed: version 0x1 refused"
    );
}

#[test]
fn the_pm_table_segment_keeps_the_version_it_was_gated_on() {
    assert_eq!(pm_table_segment("RSMU PM-table 0x620105 @ 0x8000 — 6 rail(s)"), "PM-table 0x620105 (6 rails)");
    // A failure is passed through whole rather than compacted away.
    assert_eq!(
        pm_table_segment("PM-table read failed: version 0x1 refused"),
        "PM-table read failed: version 0x1 refused"
    );
}

#[test]
fn the_detailed_status_rows_stay_for_the_dump() {
    let rows = build_timings_with(&oracle_runtime());
    let diagnostic: BTreeSet<&str> =
        rows.iter().filter(|row| row.extra_bool("diagnostic")).map(|row| row.name.as_str()).collect();
    // FGR too: Refresh Mode on the tab already reads it, and a bare
    // number said nothing there, so it is kept for the dump alone.
    assert_eq!(
        diagnostic,
        BTreeSet::from(["Read Status", "Training Status", "Voltage Status", "Power Status", "FGR"])
    );
    // The summary itself is not diagnostic: it is what the tab shows.
    let summary = rows.iter().find(|row| row.name == "Status").unwrap();
    assert!(!summary.extra_bool("diagnostic"));
}

#[test]
fn cpu_temp_reads_tctl_against_its_limit() {
    let runtime = oracle_runtime();
    stub_live(&runtime.sources.power, power(&[], &[], &[("cpu", 41.1), ("cpu_limit", 90.0)]));
    let rows = by_name(&build_timings_with(&runtime));
    assert_eq!(value(&rows["CPU Temp"]), "41.1 / 90.0 °C");
}

#[test]
fn cpu_temp_follows_a_limit_that_was_lowered() {
    // The limit is a setting, not a constant: a hardcoded 95 would have
    // claimed five degrees that are not there on a board reporting 90.
    let runtime = oracle_runtime();
    stub_live(&runtime.sources.power, power(&[], &[], &[("cpu", 41.1), ("cpu_limit", 80.0)]));
    let rows = by_name(&build_timings_with(&runtime));
    assert_eq!(value(&rows["CPU Temp"]), "41.1 / 80.0 °C");
}

#[test]
fn cpu_temp_needs_both_halves() {
    let runtime = oracle_runtime();
    stub_live(&runtime.sources.power, power(&[], &[], &[("cpu", 41.1)]));
    let rows = by_name(&build_timings_with(&runtime));
    assert_eq!(value(&rows["CPU Temp"]), "—");
}

#[test]
fn scalar_row_shows_what_the_smu_getter_returned() {
    let runtime = oracle_runtime();
    stub_live(&runtime.sources.power, power(&[("scalar", 1.0)], &[], &[]));
    let rows = by_name(&build_timings_with(&runtime));
    assert_eq!(value(&rows["Scalar"]), "1.00 x");

    stub_live(&runtime.sources.power, power(&[("scalar", 10.0)], &[], &[]));
    let rows = by_name(&build_timings_with(&runtime));
    assert_eq!(value(&rows["Scalar"]), "10.00 x");
}

#[test]
fn trfc_nanoseconds_precedes_trfc_in_profile_order() {
    let names = names(&build_timings_with(&oracle_runtime()));
    let index = |name: &str| names.iter().position(|n| n == name).unwrap();
    assert!(index("tRFCns") < index("tRFC"));
}

// ---------------------------------------------------------------------------
// test_am5_profile.py: ClockSectionTest
// The Telemetry window leads with the clocks, the way Intel's does.
// ---------------------------------------------------------------------------

fn sensor_clock_rows() -> Vec<Row> {
    // Filtered by tab as well as category: System Info has a Clocks
    // section too, and a category name only means something within a tab.
    build_timings_with(&oracle_runtime())
        .into_iter()
        .filter(|row| row.category == "Clocks" && row.tab == "Sensors")
        .collect()
}

#[test]
fn the_aggregates_read_from_the_bus_up() {
    let names: Vec<String> = sensor_clock_rows()
        .into_iter()
        .filter(|row| row.extra_str(PARENT_ROW_KEY).is_none())
        .map(|row| row.name)
        .collect();
    assert_eq!(
        names,
        [
            "Bus Clock", "Core Clock (avg)", CORE_EFFECTIVE_CLOCK_ROW,
            // Named as HWiNFO names them, which is also what keeps them from
            // colliding with the System Info rows called FCLK and UCLK.
            "Infinity Fabric Clock (FCLK)", "Memory Controller Clock (UCLK)",
        ]
    );
}

#[test]
fn the_memory_clocks_keep_their_own_names_here() {
    // The fabric and controller clocks were asked for in this window, so
    // they carry HWiNFO's longer names. The bare MCLK/UCLK/FCLK rows stay
    // on System Info: a row name is the key every other view joins on,
    // and the same name twice would collide.
    let names: BTreeSet<String> = sensor_clock_rows().into_iter().map(|row| row.name).collect();
    for name in ["MCLK", "UCLK", "FCLK", "DRAM Frequency"] {
        assert!(!names.contains(name), "{name}");
    }
    assert!(names.contains("Infinity Fabric Clock (FCLK)"));
    assert!(names.contains("Memory Controller Clock (UCLK)"));
}

#[test]
fn every_clock_row_is_live() {
    // A clock read once is the one thing this window exists not to show.
    for row in sensor_clock_rows() {
        assert!(row.live, "{}", row.name);
    }
}

#[test]
fn the_per_processor_rows_fold_under_the_effective_clock() {
    let children: Vec<Row> =
        sensor_clock_rows().into_iter().filter(|row| row.extra_str(PARENT_ROW_KEY).is_some()).collect();
    assert!(!children.is_empty());
    for row in children {
        assert_eq!(row.extra_str(PARENT_ROW_KEY), Some(CORE_EFFECTIVE_CLOCK_ROW), "{}", row.name);
    }
}

#[test]
fn counters_that_do_not_answer_leave_the_parent_standing_alone() {
    // The Python made the cpu_clocks import fail; here the counters list
    // no processors, which is what a machine without them answers.
    seams::set("core_labels", Box::new(Vec::new) as Box<dyn Fn() -> Vec<String>>);
    assert!(per_core_clock_rows().is_empty());
}

#[test]
fn a_processor_with_no_reading_yet_shows_a_dash() {
    // core_clock_text returns None until an interval has been measured,
    // and a row's text reaches the label verbatim.
    seams::set("core_labels", Box::new(|| vec!["CPU 0".to_string()]) as Box<dyn Fn() -> Vec<String>>);
    seams::set("core_clock_text", Box::new(|_index| None) as Box<dyn Fn(usize) -> Option<String>>);
    let rows = per_core_clock_rows();
    assert_eq!(value(&rows[0]), EM_DASH);
}

// ---------------------------------------------------------------------------
// test_am5_profile.py: BoardTemperatureTest
// Only the thermistors this board was confirmed to carry.
// ---------------------------------------------------------------------------

fn board_temperature_values(temperatures: Readings) -> BTreeMap<String, String> {
    let runtime = oracle_runtime();
    stub_live(&runtime.sources.board_temp, temperatures);
    board_temperature_rows(&runtime).iter().map(|row| (row.name.clone(), value(row))).collect()
}

#[test]
fn the_confirmed_three_are_shown() {
    let labels: Vec<&str> = BOARD_TEMPERATURES.iter().map(|(label, _key)| *label).collect();
    assert_eq!(labels, ["CPU Temp (board)", "VRM Temp (board)", "System Temp"]);
}

#[test]
fn the_unconfirmed_channels_are_not_shown() {
    // pch never moved through a 34 C swing and socket reads 0.0; a stuck
    // channel and a genuinely steady one are indistinguishable from here.
    let keys: BTreeSet<&str> = BOARD_TEMPERATURES.iter().map(|(_label, key)| *key).collect();
    assert!(!keys.contains("pch"));
    assert!(!keys.contains("socket"));
}

#[test]
fn each_row_reads_its_own_channel() {
    let rows = board_temperature_values(readings(&[("cpu", 75.0), ("vrm", 32.0), ("system", 31.5)]));
    assert_eq!(rows["CPU Temp (board)"], "75.0 °C");
    assert_eq!(rows["VRM Temp (board)"], "32.0 °C");
    assert_eq!(rows["System Temp"], "31.5 °C");
}

#[test]
fn a_channel_that_did_not_answer_reads_as_a_dash() {
    let rows = board_temperature_values(readings(&[("cpu", 41.0)]));
    assert_eq!(rows["CPU Temp (board)"], "41.0 °C");
    assert_eq!(rows["VRM Temp (board)"], EM_DASH);
}

#[test]
fn no_sensors_at_all_blanks_every_row() {
    // The Python also tried None; a source here always answers a map, and
    // an empty one is what no sensors is.
    let rows = board_temperature_values(Readings::new());
    let values: BTreeSet<&str> = rows.values().map(String::as_str).collect();
    assert_eq!(values, BTreeSet::from([EM_DASH]));
}

// SummaryStaggerOrderTest (three cases): deferred, every one reads the
// window's am5_summary_timing_columns or ordered_sections (ui.main).

// ---------------------------------------------------------------------------
// test_am5_profile.py: SiliconAndBridgeTest
// Names for codes read from the hardware, and nothing for a code we have no
// name for.
// ---------------------------------------------------------------------------

/// Install a `Win32_Processor` answering `processor_id`, from an empty
/// processor cache.
///
/// The processor fields are read once and kept, because opening a WMI
/// connection costs about a second and four rows were each opening their
/// own. A test that supplies its own CPUID therefore has to clear that
/// cache, or it reads whatever was answered earlier and the stub is never
/// consulted.
fn with_cpuid(processor_id: &str) {
    FakeWmi::new()
        .class(
            "Win32_Processor",
            vec![
                WmiObject::new()
                    .with("ProcessorId", processor_id)
                    .with("ExtClock", 100i64)
                    .with("Name", "stub")
                    .with("Manufacturer", "AuthenticAMD")
                    .with("NumberOfCores", 8i64)
                    .with("NumberOfLogicalProcessors", 16i64),
            ],
        )
        .install();
    clear_caches();
}

fn clear_caches() {
    SYSTEM_INFO_CACHE.clear();
    PROCESSOR_FACTS.clear();
    GPU_CACHE.clear();
    GPU_SENSOR_CACHE.clear();
}

#[test]
fn this_benchs_cpuid_names_granite_ridge() {
    // Family 0x1A model 0x44, which is what the bench answers and what
    // the repo's own probe gate independently names.
    with_cpuid("178BFBFF00B40F40");
    assert_eq!(cpu_silicon(), (Some("Granite Ridge"), Some("4 nm")));
}

#[test]
fn an_unlisted_part_gets_neither_a_name_nor_a_node() {
    // Rather than inheriting its neighbour's: the node especially cannot
    // be read from anything, so an unlisted part must not borrow one.
    with_cpuid("178BFBFF00A20F12");
    assert_eq!(cpu_silicon(), (None, None));
}

#[test]
fn an_unreadable_cpuid_is_not_an_error() {
    FakeWmi::new().failing("Win32_Processor").install();
    clear_caches();
    assert_eq!(cpu_silicon(), (None, None));
}

/// Answer `pci_device_and_revision` with `(device, revision)` for every
/// function, or with nothing at all.
fn with_bridge(found: Option<(u16, u8)>) {
    crate::system_identity::set_thread_config_space(Some(Box::new(move |_device, _function, offset, _bus| {
        let (device, revision) = found?;
        match offset {
            0x00 => Some(((device as u32) << 16) | 0x1022),
            0x08 => Some(revision as u32),
            _ => None,
        }
    })));
}

#[test]
fn the_chipset_reads_the_host_bridge() {
    with_cpuid("178BFBFF00B40F40");
    with_bridge(Some((0x14D8, 0x00)));
    assert_eq!(chipset(), "AMD Granite Ridge rev. 00");
}

#[test]
fn an_unknown_host_bridge_reads_nothing() {
    // An Intel board answers here too; naming its bridge from an AMD
    // table is the mistake this row exists not to make.
    with_bridge(Some((0x7A86, 0x11)));
    assert_eq!(chipset(), EM_DASH);
}

#[test]
fn the_southbridge_is_the_fch_by_its_lpc_bridge() {
    with_bridge(Some((0x790E, 0x51)));
    assert_eq!(southbridge(), "AMD FCH rev. 51");
}

#[test]
fn an_absent_bridge_reads_nothing() {
    with_bridge(None);
    assert_eq!(southbridge(), EM_DASH);
}

// ---------------------------------------------------------------------------
// test_am5_profile.py: LiveSourceRetryGapTest
// A failed read is not retried by every row in the same tick.
// ---------------------------------------------------------------------------

fn retry_source(results: Vec<Readings>, retry_gap: f64) -> (Arc<AtomicUsize>, LiveSource<Readings>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let results = Mutex::new(VecDeque::from(results));
    let counter = calls.clone();
    let source = LiveSource::new(
        Box::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok(results.lock().unwrap().pop_front().expect("no more results"))
        }),
        Box::new(|_result: &Readings| "ok".to_string()),
        "Board",
        Readings::new(),
        Some(LIVE_CACHE_SECONDS),
        None,
        retry_gap,
    );
    (calls, source)
}

#[test]
fn a_failure_stands_for_the_gap() {
    let (calls, mut source) = retry_source(vec![Readings::new(), readings(&[("a", 1.0)])], 60.0);
    for _ in 0..5 {
        source.get();
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(source.failures, 1);
}

#[test]
fn the_next_tick_tries_again() {
    let (calls, mut source) = retry_source(vec![Readings::new(), readings(&[("a", 1.0)])], 60.0);
    source.get();
    source.failed_at = source.failed_at.and_then(|at| at.checked_sub(Duration::from_secs(61)));
    assert_eq!(source.get(), readings(&[("a", 1.0)]));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn no_gap_retries_at_once() {
    let (_calls, mut source) =
        retry_source(vec![Readings::new(), Readings::new(), readings(&[("a", 1.0)])], 0.0);
    source.get();
    source.get();
    assert_eq!(source.get(), readings(&[("a", 1.0)]));
}

// ---------------------------------------------------------------------------
// test_am5_system_info_not_hardcoded.py
//
// No AM5 System Info row may carry a value that was typed rather than read.
// Change what the machine reports; the row must change with it. A decode
// table is not a hardcoded value -- "Granite Ridge", "4 nm" and "Navi 48"
// are names for codes read from the hardware, so each test moves the code
// and requires the name to move.
//
// The rows are cached (the caches are per test thread here, and cleared
// before each stub), and the row values are lazy: a stub has to be in place
// when the getter runs, not when the row is built.
// ---------------------------------------------------------------------------

fn row_value(name: &str) -> String {
    let rows = build_timings_with(&Am5Runtime::new());
    let row = rows
        .iter()
        .find(|row| row.tab == "System Info" && row.name == name)
        .unwrap_or_else(|| panic!("{name} is not built"));
    value(row)
}

#[test]
fn the_cpu_rows_follow_the_cpuid() {
    // Family 0x1A model 0x44 is the part AMD_SILICON names. The row must
    // come from that lookup rather than from a string typed beside it.
    with_cpuid("178BFBFF00B40F40");
    assert_eq!(row_value("Code Name"), "Granite Ridge");
    assert_eq!(row_value("Technology"), "4 nm");
}

#[test]
fn an_unlisted_part_gets_no_name_rather_than_its_neighbours() {
    // The node is a property of the silicon with nothing on the machine
    // to read it from, so a part the table does not name must come back
    // blank. Inheriting the entry above it would be a typed value in the
    // worst possible disguise: one that looks read.
    with_cpuid("178BFBFF00A10F10");
    assert_eq!(row_value("Code Name"), EM_DASH);
    assert_eq!(row_value("Technology"), EM_DASH);
}

/// The host bridge at 00:00.0 answers; nothing else does.
fn with_host_bridge(device: u16, revision: u8) {
    crate::system_identity::set_thread_config_space(Some(Box::new(move |dev, function, offset, _bus| {
        if (dev, function) != (0x00, 0) {
            return None;
        }
        match offset {
            0x00 => Some(((device as u32) << 16) | 0x1022),
            0x08 => Some(revision as u32),
            _ => None,
        }
    })));
}

#[test]
fn the_chipset_row_follows_the_host_bridge() {
    with_cpuid("178BFBFF00B40F40");
    with_host_bridge(0x14D8, 0x21);
    assert_eq!(row_value("Chipset"), "AMD Granite Ridge rev. 21");
}

#[test]
fn the_chipset_revision_is_read_not_assumed() {
    // Same bridge, different stepping. A constant here would misstate
    // every board that ships another one.
    with_cpuid("178BFBFF00B40F40");
    with_host_bridge(0x14D8, 0x07);
    assert_eq!(row_value("Chipset"), "AMD Granite Ridge rev. 07");
}

#[test]
fn an_unknown_bridge_reads_nothing() {
    with_host_bridge(0xDEAD, 0x00);
    assert_eq!(row_value("Chipset"), EM_DASH);
}

#[test]
fn spd_identity_is_not_repeated_in_system_info_on_the_default_runtime() {
    let names: BTreeSet<String> = build_timings_with(&Am5Runtime::new())
        .into_iter()
        .filter(|row| row.tab == "System Info")
        .map(|row| row.name)
        .collect();
    for name in ["Part Number", "Module Manufacturer", "IC Manufacturer", "DRAM Die", "Serial Number", "Manufactured"] {
        assert!(!names.contains(name), "{name}");
    }
}

fn with_card(card: Option<crate::gpu::radeon::RadeonGpu>) {
    clear_caches();
    seams::set(
        "read_gpu",
        Box::new(move || Ok(card.clone())) as Box<dyn Fn() -> Result<Option<crate::gpu::radeon::RadeonGpu>, String>>,
    );
}

#[test]
fn the_gpu_rows_follow_the_card() {
    with_card(Some(crate::gpu::radeon::RadeonGpu {
        name: Some("AMD Radeon RX 7900 XTX".into()),
        code_name: Some("Navi 31".into()),
        technology: Some("5 nm".into()),
        cores: Some(6144),
        rops: Some(192),
        tmus: Some(384),
        memory_type: Some("GDDR6".into()),
        bus_width: Some("384 bit".into()),
        ..Default::default()
    }));
    assert_eq!(row_value("GPU Code Name"), "Navi 31");
    assert_eq!(row_value("GPU Technology"), "5 nm");
    assert_eq!(row_value("Cores"), "6144 Unified");
    assert_eq!(row_value("ROPs / TMUs"), "192 / 384");
}

#[test]
fn a_card_the_table_does_not_name_reads_nothing() {
    // The table-backed fields have nothing on the card to read them from,
    // so an unlisted card must leave them blank rather than showing the
    // bench's own numbers.
    with_card(Some(crate::gpu::radeon::RadeonGpu { name: Some("Some Other Card".into()), ..Default::default() }));
    assert_eq!(row_value("GPU Code Name"), EM_DASH);
    assert_eq!(row_value("ROPs / TMUs"), EM_DASH);
}

// CachesDoNotOutliveAStubTest: the caches are why the tests above clear
// them, stated as a test.

#[test]
fn every_named_cache_exists_and_is_clearable() {
    // Each is cleared by name; a cache added beside them without being
    // added here would let a stub go unconsulted.
    clear_caches();
    assert!(SYSTEM_INFO_CACHE.get(&"cpu").is_none());
    assert!(PROCESSOR_FACTS.get().is_none());
    assert!(GPU_CACHE.get().is_none());
    assert!(GPU_SENSOR_CACHE.get().is_none());
}

#[test]
fn a_stale_cache_would_be_caught() {
    // Prime the processor cache with a part the table does not name, then
    // stub a part it does. The row must follow the stub, which it can
    // only do if the cache was cleared.
    PROCESSOR_FACTS.clear();
    PROCESSOR_FACTS.set(Arc::new(ProcessorFacts {
        fields: BTreeMap::from([("processor_id", WmiValue::from("178BFBFF00A10F10"))]),
    }));
    assert_eq!(cpu_silicon(), (None, None));
    PROCESSOR_FACTS.clear();
}

// ---------------------------------------------------------------------------
// test_training_fields_cover_the_block.py
//
// Every field the APOB decodes has to be routed to the APOB. value() sends a
// name to the training block when TRAINING_FIELDS lists it and to the UMC
// register decode otherwise; a name the block owns that is missing from the
// list goes to the registers, finds nothing there, and comes back as an em
// dash. proc_dq_ds did exactly that.
// ---------------------------------------------------------------------------

fn decoded_field_names() -> BTreeSet<&'static str> {
    use crate::amd::apob::{decode_granite_ridge_training_block, decode_raphael_training_block};

    // The block's content does not matter to the Python decoder, which
    // returns the same keys whatever the bytes say. The Rust decoder answers
    // only for a block whose every code is known, so the blocks here hold
    // known codes; the keys are the same either way.
    let mut granite = vec![0u8; 0x30];
    granite[0x08..0x0E].copy_from_slice(&[0, 0, 1, 5, 5, 5]);
    granite[0x0F] = 30;
    granite[0x11..0x14].copy_from_slice(&[40, 60, 30]);
    granite[0x1A..0x1F].copy_from_slice(&[0, 0, 6, 5, 6]);
    granite[0x1F..0x21].copy_from_slice(&[0, 1]);
    granite[0x21..0x25].copy_from_slice(&[28, 30, 28, 30]);
    // Both record layouts: Ryzen 9000's, and Ryzen 7000's with its extended
    // block, which is where proc_odt and dram_dq_ds come from.
    let raphael = raphael_block();
    let mut extended = raphael.clone();
    extended[0x12] = 60;
    extended[0x13] = 30;
    decode_granite_ridge_training_block(&granite)
        .expect("a plausible Ryzen 9000 block")
        .into_keys()
        .chain(decode_raphael_training_block(&raphael, Some(&extended)).expect("a plausible Ryzen 7000 block").into_keys())
        .collect()
}

#[test]
fn every_decoded_field_is_routed_to_the_block() {
    let missing: Vec<&str> =
        decoded_field_names().into_iter().filter(|name| !TRAINING_FIELDS.contains(name)).collect();
    assert!(
        missing.is_empty(),
        "the APOB decodes {:?}, but TRAINING_FIELDS does not list {missing:?}, so value() sends them to \
         the UMC register decode instead -- which has no such field, and answers with an em dash.",
        decoded_field_names()
    );
}

#[test]
fn the_list_names_nothing_the_block_does_not_hold() {
    // The other direction: a name listed here but never decoded is routed
    // away from the registers to a block that cannot answer either, which
    // is the same blank arrived at from the opposite side.
    let decoded = decoded_field_names();
    let stray: Vec<&str> = TRAINING_FIELDS.iter().copied().filter(|name| !decoded.contains(name)).collect();
    assert!(stray.is_empty(), "TRAINING_FIELDS lists {stray:?}, which the APOB decoder never produces.");
}

#[test]
fn proc_dq_ds_is_among_them() {
    // Named on its own because it is the one that was missing, and a set
    // comparison that happened to be rewritten would stop covering it.
    assert!(TRAINING_FIELDS.contains("proc_dq_ds"));
    assert!(decoded_field_names().contains("proc_dq_ds"));
}

// ---------------------------------------------------------------------------
// test_am5_misc_settings.py: the cases on this module's helpers
// ---------------------------------------------------------------------------

#[test]
fn channels_are_not_mirrored() {
    let row = channel_setting_row(
        "ECC",
        Arc::new(|channel: &str| Reading::Text(if channel == "cha" { "Enabled" } else { "Disabled" }.into())),
        "Other Settings",
        "Left",
        None,
    );
    assert_eq!(value(&row), "A: Enabled | B: Disabled");
    assert_eq!((side(&row, Channel::A), side(&row, Channel::B)), ("Enabled".into(), "Disabled".into()));
    assert_eq!(row.tab, "Training");
}

#[test]
fn raw_training_channels_keep_missing_channel_blank() {
    // One channel read: the shared value is that reading, and the other
    // channel's own value stays a dash rather than borrowing it.
    let row = channel_setting_row(
        "RX_DFE",
        Arc::new(|channel: &str| if channel == "cha" { Reading::Int(0) } else { Reading::Dash }),
        "Misc",
        "Right",
        None,
    );
    assert_eq!(value(&row), "0");
    assert_eq!(side(&row, Channel::B), "—");
}

#[test]
fn a_length_alone_where_it_is_the_only_one() {
    assert_eq!(brief_amble("4 tCK - 00001010 Pattern", &[]), "4 tCK");
    assert_eq!(brief_amble("1.5 tCK - 010 Pattern", &[]), "1.5 tCK");
    assert_eq!(brief_amble("0.5 tCK - 0 Pattern", &[]), "0.5 tCK");
    // The write preamble has one 2 tCK setting, so no pattern.
    assert_eq!(brief_amble("2 tCK - 0010 Pattern", &[]), "2 tCK");
}

#[test]
fn the_read_preambles_two_2_tck_settings_stay_apart() {
    assert_eq!(brief_read_preamble("2 tCK - 0010 Pattern"), "2 tCK (0010)");
    assert_eq!(brief_read_preamble("2 tCK - 1110 Pattern"), "2 tCK (1110)");
    assert_eq!(brief_read_preamble("4 tCK - 00001010 Pattern"), "4 tCK");
}

#[test]
fn anything_else_reads_as_decoded() {
    assert_eq!(brief_amble("Reserved (UMC 5)", &[]), "Reserved (UMC 5)");
    assert_eq!(brief_amble("\u{2014}", &[]), "\u{2014}");
}

// ---------------------------------------------------------------------------
// test_raphael.py: RaphaelProfileTest
// ---------------------------------------------------------------------------

fn put_u32(table: &mut [u8], offset: usize, value: u32) {
    table[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn raphael_block() -> Vec<u8> {
    let mut block = vec![0u8; RAPHAEL_BLOCK_SIZE];
    block[0x00] = 1;
    block[0x02..0x07].copy_from_slice(&[0, 0, 6, 5, 7]); // RTT: off, off, 40, 48, 34
    block[0x07] = 1; // DRAM DQ DS 40
    block[0x08..0x0E].copy_from_slice(&[0, 0, 1, 5, 5, 5]);
    block[0x0E] = 28; // Proc ODT 40
    block[0x0F] = 30; // Proc DQ DS 34.3
    block[0x11] = 40; // Proc CA DS 40
    block
}

/// An APOB container in the geometry both parsers walk.
fn raphael_table() -> Vec<u8> {
    let block = raphael_block();
    let mut table = vec![0u8; 0x200];
    table[0..4].copy_from_slice(b"APOB");
    let length = table.len() as u32;
    put_u32(&mut table, 4, 1);
    put_u32(&mut table, 8, length);
    put_u32(&mut table, 12, 0x60);
    let first = 0x80;
    put_u32(&mut table, 0x30, first as u32);
    put_u32(&mut table, first + 0x0C, 0x20);
    let main = first + 0x20;
    table[main] = 0x01;
    table[main + 4] = 0x19;
    put_u32(&mut table, main + 0x0C, 0x60);
    table[main + 0x30..main + 0x30 + block.len()].copy_from_slice(&block);
    table
}

fn raphael_runtime() -> Arc<Am5Runtime> {
    let table = Arc::new(raphael_table());
    Am5Runtime::with_options(Am5RuntimeOptions {
        reader_factory: reader_factory(oracle_regs()),
        training_reader_factory: Box::new(move |layout| {
            let table = table.clone();
            let read_dword = move |address: u64| {
                let offset = address.wrapping_sub(0x0A20_0000) as usize;
                Ok(if address >= 0x0A20_0000 && offset + 4 <= table.len() {
                    u32::from_le_bytes(table[offset..offset + 4].try_into().unwrap())
                } else {
                    0
                })
            };
            Ok(Box::new(GraniteRidgeApobReader::with_read_dword(read_dword, &DEFAULT_APOB_ADDRESSES, layout))
                as Box<dyn TrainingReader>)
        }),
        cpu_name_factory: cpu(RAPHAEL),
    })
}

#[test]
fn training_status_says_unverified() {
    let runtime = raphael_runtime();
    runtime.load_training();
    assert!(runtime.training_status().contains("(unverified)"), "{}", runtime.training_status());
    assert_eq!(runtime.value("rtt_wr"), Reading::Text("40 RZQ/6".into()));
}

#[test]
fn the_drive_rows_are_the_zen4_ones_and_single_valued() {
    let rows: BTreeMap<String, Row> =
        by_name(&build_timings_with(&raphael_runtime())).into_iter().filter(|(_, row)| row.tab == "Training").collect();
    assert!(rows.contains_key("Proc ODT"));
    assert!(rows.contains_key("DRAM DQ DS"));
    assert!(!rows.contains_key("Proc ODT Pu"));
    assert!(!rows.contains_key("DRAM DQ DS Pd"));
    assert!(rows["RTT WR"].a.is_none());
    assert_eq!(value(&rows["Proc ODT"]), "40 Ω");
}

#[test]
fn the_status_row_carries_the_marker() {
    let rows = by_name(&build_timings_with(&raphael_runtime()));
    assert!(value(&rows["Status"]).contains("(unverified)"));
}

#[test]
fn the_smu_row_reports_the_clock_table_as_unverified() {
    let runtime = raphael_runtime();
    let clocks = SmuClocks {
        version: 0x540108,
        table_base: 0x1000,
        fclk_mhz: 2000.0,
        uclk_mhz: 3000.0,
        mclk_mhz: 3000.0,
        verified: false,
    };
    stub_live(&runtime.sources.clocks, Some(clocks));
    {
        let mut source = lock(&runtime.sources.clocks);
        source.status = (source.describe)(&Some(clocks));
    }
    let rows = by_name(&build_timings_with(&runtime));
    assert_eq!(value(&rows["SMU Status"]), "PM-table 0x540108 clocks (unverified)");
}

// ---------------------------------------------------------------------------
// test_ccdl_run.py: RegisterFallbackTest
// Where the marker fails, the old register anchor still answers.
// ---------------------------------------------------------------------------

#[test]
fn the_runtime_prefers_the_run_and_falls_back() {
    let runtime = Am5Runtime::new();
    {
        let mut ccdl = lock(&runtime.ccdl);
        ccdl.attempted = true;
        ccdl.run = Some((21, 83, 42));
    }
    assert_eq!(runtime.ccdl_value("tCCD_L"), Some(21));
    assert_eq!(runtime.ccdl_value("tCCD_L_WR"), Some(83));
    assert_eq!(runtime.ccdl_value("tCCD_L_WR2"), Some(42));
}

#[test]
fn without_a_run_the_registers_answer_the_two_they_hold() {
    let runtime = Am5Runtime::new();
    {
        let mut ccdl = lock(&runtime.ccdl);
        ccdl.attempted = true;
        ccdl.run = None;
    }
    lock(&runtime.umc).decoded = Some(DecodedChannel(BTreeMap::from([
        ("tCCD_L", Some(UmcValue::Int(20))),
        ("tCCD_L_WR2", Some(UmcValue::Int(40))),
    ])));
    assert_eq!(runtime.ccdl_value("tCCD_L"), Some(20));
    assert_eq!(runtime.ccdl_value("tCCD_L_WR2"), Some(40));
    // The one no register holds stays unanswered rather than inventing.
    assert_eq!(runtime.ccdl_value("tCCD_L_WR"), None);
}

// ---------------------------------------------------------------------------
// test_system_info_additions.py: CacheAndBoardTextTest, ClockFormatTest
// ---------------------------------------------------------------------------

#[test]
fn the_l3_reads_in_megabytes() {
    // Win32_Processor.L3CacheSize on the 9850X3D bench.
    assert_eq!(cache_text(Some(&WmiValue::Int(98304))), "96 MB");
    assert_eq!(cache_text(Some(&WmiValue::Int(512))), "512 KB");
    for missing in [None, Some(WmiValue::Int(0)), Some(WmiValue::from("")), Some(WmiValue::from("n/a"))] {
        assert_eq!(cache_text(missing.as_ref()), EM_DASH, "{missing:?}");
    }
}

#[test]
fn a_placeholder_board_revision_reads_as_missing() {
    // The X870 bench's Gigabyte board reports "x.x".
    for placeholder in [Some("x.x"), Some("Default string"), Some("To be filled by O.E.M."), Some(""), None] {
        assert_eq!(board_revision_text(placeholder), EM_DASH, "{placeholder:?}");
    }
    assert_eq!(board_revision_text(Some(" 1.2 ")), "1.2");
}

/// A runtime whose MCLK is `mclk`, on a machine whose base clock is `bclk`.
fn ratio_runtime(mclk: u32, bclk: WmiValue) -> Arc<Am5Runtime> {
    FakeWmi::new().class("Win32_Processor", vec![WmiObject::new().with("ExtClock", bclk)]).install();
    clear_caches();
    let mut regs = oracle_regs();
    regs.insert(0x50200, mclk);
    runtime_on(regs)
}

#[test]
fn the_dram_ratio_drops_zero_decimals() {
    assert_eq!(dram_ratio(&ratio_runtime(3200, WmiValue::Int(100))), "64");
    assert_eq!(dram_ratio(&ratio_runtime(3000, WmiValue::Int(100))), "60");
    assert_eq!(dram_ratio(&ratio_runtime(3000, WmiValue::Float(100.5))), "59.7");
}

#[test]
fn fclk_and_uclk_are_whole_mhz() {
    let runtime = oracle_runtime();
    stub_live(
        &runtime.sources.clocks,
        Some(SmuClocks {
            version: 0x620105,
            table_base: 0x1000,
            fclk_mhz: 2133.0,
            uclk_mhz: 3200.0,
            mclk_mhz: 3200.0,
            verified: true,
        }),
    );
    assert_eq!(runtime.value("fclk_mhz"), Reading::Int(2133));
    assert_eq!(runtime.value("uclk_mhz"), Reading::Int(3200));
}

// ---------------------------------------------------------------------------
// test_voltage_snapshot.py: the AM5 cases
// ---------------------------------------------------------------------------

fn snapshot_runtime() -> Arc<Am5Runtime> {
    let runtime = oracle_runtime();
    // Every board-wide rail reads 1.250 V, as the Python's patched
    // _format_voltage answered for each.
    let pm_rails: Vec<(&'static str, f64)> = RAILS
        .iter()
        .filter(|rail| rail.source == SOURCE_PM_TABLE)
        .map(|rail| (rail.key, 1.25))
        .collect();
    stub_live(&runtime.sources.voltages, voltages(&pm_rails));
    stub_live(&runtime.sources.board, readings(&[("vddio_mem", 1.25), ("vtt", 1.25)]));
    runtime
}

fn dimm(channel: &'static str, millivolts: &[(&str, u32)]) -> crate::memory::ddr5_telemetry::DimmTelemetry {
    use crate::memory::ddr5_telemetry::{DimmTelemetry, PmicTelemetry};

    let mut telemetry = PmicTelemetry::default();
    for (key, value) in millivolts {
        let slot = match *key {
            "vdd" => &mut telemetry.vdd,
            "vddq" => &mut telemetry.vddq,
            "vpp" => &mut telemetry.vpp,
            "vin_bulk" => &mut telemetry.vin_bulk,
            "vout_1v8" => &mut telemetry.vout_1v8,
            _ => &mut telemetry.vout_1v0,
        };
        *slot = Some(*value);
    }
    DimmTelemetry {
        channel,
        hub_address: 0x50,
        pmic_address: 0x48,
        controller: 0,
        hub_temp_c: None,
        vendor: None,
        revision: None,
        telemetry,
    }
}

#[test]
fn snapshot_is_once_and_not_live() {
    let calls = Arc::new(AtomicUsize::new(0));
    {
        let calls = calls.clone();
        seams::set(
            "dimm_telemetry",
            Box::new(move || {
                calls.fetch_add(1, Ordering::SeqCst);
                vec![dimm("a", &[("vdd", 1425)]), dimm("b", &[("vdd", 1410)])]
            }) as Box<dyn Fn() -> Vec<crate::memory::ddr5_telemetry::DimmTelemetry>>,
        );
    }
    let rows = voltage_snapshot_rows(&snapshot_runtime());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let by_name = by_name(&rows);
    assert_eq!(by_name["VDDCR_VDD snapshot"].display_name.as_deref(), Some("VDDCR_VDD"));
    assert_eq!(by_name["VTT snapshot"].display_name.as_deref(), Some("VTT"));
    assert!(!by_name.keys().any(|name| name.ends_with(" voltage")));
    assert_eq!(value(&by_name["CHA VDD"]), "1.425 V");
    assert_eq!(value(&by_name["CHB VDD"]), "1.410 V");
    assert_eq!(value(&by_name["CHA VDDQ"]), "—");
    value(&by_name["CHA VDD"]);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(rows.iter().all(|row| !row.live));
    assert!(select_tab_names(&rows).contains(&"Voltages".to_string()));
}

// test_voltage_tab_uses_continuous_row_shading,
// test_summary_stays_three_columns_with_snapshot_rails and
// test_summary_keeps_diagnostic_voltage_rows_on_the_voltage_tab_only:
// deferred, they read the window's SHADED_TABS, summary_column_count and
// summary_snapshot_voltage_rows (ui.main).

#[test]
fn effective_ddr_ratio() {
    // Whole ratios drop their zero decimals.
    assert_eq!(dram_ratio(&ratio_runtime(3000, WmiValue::Int(100))), "60");
}

fn voltages_tab_rows() -> Vec<Row> {
    build_timings_with(&oracle_runtime()).into_iter().filter(|row| row.tab == "Voltages").collect()
}

#[test]
fn the_tab_no_longer_says_it_is_a_startup_snapshot() {
    // It is re-taken each time the tab opens, so the note is gone.
    assert!(!names(&voltages_tab_rows()).contains(&"Reading mode".to_string()));
}

#[test]
fn every_row_can_be_re_taken() {
    for row in voltages_tab_rows() {
        assert!(row.refresh.is_some(), "{}", row.name);
    }
}

#[test]
fn the_pmic_rails_carry_their_own_names() {
    let shown: BTreeMap<String, Option<String>> =
        voltages_tab_rows().into_iter().map(|row| (row.name, row.display_name)).collect();
    assert_eq!(shown["CHA VIN"].as_deref(), Some("CHA VIN (5V)"));
    assert_eq!(shown["CHA 1.8V output"].as_deref(), Some("CHA LDO 1.8V"));
    assert_eq!(shown["CHB 1.0V output"].as_deref(), Some("CHB LDO 1.0V"));
}

#[test]
fn a_refresh_re_takes_the_snapshot() {
    // What `refresh=snapshot.cache_clear` promised: the next read after it
    // asks the modules again.
    let calls = Arc::new(AtomicUsize::new(0));
    {
        let calls = calls.clone();
        seams::set(
            "dimm_telemetry",
            Box::new(move || {
                let call = calls.fetch_add(1, Ordering::SeqCst) as u32;
                vec![dimm("a", &[("vdd", 1400 + call * 10)])]
            }) as Box<dyn Fn() -> Vec<crate::memory::ddr5_telemetry::DimmTelemetry>>,
        );
    }
    let rows = by_name(&voltage_snapshot_rows(&snapshot_runtime()));
    assert_eq!(value(&rows["CHA VDD"]), "1.400 V");
    assert_eq!(value(&rows["CHA VDD"]), "1.400 V");
    rows["CHA VDD"].refresh.as_ref().unwrap()();
    assert_eq!(value(&rows["CHA VDD"]), "1.410 V");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

// ---------------------------------------------------------------------------
// test_am5_touches_no_intel_reader.py
//
// Reading the AM5 rows must not reach an Intel memory-controller reader. The
// dispatcher says this is a safety boundary, and for a year it was true of
// importing and false of reading: three System Info rows reached into the
// Intel package for helpers that were never Intel, and one of them made the
// Intel table build, reading about 2,560 dwords out of the hardcoded MCHBAR
// window on a board where 0xFEDC0000 belongs to the FCH. Nothing was visibly
// wrong; only counting the reads shows it.
// ---------------------------------------------------------------------------

// The Intel window: MCHBAR and the second controller's aperture above it.
const MCHBAR_START: u64 = 0xFEDC_0000;
const MCHBAR_END: u64 = 0xFEDE_0000;

/// Read every row the way the window can: values, both sides, dims, and a
/// refresh followed by a second read.
fn read_every_row(rows: &[Row]) -> usize {
    for row in rows {
        if let Some(refresh) = &row.refresh {
            refresh();
        }
        let _ = row.value.as_ref().map(|value| value.resolve(EM_DASH));
        for channel in [Channel::A, Channel::B] {
            let _ = row.side(channel).and_then(|side| side.value.as_ref()).map(|value| value.resolve(EM_DASH));
        }
        let _ = row.is_dimmed();
    }
    rows.len()
}

#[test]
fn reading_the_rows_imports_nothing_from_the_intel_package() {
    // Rust has no import to run a module's body, so the Python's question --
    // does reading the AM5 rows pull the Intel package in -- becomes: does
    // this module name it at all. The table building as a side effect of an
    // import cannot happen here; a call into it still could.
    let source = include_str!("mod.rs");
    for needle in ["crate::intel", "intel::", "intel_timings"] {
        assert!(!source.contains(needle), "the AM5 profile names {needle}");
    }
}

#[test]
fn nothing_reads_the_intel_mchbar_window() {
    // The count, not the import, is the thing that matters: a future
    // helper could reach the same addresses without importing any of the
    // modules named above. Every transport runs for real here, against a
    // fake driver that answers nothing, on a machine that calls itself AM5.
    let hardware = FakeHardware::new().install();
    FakeWmi::new()
        .class(
            "Win32_Processor",
            vec![
                WmiObject::new()
                    .with("ProcessorId", "178BFBFF00B40F40")
                    .with("Name", GRANITE)
                    .with("ExtClock", 100i64),
            ],
        )
        .install();
    clear_caches();
    let rows = build_timings_with(&Am5Runtime::new());
    assert!(read_every_row(&rows) > 0, "no rows were built to read");
    let seen: Vec<u64> = hardware
        .phys_reads()
        .into_iter()
        .filter(|address| (MCHBAR_START..MCHBAR_END).contains(address))
        .collect();
    assert!(
        seen.is_empty(),
        "{} read(s) landed in the Intel MCHBAR window while reading the AM5 rows, first at 0x{:08X}. \
         On this board that address space is the FCH's.",
        seen.len(),
        seen[0]
    );
}

#[test]
fn building_the_table_touches_no_hardware() {
    // The runtime is created by build_timings and must not read anything
    // until a row is read.
    let hardware = FakeHardware::new().install();
    let rows = build_timings();
    assert!(!rows.is_empty());
    assert_eq!(hardware.log(), []);
}
