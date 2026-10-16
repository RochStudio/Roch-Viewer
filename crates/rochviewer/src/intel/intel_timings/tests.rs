//! Tests ported from the Python suite's Intel modules, plus the structural
//! parity check against the Python table.
//!
//! The Python tests built the table against `tests/intel_stub`: a fake `wmi`
//! describing a populated dual-channel Z790 board and a `read` module that
//! touched no hardware. [`stub_wmi`] is that fixture's WMI half; the hardware
//! half is the test build's default, where every access fails. A table is
//! built on a fresh thread ([`on_fresh_thread`]) because the caches it fills
//! -- the generation, the CPU name, the module inventory -- are per thread
//! under test, which is what the Python fixture's re-import achieved.

use std::sync::{Arc, Mutex};

use crate::hardware::read::{
    DynamicParams, FieldRead, ReadKind, ReadSpec, WIDE_READ_BYTES, extract_value_from_bytes,
    extract_value_from_wide_bytes, read_field,
};
use crate::hardware::{Hardware, HwError, HwResult, set_thread_hardware};
use crate::platform_profiles::Platform;
use crate::row::{Channel, Row, Value};
use crate::testing::FakeHardware;
use crate::wmi_util::{FakeWmi, WmiObject};

use super::build::Build;
use super::dict::{T, V, bits};
use super::formulas::{self, lookup};
use super::layout::{
    DDR5_SYSTEM_INFO_REMOVED, DDR5_TRAINING_REMOVED, MODE_REGISTER_TIMING_SUFFIX, brief_ddr5_value, imc_switch_text,
    resistance_first_rzq_text, system_info_order,
};
use super::passes::{
    ARROW_LAKE_POWER_DOWN, ARROW_LAKE_POWER_DOWN_UNKNOWN, ARROW_LAKE_POWER_DOWN_WIDE, CCD_CATEGORY,
    DDR4_ABSENT_VREF_ROWS,
};
use super::registers::{
    self, CCD_CANDIDATE_FIELDS, CCD_CONFIRMED_FIELDS, DDR4_MODE_REGISTER_BASE, DDR4_TCCD_S,
    MISC_CKE_CONFIG_OFFSET, MISC_MODE_REGISTER_STATE, SCHEDULER_GEAR2_BIT, SCHEDULER_GEAR4_BIT, SLEW_RATE_GROUPS,
};
use super::*;

const DDR4: Platform = Platform::Lga1700Ddr4;
const DDR5: Platform = Platform::Lga1700Ddr5;

// --- The fixture.

/// What each platform's fixture says it is. Kept together because a stub that
/// claims one generation in the platform and another in SMBIOSMemoryType is
/// the failure the Python fixture's own comment warns about.
struct MemoryFacts {
    smbios_type: i64,
    part_number: &'static str,
    speed: i64,
    board: &'static str,
}

const DDR4_MEMORY: MemoryFacts = MemoryFacts {
    smbios_type: 26,
    part_number: "F4-3600C14-16GVKA",
    speed: 3600,
    board: "PRO Z790-P WIFI DDR4 (MS-7E06)",
};
const DDR5_MEMORY: MemoryFacts = MemoryFacts {
    smbios_type: 34,
    part_number: "F5-8000J3848H16G",
    speed: 8000,
    board: "PRO Z790-P WIFI (MS-7E06)",
};

fn memory_facts(platform: Platform) -> &'static MemoryFacts {
    if DDR5_TIMING_PLATFORMS.contains(&platform) { &DDR5_MEMORY } else { &DDR4_MEMORY }
}

fn fake_module(tag: &str, device_locator: &str, facts: &MemoryFacts) -> WmiObject {
    // The board's own socket name, which is what slot labels come from.
    // The reference target reports its two modules as records 1 and 3 while
    // naming them DIMMA2 and DIMMB2, so the stub says the same.
    WmiObject::new()
        .with("Tag", tag)
        .with("DeviceLocator", device_locator)
        .with("BankLabel", "BANK 0")
        .with("PartNumber", facts.part_number)
        .with("Manufacturer", "G Skill Intl")
        .with("Capacity", (16u64 * 1024 * 1024 * 1024).to_string())
        .with("Attributes", 2i64) // dual rank
        .with("Speed", facts.speed)
        .with("ConfiguredClockSpeed", facts.speed)
        .with("SMBIOSMemoryType", facts.smbios_type)
}

/// A populated dual-channel Z790 board, matching the beta's target: the
/// Python `tests/intel_stub.FakeWMI`.
fn stub_wmi_for(platform: Platform) -> FakeWmi {
    let facts = memory_facts(platform);
    FakeWmi::new()
        .class("Win32_PhysicalMemoryArray", vec![WmiObject::new().with("MemoryDevices", 4i64)])
        .class(
            "Win32_PhysicalMemory",
            vec![
                fake_module("Physical Memory 1", "Controller0-DIMMA2", facts),
                fake_module("Physical Memory 3", "Controller1-DIMMB2", facts),
            ],
        )
        .class(
            "Win32_Processor",
            vec![WmiObject::new()
                .with("Name", "Intel(R) Core(TM) i9-14900KF")
                .with("Manufacturer", "GenuineIntel")
                .with("NumberOfCores", 8i64)
                .with("NumberOfLogicalProcessors", 32i64)],
        )
        .class(
            "Win32_BaseBoard",
            vec![WmiObject::new()
                .with("Product", facts.board)
                .with("Manufacturer", "Micro-Star International Co., Ltd.")
                .with("Version", "1.0")],
        )
        .class(
            "Win32_BIOS",
            vec![WmiObject::new().with("SMBIOSBIOSVersion", "1.H0").with("Manufacturer", "American Megatrends")],
        )
        .class(
            "Win32_ComputerSystem",
            vec![WmiObject::new().with("TotalPhysicalMemory", (32u64 * 1024 * 1024 * 1024).to_string())],
        )
        .class(
            "Win32_OperatingSystem",
            vec![WmiObject::new()
                .with("Caption", "Microsoft Windows 11 Pro")
                .with("BuildNumber", "22631")
                .with("OSArchitecture", "64-bit")],
        )
        .class(
            "Win32_VideoController",
            vec![
                WmiObject::new().with("Name", "NVIDIA GeForce RTX 4070 Ti"),
                // Must be filtered out of the GPU row.
                WmiObject::new().with("Name", "Microsoft Basic Display Adapter"),
            ],
        )
}

fn stub_wmi(platform: Platform) {
    stub_wmi_for(platform).install();
}

/// Run `body` on a thread of its own, so every per-thread cache it fills
/// starts empty and ends with it.
fn on_fresh_thread<R: Send + 'static>(body: impl FnOnce() -> R + Send + 'static) -> R {
    std::thread::spawn(body).join().expect("the test body panicked")
}

/// The table `intel_stub.install(platform)` gave the Python tests, built on
/// the calling thread with the stub's WMI installed.
fn stub_table(platform: Platform) -> Vec<Row> {
    stub_wmi(platform);
    build_timings(platform)
}

/// Every row named `name`, in order.
fn named<'a>(rows: &'a [Row], name: &str) -> Vec<&'a Row> {
    rows.iter().filter(|row| row.name == name).collect()
}

fn row<'a>(rows: &'a [Row], name: &str) -> &'a Row {
    rows.iter().find(|row| row.name == name).unwrap_or_else(|| panic!("no row named {name:?}"))
}

fn names_on(rows: &[Row], tab: &str) -> Vec<String> {
    rows.iter().filter(|row| row.tab == tab).map(|row| row.name.clone()).collect()
}

fn side(row: &Row, channel: Channel) -> &crate::row::Side {
    row.side(channel).unwrap_or_else(|| panic!("{} has no {channel:?} side", row.name))
}

fn field_of(spec: Option<ReadSpec>) -> FieldRead {
    match spec {
        Some(ReadSpec::Field(field)) => field,
        other => panic!("expected a field read, got {other:?}"),
    }
}

fn dynamic_of(spec: Option<ReadSpec>) -> DynamicParams {
    match spec {
        Some(ReadSpec::Dynamic(params)) => params,
        other => panic!("expected a dynamic read, got {other:?}"),
    }
}

/// Resolve a lazy value the way the window does.
fn resolve(value: &Option<Value>) -> String {
    value.as_ref().map(|v| v.resolve("\u{2014}")).unwrap_or_else(|| "None".into())
}

/// A driver whose every read returns the same dword (or fails, for `None`),
/// logging the addresses asked for: the Python tests' patched
/// `read_physical_memory_int = lambda address, size: value`.
struct ConstHardware {
    value: Option<u32>,
    log: Mutex<Vec<u64>>,
}

impl ConstHardware {
    fn install(value: Option<u32>) -> Arc<ConstHardware> {
        let hardware = Arc::new(ConstHardware { value, log: Mutex::new(Vec::new()) });
        set_thread_hardware(Some(hardware.clone()));
        hardware
    }

    fn asked(&self) -> Vec<u64> {
        self.log.lock().unwrap().clone()
    }

    fn clear(&self) {
        self.log.lock().unwrap().clear();
    }
}

impl Hardware for ConstHardware {
    fn read_phys(&self, address: u64, size: usize) -> HwResult<Vec<u8>> {
        self.log.lock().unwrap().push(address);
        let value = self.value.ok_or_else(|| HwError::new("read failed"))?;
        Ok(value.to_le_bytes().iter().copied().cycle().take(size).collect())
    }
    fn get_phys_long(&self, address: u64) -> HwResult<u32> {
        self.log.lock().unwrap().push(address);
        self.value.ok_or_else(|| HwError::new("read failed"))
    }
    fn set_phys_long(&self, _address: u64, _value: u32) -> HwResult<()> {
        Err(HwError::new("read-only"))
    }
    fn inb(&self, _port: u16) -> HwResult<u8> {
        Err(HwError::new("no ports"))
    }
    fn outb(&self, _port: u16, _value: u8) -> HwResult<()> {
        Err(HwError::new("no ports"))
    }
    fn inl(&self, _port: u16) -> HwResult<u32> {
        Err(HwError::new("no ports"))
    }
    fn outl(&self, _port: u16, _value: u32) -> HwResult<()> {
        Err(HwError::new("no ports"))
    }
    fn is_driver_open(&self) -> bool {
        self.value.is_some()
    }
}

/// Pin the DDR generation for the calling thread, as the Python tests
/// patched `detect_ddr_generation`.
fn pin_generation(smbios_type: i64) {
    FakeWmi::new()
        .class("Win32_PhysicalMemory", vec![WmiObject::new().with("SMBIOSMemoryType", smbios_type)])
        .install();
    crate::memory::ddr_generation::clear_cache();
}

// --- Structural parity with the Python table.
//
// The expected files are the Python 1.1.0 table built against
// tests/intel_stub for each platform (py_struct.py in the porting notes):
// tab, category, column, name, label, dual and live for every row, in order.
// The per-processor clock rows are left out of both sides: their count is the
// machine's logical processor count, which the stub does not fake.

fn structure(rows: &[Row]) -> Vec<serde_json::Value> {
    rows.iter()
        .filter(|row| !row.extra.contains_key(super::sensors::PARENT_ROW_KEY))
        .map(|row| {
            serde_json::json!([row.tab, row.category, row.column, row.name, row.label(), row.is_dual(), row.live])
        })
        .collect()
}

fn assert_structure(platform: Platform, expected: &str) {
    let expected: serde_json::Value = serde_json::from_str(expected).expect("expected structure parses");
    assert_eq!(expected["platform"], platform.id());
    let expected = expected["rows"].as_array().expect("rows").clone();
    let built = on_fresh_thread(move || structure(&stub_table(platform)));
    for (index, (want, got)) in expected.iter().zip(built.iter()).enumerate() {
        assert_eq!(want, got, "row #{index} differs from the Python table on {}", platform.id());
    }
    assert_eq!(expected.len(), built.len(), "row count differs from the Python table on {}", platform.id());
}

#[test]
fn structure_matches_python_lga1700_ddr4() {
    assert_structure(DDR4, include_str!("expected/lga1700-ddr4.json"));
}

#[test]
fn structure_matches_python_lga1700_ddr5() {
    assert_structure(DDR5, include_str!("expected/lga1700-ddr5.json"));
}

#[test]
fn structure_matches_python_lga1851() {
    assert_structure(Platform::Lga1851, include_str!("expected/lga1851.json"));
}

// --- Parity over a register map.
//
// The stub above answers no register, so it checks the table's shape and not
// its decodes, and its 14900KF never takes the Core Ultra 200S paths. These
// build both tables over the same register contents instead: the Python with
// its real read module pointed at a byte map (py_struct_mem.py in the porting
// notes), the Rust with the same map in FakeHardware. The map answers some
// mode-register entries and not others, has one unmapped and one absent
// drive-strength register, and carries a Core Ultra ratio/gear word, so the
// LGA1851 table -- built for a Core Ultra 7 270K Plus -- runs every Arrow
// Lake pass, the blank-row removal included. Values are compared on the
// tabs whose readings come from that map alone.

fn memory_hardware(memory: &str) -> Arc<FakeHardware> {
    let dwords: Vec<(u64, u32)> = serde_json::from_str(memory).expect("memory map parses");
    let mut fake = FakeHardware::new();
    for (address, value) in dwords {
        fake = fake.with_dword(address, value);
    }
    fake.install()
}

fn assert_register_map_parity(platform: Platform, expected: &'static str, memory: &'static str) {
    on_fresh_thread(move || {
        let expected: serde_json::Value = serde_json::from_str(expected).expect("expected rows parse");
        assert_eq!(expected["platform"], platform.id());
        let cpu = expected["cpu"].as_str().expect("cpu").to_string();
        stub_wmi_for(platform)
            .class(
                "Win32_Processor",
                vec![WmiObject::new()
                    .with("Name", cpu.as_str())
                    .with("Manufacturer", "GenuineIntel")
                    .with("NumberOfCores", 8i64)
                    .with("NumberOfLogicalProcessors", 32i64)],
            )
            .install();
        memory_hardware(memory);
        let rows = build_timings(platform);
        let rows: Vec<&Row> = rows.iter().filter(|row| !row.extra.contains_key(super::sensors::PARENT_ROW_KEY)).collect();
        let expected = expected["rows"].as_array().expect("rows");
        for (index, (want, got)) in expected.iter().zip(rows.iter()).enumerate() {
            let shape = serde_json::json!([got.tab, got.category, got.column, got.name, got.label(), got.is_dual(), got.live]);
            assert_eq!(&want.as_array().unwrap()[..7], shape.as_array().unwrap().as_slice(), "row #{index} on {}", platform.id());
            if let Some(value) = want[7].as_str() {
                let read = crate::display_values::read_value(got, None, "N/A");
                assert_eq!(read, value, "row #{index} {:?} value on {}", got.name, platform.id());
            }
        }
        assert_eq!(expected.len(), rows.len(), "row count on {}", platform.id());
    });
}

