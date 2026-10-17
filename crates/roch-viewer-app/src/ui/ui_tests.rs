//! Tests that need a laid-out tab or a platform's real table.
//!
//! The Python suite checked these against a live Tk window
//! (`test_unscrolled_fit_live.py`) or against the Intel table built under
//! `tests/intel_stub.py`. Here a tab is laid out in a headless egui pass, and
//! the table comes from `intel_timings::build_timings` behind a stub driver
//! and a stub WMI, as the Python stub did. The cases that need the ported
//! Intel table are ignored until it lands; their assertions are written in
//! full so they can be switched on after the merge.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use eframe::egui::{self, Rect, Ui, pos2, vec2};

use rochviewer::hardware::{self, Hardware, HwResult};
use rochviewer::platform_profiles::Platform;
use rochviewer::row::{Row, Side, Value};
use rochviewer::timings::Backend;
use rochviewer::wmi_util::{FakeWmi, WmiObject};

use super::app::{Chrome, tab_names_for, window_size_for_tab};
use super::layout::*;
use super::reader::{Values, read_row};
use super::summary;
use super::tabs::{TabContext, detail_geometry, detail_layout};
use super::theme::{self, Palette};

/// Run `f` inside one headless egui pass with the window's fonts.
pub fn with_ui<R>(f: impl FnOnce(&mut Ui) -> R) -> R {
    let ctx = egui::Context::default();
    theme::install_fonts(&ctx, super::prefs::DEFAULT_FONT);
    let input = || egui::RawInput {
        screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1200.0, 1200.0))),
        ..Default::default()
    };
    // Fonts take effect from the next pass.
    ctx.run_ui(input(), |_| {}).textures_delta.clear();
    let mut f = Some(f);
    let mut result = None;
    ctx.run_ui(input(), |ui| {
        if let Some(f) = f.take() {
            result = Some(f(ui));
        }
    })
    .textures_delta
    .clear();
    result.expect("the pass ran")
}

/// Every row read once, on this thread.
pub fn values_for(backend: &Backend) -> Values {
    let values = Values::default();
    for (index, row) in backend.rows.iter().enumerate() {
        values.set(index, read_row(backend, row, None));
    }
    values
}

fn backend(platform: Platform, rows: Vec<Row>) -> Backend {
    Backend { platform, rows, unavailable: "N/A" }
}

fn dual(name: &str, category: &str, tab: &str, column: &str, a: &str, b: &str) -> Row {
    Row::new(name, category, tab, column).with_sides(Side::value(Value::text(a)), Side::value(Value::text(b)))
}

fn area() -> Rect {
    Rect::from_min_max(pos2(14.0, 79.0), pos2(686.0, 676.0))
}

// --- test_unscrolled_fit_live.py, against a headless layout

#[test]
fn each_column_names_its_module_once() {
    // The heading of the first per-channel section says "All modules" (or
    // the module picked); the headings under it leave that cell blank.
    let rows = vec![
        dual("tCL", "Primary", "Timings", "Left", "38", "38"),
        dual("tWR", "Secondary", "Timings", "Left", "48", "48"),
        dual("tCSH", "Command", "Timings", "Left", "52", "52"),
        dual("tREFI", "Refresh timings", "Timings", "Middle", "24575", "24575"),
    ];
    let backend = backend(Platform::Lga1700Ddr5, rows);
    let values = values_for(&backend);
    let palette = Palette::new(true);
    let labels = BTreeMap::new();
    let ctx = TabContext { backend: &backend, values: &values, palette: &palette, selected: None, channel_labels: &labels };
    with_ui(|ui| {
        let geometry = detail_geometry(ui, &ctx, "Timings", area());
        for column in &geometry.columns {
            assert_eq!(column.channel_labels(), vec!["All modules"]);
        }
    });
}

fn imc_rows() -> Vec<Row> {
    vec![
        Row::new("Receive Enable PI", "PHY Control", "IMC", "Left").with_value("1219"),
        Row::new("Realtime Memory Timing", "Features", "IMC", "Left").with_value("Disabled"),
        Row::new("CMD Slew Static Leg", "SComp", "IMC", "Right").with_value("Enabled"),
    ]
}

