//! Row-by-row parity with the Python profile.
//!
//! `parity/dump_python_profile.py` built rochviewer/amd/profile.py's table
//! three times under stubs -- a Ryzen 9000 with two channels that disagree,
//! a Ryzen 7000 with one training record, and a machine where nothing could
//! be read -- read every row the way the window does, and wrote what it saw
//! to `parity/am5_profile_parity.json`. This builds the Rust table under the
//! same stubs and requires every row to match: its order, name, label, tab,
//! category, column, flags, both channel sides, and the exact text each one
//! shows.
//!
//! Regenerate the fixture after a deliberate change to the Python profile:
//! `python crates/rochviewer/src/amd/profile/parity/dump_python_profile.py`.

use serde_json::{Map, Value as Json, json};

use super::tests::{FakeTrainingReader, reader_factory, stub_live};
use super::*;
use crate::gpu::radeon::RadeonGpu;
use crate::memory::ddr5_telemetry::{DimmTelemetry, PmicTelemetry};
use crate::row::Channel;
use crate::wmi_util::{FakeWmi, WmiObject};

const FIXTURE: &str = include_str!("parity/am5_profile_parity.json");

fn fixture() -> Json {
    serde_json::from_str(FIXTURE).expect("the parity fixture parses")
}

fn regs_from(json: &Json) -> Registers {
    json.as_object()
        .unwrap()
        .iter()
        .map(|(key, value)| {
            let offset = u32::from_str_radix(key.trim_start_matches("0x"), 16).unwrap();
            (offset, value.as_u64().unwrap() as u32)
        })
        .collect()
}

fn training_from(json: &Json) -> TrainingValues {
    json.as_object()
        .unwrap()
        .iter()
        .map(|(key, value)| {
            let name: &'static str = TRAINING_FIELDS.get(key.as_str()).copied().expect("a training field");
            let value = match value {
                Json::String(text) => TrainingValue::Text(text.clone()),
                other => TrainingValue::Raw(other.as_u64().unwrap() as u8),
            };
            (name, value)
        })
        .collect()
}

fn hex_bytes(text: &str) -> Vec<u8> {
    (0..text.len()).step_by(2).map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap()).collect()
}

fn cpu_name(scenario: &str) -> &'static str {
    match scenario {
        "granite" => "AMD Ryzen 7 9850X3D 8-Core Processor",
        "raphael" => "AMD Ryzen 7 7800X3D 8-Core Processor",
        _ => "AMD Ryzen 7 8700G w/ Radeon 780M Graphics",
    }
}

fn install_wmi(scenario: &str) {
    if scenario == "failing" {
        FakeWmi::new().install();
        return;
    }
    let memory = |bank: &str| {
        WmiObject::new()
            .with("Capacity", "34359738368")
            .with("PartNumber", "F5-6000J3038F16G")
            .with("Manufacturer", "G Skill Intl")
            .with("DeviceLocator", "DIMM 1")
            .with("BankLabel", bank)
            .with("SerialNumber", "00000000")
            .with("Attributes", 2i64)
            .with("SMBIOSMemoryType", 34i64)
    };
    FakeWmi::new()
        .class(
            "Win32_Processor",
            vec![
                WmiObject::new()
                    .with("ProcessorId", "178BFBFF00B40F40")
                    .with("ExtClock", 100i64)
                    .with("Name", cpu_name(scenario))
                    .with("Manufacturer", "AuthenticAMD")
                    .with("NumberOfCores", 8i64)
                    .with("NumberOfLogicalProcessors", 16i64)
                    .with("SocketDesignation", "AM5")
                    .with("L3CacheSize", 98304i64),
            ],
        )
        .class(
            "Win32_BaseBoard",
            vec![
                WmiObject::new()
                    .with("Product", "X870 AORUS ELITE WIFI7")
                    .with("Version", "x.x")
                    .with("Manufacturer", "Gigabyte Technology Co., Ltd."),
            ],
        )
        .class(
            "Win32_BIOS",
            vec![WmiObject::new().with("SMBIOSBIOSVersion", " F5 ").with("ReleaseDate", "20260315000000.000000+000")],
        )
        .class("Win32_PhysicalMemory", vec![memory("P0 CHANNEL A"), memory("P0 CHANNEL B")])
        .class("Win32_PhysicalMemoryArray", vec![WmiObject::new().with("MemoryDevices", 4i64)])
        .install();
}