#[test]
fn register_map_parity_lga1700_ddr4() {
    assert_register_map_parity(DDR4, include_str!("expected/mem-lga1700-ddr4.json"), include_str!("expected/memory-lga1700.json"));
}

#[test]
fn register_map_parity_lga1700_ddr5() {
    assert_register_map_parity(DDR5, include_str!("expected/mem-lga1700-ddr5.json"), include_str!("expected/memory-lga1700.json"));
}

#[test]
fn register_map_parity_lga1851_core_ultra() {
    assert_register_map_parity(
        Platform::Lga1851,
        include_str!("expected/mem-lga1851.json"),
        include_str!("expected/memory-lga1851.json"),
    );
}

#[test]
fn the_contract_functions_keep_their_shape() {
    // The window and board_sensors call these by name.
    FakeWmi::new()
        .class(
            "Win32_BaseBoard",
            vec![WmiObject::new()
                .with("Manufacturer", "ASUSTeK COMPUTER INC. ")
                .with("Product", "PRIME Z790-A WIFI")
                .with("Version", "Rev 1.xx")],
        )
        .install();
    assert_eq!(get_motherboard_display(), "ASUSTeK COMPUTER INC. PRIME Z790-A WIFI");
    FakeWmi::new().install();
    assert_eq!(get_motherboard_display(), "Unknown");
}

#[test]
fn no_driver_builds_a_full_table_without_panicking() {
    // Every reading fails; every row is still built and resolves to text.
    for platform in [DDR4, DDR5, Platform::Lga1851] {
        on_fresh_thread(move || {
            let rows = stub_table(platform);
            assert!(rows.len() > 300);
            for row in &rows {
                let _ = crate::display_values::read_value(row, None, "N/A");
            }
        });
    }
}

// --- tests/test_ddr4_ron.py

fn ron_words(words: &[(u64, Option<u32>)]) -> Arc<FakeHardware> {
    let mut fake = FakeHardware::new();
    for (address, value) in words {
        if let Some(value) = value {
            fake = fake.with_dword(*address, *value);
        }
    }
    fake.install()
}

#[test]
fn ddr4_ron_both_standard_codes_and_reserved_codes() {
    // Low halfword is MR0: toggling its bits 2:1 must not select RON.
    for (code, expected) in [(0u32, "34 RZQ/7"), (1, "48 RZQ/5"), (2, "Reserved (10b)"), (3, "Reserved (11b)")] {
        for mr0 in [0x0000u32, 0x0006, 0xFFFF] {
            let mr1 = 0x0701 | (code << 1); // RTT bits and DLL enable
            let raw = (mr1 << 16) | mr0;
            ron_words(&[(MCHBAR + 0xE5A0, Some(raw)), (MCHBAR + 0xF5A0, Some(raw))]);
            assert_eq!(get_ddr4_ron(MCHBAR), expected, "code {code} mr0 {mr0:#x}");
        }
    }
}

#[test]
fn ddr4_ron_channel_sources_and_refresh_are_independent() {
    on_fresh_thread(|| {
        let rows = stub_table(DDR4);
        let ron = row(&rows, "RON");
        let channel_b = super::channel_b(DDR4);
        let fake = ron_words(&[
            (MCHBAR + 0xE5A0, Some(0x00011234)),
            (MCHBAR + 0xF5A0, Some(0x00011234)),
            (channel_b + 0xE5A0, Some(0x00031234)),
            (channel_b + 0xF5A0, Some(0x00031234)),
        ]);
        assert_eq!(resolve(&side(ron, Channel::A).value), "34 RZQ/7");
        assert_eq!(resolve(&side(ron, Channel::B).value), "48 RZQ/5");
        fake.set_dword(MCHBAR + 0xE5A0, 0x00031234);
        assert_eq!(resolve(&side(ron, Channel::A).value), "W0 48 RZQ/5 / W1 34 RZQ/7");
        assert_eq!(resolve(&side(ron, Channel::B).value), "48 RZQ/5");
    });
}

#[test]
fn ddr4_ron_unreadable_or_empty_windows_do_not_fabricate_34_or_hide_missing_peer() {
    for raw in [None, Some(0u32), Some(0xFFFF_FFFF)] {
        ron_words(&[(MCHBAR + 0xE5A0, raw), (MCHBAR + 0xF5A0, Some(0x00031234))]);
        assert_eq!(get_ddr4_ron(MCHBAR), "W0 N/A / W1 48 RZQ/5", "raw {raw:?}");
        ron_words(&[(MCHBAR + 0xE5A0, raw), (MCHBAR + 0xF5A0, raw)]);
        assert_eq!(get_ddr4_ron(MCHBAR), "N/A", "raw {raw:?}");
    }
    // A read that fails outright.
    FakeHardware::new().install();
    assert_eq!(get_ddr4_ron(MCHBAR), "N/A");
}

#[test]
fn ddr4_ron_advanced_has_no_ron_shadow_diagnostics() {
    on_fresh_thread(|| {
        let rows = stub_table(DDR4);
        assert!(!rows.iter().any(|row| row.category == "RON shadow diagnostics"));
    });
}

#[test]
fn ddr4_has_one_ron_and_keeps_rtt_without_duplicate_odt() {
    on_fresh_thread(|| {
        let rows = stub_table(DDR4);
        let training: Vec<&Row> = rows.iter().filter(|row| row.tab == "Training").collect();
        let ron: Vec<&str> = training.iter().filter(|row| row.category == "RON").map(|row| row.name.as_str()).collect();
        assert_eq!(ron, ["RON"]);
        assert!(!training.iter().any(|row| row.category == "ODT"));
        let mut rtt: Vec<&str> =
            training.iter().filter(|row| row.category == "RTT").map(|row| row.name.as_str()).collect();
        rtt.sort();
        assert_eq!(rtt, ["RTT Nom", "RTT Park", "RTT Wr"]);
    });
}

#[test]
fn ddr4_rtt_and_ron_use_reference_rzq_notation() {
    assert_eq!(lookup(formulas::DDR4_RTT_NOM_PARK_FORMULA, 0b110), Some("80 RZQ/3"));
    assert_eq!(lookup(formulas::DDR4_RTT_NOM_PARK_FORMULA, 0b000), Some("0 RZQ OFF"));
    assert_eq!(lookup(formulas::DDR4_RON_LIVE_FORMULA, 0b01), Some("48 RZQ/5"));
}

#[test]
fn ddr5_retains_two_drivers_and_six_odt_groups() {
    for platform in [DDR5, Platform::Lga1851] {
        on_fresh_thread(move || {
            let rows = stub_table(platform);
            let training: Vec<&Row> = rows.iter().filter(|row| row.tab == "Training").collect();
            let mut ron: Vec<&str> =
                training.iter().filter(|row| row.category == "RON").map(|row| row.name.as_str()).collect();
            ron.sort();
            assert_eq!(ron, ["Pull Down Drv", "Pull Up Drv"]);
            assert_eq!(training.iter().filter(|row| row.category == "ODT").count(), 6);
            assert!(!rows.iter().any(|row| row.extra_bool("advanced_only")));
        });
    }
}

// --- tests/test_ddr5_channel_b.py

fn rows_with_both_dynamic_sides(rows: &[Row]) -> Vec<&Row> {
    rows.iter()
        .filter(|row| {
            matches!(row.a.as_ref().and_then(|s| s.read), Some(ReadSpec::Dynamic(_)))
                && matches!(row.b.as_ref().and_then(|s| s.read), Some(ReadSpec::Dynamic(_)))
        })
        .collect()
}

#[test]
fn ddr5_mode_register_rows_read_b1s_controller() {
    // B1 is on MC1 on Raptor Lake DDR5: SMBIOS names Controller1-DIMMB1,
    // and the MC1 DFE taps and DQ VREF are the ones the reference tool
    // shows for DIMM1. MCHBAR + 0x800 is A1's other sub-channel.
    on_fresh_thread(|| {
        let rows = stub_table(DDR5);
        let both = rows_with_both_dynamic_sides(&rows);
        assert!(!both.is_empty());
        for row in both {
            assert_eq!(dynamic_of(side(row, Channel::A).read).mchbar, MCHBAR, "{}", row.name);
            assert_eq!(dynamic_of(side(row, Channel::B).read).mchbar, MCHBAR2, "{}", row.name);
        }
    });
}

#[test]
fn ddr4_keeps_the_second_controller() {
    on_fresh_thread(|| {
        let rows = stub_table(DDR4);
        for row in rows_with_both_dynamic_sides(&rows) {
            assert_eq!(dynamic_of(side(row, Channel::B).read).mchbar, MCHBAR2, "{}", row.name);
        }
    });
}

#[test]
fn ddr5_ccd_s_is_half_the_burst_length() {
    // 0x20 is the bench's MR0: BL16 with CL 38 in the upper bits.
    assert_eq!(registers::ddr5_ccd_s_from(Some(0x20)), Some(8));
    assert_eq!(registers::ddr5_ccd_s_from(Some(0x21)), Some(8));
    assert_eq!(registers::ddr5_ccd_s_from(Some(0x22)), Some(16));
}

#[test]
fn ddr5_ccd_s_on_the_fly_bl32_and_an_unread_mr0_stay_empty() {
    assert_eq!(registers::ddr5_ccd_s_from(Some(0x23)), None);
    assert_eq!(registers::ddr5_ccd_s_from(None), None);
}

// --- tests/test_ddr5_system_info.py

#[test]
fn core_types_a_hybrid_part_counts_each_class() {
    // A 14600KF as sold: six P-cores with two threads, eight E-cores.
    let mut cores = vec![(1u8, true); 6];
    cores.extend(vec![(0u8, false); 8]);
    assert_eq!(core_types_text(&cores).as_deref(), Some("6P + 8E"));
}

#[test]
fn core_types_e_cores_off_leaves_two_thread_cores_that_are_all_p_cores() {
    // This bench: E-cores disabled in the BIOS, so every core shares one
    // class, and each runs two threads, which no E-core does.
    assert_eq!(core_types_text(&[(0, true); 6]).as_deref(), Some("6P + 0E"));
}

#[test]
fn core_types_one_class_without_smt_is_not_guessed() {
    assert_eq!(core_types_text(&[(0, false); 6]), None);
    assert_eq!(core_types_text(&[]), None);
}

#[test]
fn pcie_link_written_as_gpu_z_writes_it() {
    use crate::gpu::nvidia_gpu::pcie_link_text;
    assert_eq!(pcie_link_text(5, 16), "PCIe 5.0 x16");
    assert_eq!(pcie_link_text(4, 8), "PCIe 4.0 x8");
}

#[test]
fn uncore_ratio_ddr5_writes_the_whole_number() {
    assert_eq!(registers::uncore_ratio_text(Some(50), DDR5).as_deref(), Some("50 x"));
}

#[test]
fn uncore_ratio_ddr4_is_unchanged() {
    assert_eq!(registers::uncore_ratio_text(Some(50), DDR4).as_deref(), Some("50.0 x"));
}

#[test]
fn ddr5_drops_the_moving_ratio_and_the_socket_label() {
    assert_eq!(DDR5_SYSTEM_INFO_REMOVED, ["Core Ratio", "CPU Package"]);
}

#[test]
fn the_added_system_info_rows_have_their_places() {
    let order = system_info_order();
    let at = |name: &str| order.iter().position(|n| *n == name).unwrap();
    assert_eq!(order[at("Cores / Threads") + 1], "Core Types");
    let names: Vec<&str> = DDR5_GPU_ROWS.iter().map(|(label, _)| *label).collect();
    assert_eq!(names, ["PCIe Link", "VBIOS Version"]);
    assert_eq!(order[at("Resizable BAR") + 1..at("Driver Version")].to_vec(), names);
}

// --- tests/test_dram_frequency.py

/// get_speed on the pre-Arrow-Lake path with 0x5E04 and BCLK given.
///
/// Ratio 40 at the 100 MHz QCLK multiplier in Gear 2 is DDR5-8000.
fn speed_at(bclk: f64) -> String {
    registers::speed_from(bclk, 40, 1, 1)
}

#[test]
fn dram_frequency_the_bclk_wander_does_not_move_the_frequency() {
    for bclk in [99.98, 99.99, 100.0, 100.02] {
        assert_eq!(speed_at(bclk), "8000.0 MHz", "bclk {bclk}");
    }
}

#[test]
fn dram_frequency_a_real_bclk_change_still_shows() {
    assert_eq!(speed_at(102.5), "8200.0 MHz");
}

#[test]
fn dram_frequency_reads_the_registers_it_names() {
    // The same arithmetic through the registers: BCLK in kHz at 0x5F60,
    // ratio 40 / multiplier 1 / gear 1 in 0x5E04, and a CPU that is not
    // Core Ultra so the Raptor Lake path is taken.
    FakeWmi::new()
        .class("Win32_Processor", vec![WmiObject::new().with("Name", "Intel(R) Core(TM) i5-14600KF")])
        .install();
    FakeHardware::new()
        .with_dword(MCHBAR + 0x5F60, 99_980)
        .with_dword(MCHBAR + 0x5E04, 40 | (1 << 8) | (1 << 12))
        .with_dword(MCHBAR + 0x13D10, 0)
        .install();
    assert_eq!(get_speed(), "8000.0 MHz");
    assert_eq!(get_dram_frequency(DDR5), "8000 MT/s");
    assert_eq!(get_dram_frequency(DDR4), "8000 MHz");
    assert_eq!(get_mclk(), "4000 MHz");
}

// --- tests/test_timing_labels.py

#[test]
fn ddr5_names_the_all_bank_interval_trfc2() {
    for platform in [DDR5, Platform::Lga1851] {
        assert_eq!(ddr5_timing_label(platform, "tRFC"), "tRFC2");
    }
}

#[test]
fn ddr4_keeps_trfc() {
    // DDR4 has one refresh interval and no per-bank refresh at all, so
    // tRFC2 would name something the modules do not have.
    assert_eq!(ddr5_timing_label(DDR4, "tRFC"), "tRFC");
}

#[test]
fn the_derived_nanosecond_rows_are_named_alike_on_both() {
    // A derived row restating a raw one in nanoseconds is the same row on
    // either generation, so it carries one name on both.
    for platform in [DDR4, DDR5, Platform::Lga1851] {
        for name in ["tRFCns", "tREFIns"] {
            assert_eq!(ddr5_timing_label(platform, name), name);
        }
    }
}

#[test]
fn only_trfc_is_still_renamed_per_platform() {
    // tRFC really is a different register on DDR5. Nothing else is.
    let declared: Vec<&str> = DDR5_TIMING_LABELS.iter().map(|(name, _)| *name).collect();
    assert_eq!(declared, ["tRFC"]);
}

#[test]
fn an_unlisted_row_is_never_renamed() {
    for name in ["tCL", "tRFCpb", "tREFI", "tRP", "tRFCns", "tREFIns"] {
        assert_eq!(ddr5_timing_label(DDR5, name), name);
    }
}