#[test]
fn imc_columns_keep_the_25px_gutter() {
    // Like every other tab: the leading column is as wide as its content,
    // and the next column's text starts 25px after it -- the two panels'
    // insets and the spacing between them.
    let backend = backend(Platform::Lga1700Ddr5, imc_rows());
    let values = values_for(&backend);
    let palette = Palette::new(true);
    let labels = BTreeMap::new();
    let ctx = TabContext { backend: &backend, values: &values, palette: &palette, selected: None, channel_labels: &labels };
    with_ui(|ui| {
        let geometry = detail_geometry(ui, &ctx, "IMC", area());
        let (leading, following) = (&geometry.columns[0], &geometry.columns[1]);
        assert_eq!(following.left - (leading.left + leading.frame_width), 25.0);
        // "Realtime Memory Timing" + 25 + "Disabled": 22 and 8 characters of
        // 7px, the width the Tk window gave a 12px Consolas character.
        assert_eq!(leading.value_edge - leading.left, (22.0 + 8.0) * 7.0 + 25.0);
    });
}

#[test]
fn each_column_sits_inside_its_own_panel() {
    let backend = backend(Platform::Lga1700Ddr5, imc_rows());
    let values = values_for(&backend);
    let palette = Palette::new(false);
    let labels = BTreeMap::new();
    let ctx = TabContext { backend: &backend, values: &values, palette: &palette, selected: None, channel_labels: &labels };
    with_ui(|ui| {
        let area = area();
        let geometry = detail_geometry(ui, &ctx, "IMC", area);
        for column in &geometry.columns {
            assert_eq!(column.left - column.panel.left(), 6.0);
            assert!(column.panel.bottom() <= area.bottom(), "the panel's bottom border is cut off");
            assert!(column.panel.right() <= area.right());
        }
    });
}

#[test]
fn a_narrower_selection_keeps_the_columns_it_was_built_with() {
    // The Tk grid sized its columns in the all-modules view; picking one
    // module never pulled them in.
    let rows = vec![dual("tRFCns", "Refresh timings", "Timings", "Left", "128 ns", "104 ns")];
    let backend = backend(Platform::Lga1700Ddr5, rows);
    let values = values_for(&backend);
    let palette = Palette::new(true);
    let labels = BTreeMap::new();
    let all = TabContext { backend: &backend, values: &values, palette: &palette, selected: None, channel_labels: &labels };
    let one = TabContext { selected: Some(rochviewer::row::Channel::B), ..all };
    with_ui(|ui| {
        let wide = detail_geometry(ui, &all, "Timings", area());
        let narrow = detail_geometry(ui, &one, "Timings", area());
        assert_eq!(wide.columns[0].value_edge, narrow.columns[0].value_edge);
        assert_eq!(narrow.columns[0].channel_labels(), vec!["B"]);
    });
}

#[test]
fn utility_buttons_align_with_the_tab_strip() {
    // The tools are placed 10px into the tab view and the tabs 11px, each
    // on the same strip and clear of one another.
    let chrome = Chrome::new(Rect::from_min_size(pos2(0.0, 0.0), vec2(700.0, 750.0)), false);
    assert_eq!(chrome.tabview.top() + 10.0, 44.0);
    assert_eq!(chrome.tabview.top() + 11.0, 45.0);
}

// --- test_window_fit.py (the parts that are not Tk source inspection)

#[test]
fn intel_timings_sections_balance_across_three_columns() {
    let names = ["Primary", "Secondary", "Command", "Refresh timings", "Tertiary", "CAS to CAS", "Power down", "Other Timings"];
    let layout = timings_three_column_layout(names);
    let placed: Vec<&str> = names.iter().map(|n| layout.iter().find(|(c, _)| c == n).unwrap().1).collect();
    assert_eq!(placed, vec!["Left", "Left", "Left", "Middle", "Middle", "Right", "Right", "Right"]);
}