fn gpu_card() -> RadeonGpu {
    RadeonGpu {
        device_id: 0x7550,
        revision: 0xC0,
        revision_text: "C0".into(),
        board_manufacturer: Some("Sapphire".into()),
        name: Some("AMD Radeon RX 9070 XT".into()),
        vbios: Some("113-EXT114484-100".into()),
        memory_size: Some("16 GB".into()),
        code_name: Some("Navi 48".into()),
        technology: Some("4 nm".into()),
        cores: Some(4096),
        rops: Some(128),
        tmus: Some(256),
        memory_type: Some("GDDR6".into()),
        bus_width: Some("256 bit".into()),
        resizable_bar: Some("Enabled (16 GB)".into()),
        pcie_link: Some("PCIe 5.0 x16 @ 5.0 x2".into()),
        driver_version: Some("32.0.21001.9024".into()),
        driver_date: Some("2026-01-15".into()),
        memory_vendor: None,
        memory_bytes: None,
    }
}

fn dimm(channel: &'static str, telemetry: PmicTelemetry) -> DimmTelemetry {
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

/// Every reader the Python's mock.patch list replaced, replaced the same way.
fn install_seams(scenario: &str) {
    let failing = scenario == "failing";
    type Text = Box<dyn Fn() -> Option<String>>;
    let text = |value: Option<&'static str>| Box::new(move || value.map(str::to_string)) as Text;
    seams::set(
        "os_name",
        text((!failing).then_some("Microsoft Windows 11 Professional (x64) 24H2 Build 26100.4061")),
    );
    seams::set("microcode", text((!failing).then_some("0xB404032")));
    seams::set("lpcio_name", text((!failing).then_some("ITE IT8696E")));
    crate::system_identity::set_thread_config_space(Some(Box::new(move |device, function, offset, _bus| {
        if failing {
            return None;
        }
        let (id, revision) = match (device, function) {
            (0x00, 0) => (0x14D8u32, 0x00u32),
            (0x14, 3) => (0x790E, 0x51),
            _ => return None,
        };
        match offset {
            0x00 => Some((id << 16) | 0x1022),
            0x08 => Some(revision),
            _ => None,
        }
    })));
    seams::set(
        "read_gpu",
        Box::new(move || Ok(if failing { None } else { Some(gpu_card()) }))
            as Box<dyn Fn() -> Result<Option<RadeonGpu>, String>>,
    );
    seams::set(
        "gpu_sensors",
        Box::new(move || {
            if failing { BTreeMap::new() } else { BTreeMap::from([("GPU Temperature", "45 °C".to_string())]) }
        }) as Box<dyn Fn() -> BTreeMap<&'static str, String>>,
    );
    seams::set(
        "board_power_limit",
        Box::new(move || if failing { EM_DASH.to_string() } else { "304 W".to_string() }) as Box<dyn Fn() -> String>,
    );
    seams::set(
        "whea",
        Box::new(move || if failing { EM_DASH.to_string() } else { "0".to_string() }) as Box<dyn Fn() -> String>,
    );
    seams::set(
        "clock_text",
        Box::new(move |key: &str| {
            if failing {
                return None;
            }
            match key {
                "core_avg" => Some("4712 MHz".to_string()),
                "core_effective" => Some("803 MHz".to_string()),
                _ => None,
            }
        }) as Box<dyn Fn(&str) -> Option<String>>,
    );
    seams::set(
        "core_labels",
        Box::new(move || if failing { Vec::new() } else { vec!["CPU 0".into(), "CPU 1".into(), "CPU 2".into()] })
            as Box<dyn Fn() -> Vec<String>>,
    );
    seams::set(
        "core_clock_text",
        Box::new(|index: usize| match index {
            0 => Some("5012 MHz".to_string()),
            2 => Some("4790 MHz".to_string()),
            _ => None,
        }) as Box<dyn Fn(usize) -> Option<String>>,
    );
    seams::set(
        "dimm_telemetry",
        Box::new(move || {
            if failing {
                return Vec::new();
            }
            vec![
                dimm(
                    "a",
                    PmicTelemetry {
                        vdd: Some(1425),
                        vddq: Some(1410),
                        vpp: Some(1800),
                        vin_bulk: Some(5040),
                        vout_1v8: Some(1795),
                        vout_1v0: Some(1000),
                        ..Default::default()
                    },
                ),
                dimm("b", PmicTelemetry { vdd: Some(1410), vddq: Some(1395), ..Default::default() }),
            ]
        }) as Box<dyn Fn() -> Vec<DimmTelemetry>>,
    );
    seams::set(
        "spd_identity",
        Box::new(Vec::new) as Box<dyn Fn() -> Vec<crate::memory::ddr5_spd::SpdIdentity>>,
    );
    seams::set(
        "agesa",
        Box::new(move || if failing { String::new() } else { "ComboAm5PI 1.2.0.3e".to_string() })
            as Box<dyn Fn() -> String>,
    );
}

pub(super) fn stub_with_status<T: Clone + Truthy>(source: &Mutex<LiveSource<T>>, value: T) {
    stub_live(source, value.clone());
    let mut source = lock(source);
    if value.truthy() {
        source.status = (source.describe)(&value);
    }
}

fn stub_sources(runtime: &Am5Runtime, scenario: &str) {
    let sources = &runtime.sources;
    if scenario == "failing" {
        stub_with_status(&sources.clocks, None);
        stub_with_status(&sources.voltages, None);
        stub_with_status(&sources.power, None);
        for source in [
            &sources.dram,
            &sources.board,
            &sources.board_monitor,
            &sources.board_temp,
            &sources.dimm_temp,
        ] {
            stub_with_status(source, Readings::new());
        }
        return;
    }
    let raphael = scenario == "raphael";
    stub_with_status(
        &sources.clocks,
        Some(SmuClocks {
            version: if raphael { 0x540108 } else { 0x620105 },
            table_base: 0x1000,
            fclk_mhz: 2133.0,
            uclk_mhz: 2050.0,
            mclk_mhz: 4100.0,
            verified: !raphael,
        }),
    );
    stub_with_status(
        &sources.voltages,
        Some(SmuVoltages {
            version: 0x620105,
            table_base: 0x1000,
            values: BTreeMap::from([
                ("vddcr_vdd", 1.173),
                ("vddcr_soc", 1.2032),
                ("cldo_vddq", 1.0),
                ("vddg_ccd", 0.95),
                ("vddg_iod", 0.9915),
                ("cdd_misc", 1.1),
            ]),
        }),
    );
    stub_with_status(
        &sources.power,
        Some(SmuPower {
            version: 0x620105,
            table_base: 0x1000,
            values: BTreeMap::from([("ppt", 41.0244), ("tdc", 9.6954), ("edc", 50.25), ("scalar", 1.0)]),
            limits: BTreeMap::from([("ppt", 162.0), ("tdc", 120.0), ("edc", 180.0)]),
            temperatures: BTreeMap::from([
                ("cpu", 41.1),
                ("cpu_limit", 90.0),
                ("iod_average", 38.2),
                ("iod_hotspot", 40.0),
                ("l3", 37.5),
                ("vdd_vrm", 45.0),
                ("soc_vrm", 44.25),
                ("misc_vrm", 41.0),
            ]),
        }),
    );
    stub_with_status(&sources.dram, Readings::from([("dram_vdd", 1.4), ("dram_vddq", 1.39), ("dram_vpp", 1.8)]));
    stub_with_status(&sources.board, Readings::from([("vddio_mem", 1.424), ("vtt", 1.8)]));
    stub_with_status(
        &sources.board_monitor,
        Readings::from([
            ("board_vcore", 1.206),
            ("plus12v", 12.096),
            ("plus5v", 5.04),
            ("cpu_fan", 1403.0),
            ("cpu_opt", 3648.0),
            ("pch", 37.0),
        ]),
    );
    stub_with_status(&sources.board_temp, Readings::from([("cpu", 41.0), ("vrm", 32.0), ("system", 31.5)]));
    stub_with_status(&sources.dimm_temp, Readings::from([("a", 38.25), ("b", 39.0)]));
}

fn runtime_for(scenario: &str, inputs: &Json) -> Arc<Am5Runtime> {
    let (regs, training): (Registers, Option<FakeTrainingReader>) = match scenario {
        "granite" => {
            let cha = training_from(&inputs["training_a"]);
            let chb = training_from(&inputs["training_b"]);
            let mut reader = FakeTrainingReader::new(cha.clone());
            reader.channel_values = BTreeMap::from([("cha", cha), ("chb", chb)]);
            reader.raw_table = hex_bytes(inputs["ccdl_table"].as_str().unwrap());
            (regs_from(&inputs["two_channel_regs"]), Some(reader))
        }
        "raphael" => (
            regs_from(&inputs["oracle_regs"]),
            Some(FakeTrainingReader::new(training_from(&inputs["training_raphael"]))),
        ),
        _ => (Registers::new(), None),
    };
    let name = cpu_name(scenario);
    let runtime = Am5Runtime::with_options(Am5RuntimeOptions {
        reader_factory: reader_factory(regs),
        training_reader_factory: Box::new(move |_layout| match &training {
            Some(reader) => Ok(Box::new(reader.clone()) as Box<dyn TrainingReader>),
            None => Err("APOB boom".into()),
        }),
        cpu_name_factory: Box::new(move || name.to_string()),
    });
    stub_sources(&runtime, scenario);
    runtime
}

fn resolve(value: Option<&crate::row::Value>) -> String {
    value.map(|value| value.resolve(EM_DASH)).unwrap_or_default()
}

/// One row as the dump script wrote it.
pub(super) fn dump_row(row: &Row) -> Json {
    let dual = row.a.is_some() || row.b.is_some();
    let mut extra = Map::new();
    for key in ["hide_when_blank", "diagnostic", "Parent", "rail_key", "rail_group"] {
        match row.extra.get(key) {
            Some(Extra::Bool(flag)) => {
                extra.insert(key.into(), json!(flag));
            }
            Some(Extra::Str(text)) => {
                extra.insert(key.into(), json!(text));
            }
            Some(other) => panic!("{}: unexpected extra {key} = {other:?}", row.name),
            None => {}
        }
    }
    let mut out = json!({
        "name": row.name,
        "display_name": row.display_name,
        "tab": row.tab,
        "category": row.category,
        "column": row.column,
        "live": row.live,
        "lone_channel_alone": row.lone_channel_alone,
        "dual": dual,
        "name_a": row.a.as_ref().and_then(|side| side.name.clone()),
        "name_b": row.b.as_ref().and_then(|side| side.name.clone()),
        "refresh": row.refresh.is_some(),
        "dim": row.dim.as_ref().map(|dim| dim()),
        "extra": extra,
        "value": resolve(row.value.as_ref()),
    });
    if dual {
        let side = |channel| resolve(row.side(channel).and_then(|side| side.value.as_ref()));
        out["value_a"] = json!(side(Channel::A));
        out["value_b"] = json!(side(Channel::B));
    }
    out
}

fn check_scenario(scenario: &str) {
    let fixture = fixture();
    let expected = &fixture["scenarios"][scenario];
    install_wmi(scenario);
    install_seams(scenario);
    let runtime = runtime_for(scenario, &fixture["inputs"]);
    let rows: Vec<Json> = build_timings_with(&runtime).iter().map(dump_row).collect();
    let python = expected["rows"].as_array().unwrap();

    let mut problems = Vec::new();
    let names = |rows: &[Json]| rows.iter().map(|row| row["name"].as_str().unwrap().to_string()).collect::<Vec<_>>();
    if names(&rows) != names(python) {
        problems.push(format!("row order differs:\n  rust   {:?}\n  python {:?}", names(&rows), names(python)));
    }
    for (rust, python) in rows.iter().zip(python) {
        if rust != python {
            problems.push(format!("{}:\n  rust   {rust}\n  python {python}", python["name"]));
        }
    }
    for (key, actual) in [
        ("status", runtime.status()),
        ("training_status", runtime.training_status()),
        ("clock_status", runtime.clock_status()),
        ("voltage_status", runtime.voltage_status()),
        ("power_status", runtime.power_status()),
        ("dram_rail_status", runtime.dram_rail_status()),
        ("board_rail_status", runtime.board_rail_status()),
    ] {
        if expected[key].as_str() != Some(actual.as_str()) {
            problems.push(format!("{key}: rust {actual:?}, python {}", expected[key]));
        }
    }
    assert!(problems.is_empty(), "{scenario}: {} difference(s)\n{}", problems.len(), problems.join("\n"));
}

#[test]
fn ryzen_9000_with_two_channels_matches_the_python() {
    check_scenario("granite");
}

#[test]
fn ryzen_7000_with_one_record_matches_the_python() {
    check_scenario("raphael");
}

#[test]
fn a_machine_that_reads_nothing_matches_the_python() {
    check_scenario("failing");
}

#[test]
fn the_training_fields_match_the_python() {
    let fixture = fixture();
    let python: Vec<&str> =
        fixture["training_fields"].as_array().unwrap().iter().map(|name| name.as_str().unwrap()).collect();
    let rust: Vec<&str> = TRAINING_FIELDS.iter().copied().collect();
    assert_eq!(rust, python);
}

#[test]
fn the_system_info_sections_match_the_python() {
    let fixture = fixture();
    let rust: Vec<Json> = SYSTEM_INFO_SECTIONS
        .iter()
        .map(|(title, column, names)| json!([title, column, names]))
        .collect();
    assert_eq!(Json::Array(rust), fixture["system_info_sections"]);
}