#[test]
fn am5_is_not_an_intel_ddr5_platform() {
    // It has its own profile and its own tRFC2; this map must not reach it.
    assert!(!DDR5_TIMING_PLATFORMS.contains(&Platform::Am5));
}

#[test]
fn a_ddr5_table_carries_the_renamed_rows() {
    on_fresh_thread(|| {
        let rows = stub_table(DDR5);
        assert!(!named(&rows, "tRFC2").is_empty());
        assert!(!named(&rows, "tRFCns").is_empty());
        assert!(named(&rows, "tRFC").is_empty());
    });
}

#[test]
fn a_ddr5_table_places_tcl_mr_directly_under_tcl() {
    on_fresh_thread(|| {
        let rows = stub_table(DDR5);
        let names = names_on(&rows, "Timings");
        let at = names.iter().position(|n| n == "tCL").unwrap();
        assert_eq!(names[at + 1], "tCL_MR");
        assert!(row(&rows, "tCL_MR").is_dual());
    });
}

#[test]
fn a_ddr4_table_does_not_use_the_ddr5_tcl_encoding() {
    on_fresh_thread(|| assert!(named(&stub_table(DDR4), "tCL_MR").is_empty()));
}

#[test]
fn a_ddr4_table_keeps_the_original_rows() {
    on_fresh_thread(|| {
        let rows = stub_table(DDR4);
        assert!(!named(&rows, "tRFC").is_empty());
        assert!(named(&rows, "tRFC2").is_empty());
        assert!(!named(&rows, "tRFCpb").is_empty());
        for gone in ["tDLLK", "tCCD_L_WR", "tCCD_L_WR2", "Allow 2cyc B2B LPDDR"] {
            assert!(named(&rows, gone).is_empty(), "{gone}");
        }
    });
}

#[test]
fn ddr4_trfcpb_exposes_the_reference_register_field() {
    on_fresh_thread(|| {
        let rows = stub_table(DDR4);
        let trfcpb = row(&rows, "tRFCpb");
        assert_eq!(trfcpb.tab, "Timings");
        assert_eq!(field_of(trfcpb.read), FieldRead::new(MCHBAR + 0xE488, 10, 11));
    });
}

#[test]
fn ddr5_keeps_its_mr13_timings() {
    on_fresh_thread(|| {
        let rows = stub_table(DDR5);
        for name in ["tDLLK", "tCCD_L_WR", "tCCD_L_WR2"] {
            assert!(!named(&rows, name).is_empty(), "{name}");
        }
    });
}

#[test]
fn controller_cwl_adjustments_replace_the_false_twpre_row() {
    on_fresh_thread(|| {
        let rows = stub_table(DDR4);
        assert!(!rows.iter().any(|row| row.tab == "Timings" && row.name == "tWPRE"));
        let training = |name: &str| rows.iter().find(|row| row.tab == "Training" && row.name == name).unwrap();
        let add = field_of(side(training("Add tCWL"), Channel::A).read);
        assert_eq!((add.bit_start, add.bit_length), (6, 6));
        let dec = field_of(side(training("Dec tCWL"), Channel::A).read);
        assert_eq!((dec.bit_start, dec.bit_length), (0, 6));
        for name in ["Add tCWL", "Dec tCWL"] {
            let r = training(name);
            assert!(r.is_dual());
            let (a, b) = (field_of(side(r, Channel::A).read), field_of(side(r, Channel::B).read));
            assert_eq!(b.address - a.address, channel_b_offset(DDR4), "{name}");
        }
    });
}

#[test]
fn a_ddr5_table_keeps_per_bank_refresh() {
    on_fresh_thread(|| assert!(!named(&stub_table(DDR5), "tRFCpb").is_empty()));
}

#[test]
fn the_renamed_rows_keep_what_reads_them() {
    // The rename runs last precisely so the passes that match rows by name
    // have already attached addresses and dual-channel getters. A row that
    // lost either would render blank under its new name.
    on_fresh_thread(|| {
        let rows = stub_table(DDR5);
        let trfc2 = row(&rows, "tRFC2");
        assert!(matches!(trfc2.read, Some(ReadSpec::Field(_))));
        assert!(matches!(side(trfc2, Channel::A).read, Some(ReadSpec::Field(_))));
        assert!(matches!(side(trfc2, Channel::B).read, Some(ReadSpec::Field(_))));
        let trfcns = row(&rows, "tRFCns");
        assert!(trfcns.value.as_ref().is_some_and(Value::is_lazy));
        assert!(side(trfcns, Channel::A).value.as_ref().is_some_and(Value::is_lazy));
    });
}

/// tRFCns as get_trfc_ns computes it from these readings.
fn trfc_ns(platform: Platform, all_bank: Option<u64>, per_bank: Option<u64>, mclk: Option<&str>) -> Option<String> {
    let readings = [(ddr5_timing_label(platform, "tRFC").to_string(), all_bank), ("tRFCpb".to_string(), per_bank)];
    super::trfc_ns_from(platform, mclk.map(str::to_string), |name| {
        readings.iter().find(|(n, _)| n == name).and_then(|(_, v)| *v)
    })
}

#[test]
fn trfc_ns_ddr5_shows_the_all_bank_then_the_same_bank_interval() {
    // 480 and 390 ticks at MCLK 4000 are the bench's own numbers. The
    // all-bank interval then the per-bank one, unit named once at the end.
    assert_eq!(trfc_ns(DDR5, Some(480), Some(390), Some("4000 MHz")).as_deref(), Some("120/98 ns"));
}

#[test]
fn trfc_ns_every_nanosecond_row_names_its_unit_the_same_way() {
    for per_bank in [Some(390), None] {
        let value = trfc_ns(DDR5, Some(480), per_bank, Some("4000 MHz")).unwrap();
        assert!(value.ends_with(" ns"), "{value}");
        assert!(!value.contains('('), "{value}");
    }
}

#[test]
fn trfc_ns_ddr4_shows_one_interval() {
    assert_eq!(trfc_ns(DDR4, Some(480), Some(390), Some("4000 MHz")).as_deref(), Some("120 ns"));
}

#[test]
fn trfc_ns_a_board_without_the_per_bank_interval_still_reports() {
    assert_eq!(trfc_ns(DDR5, Some(480), None, Some("4000 MHz")).as_deref(), Some("120 ns"));
}

#[test]
fn trfc_ns_no_mclk_means_no_reading() {
    assert_eq!(trfc_ns(DDR5, Some(480), Some(390), None), None);
}

#[test]
fn trfc_ns_no_interval_means_no_reading() {
    assert_eq!(trfc_ns(DDR5, None, Some(390), Some("4000 MHz")), None);
}

// The gear row read two ways, and what happens when they disagree. Values are
// the ones measured across a Gear 2 -> Gear 4 -> Gear 2 round trip on the
// LGA1700 DDR5 bench.
const GEAR2_REGISTER: u64 = 0x8FC00029;
const GEAR4_REGISTER: u64 = 0x0FC08029;

#[test]
fn scheduler_gear_the_measured_registers_decode_to_the_gear_they_were_read_under() {
    for (raw, gear) in [(GEAR2_REGISTER, 2), (GEAR4_REGISTER, 4)] {
        assert_eq!(registers::scheduler_gear_from(Some(raw), false), Some(gear));
        // And through the register itself.
        FakeHardware::new().with_dword(MCHBAR + registers::SCHEDULER_CONFIG_OFFSET, raw as u32).install();
        FakeWmi::new().install();
        assert_eq!(scheduler_gear_mode(None), Some(gear));
    }
}

#[test]
fn scheduler_gear_exactly_one_flag_moved_between_the_two() {
    // If both bits belonged to the same field, or one of them were
    // something else that happened to move, this would not hold.
    let flipped = GEAR2_REGISTER ^ GEAR4_REGISTER;
    let bits: Vec<u32> = (0..32).filter(|bit| flipped >> bit & 1 != 0).collect();
    assert_eq!(bits, [SCHEDULER_GEAR4_BIT, SCHEDULER_GEAR2_BIT]);
}

#[test]
fn scheduler_gear_bit_zero_is_not_the_gear_flag() {
    // It was the obvious candidate before the measurement -- the only set
    // bit below the known fields -- and it did not move.
    assert_eq!(GEAR2_REGISTER & 1, GEAR4_REGISTER & 1);
    assert!(![SCHEDULER_GEAR2_BIT, SCHEDULER_GEAR4_BIT].contains(&0));
}

#[test]
fn scheduler_gear_says_nothing_rather_than_guessing() {
    // Neither flag set, both set, an absent driver, an unclaimed register.
    for raw in [Some(0x00000000u64), Some(0x80008000), None, Some(0xFFFFFFFF)] {
        assert_eq!(registers::scheduler_gear_from(raw, false), None, "{raw:?}");
    }
}

#[test]
fn every_turnaround_separates_its_suffix() {
    on_fresh_thread(|| {
        let rows = stub_table(DDR5);
        for stem in ["tRDRD", "tRDWR", "tWRRD", "tWRWR"] {
            for suffix in ["sg", "dg", "dr", "dd"] {
                assert!(!named(&rows, &format!("{stem}_{suffix}")).is_empty());
                assert!(named(&rows, &format!("{stem}{suffix}")).is_empty());
            }
        }
    });
}

#[test]
fn the_long_and_short_pairs_separate_theirs() {
    on_fresh_thread(|| {
        let rows = stub_table(DDR5);
        for stem in ["tRRD", "tWTR"] {
            for suffix in ["L", "S"] {
                assert!(!named(&rows, &format!("{stem}_{suffix}")).is_empty());
                assert!(named(&rows, &format!("{stem}{suffix}")).is_empty());
            }
        }
    });
}

// --- tests/test_wide_read.py

/// The eight bytes a 64-bit read of an adjacent dword pair returns.
fn qword(low: u32, high: u32) -> Vec<u8> {
    (((high as u64) << 32) | low as u64).to_le_bytes()[..WIDE_READ_BYTES].to_vec()
}

// Measured on a Z890 bench either side of a single BIOS change, everything
// else held constant.
const BIOS_90: (u32, u32) = (0xD0658204, 0x2206EA02);
const BIOS_120: (u32, u32) = (0xC0658204, 0x2206EA03);
const TWRPDEN_START: u32 = 27;
const TWRPDEN_LENGTH: i32 = 10;

#[test]
fn wide_reads_a_field_spanning_the_dword_boundary() {
    for ((low, high), expected) in [(BIOS_90, 90), (BIOS_120, 120)] {
        assert_eq!(extract_value_from_wide_bytes(&qword(low, high), TWRPDEN_START, TWRPDEN_LENGTH), Some(expected));
    }
}

#[test]
fn wide_neither_dword_alone_contains_the_value() {
    // The point of the wide path: a dword-local search cannot find this
    // field at any bit position or width, so its absence was never
    // evidence that the silicon lacked the register.
    for (low, high) in [BIOS_90, BIOS_120] {
        for dword in [low, high] {
            for width in 7..13 {
                for shift in 0..(33 - width) {
                    let field = extract_value_from_bytes(&dword.to_le_bytes(), shift as u32, width).unwrap();
                    assert!(field != 90 && field != 120);
                }
            }
        }
    }
}

#[test]
fn wide_low_bits_still_come_from_the_low_dword() {
    let (low, high) = BIOS_90;
    assert_eq!(
        extract_value_from_wide_bytes(&qword(low, high), 0, 7),
        extract_value_from_bytes(&low.to_le_bytes(), 0, 7)
    );
}

#[test]
fn wide_rejects_input_that_is_not_eight_bytes() {
    assert_eq!(extract_value_from_wide_bytes(&0xD0658204u32.to_le_bytes(), 0, 8), None);
}

// The Core Ultra 200S positions, as wired.
const EXPECTED_WIDE: [(&str, (u64, u32, i32)); 4] = [
    ("tWRPDEN", (0xE050, 27, 10)),
    ("tCKCKEH", (0xE050, 37, 5)),
    ("tSR", (0xE4C0, 45, 6)),
    ("tXSDLL", (0xE4C0, 51, 13)),
];

#[test]
fn wide_fields_are_declared_where_they_were_measured() {
    assert_eq!(ARROW_LAKE_POWER_DOWN_WIDE, EXPECTED_WIDE);
}

#[test]
fn twrpden_is_no_longer_listed_as_unreadable() {
    assert!(!ARROW_LAKE_POWER_DOWN_UNKNOWN.contains(&"tWRPDEN"));
}

#[test]
fn every_wide_field_actually_needs_the_wide_read() {
    for (name, (_, start, length)) in EXPECTED_WIDE {
        assert!(start as i32 + length > 32, "{name}");
    }
}

#[test]
fn no_two_arrow_lake_rows_claim_the_same_bits() {
    // tXPDLL was reading 0xE050 bits 14..20, which is where this platform
    // keeps tCPDED, so both rows printed the same number.
    let mut claimed: Vec<((u64, u32), &str)> = Vec::new();
    for source in [ARROW_LAKE_POWER_DOWN, ARROW_LAKE_POWER_DOWN_WIDE] {
        for (name, (offset, start, length)) in source {
            for bit in *start..*start + *length as u32 {
                match claimed.iter().find(|(key, _)| *key == (*offset, bit)) {
                    Some((_, owner)) => assert_eq!(owner, name, "0x{offset:X} bit {bit} claimed twice"),
                    None => claimed.push(((*offset, bit), name)),
                }
            }
        }
    }
}

#[test]
fn txpdll_reads_nothing_rather_than_tcpded() {
    assert!(ARROW_LAKE_POWER_DOWN_UNKNOWN.contains(&"tXPDLL"));
    assert!(!ARROW_LAKE_POWER_DOWN.iter().any(|(name, _)| *name == "tXPDLL"));
    assert!(!ARROW_LAKE_POWER_DOWN_WIDE.iter().any(|(name, _)| *name == "tXPDLL"));
}

#[test]
fn the_channel_mirror_carries_the_read_width() {
    // A wide row is mirrored onto the second controller for display. The
    // mirror used to stamp both sides "standard" regardless, so the table
    // showed tWRPDEN as 26 -- the field cut off at the dword boundary --
    // while the register table read 90. The row and its two sides must
    // agree on width.
    let build = Build::new(Platform::Lga1851);
    let mut wide = T::new()
        .s("name", "tWRPDEN")
        .with("address", V::Int(MCHBAR + 0xE050))
        .with("parameters", bits(27, 10))
        .s("read_type", "wide");
    assert!(build.promote_standard_row(&mut wide));
    assert!(wide.is("read_type_a", "wide") && wide.is("read_type_b", "wide"));
    let converted = super::build::convert(&wide);
    for channel in [Channel::A, Channel::B] {
        assert_eq!(field_of(side(&converted, channel).read).kind, ReadKind::Wide);
    }
}