#[test]
fn am5_timings_sections_use_all_three_columns() {
    let layout = timings_three_column_layout([
        "Primary", "Secondary", "Refresh timings", "CAS to CAS", "Power down", "Stagger", "Mode register", "Turnaround",
        "Read to read", "Write to write", "PHY", "Preamble / postamble",
    ]);
    let column = |name: &str| layout.iter().find(|(c, _)| *c == name).unwrap().1;
    assert_eq!(column("Primary"), "Left");
    assert_eq!(column("Power down"), "Middle");
    assert_eq!(column("Refresh timings"), "Right");
    // PHY closes the middle column, which leaves the middle and right
    // columns twenty rows each.
    assert_eq!(column("PHY"), "Middle");
    assert_eq!(
        SKEW_SECTION_ORDER,
        &[
            "RTT", "ODT", "RON", "ODT DELAY", "VREF", "DLL / LATENCY", "DATA CONTROL", "DFE", "ODTL", "Command",
            "MPR / ACCESS", "PARITY / CRC", "REFRESH / POWER", "PREAMBLE / PPR", "MR0 / MR1", "MR2 / MR3", "MR4",
            "MR5 / MR6", "Mode Registers", "DQS", "Preamble", "ECS", "Features", "Power Down", "Refresh",
        ]
    );
}

#[test]
fn imc_uses_the_balanced_section_order() {
    assert_eq!(
        IMC_SECTION_ORDER,
        &[
            "VREF", "Command", "Refresh", "Power Down", "DATA", "CMD", "CLK", "CTL", "SComp", "ODTL", "PHY Control",
            "Features", "MR0 / MR1", "MR2 / MR3", "MR4", "MR5 / MR6",
        ]
    );
}

#[test]
fn only_tabs_that_fit_are_drawn_without_scrollbars() {
    assert_eq!(
        super::app::UNSCROLLED_TABS,
        &["Summary", "System Info", "SPD", "Timings", "Training", "IMC", "RTL", "Voltages"]
    );
}

#[test]
fn short_banded_tabs_extend_their_panels_when_selected() {
    let tabs: BTreeSet<&str> = VIEWPORT_SHADED_TABS.iter().copied().collect();
    assert_eq!(tabs, ["System Info", "Timings", "Training", "IMC", "RTL", "Voltages"].into_iter().collect());
}

#[test]
fn system_info_columns_are_independent() {
    // Four left sections face two right sections; padding them to each other
    // would insert blank rows instead of stacking each column from its top.
    assert!(!paired_section_tabs().contains(&"System Info"));
}

#[test]
fn the_footer_links_to_the_handle_it_names() {
    use super::app::*;
    assert_eq!(TWITTER_URL, "https://x.com/MateoPCTech");
    assert!(TWITTER_URL.ends_with(TWITTER_HANDLE.trim_start_matches('@')));
    assert_eq!(YOUTUBE_URL, "https://www.youtube.com/@MateoPcTech");
    assert_eq!(YOUTUBE_LABEL, "YouTube");
    assert_eq!(TWITTER_LABEL, "X");
    assert_eq!(DISCORD_URL, "https://discord.gg/KfzExpKQHB");
    assert_eq!(DISCORD_LABEL, "Discord");
}

#[test]
fn the_selected_tab_carries_readable_text_on_the_red() {
    // All Roch tools share the selected-tab red in both modes; white reaches
    // 4.9:1 on #D0343A.
    for dark in [false, true] {
        let palette = Palette::new(dark);
        assert_eq!(palette.tab_selected, egui::Color32::from_rgb(0xD0, 0x34, 0x3A));
        assert_eq!(palette.tab_selected_text, egui::Color32::WHITE);
    }
}

// --- test_dual_channel_timings.py / test_sensor_rows.py: the window's tables

#[test]
fn no_section_is_ordered_twice() {
    // A category listed twice sorts by its first appearance and reads as if
    // the second placement were doing something.
    let unique: BTreeSet<&str> = TIMINGS_SECTION_ORDER.iter().copied().collect();
    assert_eq!(unique.len(), TIMINGS_SECTION_ORDER.len());
}

#[test]
fn refresh_sits_above_tertiary() {
    let at = |name: &str| TIMINGS_SECTION_ORDER.iter().position(|n| *n == name).unwrap();
    assert!(at("Refresh timings") < at("Tertiary"));
    let layout = timings_three_column_layout(["Refresh timings", "Tertiary", "Command"]);
    let column = |name: &str| layout.iter().find(|(c, _)| *c == name).unwrap().1;
    assert_eq!(column("Refresh timings"), column("Tertiary"));
}

#[test]
fn the_clocks_lead_the_window() {
    // What the silicon is running at, then what that costs it in heat and
    // power, then the rails feeding it.
    assert_eq!(SENSOR_GROUP_ORDER[0], "Clocks");
    // An unlisted category sorts to the end, so a typo here would put the
    // whole section below Voltages rather than above it.
    assert!(SENSOR_GROUP_ORDER.contains(&"Thermal & Power"));
}

#[test]
fn imc_signal_groups_share_one_ordered_column() {
    let expected = ["DATA", "CMD", "CLK", "CTL", "SComp"];
    let ordered: Vec<&str> = IMC_SECTION_ORDER.iter().copied().filter(|n| expected.contains(n)).collect();
    assert_eq!(ordered, expected);
    let at = |name: &str| IMC_SECTION_ORDER.iter().position(|n| *n == name).unwrap();
    assert_eq!(at("Power Down"), at("Refresh") + 1);
}

// --- The real tables. Built as tests/intel_stub.py built them: a driver
//     that answers every register with 5, and a populated dual-channel Z790
//     over WMI.

struct StubDriver;

impl Hardware for StubDriver {
    fn read_phys(&self, _address: u64, size: usize) -> HwResult<Vec<u8>> {
        let mut bytes = vec![0; size];
        if let Some(first) = bytes.first_mut() {
            *first = 5;
        }
        Ok(bytes)
    }
    fn get_phys_long(&self, _address: u64) -> HwResult<u32> {
        Ok(5)
    }
    fn set_phys_long(&self, _address: u64, _value: u32) -> HwResult<()> {
        Ok(())
    }
    fn inb(&self, _port: u16) -> HwResult<u8> {
        Ok(0xFF)
    }
    fn outb(&self, _port: u16, _value: u8) -> HwResult<()> {
        Ok(())
    }
    fn inl(&self, _port: u16) -> HwResult<u32> {
        Ok(0xFFFF_FFFF)
    }
    fn outl(&self, _port: u16, _value: u32) -> HwResult<()> {
        Ok(())
    }
    fn is_driver_open(&self) -> bool {
        true
    }
}

fn memory_module(tag: &str, locator: &str, ddr5: bool) -> WmiObject {
    WmiObject::new()
        .with("Tag", tag)
        .with("DeviceLocator", locator)
        .with("BankLabel", "BANK 0")
        .with("PartNumber", if ddr5 { "F5-8000J3848H16G" } else { "F4-3600C14-16GVKA" })
        .with("Manufacturer", "G Skill Intl")
        .with("Capacity", (16u64 << 30).to_string())
        .with("Attributes", 2i64)
        .with("Speed", if ddr5 { 8000i64 } else { 3600 })
        .with("ConfiguredClockSpeed", if ddr5 { 8000i64 } else { 3600 })
        .with("SMBIOSMemoryType", if ddr5 { 34i64 } else { 26 })
}

/// The Intel table for `platform`, built against the stubs.
fn intel_table(platform: Platform) -> Vec<Row> {
    let ddr5 = platform != Platform::Lga1700Ddr4;
    hardware::set_thread_hardware(Some(Arc::new(StubDriver)));
    FakeWmi::new()
        .class("Win32_PhysicalMemoryArray", vec![WmiObject::new().with("MemoryDevices", 4i64)])
        .class(
            "Win32_PhysicalMemory",
            vec![memory_module("Physical Memory 1", "Controller0-DIMMA2", ddr5), memory_module("Physical Memory 3", "Controller1-DIMMB2", ddr5)],
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
                .with("Product", if ddr5 { "PRO Z790-P WIFI (MS-7E06)" } else { "PRO Z790-P WIFI DDR4 (MS-7E06)" })
                .with("Manufacturer", "Micro-Star International Co., Ltd.")
                .with("Version", "1.0")],
        )
        .class("Win32_BIOS", vec![WmiObject::new().with("SMBIOSBIOSVersion", "1.H0").with("Manufacturer", "American Megatrends")])
        .class("Win32_ComputerSystem", vec![WmiObject::new().with("TotalPhysicalMemory", (32u64 << 30).to_string())])
        .install();
    rochviewer::intel::intel_timings::build_timings(platform)
}

fn names(rows: &[Row]) -> BTreeSet<String> {
    rows.iter().map(|row| row.name.clone()).collect()
}