#[test]
fn declared_widths_stay_inside_the_register() {
    for (name, (_, start, length)) in EXPECTED_WIDE {
        assert!(start as i32 + length <= 64, "{name}");
    }
}

#[test]
fn a_fully_set_dword_reads_as_nothing() {
    FakeHardware::new().with_dword(0xFEDC2CE8, 0xFFFF_FFFF).install();
    assert_eq!(read_field(&FieldRead::new(0xFEDC2CE8, 0, 8)), None);
}

#[test]
fn a_narrow_field_of_ones_is_still_a_value() {
    // Scoped to the whole dword on purpose: tXSR legitimately reads 0x3FF
    // out of a register holding 0x000003FF.
    FakeHardware::new().with_dword(0xFEDCE4C0, 0x0000_03FF).install();
    assert_eq!(read_field(&FieldRead::new(0xFEDCE4C0, 0, 13)), Some(0x3FF));
}

// --- tests/test_system_info_not_hardcoded.py

/// The custom General rows as written in this port's source: (name, value
/// expression) for each `row("...", ...)` / `named("...", ...)` call.
fn general_row_source() -> Vec<(String, String)> {
    let source = include_str!("passes.rs");
    let start = source.find("let mut custom = vec![").expect("the custom General rows");
    let end = start + source[start..].find("let rest = std::mem::take").expect("their end");
    let section = &source[start..end];
    let pattern = regex::Regex::new(r#"(?:row|named)\(\s*"([^"]+)",\s*(?:"[^"]*",\s*)?(.+?)\)?,?\s*$"#).unwrap();
    section
        .lines()
        .filter_map(|line| pattern.captures(line.trim()))
        .map(|found| (found[1].to_string(), found[2].to_string()))
        .collect()
}

#[test]
fn no_system_info_row_has_a_literal_value() {
    // A row may hold a constant -- most of this tab cannot change while
    // the machine runs, so it is read once at build rather than on every
    // draw. What it may not do is hold a constant that never came from the
    // machine: get_cpu_name() is a reading, "i9-14900KS" is a transcript.
    let literal = regex::Regex::new(r#"^text\("[^"]*"\)"#).unwrap();
    let offenders: Vec<(String, String)> =
        general_row_source().into_iter().filter(|(_, value)| literal.is_match(value)).collect();
    assert!(offenders.is_empty(), "{offenders:?}");
}

#[test]
fn the_guard_would_catch_a_pasted_value() {
    // The check above passes trivially if the scan finds nothing, so prove
    // it finds the rows it is supposed to be policing.
    let names: Vec<String> = general_row_source().into_iter().map(|(name, _)| name).collect();
    for expected in ["CPU", "Manufacturer", "Model", "BIOS", "Microcode"] {
        assert!(names.iter().any(|n| n == expected), "{expected} not found in {names:?}");
    }
}

/// One System Info row's value from the Intel table, or `None` when the row
/// is not built.
fn system_info_value(rows: &[Row], name: &str) -> Option<String> {
    rows.iter()
        .find(|row| row.tab == SYSTEM_INFO_TAB && row.name == name)
        .map(|row| row.value.as_ref().and_then(|v| match v {
            Value::Text(text) => Some(text.clone()),
            Value::Lazy(f) => f(),
        }).unwrap_or_else(|| "None".into()))
}

#[test]
fn the_cpu_rows_follow_the_cpuid_model() {
    on_fresh_thread(|| {
        let mut seen = Vec::new();
        let mut rows = Vec::new();
        for (index, processor_id) in ["BFEBFBFF000B0671", "BFEBFBFF00090672"].iter().enumerate() {
            let wmi = stub_wmi_for(DDR5).class(
                "Win32_Processor",
                vec![WmiObject::new()
                    .with("Name", "Intel(R) Core(TM) i9-14900KF")
                    .with("Manufacturer", "GenuineIntel")
                    .with("ProcessorId", *processor_id)],
            );
            wmi.install();
            clear_identity_caches();
            if index == 0 {
                rows = build_timings(DDR5);
            }
            seen.push(system_info_value(&rows, "Code Name").expect("Code Name is built"));
        }
        assert_eq!(seen, ["Raptor Lake", "Alder Lake"]);
    });
}

#[test]
fn spd_identity_is_not_repeated_in_system_info() {
    on_fresh_thread(|| {
        let rows = stub_table(DDR5);
        let names = names_on(&rows, "System Info");
        for name in ["RAM Manufacturer", "DRAM Manufacturer", "DRAM Die", "Part Number", "Serial Number", "Manufactured"] {
            assert!(!names.iter().any(|n| n == name), "{name}");
        }
    });
}

#[test]
fn no_gpu_row_is_answered_from_the_device_id() {
    // ROP/TMU counts and a chip's SKU have no entry point to read them
    // from, so neither is shown: there is no ROPs / TMUs row.
    on_fresh_thread(|| {
        let rows = stub_table(DDR5);
        assert!(!names_on(&rows, "System Info").iter().any(|n| n == "ROPs / TMUs"));
    });
}

// --- tests/test_odt_groups.py

const GROUP_ROWS: [&str; 6] =
    ["CA ODT Group A", "CS ODT Group A", "CK ODT Group A", "CA ODT Group B", "CS ODT Group B", "CK ODT Group B"];

// What each code means, as a resistance in ohms. RZQ is 240.
const LADDER: [(u64, Option<u32>); 8] =
    [(0, None), (1, Some(480)), (2, Some(240)), (3, Some(120)), (4, Some(80)), (5, Some(60)), (6, Some(48)), (7, Some(40))];

/// A table as the rows read it, after the reference presentation.
fn presented(table: formulas::Table) -> Vec<(u64, String)> {
    table.iter().map(|(code, text)| (*code, resistance_first_rzq_text(text))).collect()
}

fn odt_tables() -> [formulas::Table; 3] {
    [formulas::CA_ODT_FORMULA, formulas::CS_ODT_FORMULA, formulas::CK_ODT_FORMULA]
}

#[test]
fn every_odt_table_is_the_three_bit_field_and_no_more() {
    for table in odt_tables() {
        let codes: Vec<u64> = table.iter().map(|(code, _)| *code).collect();
        assert_eq!(codes, (0..8).collect::<Vec<u64>>());
    }
}

#[test]
fn the_odt_ladder_has_no_gap() {
    // The defect: 480, 240, 120, 80, 60, RFU, 40 -- every rung present but
    // the 48. A reserved code sitting between two defined ones is the shape
    // of a missing entry, not of a reserved code.
    for table in odt_tables() {
        let table = presented(table);
        for (code, ohms) in LADDER {
            let text = &table.iter().find(|(c, _)| *c == code).unwrap().1;
            match ohms {
                None => assert_eq!(text, "0 RZQ OFF"),
                Some(ohms) => {
                    assert!(text.starts_with(&format!("{ohms} RZQ")), "{text}");
                    assert!(!text.contains("RFU"));
                }
            }
        }
    }
}

#[test]
fn the_odt_ladder_descends() {
    let ohms: Vec<u32> = LADDER[1..].iter().map(|(_, ohms)| ohms.unwrap()).collect();
    let mut sorted = ohms.clone();
    sorted.sort_by(|a, b| b.cmp(a));
    assert_eq!(ohms, sorted);
}

#[test]
fn odt_resistance_first_notation() {
    let text = |table, code| presented(table).into_iter().find(|(c, _)| *c == code).unwrap().1;
    assert_eq!(text(formulas::CA_ODT_FORMULA, 0), "0 RZQ OFF");
    assert_eq!(text(formulas::CA_ODT_FORMULA, 1), "480 RZQ/0.5");
    assert_eq!(text(formulas::CA_ODT_FORMULA, 6), "48 RZQ/5");
    assert_eq!(text(formulas::RTT_WR_FORMULA, 6), "40 RZQ/6");
    assert_eq!(text(formulas::RON_FORMULA, 0), "34 RZQ/7");
}

// The row tests are DDR5's: a DDR4 table does not build these rows at all,
// so the Python, run on its DDR4 default fixture, skipped them.
fn odt_rows(rows: &[Row]) -> Vec<&Row> {
    rows.iter().filter(|row| GROUP_ROWS.contains(&row.name.as_str())).collect()
}

#[test]
fn all_six_odt_rows_are_present() {
    on_fresh_thread(|| {
        let rows = stub_table(DDR5);
        let mut found: Vec<&str> = odt_rows(&rows).iter().map(|row| row.name.as_str()).collect();
        found.sort();
        let mut expected = GROUP_ROWS.to_vec();
        expected.sort();
        assert_eq!(found, expected);
    });
}

#[test]
fn every_odt_side_reads_three_bits() {
    // Four bits reaches a neighbouring field, whose bit made Group B's
    // code 6 look like a 14 and sent the first diagnosis the wrong way.
    on_fresh_thread(|| {
        let rows = stub_table(DDR5);
        for row in odt_rows(&rows) {
            for channel in [Channel::A, Channel::B] {
                let params = dynamic_of(side(row, channel).read);
                assert_eq!(params.bit_length_dynamic, 3, "{}", row.name);
                assert_eq!(params.bit_start_dynamic, 0, "{}", row.name);
            }
        }
    });
}

#[test]
fn the_two_odt_groups_look_up_different_entries() {
    // Group A and Group B differ only by which table entry they follow, so
    // a copied key would silently make one group report the other's value.
    on_fresh_thread(|| {
        let rows = stub_table(DDR5);
        let mut keys: Vec<u64> =
            odt_rows(&rows).iter().map(|row| dynamic_of(side(row, Channel::A).read).value_to_find).collect();
        keys.sort();
        keys.dedup();
        assert_eq!(keys.len(), GROUP_ROWS.len());
    });
}

// --- tests/test_ccd_timings.py

const CCD_NAMES: [&str; 4] = ["tCCD", "tCCD_L", "tCCD_L_WR", "tCCD_L_WR2"];
const DDR4_INSTALLED_CCD_NAMES: [&str; 2] = ["tCCD", "tCCD_L"];
const MR13_NAMES: [&str; 3] = ["tCCD_L", "tCCD_L_WR", "tCCD_L_WR2"];

// A name nothing carries, used to check that the gate still refuses one.
const UNSOURCED_NAME: &str = "tNOPE";

// A table entry names a mode register in its data byte and points at the slot
// holding that register's contents with its index byte. It does not carry the
// value itself, which is what the earlier reading of this table assumed.
const PAYLOAD_SLOT: u32 = 0x12;

/// One row of the controller's mode-register table.
fn mr_entry(number: u32, slot: u32) -> u32 {
    0x8000_0000 | ((slot & 0xFF) << 8) | (number & 0xFF)
}

/// The controller's table at `base`: every entry zero except those given,
/// and the payload bytes beside it.
fn mr_table(base: u64, entries: &[(u64, u32)]) -> Arc<FakeHardware> {
    let mut fake = FakeHardware::new();
    for offset in (0xE600..0xE800).step_by(4) {
        fake = fake.with_dword(base + offset, 0);
    }
    for offset in (0xE200..0xE300).step_by(4) {
        fake = fake.with_dword(base + offset, 0);
    }
    for (offset, value) in entries {
        fake = fake.with_dword(base + offset, *value);
    }
    fake.install()
}

/// An entry naming `number` at 0xE648, plus the payload byte it points at.
fn mr13(base: u64, number: u32, value: u32) -> Arc<FakeHardware> {
    mr_table(base, &[(0xE648, mr_entry(number, PAYLOAD_SLOT)), (0xE200 + PAYLOAD_SLOT as u64, value)])
}

// 0xE08C across two controlled BIOS changes on the Z790 target: tCCD 6 /
// tCCD_L 10, then tCCD 8 / tCCD_L 14. Identical, so the 6 and 10 it appeared to
// hold were numbers it always held.
const E08C_AT_6_10: u32 = 0x04008A06;
const E08C_AT_8_14: u32 = 0x04008A06;
// TC_RDRD across the same two changes: also identical, which is what refuted it
// as the source of these timings.
const TC_RDRD_AT_6_10: u32 = 0x08080488;
const TC_RDRD_AT_8_14: u32 = 0x08080488;

#[test]
fn ccd_no_field_ships_confirmed() {
    assert!(CCD_CONFIRMED_FIELDS.is_empty());
}

#[test]
fn ccd_an_unknown_timing_name_reports_no_value() {
    pin_generation(26);
    assert_eq!(get_ccd_timing(UNSOURCED_NAME, None), None);
}

#[test]
fn ccd_an_unknown_name_does_not_read_hardware_at_all() {
    pin_generation(26);
    let fake = FakeHardware::new().install();
    assert_eq!(get_ccd_timing(UNSOURCED_NAME, None), None);
    assert!(fake.phys_reads().is_empty());
}

#[test]
fn tccd_is_empty_when_the_shadow_cannot_be_read() {
    // The whole reason it is a table keyed on a register rather than a
    // bare 4.
    assert_eq!(registers::ddr4_ccd_s_from(None), None);
    FakeHardware::new().install();
    assert_eq!(registers::ddr4_ccd_s(None), None);
}

#[test]
fn tccd_is_empty_on_a_burst_code_ddr4_does_not_define() {
    // DDR4 defines codes 0-2. Code 3 is reserved.
    assert!(!DDR4_TCCD_S.iter().any(|(code, _)| *code == 3));
    assert_eq!(registers::ddr4_ccd_s_from(Some(3)), None);
}

#[test]
fn every_ddr4_burst_mode_gives_the_jedec_four() {
    assert!(DDR4_TCCD_S.iter().all(|(_, value)| *value == 4));
}

#[test]
fn mode_register_the_table_is_searched_for_the_requested_register() {
    pin_generation(34);
    mr13(MCHBAR, 0x0D, 0x08);
    assert_eq!(read_mode_register(0x0D, None), Some(0x08));
}

#[test]
fn mode_register_the_entry_names_the_register_and_points_at_the_value() {
    // The distinction the old reading got backwards. Here the entry names
    // MR13 and points at slot 0x12; the value lives there, not in the entry.
    pin_generation(34);
    mr13(MCHBAR, 0x0D, 0x08);
    let value = read_mode_register(0x0D, None);
    assert_eq!(value, Some(0x08));
    assert!(value != Some(0x12) && value != Some(0x0D));
}

#[test]
fn mode_register_a_register_the_table_does_not_list_reports_nothing() {
    pin_generation(34);
    mr13(MCHBAR, 0x0D, 0x08);
    assert_eq!(read_mode_register(0x0A, None), None);
}

#[test]
fn mode_register_an_entry_without_the_prefix_is_not_a_mode_register() {
    // The table shares its range with other data; the prefix is what says
    // a dword is one of these entries.
    pin_generation(34);
    mr_table(MCHBAR, &[(0xE648, 0x1234120D), (0xE212, 0x08)]);
    assert_eq!(read_mode_register(0x0D, None), None);
}

#[test]
fn mode_register_the_bench_code_decodes_to_the_three_timings() {
    // MR13 holds 0x08 on the Z790 bench, identically on both controllers.
    pin_generation(34);
    mr13(MCHBAR, 0x0D, 0x08);
    assert_eq!(get_ccd_timing("tCCD_L", None), Some(16));
    assert_eq!(get_ccd_timing("tCCD_L_WR", None), Some(64));
    assert_eq!(get_ccd_timing("tCCD_L_WR2", None), Some(32));
}

#[test]
fn mode_register_the_encoding_holds_across_the_range() {
    // JESD79-5: 8 + code, then four and two times that. Code 12 is the
    // MPOWER's 20 / 80 / 40, as the reference tool reads it.
    pin_generation(34);
    for (code, expected) in [(0u32, (8, 32, 16)), (6, (14, 56, 28)), (12, (20, 80, 40)), (14, (22, 88, 44))] {
        mr13(MCHBAR, 0x0D, code);
        let got: Vec<Option<u64>> = MR13_NAMES.iter().map(|n| get_ccd_timing(n, None)).collect();
        assert_eq!(got, [Some(expected.0), Some(expected.1), Some(expected.2)], "code {code}");
    }
}

#[test]
fn mode_register_the_reserved_code_is_not_a_timing() {
    pin_generation(34);
    mr13(MCHBAR, 0x0D, 0x0F);
    for name in MR13_NAMES {
        assert_eq!(get_ccd_timing(name, None), None, "{name}");
    }
}

#[test]
fn mode_register_only_the_low_nibble_carries_the_timing() {
    // The rest of MR13 is other settings and must not shift the answer.
    pin_generation(34);
    mr13(MCHBAR, 0x0D, 0xF8);
    assert_eq!(get_ccd_timing("tCCD_L", None), Some(16));
}

#[test]
fn mode_register_it_reads_the_controller_it_was_given() {
    pin_generation(34);
    mr13(MCHBAR2, 0x0D, 0x06);
    assert_eq!(get_ccd_timing("tCCD_L", Some(MCHBAR2)), Some(14));
}

#[test]
fn mode_register_an_absent_table_reports_nothing_rather_than_guessing() {
    pin_generation(34);
    mr_table(MCHBAR, &[]);
    for name in MR13_NAMES {
        assert_eq!(get_ccd_timing(name, None), None, "{name}");
    }
}

// DDR4 keeps tCCD_L in MR6, out of the shadow at 0xE5A0. MR6 = 0x101C:
// VrefDQ 28, range 0, tCCD_L code 4.
const BENCH_MR6: u32 = 0x101C;

fn ddr4_shadow(mr6: u32, base: u64) -> Arc<FakeHardware> {
    let where_ = base + DDR4_MODE_REGISTER_BASE + 12;
    let mut fake = FakeHardware::new().with_dword(where_, mr6);
    // Every other register reads 0, as the patched reader returned.
    for other in [MCHBAR, MCHBAR2] {
        if other != base {
            fake = fake.with_dword(other + DDR4_MODE_REGISTER_BASE + 12, 0);
        }
    }
    fake.install()
}

#[test]
fn ddr4_mode_register_the_bench_register_gives_the_timing_the_bios_shows() {
    pin_generation(26);
    ddr4_shadow(BENCH_MR6, MCHBAR);
    assert_eq!(get_ccd_timing("tCCD_L", None), Some(8));
}

#[test]
fn ddr4_mode_register_the_encoding_holds_across_its_range() {
    pin_generation(26);
    for (code, expected) in [4u64, 5, 6, 7, 8].iter().enumerate() {
        ddr4_shadow(0x1C | ((code as u32) << 10), MCHBAR);
        assert_eq!(get_ccd_timing("tCCD_L", None), Some(*expected), "code {code}");
    }
}

#[test]
fn ddr4_mode_register_an_undefined_code_is_not_a_timing() {
    pin_generation(26);
    ddr4_shadow(0x1C | (5 << 10), MCHBAR);
    assert_eq!(get_ccd_timing("tCCD_L", None), None);
}

#[test]
fn ddr4_mode_register_the_write_variants_have_no_ddr4_register() {
    // DDR5 derives them from the same MR13 nibble. DDR4 defines no such
    // field, so deriving them here would print a DDR5 figure.
    pin_generation(26);
    ddr4_shadow(BENCH_MR6, MCHBAR);
    for name in ["tCCD_L_WR", "tCCD_L_WR2"] {
        assert_eq!(get_ccd_timing(name, None), None, "{name}");
    }
}

#[test]
fn ddr4_mode_register_it_reads_the_controller_it_was_given() {
    // Only the second controller's shadow holds the bench value; the
    // first reads zero, which is a different code and a different timing.
    pin_generation(26);
    ddr4_shadow(BENCH_MR6, MCHBAR2);
    assert_eq!(get_ccd_timing("tCCD_L", Some(MCHBAR2)), Some(8));
    assert_eq!(get_ccd_timing("tCCD_L", Some(MCHBAR)), Some(4));
}

#[test]
fn ddr4_mode_register_the_ddr5_table_is_not_consulted_at_all() {
    pin_generation(26);
    let fake = ddr4_shadow(BENCH_MR6, MCHBAR);
    get_ccd_timing("tCCD_L", None);
    let table = MCHBAR + registers::MODE_REGISTER_TABLE_START..MCHBAR + registers::MODE_REGISTER_TABLE_END;
    assert!(!fake.phys_reads().iter().any(|address| table.contains(address)));
}

#[test]
fn refuted_the_scheduler_register_did_not_move_across_the_bios_changes() {
    assert_eq!(TC_RDRD_AT_6_10, TC_RDRD_AT_8_14);
}

#[test]
fn refuted_the_scheduler_register_never_held_any_of_the_settings() {
    // Neither 6/10 nor 8/14 appears in the .sg or .dg field.
    assert_eq!(TC_RDRD_AT_8_14 & 0x7F, 8);
    assert_eq!((TC_RDRD_AT_8_14 >> 8) & 0x7F, 4);
}

#[test]
fn refuted_the_looked_promising_register_did_not_move_either() {
    assert_eq!(E08C_AT_6_10, E08C_AT_8_14);
}

#[test]
fn refuted_that_register_held_the_first_pair_by_coincidence() {
    // It reads 6 and 10 at both settings, so agreeing once meant nothing.
    assert_eq!(E08C_AT_8_14 & 0x7F, 6);
    assert_eq!((E08C_AT_8_14 >> 8) & 0x7F, 10);
}

#[test]
fn refuted_no_candidate_survived() {
    assert!(CCD_CANDIDATE_FIELDS.is_empty());
}

#[test]
fn refuted_the_effective_delays_are_still_shown_under_their_own_names() {
    on_fresh_thread(|| {
        let rows = stub_table(DDR4);
        for name in ["tRDRD_sg", "tRDRD_dg", "tWRWR_sg"] {
            assert!(!named(&rows, name).is_empty(), "{name}");
        }
    });
}

/// Behaviour once a field is confirmed, driven through the map.
const CONFIRMED: &[(&str, (u64, u32))] = &[("tCCD", (0xE08C, 8))];

#[test]
fn confirmed_a_field_is_read_from_its_register_and_bits() {
    pin_generation(34);
    let fake = FakeHardware::new().with_dword(MCHBAR + 0xE08C, 0).install();
    registers::ccd_timing_with("tCCD", None, CONFIRMED);
    assert_eq!(fake.phys_reads().last(), Some(&(MCHBAR + 0xE08C)));
}

#[test]
fn confirmed_a_field_reads_the_controller_it_was_given() {
    pin_generation(34);
    let fake = FakeHardware::new().with_dword(MCHBAR2 + 0xE08C, 0).install();
    registers::ccd_timing_with("tCCD", Some(MCHBAR2), CONFIRMED);
    assert_eq!(fake.phys_reads().last(), Some(&(MCHBAR2 + 0xE08C)));
}

#[test]
fn confirmed_a_field_decodes_from_its_own_bits() {
    // 14 at bits 8-14, the value a confirmed field would have to report.
    pin_generation(34);
    FakeHardware::new().with_dword(MCHBAR + 0xE08C, 14 << 8).install();
    assert_eq!(registers::ccd_timing_with("tCCD", None, CONFIRMED), Some(14));
}

#[test]
fn confirmed_an_unreadable_register_still_reports_no_value() {
    pin_generation(34);
    FakeHardware::new().install();
    assert_eq!(registers::ccd_timing_with("tCCD", None, CONFIRMED), None);
}

#[test]
fn confirmed_a_mode_register_row_ignores_this_map() {
    // MR13 is the source for those, so a stale entry here must not win.
    pin_generation(34);
    mr_table(MCHBAR, &[(0xE08C, 0)]);
    assert_eq!(registers::ccd_timing_with("tCCD_L", None, &[("tCCD_L", (0xE08C, 8))]), None);
}

#[test]
fn installed_ccd_rows_are_on_the_timings_tab() {
    on_fresh_thread(|| {
        let rows = stub_table(DDR4);
        for name in DDR4_INSTALLED_CCD_NAMES {
            assert_eq!(row(&rows, name).tab, "Timings", "{name}");
        }
    });
}

#[test]
fn ddr5_write_variants_are_absent_from_ddr4() {
    on_fresh_thread(|| {
        let rows = stub_table(DDR4);
        for name in ["tCCD_L_WR", "tCCD_L_WR2"] {
            assert!(named(&rows, name).is_empty(), "{name}");
        }
        let _ = CCD_NAMES;
    });
}

#[test]
fn the_mode_register_ccd_rows_are_gone() {
    on_fresh_thread(|| {
        let rows = stub_table(DDR4);
        for stale in ["tCCDL", "tCCDL WR"] {
            assert!(named(&rows, stale).is_empty(), "{stale}");
        }
    });
}

#[test]
fn no_ccd_row_still_decodes_through_a_mode_register_table() {
    on_fresh_thread(|| {
        let rows = stub_table(DDR4);
        for name in DDR4_INSTALLED_CCD_NAMES {
            let r = row(&rows, name);
            assert!(r.formula.is_none(), "{name}");
            assert!(!matches!(r.read, Some(ReadSpec::Dynamic(_))), "{name}");
        }
    });
}

#[test]
fn every_ccd_row_reads_both_channels() {
    on_fresh_thread(|| {
        let rows = stub_table(DDR4);
        for name in DDR4_INSTALLED_CCD_NAMES {
            assert!(row(&rows, name).is_dual(), "{name}");
        }
    });
}

fn ccd_section(rows: &[Row]) -> Vec<String> {
    rows.iter().filter(|row| row.tab == "Timings" && row.category == CCD_CATEGORY).map(|row| row.name.clone()).collect()
}

#[test]
fn the_ccd_rows_form_one_cas_to_cas_section() {
    // Mode-register copies are taken out first. tCCD_L_MR sits directly
    // under tCCD_L by the rule every _MR row follows, which puts it inside
    // this group without belonging to it.
    on_fresh_thread(|| {
        let rows = stub_table(DDR4);
        let names: Vec<String> =
            ccd_section(&rows).into_iter().filter(|n| !n.ends_with(MODE_REGISTER_TIMING_SUFFIX)).collect();
        assert_eq!(names, DDR4_INSTALLED_CCD_NAMES);
    });
}

#[test]
fn the_mode_register_copy_sits_directly_under_tccd_l() {
    // DDR4 only: DDR5 keeps tCCD_L in a register this project has no
    // verified mapping for, so the row is absent there rather than wrong.
    on_fresh_thread(|| {
        let rows = stub_table(DDR4);
        let names = ccd_section(&rows);
        let at = names.iter().position(|n| n == "tCCD_L").unwrap();
        assert_eq!(names[at + 1], "tCCD_L_MR");
    });
}

#[test]
fn the_ccd_values_are_read_lazily_rather_than_at_build() {
    on_fresh_thread(|| {
        let rows = stub_table(DDR4);
        assert!(row(&rows, "tCCD_L").value.as_ref().is_some_and(Value::is_lazy));
    });
}

// --- tests/test_am5_touches_no_intel_reader.py
//
// The two that read AM5 rows skip off AM5 in the Python and need the AM5
// profile, which this branch has not ported yet (amd::profile returns no
// rows); they are checked here against whatever the AM5 backend builds, so
// they start meaning something the moment it does.

const MCHBAR_END: u64 = 0xFEDE_0000;

#[test]
fn reading_the_am5_rows_reads_nothing_in_the_intel_mchbar_window() {
    let fake = FakeHardware::new().install();
    FakeWmi::new().install();
    let backend = crate::timings::load_timing_backend(Platform::Am5);
    for row in &backend.rows {
        let _ = backend.read_value(row, None);
    }
    assert!(!fake.phys_reads().iter().any(|address| (MCHBAR..MCHBAR_END).contains(address)));
}

#[test]
fn generation_detection_lives_outside_the_intel_package() {
    let path = std::any::type_name::<crate::memory::ddr_generation::DdrGeneration>();
    assert!(!path.contains("::intel::"), "{path}");
}

#[test]
fn the_spd_module_reaches_the_generation_without_the_intel_table() {
    let source = include_str!("../../memory/ddr5_spd.rs");
    let start = source.find("pub fn installed_generation").expect("installed_generation");
    let body = &source[start..start + source[start..].find("\n}").expect("its end")];
    assert!(!body.contains("intel"), "{body}");
    assert!(body.contains("detect_ddr_generation"), "{body}");
}

// --- tests/test_intel_stub_restores.py
//
// The Python fixture swapped modules in and out of sys.modules, and these
// checked it put everything back. A Rust table is built for the platform it
// is given, with per-thread fakes and per-thread caches, so there is nothing
// to restore; what remains worth checking is that building one platform's
// table leaves the next one's untouched.

#[test]
fn repeated_cycles_do_not_drift() {
    for platform in [DDR4, DDR5, DDR4] {
        let trfc = if platform == DDR5 { "tRFC2" } else { "tRFC" };
        let rows = on_fresh_thread(move || stub_table(platform));
        assert!(!named(&rows, trfc).is_empty(), "{platform:?}");
    }
}

#[test]
fn a_table_is_built_for_the_platform_it_is_given() {
    on_fresh_thread(|| {
        // The active platform says DDR5; the table asked for is DDR4.
        crate::platform_profiles::set_active_platform(DDR5);
        let rows = stub_table(DDR4);
        assert!(!named(&rows, "tRFC").is_empty());
        assert!(named(&rows, "tRFC2").is_empty());
    });
}

// --- tests/test_nct6687d_board_map.py (the two cases on this module)

fn on_board(product: &str) {
    FakeWmi::new()
        .class(
            "Win32_BaseBoard",
            vec![WmiObject::new().with("Manufacturer", "Micro-Star International Co., Ltd.").with("Product", product)],
        )
        .install();
}

#[test]
fn each_duty_row_follows_its_fan() {
    on_board("Z790MPOWER (MS-7E01)");
    let rows: Vec<&str> =
        super::sensors::nct6687d_rows().into_iter().filter(|row| row.category == "Fans").map(|row| row.name).collect();
    assert_eq!(rows, ["CPU Fan", "CPU Fan Duty", "PUMP1", "PUMP1 Duty", "System 1", "System 1 Duty"]);
}

#[test]
fn nct6687d_rows_follow_the_board_map() {
    on_board("Z790MPOWER (MS-7E01)");
    let names: Vec<&str> = super::sensors::nct6687d_rows().into_iter().map(|row| row.name).collect();
    assert_eq!(
        names,
        ["T1", "+12V", "+5V", "VIN3", "VIN7", "+3.3V", "CPU Fan", "CPU Fan Duty", "PUMP1", "PUMP1 Duty", "System 1", "System 1 Duty"]
    );
    on_board("PRO Z790-P WIFI DDR4 (MS-7E06)");
    assert!(super::sensors::nct6687d_rows().is_empty());
}

// --- tests/test_ddr5_additional_fields.py

const MODE_REGISTER_ROWS: [&str; 11] = [
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
const PHY_ROWS: [&str; 3] = ["Weak Lock End Delay", "SCR DLL Enable Timer", "SCR PI Enable Timer"];
const CWL_ROWS: [&str; 2] = ["Add tCWL", "Dec tCWL"];
const REGISTER_ROWS: [&str; 10] = [
    "Refresh Interval",
    "Refresh Stagger",
    "Refresh Stagger Mode",
    "Stolen Refresh",
    "Refresh Type Display",
    "tREFI Pulse Stagger",
    "Wake Up On HPM",
    "Write 0",
    "Multi-Cycle Command",
    "Add 1 QCLK Delay",
];

/// `{name: row}` for the rows named, the last of each name winning.
fn rows_named<'a>(rows: &'a [Row], names: &[&str]) -> Vec<(&'a str, &'a Row)> {
    let mut found: Vec<(&str, &Row)> = Vec::new();
    for row in rows.iter().filter(|row| names.contains(&row.name.as_str())) {
        match found.iter_mut().find(|(name, _)| *name == row.name) {
            Some(entry) => entry.1 = row,
            None => found.push((row.name.as_str(), row)),
        }
    }
    found
}

fn name_set(found: &[(&str, &Row)]) -> Vec<String> {
    let mut names: Vec<String> = found.iter().map(|(name, _)| name.to_string()).collect();
    names.sort();
    names
}

fn sorted(names: &[&str]) -> Vec<String> {
    let mut names: Vec<String> = names.iter().map(|n| n.to_string()).collect();
    names.sort();
    names
}

#[test]
fn ddr5_mode_registers_read_both_modules_on_training() {
    on_fresh_thread(|| {
        let rows = stub_table(DDR5);
        // Refresh Interval Rate Indicator only says what the DRAM can do and
        // is left off LGA1700 DDR5's Training; see DDR5_TRAINING_REMOVED.
        let shown: Vec<&str> =
            MODE_REGISTER_ROWS.iter().copied().filter(|n| !DDR5_TRAINING_REMOVED.contains(n)).collect();
        let found = rows_named(&rows, &MODE_REGISTER_ROWS);
        assert_eq!(name_set(&found), sorted(&shown));
        for (name, r) in found {
            assert_eq!(r.tab, "Training", "{name}");
            assert_eq!(r.category, "Mode Registers", "{name}");
            assert!(r.is_dual(), "{name}");
        }
    });
}

#[test]
fn ddr5_the_capability_rows_are_left_off() {
    on_fresh_thread(|| {
        let rows = stub_table(DDR5);
        let names = names_on(&rows, "Training");
        for name in ["Wide Range", "Package Output Driver Test Mode", "Refresh Interval Rate Indicator", "SRX/NOP Clock-Sync Support"] {
            assert!(!names.iter().any(|n| n == name), "{name}");
        }
    });
}

#[test]
fn ddr5_long_values_are_worded_briefly() {
    for (long, short) in [
        ("0 RZQ OFF", "Off"),
        ("1 tCK - 10 Pattern", "1 tCK"),
        ("1.5 tCK - 010 Pattern", "1.5 tCK"),
        // The read preamble's two 2 tCK settings stay apart.
        ("2 tCK - 0010 Pattern", "2 tCK (0010)"),
        ("2 tCK - 1110 Pattern", "2 tCK (1110)"),
        ("Manual ECS Mode Disabled", "Disabled"),
        ("ECS counts Rows with errors", "Rows"),
        ("Timer Stops at 2048th clocks", "2048 clocks"),
        ("Timer Stops at 1st clocks", "1 clocks"),
        ("40 RZQ/6", "40 RZQ/6"),
    ] {
        assert_eq!(brief_ddr5_value(long, None), short, "{long}");
    }
    // A number is shown as itself.
    assert_eq!(brief_ddr5_value("7", None), "7");
    // The write preamble has one 2 tCK setting, so it needs no pattern.
    assert_eq!(brief_ddr5_value("2 tCK - 0010 Pattern", Some("Write Preamble")), "2 tCK");
}

#[test]
fn ddr5_the_rows_read_briefly_and_the_tables_are_untouched() {
    on_fresh_thread(|| {
        let rows = stub_table(DDR5);
        let training = |name: &str| rows.iter().find(|r| r.tab == "Training" && r.name == name).unwrap();
        match &training("RTT Nom Rd").formula {
            Some(crate::row::Formula::Map(map)) => assert_eq!(map.get(&0).map(String::as_str), Some("Off")),
            other => panic!("RTT Nom Rd has no table: {other:?}"),
        }
        assert_eq!(training("ECS Mode").display_name.as_deref(), Some("Manual ECS"));
        // The shared tables DDR4 and the other platforms read keep their words.
        assert_eq!(resistance_first_rzq_text(lookup(formulas::RTT_NOM_RD_FORMULA, 0).unwrap()), "0 RZQ OFF");
        assert_eq!(lookup(formulas::MISC_POSTAMBLE, 1), Some("1.5 tCK - 010 Pattern"));
    });
}

#[test]
fn ddr5_the_imc_rows_read_by_their_proper_names() {
    on_fresh_thread(|| {
        let rows = stub_table(DDR5);
        assert_eq!(row(&rows, "CMD SlewStatlegen").display_name.as_deref(), Some("CMD Slew Static Leg"));
        assert_eq!(row(&rows, "Realtime Memory").display_name.as_deref(), Some("Realtime Memory Timing"));
    });
    assert_eq!(imc_switch_text("1"), "Enabled");
    assert_eq!(imc_switch_text("0"), "Disabled");
    assert_eq!(imc_switch_text("N/A"), "N/A");
}

/// Each category in its first-seen order; a category seen twice is a section
/// drawn twice.
fn sections(rows: &[Row], tab: &str) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for row in rows.iter().filter(|row| row.tab == tab) {
        if seen.last() != Some(&row.category) {
            seen.push(row.category.clone());
        }
    }
    seen
}

fn assert_each_once(seen: &[String]) {
    let mut unique = seen.to_vec();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), seen.len(), "{seen:?}");
}