fn plain(entries: &[SummaryEntry]) -> Vec<String> {
    entries.iter().filter_map(SummaryEntry::as_name).map(str::to_string).collect()
}

#[test]
fn summary_drops_the_rows_that_restate_a_row_it_already_lists() {
    // tWR_MR restates tWR and tRTP_MR restates tRTP. On Timings each sits
    // directly under the row it refers to and the pairing is the point; on
    // the Summary the referent is already there.
    let rows = intel_table(Platform::Lga1700Ddr4);
    let (primary, tertiary) = intel_summary_timing_columns(&rows, &[], &[]);
    let listed: Vec<String> = plain(&primary).into_iter().chain(plain(&tertiary)).collect();
    for (name, referent) in [("tWR_MR", "tWR"), ("tRTP_MR", "tRTP")] {
        assert!(SUMMARY_EXCLUDED_TIMING_NAMES.contains(&name));
        assert!(!listed.iter().any(|n| n == name), "{name}");
        // The row it restates does stay.
        assert!(listed.iter().any(|n| n == referent), "{referent}");
    }
}

#[test]
fn summary_leaves_tcpded_on_the_timings_tab() {
    let rows = intel_table(Platform::Lga1700Ddr4);
    let (primary, tertiary) = intel_summary_timing_columns(&rows, &[], &[]);
    assert!(SUMMARY_EXCLUDED_TIMING_NAMES.contains(&"tCPDED"));
    assert!(!plain(&primary).iter().chain(&plain(&tertiary)).any(|n| n == "tCPDED"));
}

#[test]
fn the_refresh_cycle_rows_follow_the_write_recovery() {
    for platform in [Platform::Lga1700Ddr4, Platform::Lga1700Ddr5] {
        let rows = intel_table(platform);
        let present = names(&rows);
        let (primary, _) = intel_summary_timing_columns(&rows, &[], &[]);
        let primary = plain(&primary);
        let mut expected: Vec<String> =
            ["tRFCns", "tRFC2", "tRFC"].iter().filter(|n| present.contains(**n)).map(|n| n.to_string()).collect();
        if present.contains("tRFC2") && present.contains("tRFCpb") {
            expected.push("tRFCpb".into());
        }
        assert!(!expected.is_empty(), "no refresh cycle row to place");
        let start = primary.iter().position(|n| n == "tWR").unwrap() + 1;
        assert_eq!(primary[start..start + expected.len()], expected[..]);
        if !present.contains("tRFC2") {
            assert!(!primary.iter().any(|n| n == "tRFCpb"));
        }
    }
}

#[test]
fn tmod_follows_the_last_write_turnaround() {
    let rows = intel_table(Platform::Lga1700Ddr4);
    let (primary, tertiary) = intel_summary_timing_columns(&rows, &[], &[]);
    let tertiary = plain(&tertiary);
    assert!(!plain(&primary).iter().any(|n| n == "tMOD"));
    let at = tertiary.iter().position(|n| n == "tWRWR_dd").expect("tWRWR_dd");
    assert_eq!(tertiary[at + 1], "tMOD");
}

#[test]
fn tcke_follows_trefix9_and_other_power_down_rows_are_omitted() {
    let rows = intel_table(Platform::Lga1700Ddr4);
    let (primary, tertiary) = intel_summary_timing_columns(&rows, &[], &[]);
    let (primary, tertiary) = (plain(&primary), plain(&tertiary));
    let at = tertiary.iter().position(|n| n == "tREFIx9").unwrap();
    assert_eq!(tertiary[at + 1], "tCKE");
    assert!(!primary.iter().any(|n| n == "tCKE"));
    for name in ["tXP", "tRDPDEN", "tWRPDEN"] {
        assert!(!primary.iter().chain(&tertiary).any(|n| n == name), "{name}");
    }
}

#[test]
fn only_the_spelling_this_platform_uses_is_listed() {
    // Both spellings are offered and the one that does not exist here
    // resolves to nothing, so the column never carries a name with no row
    // behind it.
    for platform in [Platform::Lga1700Ddr4, Platform::Lga1700Ddr5] {
        let rows = intel_table(platform);
        let present = names(&rows);
        let (primary, tertiary) = intel_summary_timing_columns(&rows, &[], &[]);
        let listed: Vec<String> = plain(&primary).into_iter().chain(plain(&tertiary)).collect();
        for name in &listed {
            assert!(present.contains(name), "{name}");
        }
        assert!(!(listed.contains(&"tRFC2".to_string()) && listed.contains(&"tRFC".to_string())));
    }
}

#[test]
fn the_exclusion_list_governs_both_summary_columns() {
    // tREFI is the exception and is meant to be: the Tertiary column pins it
    // at its head deliberately.
    let rows = intel_table(Platform::Lga1700Ddr4);
    let (primary, tertiary) = intel_summary_timing_columns(&rows, &[], &[]);
    let shown: BTreeSet<String> = plain(&primary).into_iter().chain(plain(&tertiary)).collect();
    for name in SUMMARY_EXCLUDED_TIMING_NAMES.iter().filter(|n| **n != "tREFI") {
        assert!(!shown.contains(*name), "{name}");
    }
}

#[test]
fn every_excluded_name_is_a_row_that_exists() {
    // An excluded name with no row behind it excludes nothing. The list is
    // shared, so every platform's table counts.
    // Each table on a thread of its own, so one platform's cached identity
    // cannot leak into the next build.
    let build = |f: fn() -> Vec<Row>| std::thread::spawn(move || names(&f())).join().unwrap();
    let mut present = build(|| intel_table(Platform::Lga1700Ddr4));
    present.extend(build(|| intel_table(Platform::Lga1700Ddr5)));
    present.extend(build(rochviewer::amd::profile::build_timings));
    for name in SUMMARY_EXCLUDED_TIMING_NAMES {
        assert!(present.contains(*name), "{name}");
    }
}

#[test]
fn the_signal_tail_is_not_a_vref_row_and_names_rows_that_exist() {
    let rows = intel_table(Platform::Lga1700Ddr4);
    let vref = summary_vref_row_names(&rows);
    let present = names(&rows);
    for name in SUMMARY_SIGNAL_TAIL_ROWS {
        assert!(!vref.iter().any(|n| n == name), "{name}");
        if present.contains("DLL BWSEL") {
            assert!(present.contains(*name), "{name}");
        }
    }
}

#[test]
fn summary_keeps_the_compact_rtl_rows() {
    let rows = intel_table(Platform::Lga1700Ddr5);
    let (primary, tertiary) = intel_summary_timing_columns(&rows, &[], &[]);
    let pairs: Vec<SummaryEntry> = primary.into_iter().filter(SummaryEntry::is_summary_pair).collect();
    assert_eq!(pairs, summary_rtl_entries());
    assert!(!tertiary.iter().any(SummaryEntry::is_summary_pair));
}

// test_misc_tab.py, the window's half: what the tabs make of the table.

#[test]
fn training_sections_are_drawn_in_the_declared_order() {
    for platform in [Platform::Lga1700Ddr4, Platform::Lga1700Ddr5] {
        let rows = intel_table(platform);
        let mut seen: Vec<String> = Vec::new();
        for row in rows.iter().filter(|r| r.tab == "Training" && r.extra_str("source_scope") == Some("module")) {
            if seen.last() != Some(&row.category) {
                seen.push(row.category.clone());
            }
        }
        // Each section appears once and only once.
        let unique: BTreeSet<&String> = seen.iter().collect();
        assert_eq!(unique.len(), seen.len(), "{seen:?}");
        let ordered: Vec<String> =
            ordered_sections(seen.iter().map(|n| (n.clone(), ())).collect(), SKEW_SECTION_ORDER).into_iter().map(|(n, _)| n).collect();
        let declared: Vec<String> = SKEW_SECTION_ORDER.iter().filter(|n| seen.contains(&n.to_string())).map(|n| n.to_string()).collect();
        assert_eq!(ordered, declared);
    }
}

#[test]
fn the_imc_columns_hold_the_signal_groups_and_power_down() {
    let rows = intel_table(Platform::Lga1700Ddr4);
    let column = |category: &str| rows.iter().find(|r| r.tab == "IMC" && r.category == category).map(|r| r.column.clone());
    for name in ["DATA", "CMD", "CLK", "CTL", "SComp"] {
        if let Some(column) = column(name) {
            assert_eq!(column, "Right", "{name}");
        }
    }
    assert_eq!(column("Power Down").as_deref(), Some("Left"));
}