#[test]
fn ddr5_the_mode_register_section_is_one_block() {
    // The new rows sit ahead of Read DQS Offset Timing, which moves to DQS;
    // after it, Mode Registers and DQS would each be drawn twice.
    on_fresh_thread(|| assert_each_once(&sections(&stub_table(DDR5), "Training")));
}

#[test]
fn ddr5_cwl_reads_both_modules_on_training() {
    on_fresh_thread(|| {
        let rows = stub_table(DDR5);
        let found = rows_named(&rows, &CWL_ROWS);
        assert_eq!(found.len(), 2);
        for (name, r) in found {
            assert_eq!(r.tab, "Training", "{name}");
            assert!(r.is_dual(), "{name}");
        }
    });
}

#[test]
fn ddr5_phy_fields_are_one_shared_value_on_imc() {
    on_fresh_thread(|| {
        let rows = stub_table(DDR5);
        let found = rows_named(&rows, &PHY_ROWS);
        assert_eq!(name_set(&found), sorted(&PHY_ROWS));
        for (name, r) in found {
            assert_eq!(r.tab, "IMC", "{name}");
            assert!(!r.is_dual(), "{name}");
        }
    });
}

#[test]
fn ddr5_register_fields_read_both_modules_on_training() {
    on_fresh_thread(|| {
        let rows = stub_table(DDR5);
        let found = rows_named(&rows, &REGISTER_ROWS);
        assert_eq!(name_set(&found), sorted(&REGISTER_ROWS));
        for (name, r) in found {
            assert_eq!(r.tab, "Training", "{name}");
            assert!(r.is_dual(), "{name}");
            let (a, b) = (field_of(side(r, Channel::A).read), field_of(side(r, Channel::B).read));
            assert_eq!(b.address - a.address, channel_b_offset(DDR5), "{name}");
        }
    });
}

#[test]
fn ddr5_add_one_qclk_delay_shares_the_cwl_register() {
    on_fresh_thread(|| {
        let rows = stub_table(DDR5);
        let a = field_of(side(row(&rows, "Add 1 QCLK Delay"), Channel::A).read);
        assert_eq!(a.address, MCHBAR + 0xE478);
        assert_eq!((a.bit_start, a.bit_length), (12, 1));
    });
}

#[test]
fn ddr5_controller_level_settings_read_b1s_controller() {
    // Error Correction and Self Refresh have a copy in MC1's window; the
    // global Realtime Memory and Memory Scrambler do not.
    on_fresh_thread(|| {
        let rows = stub_table(DDR5);
        let tab = |name: &str| rows_named(&rows, &[name])[0].1.tab.clone();
        assert_eq!(tab("Error Correction"), "Training");
        assert_eq!(tab("Self Refresh"), "Training");
        assert_eq!(tab("Realtime Memory"), "IMC");
        assert_eq!(tab("Memory Scrambler"), "IMC");
    });
}

#[test]
fn ddr5_each_mode_register_field_decodes_the_bench_register() {
    // The bench's MR2 0x90, MR3 0x04 and MR4 0x12, as the reference tool read them.
    let expected = [
        ("Write Leveling", 0x90u64, "Normal Mode"),
        ("N-Mode", 0x90, "2N Mode"),
        ("MPSM", 0x90, "Disabled"),
        ("CS Assertion Duration", 0x90, "Single Cycle"),
        ("Device 15 MPSM", 0x90, "Disabled"),
        ("Internal Write Timing", 0x90, "Enabled"),
        ("Write Leveling LB", 0x04, "-4 tCK"),
        ("Write Leveling UB", 0x04, "0 tCK"),
        ("Minimum Refresh Rate", 0x12, "tREFI x1"),
        ("Refresh Interval Rate Indicator", 0x12, "Not implemented"),
        ("TUF", 0x12, "No Change"),
    ];
    for (name, raw, shown) in expected {
        let (_, _, start, length, decode) =
            MISC_MODE_REGISTER_STATE.iter().find(|(n, ..)| *n == name).copied().unwrap();
        let value = (raw >> start) & ((1 << length) - 1);
        let text = decode.and_then(|table| lookup(table, value)).map(str::to_string).unwrap_or_else(|| value.to_string());
        assert_eq!(text, shown, "{name}");
    }
}

#[test]
fn ddr4_has_every_group_and_none_of_the_ddr5_mode_registers() {
    on_fresh_thread(|| {
        let rows = stub_table(DDR4);
        // DDR4 has a Write Leveling of its own, in MR1 and its own section;
        // none of the DDR5 MR2-MR4 decodes may appear beside it.
        assert!(!rows_named(&rows, &MODE_REGISTER_ROWS).iter().any(|(_, r)| r.category == "Mode Registers"));
        for names in [&PHY_ROWS[..], &CWL_ROWS[..], &REGISTER_ROWS[..]] {
            assert_eq!(name_set(&rows_named(&rows, names)), sorted(names));
        }
    });
}

// The IMC rows whose register each module's controller has a copy of.
const PER_CHANNEL_IMC_ROWS: [&str; 14] = [
    "Idle Length",
    "Power Down Enable",
    "Self Refresh Enable",
    "CMD Stretch",
    "N:1 Ratio",
    "Power Down",
    "Row Hammer",
    "Page Close Idle Timeout",
    "Error Correction",
    "Self Refresh",
    "PBR Disable",
    "Refresh HP WM",
    "Raise Block Wait",
    "Allow 2cyc B2B LPDDR",
];
const SHARED_IMC_ROWS: [&str; 4] = ["Realtime Memory", "Memory Scrambler", "DLL Code PI", "Dq Vref Up"];

#[test]
fn ddr5_per_channel_rows_read_both_modules_on_training() {
    on_fresh_thread(|| {
        let rows = stub_table(DDR5);
        let found = rows_named(&rows, &PER_CHANNEL_IMC_ROWS);
        assert_eq!(name_set(&found), sorted(&PER_CHANNEL_IMC_ROWS));
        for (name, r) in found {
            assert_eq!(r.tab, "Training", "{name}");
            assert_eq!(r.extra_str("source_scope"), Some("module"), "{name}");
            assert!(r.is_dual(), "{name}");
        }
    });
}

#[test]
fn ddr5_shared_rows_stay_single_on_imc() {
    on_fresh_thread(|| {
        let rows = stub_table(DDR5);
        for (name, r) in rows_named(&rows, &SHARED_IMC_ROWS) {
            assert_eq!(r.tab, "IMC", "{name}");
            assert!(!r.is_dual(), "{name}");
        }
    });
}

#[test]
fn ddr5_a_computed_register_row_reads_b1_at_its_base() {
    on_fresh_thread(|| {
        let rows = stub_table(DDR5);
        let idle = rows_named(&rows, &["Idle Length"])[0].1;
        let hardware = ConstHardware::install(Some(0));
        let _ = resolve(&side(idle, Channel::B).value);
        assert_eq!(hardware.asked(), [channel_b(DDR5) + MISC_CKE_CONFIG_OFFSET]);
        assert_eq!(channel_b(DDR5), MCHBAR2);
    });
}

#[test]
fn ddr5_imc_puts_phy_control_and_features_under_vref() {
    on_fresh_thread(|| {
        let rows = stub_table(DDR5);
        for r in rows.iter().filter(|r| r.tab == "IMC" && ["VREF", "PHY Control", "Features"].contains(&r.category.as_str())) {
            assert_eq!(r.column, "Left", "{}", r.name);
        }
    });
}

#[test]
fn ddr5_each_imc_section_is_drawn_once() {
    on_fresh_thread(|| assert_each_once(&sections(&stub_table(DDR5), "IMC")));
}

#[test]
fn ddr5_multi_cycle_command_reads_as_a_switch() {
    on_fresh_thread(|| {
        let rows = stub_table(DDR5);
        match &row(&rows, "Multi-Cycle Command").formula {
            Some(crate::row::Formula::Map(map)) => {
                let pairs: Vec<(u64, &str)> = map.iter().map(|(k, v)| (*k, v.as_str())).collect();
                assert_eq!(pairs, [(0, "Disabled"), (1, "Enabled")]);
            }
            other => panic!("no switch table: {other:?}"),
        }
    });
}