#[test]
fn the_reading_tabs_are_offered_and_rtl_has_two_channel_columns() {
    let rows = intel_table(Platform::Lga1700Ddr5);
    let tabs = tab_names_for(&rows);
    for tab in ["Training", "IMC", "RTL"] {
        assert!(tabs.iter().any(|t| t == tab), "{tab}");
    }
    let rtl: Vec<&Row> = rows.iter().filter(|r| r.tab == "RTL").collect();
    assert_eq!(rtl.iter().filter(|r| !r.name.is_empty()).count(), 32);
    assert_eq!(rtl.iter().filter(|r| r.column == "Left").count(), 17);
    assert_eq!(rtl.iter().filter(|r| r.column == "Right").count(), 17);
    let categories: BTreeSet<&str> = rtl.iter().map(|r| r.category.as_str()).collect();
    assert_eq!(categories, ["RTL CHA", "RTL CHB"].into_iter().collect());
    // One blank row in each channel separates MC0 from MC1.
    for category in categories {
        assert_eq!(rtl.iter().filter(|r| r.category == category && r.name.trim().is_empty()).count(), 1);
    }
    for name in ["RTL MC0 CHA R0", "RTL MC1 CHA R0", "RTL MC0 CHB R0", "RTL MC1 CHB R0"] {
        assert!(rtl.iter().any(|r| r.name == name), "{name}");
    }
}

#[test]
fn each_reading_tab_section_draws_as_one_block() {
    // A category whose rows are not contiguous renders as two headings with
    // the same name -- the trap the Timings tab hit.
    for platform in [Platform::Lga1700Ddr4, Platform::Lga1700Ddr5] {
        let rows = intel_table(platform);
        for tab in ["Timings", "System Info", "IMC"] {
            for column in detail_layout(&rows, tab) {
                let mut seen = BTreeSet::new();
                for section in &column.sections {
                    assert!(seen.insert(section.name.clone()), "{tab}: {} draws as more than one block", section.name);
                }
            }
        }
    }
}

#[test]
fn system_info_columns_follow_the_requested_section_order() {
    let rows = intel_table(Platform::Lga1700Ddr5);
    let layout = detail_layout(&rows, "System Info");
    let shown: Vec<(&str, Vec<String>)> =
        layout.iter().map(|c| (c.key, c.sections.iter().map(|s| s.name.clone()).collect())).collect();
    assert_eq!(
        shown,
        vec![("Left", vec!["System".into(), "Processor".into(), "Motherboard".into()]), ("Right", vec!["Clocks".into(), "Graphics".into()])]
    );
}

#[test]
fn every_sensor_category_has_a_place_in_the_window() {
    // The Telemetry window orders its groups by SENSOR_GROUP_ORDER, and a
    // category absent from it sorts to the end with no heading of its own.
    let rows = intel_table(Platform::Lga1700Ddr5);
    for row in rows.iter().filter(|r| r.tab == "Sensors") {
        assert!(SENSOR_GROUP_ORDER.contains(&row.category.as_str()), "{}", row.category);
    }
}

// --- test_unscrolled_fit_live.py: the machine's own table in the window.

/// The active platform's table, read once, as the window would show it.
fn live() -> (Backend, Values) {
    let backend = rochviewer::timings::load_active_backend();
    let values = values_for(&backend);
    (backend, values)
}

#[test]
#[ignore = "needs hardware"]
fn every_unscrolled_tab_fits_the_window() {
    // Without a scrollbar, content past the bottom or right edge is cut off
    // with nothing to reach it.
    let (backend, values) = live();
    let palette = Palette::new(true);
    let labels = BTreeMap::new();
    let ctx = TabContext { backend: &backend, values: &values, palette: &palette, selected: None, channel_labels: &labels };
    let plan = summary::summary_plan(&backend.rows, backend.platform);
    with_ui(|ui| {
        for tab in tab_names_for(&backend.rows) {
            if tab == "SPD" {
                continue;
            }
            let (width, height) = window_size_for_tab(backend.platform, &tab, None);
            let chrome = Chrome::new(Rect::from_min_size(pos2(0.0, 0.0), vec2(width, height)), false);
            let area = chrome.content(&tab);
            let needed = if tab == "Summary" {
                summary::needed_size(ui, &ctx, &plan)
            } else {
                let geometry = detail_geometry(ui, &ctx, &tab, area);
                vec2(geometry.needed_width(), geometry.needed_height())
            };
            assert!(needed.y <= area.height(), "{tab} needs {}px and has {}px", needed.y, area.height());
            assert!(needed.x <= area.width(), "{tab} needs {}px across and has {}px", needed.x, area.width());
        }
    });
}