#[test]
fn ddr4_controller_rows_stay_single_on_imc() {
    on_fresh_thread(|| {
        let rows = stub_table(DDR4);
        for (name, r) in rows_named(&rows, &PER_CHANNEL_IMC_ROWS) {
            assert_eq!(r.tab, "IMC", "{name}");
            assert!(!r.is_dual(), "{name}");
        }
    });
}

#[test]
fn ddr4_phy_control_keeps_the_second_imc_column() {
    on_fresh_thread(|| {
        let rows = stub_table(DDR4);
        let phy: Vec<&Row> = rows.iter().filter(|r| r.tab == "IMC" && r.category == "PHY Control").collect();
        assert!(!phy.is_empty());
        assert!(phy.iter().all(|r| r.column == "Right"));
    });
}

#[test]
fn ddr4_add_one_qclk_and_multi_cycle_are_as_they_were() {
    on_fresh_thread(|| {
        let rows = stub_table(DDR4);
        assert_eq!(field_of(row(&rows, "Add 1 QCLK Delay").read).address, MCHBAR + 0xE278);
        assert!(row(&rows, "Multi-Cycle Command").formula.is_none());
    });
}

#[test]
fn ddr4_training_keeps_two_columns() {
    on_fresh_thread(|| {
        let rows = stub_table(DDR4);
        let mut columns: Vec<&str> = rows.iter().filter(|r| r.tab == "Training").map(|r| r.column.as_str()).collect();
        columns.sort();
        columns.dedup();
        assert_eq!(columns, ["Left", "Right"]);
    });
}

// --- tests/test_skew_live.py
//
// "Live" there means the rows read hardware on each refresh rather than at
// build -- the file runs against the stub like the others, so these are
// ordinary tests here too.

const SKEW_TAB: &str = "Training";

/// All rows historically covered here, now split by source scope.
fn skew_rows(rows: &[Row]) -> Vec<&Row> {
    rows.iter().filter(|row| row.tab == SKEW_TAB || row.tab == "IMC").collect()
}

fn per_module_skew_rows(rows: &[Row]) -> Vec<&Row> {
    rows.iter().filter(|row| row.tab == SKEW_TAB).collect()
}

/// Concrete readings stored on a row, which is the defect being guarded.
///
/// A row is fine if it carries an address, a dynamic lookup or a getter --
/// all of those reach hardware when the tab is drawn. It is broken if it
/// carries the reading itself, because that was resolved once while the
/// table was built and cannot change afterwards.
fn frozen_values(row: &Row) -> Vec<String> {
    let mut stored = Vec::new();
    let mut check = |key: &str, value: &Option<Value>| {
        if let Some(Value::Text(text)) = value
            && !text.is_empty()
        {
            stored.push(format!("{key}={text}"));
        }
    };
    check("value", &row.value);
    if let Some(a) = &row.a {
        check("value_a", &a.value);
    }
    if let Some(b) = &row.b {
        check("value_b", &b.value);
    }
    stored
}

/// Whether drawing this row reaches a register or a live getter.
fn has_live_source(row: &Row) -> bool {
    let lazy = |value: &Option<Value>| value.as_ref().is_some_and(Value::is_lazy);
    lazy(&row.value)
        || row.a.as_ref().is_some_and(|s| lazy(&s.value) || s.read.is_some())
        || row.b.as_ref().is_some_and(|s| lazy(&s.value) || s.read.is_some())
        || row.read.is_some()
}

fn by_category(rows: &[&Row]) -> Vec<(String, Vec<String>)> {
    let mut grouped: Vec<(String, Vec<String>)> = Vec::new();
    for row in rows {
        match grouped.iter_mut().find(|(c, _)| *c == row.category) {
            Some((_, names)) => names.push(row.name.clone()),
            None => grouped.push((row.category.clone(), vec![row.name.clone()])),
        }
    }
    grouped
}

#[test]
fn ddr4_training_and_imc_have_no_other_memory_generation_rows() {
    on_fresh_thread(|| {
        let rows = stub_table(DDR4);
        for r in skew_rows(&rows) {
            assert!(!["tDLLK", "tCCD_L_WR", "tCCD_L_WR2", "Allow 2cyc B2B LPDDR"].contains(&r.name.as_str()));
        }
    });
}

#[test]
fn rtt_ron_and_vref_labels_match_reference() {
    on_fresh_thread(|| {
        let rows = stub_table(DDR4);
        let skew = skew_rows(&rows);
        let groups = by_category(&skew);
        let names_in = |category: &str| groups.iter().find(|(c, _)| c == category).map(|(_, n)| n.clone()).unwrap_or_default();
        let common: [(&str, &[&str]); 3] = [
            ("RTT", &["RTT Wr", "RTT Park"]),
            ("RON", &["RON"]),
            (
                "VREF",
                &[
                    "Dq Vref Up", "Dq Vref Dn", "Dq Odt Vref Up", "Dq Odt Vref Dn", "Cmd Vref Up", "Cmd Vref Dn",
                    "Ctl Vref Up", "Ctl Vref Dn", "Clk Vref Up", "Clk Vref Dn", "CkeCs Vref Up", "RX VREF", "QX Count",
                ],
            ),
        ];
        for (category, expected) in common {
            let present = names_in(category);
            assert!(!present.is_empty(), "platform has no {category} block");
            for name in expected {
                assert!(present.iter().any(|n| n == name), "{category} lacks {name}");
            }
        }
        // These rows only exist in the DDR5 profile; DDR4 intentionally has
        // its own combined RTT NOM and DQ VREF rows.
        let rtt = names_in("RTT");
        let ddr5_rtt = ["RTT Nom Wr", "RTT Nom Rd", "RTT Park Dqs", "RTT Loopback"];
        if ddr5_rtt.iter().any(|n| rtt.iter().any(|r| r == n)) {
            assert!(ddr5_rtt.iter().all(|n| rtt.iter().any(|r| r == n)));
        }
    });
}

const DDR5_VREF_NAMES: [&str; 6] = ["DQ VREF 0", "DQ VREF 1", "DQ VREF 2", "DQ VREF 3", "CA VREF", "CS VREF"];

#[test]
fn ddr5_mode_register_vrefs_have_independent_a1_b1_sources() {
    on_fresh_thread(|| {
        let rows = stub_table(DDR5);
        let vrefs: Vec<&Row> = rows.iter().filter(|r| DDR5_VREF_NAMES.contains(&r.name.as_str())).collect();
        let mut found: Vec<&str> = vrefs.iter().map(|r| r.name.as_str()).collect();
        found.sort();
        let mut expected = DDR5_VREF_NAMES.to_vec();
        expected.sort();
        assert_eq!(found, expected);
        for r in vrefs {
            assert_eq!(r.tab, SKEW_TAB, "{}", r.name);
            assert_eq!(r.column, "Middle", "{}", r.name);
            assert!(r.is_dual(), "{}", r.name);
            assert_eq!(dynamic_of(side(r, Channel::A).read).mchbar, MCHBAR, "{}", r.name);
            assert_eq!(dynamic_of(side(r, Channel::B).read).mchbar, channel_b(DDR5), "{}", r.name);
        }
    });
}

#[test]
fn ddr5_odtl_has_independent_a1_b1_sources() {
    on_fresh_thread(|| {
        let rows = stub_table(DDR5);
        let odtl: Vec<&Row> = rows.iter().filter(|r| r.category == "ODTL").collect();
        assert_eq!(odtl.len(), 6);
        for r in odtl {
            assert_eq!(r.tab, SKEW_TAB, "{}", r.name);
            assert_eq!(r.column, "Left", "{}", r.name);
            assert!(r.is_dual(), "{}", r.name);
            assert_eq!(dynamic_of(side(r, Channel::A).read).mchbar, MCHBAR, "{}", r.name);
            assert_eq!(dynamic_of(side(r, Channel::B).read).mchbar, channel_b(DDR5), "{}", r.name);
        }
    });
}

const SHARED_VREF_NAMES: [&str; 13] = [
    "Dq Vref Up", "Dq Vref Dn", "Dq Odt Vref Up", "Dq Odt Vref Dn", "Cmd Vref Up", "Cmd Vref Dn", "Ctl Vref Up",
    "Ctl Vref Dn", "Clk Vref Up", "Clk Vref Dn", "CkeCs Vref Up", "RX VREF", "QX Count",
];

#[test]
fn fixed_phy_groups_remain_shared_controller_rows() {
    on_fresh_thread(|| {
        let rows = stub_table(DDR5);
        let fixed: Vec<&Row> =
            rows.iter().filter(|r| ["DATA", "CMD", "CLK", "CTL", "SComp"].contains(&r.category.as_str())).collect();
        assert!(!fixed.is_empty());
        for r in fixed {
            assert_eq!(r.tab, "IMC", "{}", r.name);
            assert!(!r.is_dual(), "{}", r.name);
        }
        let shared: Vec<&Row> = rows.iter().filter(|r| SHARED_VREF_NAMES.contains(&r.name.as_str())).collect();
        let mut found: Vec<&str> = shared.iter().map(|r| r.name.as_str()).collect();
        found.sort();
        let mut expected = SHARED_VREF_NAMES.to_vec();
        expected.sort();
        assert_eq!(found, expected);
        for r in shared {
            assert_eq!(r.tab, "IMC", "{}", r.name);
            assert!(!r.is_dual(), "{}", r.name);
        }
    });
}

#[test]
fn the_skew_tab_has_rows_to_check() {
    // Guards every other test here from passing vacuously.
    on_fresh_thread(|| assert!(!skew_rows(&stub_table(DDR4)).is_empty()));
}

#[test]
fn skew_contains_only_independent_a1_b1_rows() {
    on_fresh_thread(|| {
        let rows = stub_table(DDR4);
        let skew = per_module_skew_rows(&rows);
        assert!(!skew.is_empty());
        for r in skew {
            assert!(r.is_dual(), "{}", r.name);
        }
    });
}

#[test]
fn every_training_pair_uses_distinct_hardware_sources() {
    on_fresh_thread(|| {
        let rows = stub_table(DDR4);
        for r in per_module_skew_rows(&rows) {
            if r.name.trim().is_empty() {
                continue;
            }
            match (side(r, Channel::A).read, side(r, Channel::B).read) {
                (Some(ReadSpec::Field(a)), Some(ReadSpec::Field(b))) if side(r, Channel::A).value.is_none() => {
                    assert_ne!(a.address, b.address, "{}", r.name);
                    assert_eq!(b.address - a.address, channel_b_offset(DDR4), "{}", r.name);
                }
                (Some(ReadSpec::Dynamic(a)), Some(ReadSpec::Dynamic(b))) if side(r, Channel::A).value.is_none() => {
                    assert_ne!(a.mchbar, b.mchbar, "{}", r.name);
                }
                _ => {
                    assert!(side(r, Channel::A).value.as_ref().is_some_and(Value::is_lazy), "{}", r.name);
                    assert!(side(r, Channel::B).value.as_ref().is_some_and(Value::is_lazy), "{}", r.name);
                }
            }
        }
    });
}

#[test]
fn cwl_adjustments_are_no_longer_shared_imc_rows() {
    on_fresh_thread(|| {
        let rows = stub_table(DDR4);
        for name in ["Add tCWL", "Dec tCWL"] {
            let r = row(&rows, name);
            assert_eq!(r.tab, SKEW_TAB, "{name}");
            assert!(r.is_dual(), "{name}");
        }
    });
}

#[test]
fn single_source_rows_moved_to_phy() {
    on_fresh_thread(|| {
        let rows = stub_table(DDR4);
        let imc: Vec<&Row> = rows.iter().filter(|r| r.tab == "IMC").collect();
        assert!(!imc.is_empty());
        for r in imc {
            assert!(!r.is_dual(), "{}", r.name);
            assert_eq!(r.extra_str("source_scope"), Some("controller"), "{}", r.name);
        }
    });
}

#[test]
fn no_skew_row_holds_a_baked_in_reading() {
    on_fresh_thread(|| {
        let rows = stub_table(DDR4);
        for r in skew_rows(&rows) {
            assert!(frozen_values(r).is_empty(), "{} stores a reading taken while building the table", r.name);
        }
    });
}

#[test]
fn ddr4_display_names_are_human_readable() {
    on_fresh_thread(|| {
        let rows = stub_table(DDR4);
        for r in skew_rows(&rows) {
            assert!(!r.name.contains('_'), "{}", r.name);
        }
    });
}

#[test]
fn every_supported_intel_profile_is_entirely_live() {
    // A platform-specific installer must not escape the default-profile
    // test above. Unsupported DDR4 CA/CS VREF rows are omitted; DDR5 and
    // Arrow Lake supply their mode-register readers.
    for platform in [DDR4, DDR5, Platform::Lga1851] {
        on_fresh_thread(move || {
            let rows = stub_table(platform);
            let training: Vec<&Row> = rows.iter().filter(|r| r.tab == SKEW_TAB).collect();
            assert!(!training.is_empty(), "{platform:?}");
            for r in training {
                assert!(frozen_values(r).is_empty(), "{platform:?} {}", r.name);
                assert!(has_live_source(r), "{platform:?} {} has no hardware reader", r.name);
            }
        });
    }
}

#[test]
fn the_clk_pulldown_matches_the_reference_tools() {
    // Bits 16-21, which is what both reference maps specify and what they
    // display. 0x031204D9 is the bench's 0x2CE4: bit 6 holds 19, bits 16-21
    // hold 18, and bits 20-25 hold 49. Each window gives its own answer, so
    // the assertion cannot pass by coincidence.
    pin_generation(34);
    ConstHardware::install(Some(0x031204D9));
    assert_eq!(registers::read_ddr4_clk_slew_field("dn"), "18");
    // The three fields it shares with CTL are unchanged.
    assert_eq!(registers::read_ddr4_clk_slew_field("up"), "25");
    assert_eq!(registers::read_ddr4_clk_slew_field("scomp"), "32");
    assert_eq!(registers::read_ddr4_clk_slew_field("vsshiff"), "49");
}