#[test]
#[ignore = "needs hardware"]
fn summary_static_rows_are_not_on_the_live_refresh() {
    // The Telemetry copy of DRAM Frequency shares the row's name, and the
    // Summary picked it up by name, so the Summary flickered with BCLK.
    let backend = rochviewer::timings::load_active_backend();
    let plan = summary::summary_plan(&backend.rows, backend.platform);
    for pair in plan.aligned.iter().flatten().flatten().chain(plan.identity.iter().flatten()) {
        let row = &backend.rows[pair.index];
        if SUMMARY_STATIC_ROWS.contains(&row.name.as_str()) {
            assert!(!row.live, "{}", row.name);
        }
    }
}

#[test]
fn opening_a_tab_re_takes_its_snapshot() {
    // Only the tab's own rows that can be re-taken, and never a diagnostic.
    let mut fresh = Row::new("VDD", "DIMM A1", "Voltages", "Left").with_value("1.1 V");
    fresh.refresh = Some(Arc::new(|| {}));
    let mut hidden = fresh.clone();
    hidden.extra.insert("diagnostic".into(), rochviewer::row::Extra::Bool(true));
    let mut elsewhere = fresh.clone();
    elsewhere.tab = "Summary".into();
    let fixed = Row::new("VPP", "DIMM A1", "Voltages", "Left").with_value("1.8 V");
    let rows = vec![fresh, hidden, elsewhere, fixed];
    assert_eq!(super::app::reread_rows(&rows, "Voltages"), vec![0]);
    assert!(super::app::reread_rows(&rows, "Timings").is_empty());
}

#[test]
fn the_summary_clocks_are_the_telemetry_copies_on_every_intel_platform() {
    // The Python Summary built its clock rows from a dict of the table, so
    // the last row of a name won: for MCLK and UCLK that is the live
    // Telemetry copy. The main window has to read those two although it
    // leaves the rest of the Sensors tab to the Telemetry window (the
    // reader's `first` set covers them); AM5 has no such row.
    for platform in [Platform::Lga1700Ddr5, Platform::Lga1700Ddr4, Platform::Lga1851] {
        let rows = std::thread::spawn(move || intel_table(platform)).join().unwrap();
        let plan = summary::summary_plan(&rows, platform);
        let windowed: Vec<(&str, bool)> = plan
            .row_indices()
            .into_iter()
            .filter(|&i| !super::reader::shown_in_main_window(&rows[i]))
            .map(|i| (rows[i].name.as_str(), rows[i].live))
            .collect();
        assert_eq!(windowed, vec![("MCLK", true), ("UCLK", true)], "{platform:?}");
    }
    let rows = std::thread::spawn(rochviewer::amd::profile::build_timings).join().unwrap();
    let plan = summary::summary_plan(&rows, Platform::Am5);
    assert!(plan.row_indices().into_iter().all(|i| super::reader::shown_in_main_window(&rows[i])));
}

// --- a hand-made Summary starts as the built-in one.

#[test]
fn every_platforms_built_in_summary_survives_becoming_a_layout() {
    use super::summary_layout::SummaryLayout;
    // Each table on a thread of its own, as above.
    let check = |platform: Platform| {
        std::thread::spawn(move || {
            let rows = if platform == Platform::Am5 { rochviewer::amd::profile::build_timings() } else { intel_table(platform) };
            let plan = summary::summary_plan(&rows, platform);
            let layout = SummaryLayout::from_plan(&rows, &plan);
            assert_eq!(layout.to_plan(&rows), plan, "{platform:?}");
            let saved = SummaryLayout::from_json(&layout.to_json()).unwrap();
            assert_eq!(saved.to_plan(&rows), plan, "{platform:?} after saving");
        })
        .join()
        .unwrap()
    };
    for platform in [Platform::Lga1700Ddr4, Platform::Lga1700Ddr5, Platform::Lga1851, Platform::Am5] {
        check(platform);
    }
}