#[test]
fn the_compensation_panel_matches_the_reference_dump() {
    // Pin all five groups, including DATA's non-global register.
    on_fresh_thread(|| {
        let rows = stub_table(DDR4);
        pin_generation(34);
        FakeHardware::new()
            // DATA0CH0_CR_DDRCRDATACOMP0
            .with_dword(MCHBAR + 0x01B4, 52 | (42 << 6) | (17 << 12) | (14 << 18) | (48 << 24))
            // DDRPHY_COMP_CR_DDRCRCMDCOMP
            .with_dword(MCHBAR + 0x2CDC, 20 | (16 << 6) | (121 << 12) | (48 << 20))
            // The upper dword of CLK's 0x2CE0 block is CTL's 0x2CE4 block.
            .with_dword(MCHBAR + 0x2CE4, 63 | (54 << 6) | (32 << 12) | (63 << 20))
            // Common compensation diagnostics.
            .with_dword(MCHBAR + 0x2C24, 1 | (4 << 12))
            .install();
        let expected = [
            ("Data Drv Up", "52"), ("Data Drv Dn", "42"), ("Data ODT Up", "17"), ("Data ODT Dn", "14"),
            ("Data VssHiFFdq", "48"), ("CMD Drv Up", "20"), ("CMD Drv Dn", "16"), ("CMD SComp", "121"),
            ("CMD VssHiFF", "48"), ("CLK Drv Up", "63"), ("CLK Drv Dn", "50"), ("CLK SComp", "32"),
            ("CLK VssHiFF", "63"), ("CTL Drv Up", "63"), ("CTL Drv Dn", "54"), ("CTL SComp", "32"),
            ("CTL VssHiFF", "63"), ("CTL CkeCsUp", "0"), ("CMD SlewStatlegen", "1"), ("SComp Code Live", "0"),
            ("SComp Common Bonus", "4"),
        ];
        for (name, value) in expected {
            assert_eq!(resolve(&row(&rows, name).value), value, "{name}");
        }
    });
}

#[test]
fn the_compensation_rows_are_getters() {
    on_fresh_thread(|| {
        let rows = stub_table(DDR4);
        let comp: Vec<&Row> = skew_rows(&rows).into_iter().filter(|r| SLEW_RATE_GROUPS.contains(&r.category.as_str())).collect();
        assert!(!comp.is_empty());
        for r in comp {
            assert!(r.value.as_ref().is_some_and(Value::is_lazy), "{}", r.name);
        }
    });
}

#[test]
fn each_compensation_row_reads_its_own_field() {
    // These getters are built in a loop. Without per-row binding they
    // would all close over the last field and the block would show one
    // number repeated. Checked by recording what each getter asks for.
    on_fresh_thread(|| {
        let rows = stub_table(DDR4);
        let hardware = ConstHardware::install(Some(0x12345678));
        let mut requests: Vec<(String, String, Vec<u64>)> = Vec::new();
        for r in skew_rows(&rows).into_iter().filter(|r| SLEW_RATE_GROUPS.contains(&r.category.as_str())) {
            hardware.clear();
            let reading = resolve(&r.value);
            requests.push((r.name.clone(), reading, hardware.asked()));
        }
        assert!(!requests.is_empty());
        for (name, _, addresses) in &requests {
            assert!(!addresses.is_empty(), "{name} read no register");
        }
        let mut distinct: Vec<(String, Vec<u64>)> = requests.iter().map(|(_, r, a)| (r.clone(), a.clone())).collect();
        distinct.sort();
        distinct.dedup();
        assert!(distinct.len() > 1);
    });
}

#[test]
fn a_compensation_getter_survives_an_unreadable_register() {
    // Refreshing must not raise: the tab is redrawn on a timer.
    on_fresh_thread(|| {
        let rows = stub_table(DDR4);
        for reading in [None, Some(0xFFFF_FFFF)] {
            ConstHardware::install(reading);
            for r in skew_rows(&rows).into_iter().filter(|r| SLEW_RATE_GROUPS.contains(&r.category.as_str())) {
                assert_eq!(resolve(&r.value), "N/A", "{} {reading:?}", r.name);
            }
        }
    });
}

#[test]
fn the_slew_group_prefixes_survive_the_reference_spelling() {
    // Their name is "Data Drv Up" against a heading of DATA.
    assert_eq!(registers::slew_rate_category("Data Drv Up"), "DATA");
    assert_eq!(registers::slew_rate_category("CMD Drv Dn"), "CMD");
}

#[test]
fn each_slew_group_kept_its_rows() {
    on_fresh_thread(|| {
        let rows = stub_table(DDR4);
        let skew = skew_rows(&rows);
        let count = |group: &str| skew.iter().filter(|r| r.category == group).count();
        assert_eq!(count("DATA"), 5);
        assert_eq!(count("CMD"), 4);
        assert_eq!(count("CLK"), 4);
        assert_eq!(count("CTL"), 5);
        assert_eq!(count("SComp"), 3);
    });
}

#[test]
fn no_skew_row_kept_the_old_spelling() {
    on_fresh_thread(|| {
        let rows = stub_table(DDR4);
        let skew = skew_rows(&rows);
        for stale in [
            "DATA RComp Drv Up", "CMD RComp Drv Dn", "CLK RComp Drv Up", "CTL RComp Drv Dn", "SComp comp bonus",
            "DQ VREFUP", "CMD VREFDN",
        ] {
            assert!(!skew.iter().any(|r| r.name == stale), "{stale}");
        }
    });
}

#[test]
fn combined_skew_and_phy_categories_use_their_columns() {
    on_fresh_thread(|| {
        let rows = stub_table(DDR4);
        let skew = skew_rows(&rows);
        let expected = [
            ("RTT", "Left"), ("ODT", "Left"), ("RON", "Left"), ("ODT DELAY", "Left"), ("DFE", "Left"),
            ("PHY Control", "Right"), ("DATA", "Right"), ("CMD", "Right"), ("CLK", "Right"), ("CTL", "Right"),
            ("SComp", "Right"),
        ];
        for (category, column) in expected {
            for r in skew.iter().filter(|r| r.category == category) {
                assert_eq!(r.column, column, "{category} {}", r.name);
            }
        }
        // The additional refresh rows balance the final two-column IMC layout
        // by placing ODTL on the right while VREF stays on the left.
        for r in skew.iter().filter(|r| r.category == "VREF" || r.category == "ODTL") {
            let column = if r.category == "VREF" { "Left" } else { "Right" };
            assert_eq!(r.column, column, "{}", r.name);
        }
    });
}

#[test]
fn phy_control_uses_the_second_imc_column() {
    on_fresh_thread(|| {
        let rows = stub_table(DDR4);
        let phy: Vec<&Row> = skew_rows(&rows).into_iter().filter(|r| r.category == "PHY Control").collect();
        assert!(!phy.is_empty());
        assert!(phy.iter().all(|r| r.column == "Right"));
    });
}

#[test]
fn the_cke_cs_up_level_is_present() {
    on_fresh_thread(|| {
        let rows = stub_table(DDR4);
        assert!(skew_rows(&rows).iter().any(|r| r.name == "CkeCs Vref Up"));
    });
}

#[test]
fn the_cke_cs_down_level_is_not_invented() {
    on_fresh_thread(|| {
        let rows = stub_table(DDR4);
        for name in ["WrDSCke CS Dn", "CKE CS VREFDN", "CkeCsVrefDn"] {
            assert!(named(&rows, name).is_empty(), "{name}");
        }
    });
}

#[test]
fn every_other_drive_level_still_pairs() {
    on_fresh_thread(|| {
        let rows = stub_table(DDR4);
        let skew = skew_rows(&rows);
        for (up, down) in [
            ("Dq Vref Up", "Dq Vref Dn"),
            ("Dq Odt Vref Up", "Dq Odt Vref Dn"),
            ("Cmd Vref Up", "Cmd Vref Dn"),
            ("Ctl Vref Up", "Ctl Vref Dn"),
            ("Clk Vref Up", "Clk Vref Dn"),
        ] {
            assert!(skew.iter().any(|r| r.name == up), "{up}");
            assert!(skew.iter().any(|r| r.name == down), "{down}");
        }
    });
}

#[test]
fn a_positive_odtl_offset_is_signed() {
    assert_eq!(clock_offset(2), "+2 Clocks");
    assert_eq!(clock_offset(1), "+1 Clock");
}

#[test]
fn a_negative_odtl_offset_keeps_its_sign_and_its_singular() {
    assert_eq!(clock_offset(-2), "-2 Clocks");
    assert_eq!(clock_offset(-1), "-1 Clock");
}

#[test]
fn a_zero_odtl_offset_is_not_signed() {
    // "+0 Clocks" is what "%+d" gives and it reads as a setting rather
    // than as no offset at all.
    assert_eq!(clock_offset(0), "0 Clocks");
}

#[test]
fn every_odtl_table_signs_its_positives() {
    let signed = regex::Regex::new(r"^[+-]\d+ Clocks?$").unwrap();
    for table in [
        formulas::odtl_on_wr(),
        formulas::odtl_off_wr(),
        formulas::odtl_on_wr_nt(),
        formulas::odtl_off_wr_nt(),
        formulas::odtl_on_rd_nt(),
        formulas::odtl_off_rd_nt(),
    ] {
        assert_eq!(table.keys().copied().collect::<Vec<u64>>(), (0..8).collect::<Vec<u64>>());
        for label in table.values() {
            if label == "RFU" || label.starts_with("0 ") {
                continue;
            }
            assert!(signed.is_match(label), "{label}");
        }
    }
}

#[test]
fn ddr4_the_per_device_vref_rows_collapse_to_one() {
    on_fresh_thread(|| {
        let rows = stub_table(DDR4);
        let skew = skew_rows(&rows);
        assert!(skew.iter().any(|r| r.name == "DQ VREF"));
        for device in 0..4 {
            assert!(!skew.iter().any(|r| r.name == format!("DQ VREF D{device}")));
        }
    });
}

#[test]
fn ddr4_the_vref_row_reads_the_mr6_decoder() {
    on_fresh_thread(|| {
        let rows = stub_table(DDR4);
        let dq = skew_rows(&rows).into_iter().find(|r| r.name == "DQ VREF").unwrap();
        // Read through the row and through the decoder, against the same
        // register: they are the same reading.
        ConstHardware::install(Some(0x101C));
        let direct = decode_ddr4_dq_vref();
        assert_eq!(direct, "78.20%");
        assert!(dq.value.as_ref().is_some_and(Value::is_lazy));
        assert_eq!(resolve(&dq.value), direct);
    });
}

#[test]
fn ddr4_the_vref_row_is_not_left_without_a_source() {
    // The regression this covers: the DDR5 work split "DQ VREF" into
    // D0-D3, so the DDR4 branch that matched the old name stopped firing.
    on_fresh_thread(|| {
        let rows = stub_table(DDR4);
        let dq = skew_rows(&rows).into_iter().find(|r| r.name == "DQ VREF").unwrap();
        assert!(dq.value.is_some() || dq.read.is_some());
    });
}

#[test]
fn ddr4_the_command_bus_references_come_off() {
    // DDR5 trains VREFCA and VREFCS in MR11 and MR12. DDR4 defines
    // neither: the command bus references an external half-VDD supply and
    // CS shares it, so there is nothing on the module holding a value.
    on_fresh_thread(|| {
        let rows = stub_table(DDR4);
        let skew = skew_rows(&rows);
        for name in DDR4_ABSENT_VREF_ROWS {
            assert!(!skew.iter().any(|r| r.name == name), "{name}");
        }
    });
}

#[test]
fn ddr4_the_dq_row_is_what_is_left_of_the_vref_section() {
    on_fresh_thread(|| {
        let rows = stub_table(DDR4);
        assert!(skew_rows(&rows).iter().any(|r| r.category == "VREF" && r.name == "DQ VREF"));
    });
}

#[test]
fn ddr4_the_range_selector_is_not_read_off_the_ccd_bits() {
    // Bit 12 was read as the range selector, which put this row at 63.20%
    // instead of 78.20%. It is the top of tCCD_L.
    ConstHardware::install(Some(0x101C));
    // 60.0% + 28 * 0.65%, the Range 1 reading.
    assert_eq!(decode_ddr4_dq_vref(), "78.20%");
}

#[test]
fn ddr4_the_range_bit_still_selects_the_low_range() {
    ConstHardware::install(Some(0x109C));
    // Bit 7 set: 45.0% + 28 * 0.65%.
    assert_eq!(decode_ddr4_dq_vref(), "63.20%");
}

#[test]
fn ddr4_no_dfe_rows_are_installed() {
    on_fresh_thread(|| {
        let rows = stub_table(DDR4);
        assert!(!skew_rows(&rows).iter().any(|r| r.category.starts_with("DFE")));
    });
}

#[test]
fn ddr4_the_dfe_gain_does_not_survive_on_its_own() {
    // It moved off the Jedec tab with the taps. Carrying it across where
    // the taps are not installed would leave one DFE row on a generation
    // that has none.
    on_fresh_thread(|| {
        let rows = stub_table(DDR4);
        for name in ["DFE Gain", "DFE Gain Enable"] {
            assert!(named(&rows, name).is_empty(), "{name}");
        }
    });
}

// --- tests/test_gear_witness_live.py
//
// Every other Intel test runs against the stub. This asks the running
// machine whether its two gear witnesses agree, and is ignored by default
// because it needs the driver and an Intel DDR5 board.

/// The real driver for the calling test thread. WMI stays unavailable
/// under test, so the CPU-name half of the Arrow Lake check reads "Error":
/// run these on an LGA1700 DDR5 bench, where that is the right answer.
fn real_hardware() {
    let hardware = crate::hardware::inpout::InpOut::load().expect("the driver loads -- run elevated beside it");
    set_thread_hardware(Some(Arc::new(hardware)));
}

#[test]
#[ignore = "needs hardware"]
fn the_scheduler_agrees_with_the_gear_row() {
    // 0x5E04 bits 12-13 against SC_GS_CFG bits 15 and 31. If a future map
    // moves the field the row reads, this is what notices: a duplicate
    // row would print the same wrong answer twice.
    real_hardware();
    let scheduler = scheduler_gear_mode(None);
    let Some(scheduler) = scheduler else {
        let raw = crate::hardware::read::read_physical_memory_int(MCHBAR + SCHEDULER_CONFIG_OFFSET, 4);
        panic!("SC_GS_CFG {raw:?} does not carry the gear flags at bits {SCHEDULER_GEAR2_BIT}/{SCHEDULER_GEAR4_BIT}");
    };
    assert_eq!(get_gear_mode_value(), scheduler.to_string());
}

#[test]
#[ignore = "needs hardware"]
fn the_scheduler_register_is_actually_claimed() {
    // 0xFFFFFFFF is an unclaimed window, and scheduler_gear_mode reports
    // None for it. Distinguish "no answer because no hardware" from "no
    // answer because both flags read the same".
    real_hardware();
    let raw = crate::hardware::read::read_physical_memory_int(MCHBAR + SCHEDULER_CONFIG_OFFSET, 4)
        .expect("the register reads");
    assert_ne!(raw, 0xFFFF_FFFF);
    if is_arrow_lake_platform() {
        // One flag here, not two, so "both read the same" is the normal
        // state rather than a fault.
        assert!(matches!(scheduler_gear_mode(None), Some(2) | Some(4)));
        return;
    }
    let gear2 = (raw >> SCHEDULER_GEAR2_BIT) & 1;
    let gear4 = (raw >> SCHEDULER_GEAR4_BIT) & 1;
    assert_ne!(gear2, gear4, "SC_GS_CFG 0x{raw:08X} sets both gear flags or neither");
}
