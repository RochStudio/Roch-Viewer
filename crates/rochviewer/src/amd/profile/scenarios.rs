//! Row-by-row parity with the Python profile over varied inputs.
//!
//! `parity.rs` pins three machines. `parity/dump_python_scenarios.py` built
//! rochviewer/amd/profile.py's table over eight more -- other refresh modes,
//! reserved preamble codes, a lone channel, failed and out-of-range
//! registers, both tCCD_L fallbacks, training records that come back empty or
//! without channel geometry, transports that raise, readings on a rounding
//! boundary, NaN readings, falsy card fields and odd WMI shapes, on Ryzen
//! 9000, Ryzen 7000 and neither -- and wrote each scenario's inputs into the
//! fixture beside what Python displayed. This builds the Rust table from those
//! same inputs and requires every row and every status line to match.
//!
//! Regenerate the fixture after a deliberate change to the Python profile:
//! `python crates/rochviewer/src/amd/profile/parity/dump_python_scenarios.py`
//! from the root of a v1.1.0 checkout.

use serde_json::Value as Json;

use super::parity::{dump_row, stub_with_status};
use super::*;
use crate::gpu::radeon::RadeonGpu;
use crate::memory::ddr5_telemetry::{DimmTelemetry, PmicTelemetry};
use crate::wmi_util::{FakeWmi, WmiObject};

const FIXTURE: &str = include_str!("parity/am5_profile_scenarios.json");

fn fixture() -> Json {
    serde_json::from_str(FIXTURE).expect("the scenario fixture parses")
}

fn leak(text: &str) -> &'static str {
    Box::leak(text.to_string().into_boxed_str())
}

/// A float as the dump script wrote it: its Python `repr`.
fn float(json: &Json) -> f64 {
    let text = json.as_str().expect("floats travel as strings");
    match text {
        "nan" => f64::NAN,
        "inf" => f64::INFINITY,
        "-inf" => f64::NEG_INFINITY,
        _ => text.parse().expect("a float repr"),
    }
}

fn readings(json: &Json) -> Readings {
    json.as_object().unwrap().iter().map(|(key, value)| (leak(key), float(value))).collect()
}

fn text(json: &Json) -> Option<String> {
    json.as_str().map(str::to_string)
}

fn wmi_value(json: &Json) -> WmiValue {
    match json {
        Json::Null => WmiValue::Null,
        Json::Bool(flag) => WmiValue::Bool(*flag),
        Json::String(text) => WmiValue::Str(text.clone()),
        Json::Number(number) if number.is_i64() => WmiValue::Int(number.as_i64().unwrap()),
        Json::Number(number) => WmiValue::Float(number.as_f64().unwrap()),
        other => panic!("unexpected WMI value {other}"),
    }
}

fn install_wmi(json: &Json) {
    let mut fake = FakeWmi::new();
    if let Some(classes) = json.as_object() {
        for (class, objects) in classes {
            let objects = objects
                .as_array()
                .unwrap()
                .iter()
                .map(|fields| {
                    fields
                        .as_object()
                        .unwrap()
                        .iter()
                        .fold(WmiObject::new(), |object, (field, value)| object.with(field, wmi_value(value)))
                })
                .collect();
            fake = fake.class(class, objects);
        }
    }
    fake.install();
}

fn gpu_card(json: &Json) -> Option<RadeonGpu> {
    let card = json.as_object()?;
    let text = |key: &str| card.get(key).and_then(Json::as_str).map(str::to_string);
    let number = |key: &str| card.get(key).and_then(Json::as_u64);
    Some(RadeonGpu {
        device_id: number("device_id").unwrap() as u16,
        revision: number("revision").unwrap() as u8,
        revision_text: text("revision_text").unwrap(),
        board_manufacturer: text("board_manufacturer"),
        name: text("name"),
        vbios: text("vbios"),
        memory_size: text("memory_size"),
        code_name: text("code_name"),
        technology: text("technology"),
        cores: number("cores").map(|value| value as u32),
        rops: number("rops").map(|value| value as u32),
        tmus: number("tmus").map(|value| value as u32),
        memory_type: text("memory_type"),
        bus_width: text("bus_width"),
        resizable_bar: text("resizable_bar"),
        pcie_link: text("pcie_link"),
        driver_version: text("driver_version"),
        driver_date: text("driver_date"),
        memory_vendor: text("memory_vendor"),
        memory_bytes: None,
    })
}

fn dimm(json: &Json) -> DimmTelemetry {
    let millivolts = |key: &str| json.get(key).and_then(Json::as_u64).map(|value| value as u32);
    DimmTelemetry {
        channel: leak(json["channel"].as_str().unwrap()),
        hub_address: 0x50,
        pmic_address: 0x48,
        controller: 0,
        hub_temp_c: None,
        vendor: None,
        revision: None,
        telemetry: PmicTelemetry {
            vdd: millivolts("vdd"),
            vddq: millivolts("vddq"),
            vpp: millivolts("vpp"),
            vin_bulk: millivolts("vin_bulk"),
            vout_1v8: millivolts("vout_1v8"),
            vout_1v0: millivolts("vout_1v0"),
            ..Default::default()
        },
    }
}

/// Every reader the Python's mock.patch list replaced, from the scenario.
fn install_seams(seams: &Json) {
    type Text = Box<dyn Fn() -> Option<String>>;
    let constant = |value: Option<String>| Box::new(move || value.clone()) as Text;
    seams::set("os_name", constant(text(&seams["os_name"])));
    seams::set("microcode", constant(text(&seams["microcode"])));
    seams::set("lpcio_name", constant(text(&seams["lpcio"])));

    let bridges: Vec<(u8, u8, u32, u32)> = seams["bridges"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| {
            let field = |index: usize| entry[index].as_u64().unwrap();
            (field(0) as u8, field(1) as u8, field(2) as u32, field(3) as u32)
        })
        .collect();
    crate::system_identity::set_thread_config_space(Some(Box::new(move |device, function, offset, _bus| {
        let &(_, _, id, revision) = bridges.iter().find(|entry| (entry.0, entry.1) == (device, function))?;
        match offset {
            0x00 => Some((id << 16) | 0x1022),
            0x08 => Some(revision),
            _ => None,
        }
    })));

    let card = gpu_card(&seams["gpu"]);
    seams::set(
        "read_gpu",
        Box::new(move || Ok(card.clone())) as Box<dyn Fn() -> Result<Option<RadeonGpu>, String>>,
    );
    let sensors: BTreeMap<&'static str, String> = seams["gpu_sensors"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(key, value)| (leak(key), value.as_str().unwrap().to_string()))
        .collect();
    seams::set(
        "gpu_sensors",
        Box::new(move || sensors.clone()) as Box<dyn Fn() -> BTreeMap<&'static str, String>>,
    );
    let limit = seams["board_power_limit"].as_str().unwrap().to_string();
    seams::set("board_power_limit", Box::new(move || limit.clone()) as Box<dyn Fn() -> String>);
    let whea = seams["whea"].as_str().unwrap().to_string();
    seams::set("whea", Box::new(move || whea.clone()) as Box<dyn Fn() -> String>);
    let clocks = seams["clock_text"].clone();
    seams::set(
        "clock_text",
        Box::new(move |key: &str| text(&clocks[key])) as Box<dyn Fn(&str) -> Option<String>>,
    );
    let labels: Vec<String> =
        seams["core_labels"].as_array().unwrap().iter().map(|label| label.as_str().unwrap().into()).collect();
    seams::set("core_labels", Box::new(move || labels.clone()) as Box<dyn Fn() -> Vec<String>>);
    let core_clocks = seams["core_clocks"].clone();
    seams::set(
        "core_clock_text",
        Box::new(move |index: usize| text(&core_clocks[index.to_string()])) as Box<dyn Fn(usize) -> Option<String>>,
    );
    let modules: Vec<Json> = seams["dimm_telemetry"].as_array().unwrap().clone();
    seams::set(
        "dimm_telemetry",
        Box::new(move || modules.iter().map(dimm).collect()) as Box<dyn Fn() -> Vec<DimmTelemetry>>,
    );
    seams::set(
        "spd_identity",
        Box::new(Vec::new) as Box<dyn Fn() -> Vec<crate::memory::ddr5_spd::SpdIdentity>>,
    );
    let agesa = seams["agesa"].as_str().unwrap().to_string();
    seams::set("agesa", Box::new(move || agesa.clone()) as Box<dyn Fn() -> String>);
}

/// The dump script's FakeReader: a register it was not given is a failed
/// read, and the reader says why.
struct ScenarioReader(Registers);

impl SmnTransport for ScenarioReader {
    fn read_many(&self, smn_addresses: &[u32]) -> BTreeMap<u32, Option<u32>> {
        smn_addresses.iter().map(|address| (*address, self.0.get(address).copied())).collect()
    }

    fn last_error(&self) -> String {
        "SMN read refused".into()
    }
}

/// The dump script's FakeTrainingReader.
#[derive(Clone)]
struct ScenarioTraining {
    values: TrainingValues,
    channel_values: BTreeMap<&'static str, TrainingValues>,
    addresses: BTreeMap<&'static str, u64>,
    raw_table: Vec<u8>,
    last_error: String,
}

impl TrainingReader for ScenarioTraining {
    fn read(&mut self) -> Option<TrainingValues> {
        Some(self.values.clone())
    }
    fn raw_table(&self) -> Vec<u8> {
        self.raw_table.clone()
    }
    fn channel_values(&self) -> BTreeMap<&'static str, TrainingValues> {
        self.channel_values.clone()
    }
    fn channel_record_addresses(&self) -> BTreeMap<&'static str, u64> {
        self.addresses.clone()
    }
    fn table_address(&self) -> u64 {
        0x0A20_0000
    }
    fn record_address(&self) -> u64 {
        0x0A20_00D0
    }
    fn last_error(&self) -> String {
        self.last_error.clone()
    }
}

fn training_values(json: &Json) -> TrainingValues {
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

fn scenario_training(json: &Json) -> ScenarioTraining {
    let channel = |key: &str| -> &'static str {
        match key {
            "cha" => "cha",
            "chb" => "chb",
            other => panic!("unexpected channel {other}"),
        }
    };
    ScenarioTraining {
        values: training_values(&json["values"]),
        channel_values: json["channel_values"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(key, values)| (channel(key), training_values(values)))
            .collect(),
        addresses: json["addresses"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(key, address)| (channel(key), address.as_u64().unwrap()))
            .collect(),
        raw_table: hex_bytes(json["raw_table"].as_str().unwrap()),
        last_error: json["last_error"].as_str().unwrap().to_string(),
    }
}

fn runtime_for(inputs: &Json) -> Arc<Am5Runtime> {
    let reader_factory: SmnReaderFactory = match inputs.get("regs_raise").and_then(Json::as_str) {
        Some(message) => {
            let message = message.to_string();
            Box::new(move || Err(message.clone()))
        }
        None => {
            let regs: Registers = inputs["regs"]
                .as_object()
                .unwrap()
                .iter()
                .map(|(key, value)| {
                    (u32::from_str_radix(key.trim_start_matches("0x"), 16).unwrap(), value.as_u64().unwrap() as u32)
                })
                .collect();
            Box::new(move || Ok(Box::new(ScenarioReader(regs.clone())) as Box<dyn SmnTransport>))
        }
    };
    let training_reader_factory: TrainingReaderFactory = match inputs.get("training_raise").and_then(Json::as_str) {
        Some(message) => {
            let message = message.to_string();
            Box::new(move |_layout| Err(message.clone()))
        }
        None => {
            let training = scenario_training(&inputs["training"]);
            Box::new(move |_layout| Ok(Box::new(training.clone()) as Box<dyn TrainingReader>))
        }
    };
    let name = inputs["cpu"].as_str().unwrap().to_string();
    let runtime = Am5Runtime::with_options(Am5RuntimeOptions {
        reader_factory,
        training_reader_factory,
        cpu_name_factory: Box::new(move || name.clone()),
    });
    stub_sources(&runtime, &inputs["sources"]);
    runtime
}

fn stub_sources(runtime: &Am5Runtime, json: &Json) {
    let sources = &runtime.sources;
    let number = |json: &Json, key: &str| json[key].as_u64().unwrap();
    let clocks = &json["clocks"];
    stub_with_status(
        &sources.clocks,
        (!clocks.is_null()).then(|| SmuClocks {
            version: number(clocks, "version") as u32,
            table_base: number(clocks, "table_base"),
            fclk_mhz: float(&clocks["fclk_mhz"]),
            uclk_mhz: float(&clocks["uclk_mhz"]),
            mclk_mhz: float(&clocks["mclk_mhz"]),
            verified: clocks["verified"].as_bool().unwrap(),
        }),
    );
    let voltages = &json["voltages"];
    stub_with_status(
        &sources.voltages,
        (!voltages.is_null()).then(|| SmuVoltages {
            version: number(voltages, "version") as u32,
            table_base: number(voltages, "table_base"),
            values: readings(&voltages["values"]),
        }),
    );
    let power = &json["power"];
    stub_with_status(
        &sources.power,
        (!power.is_null()).then(|| SmuPower {
            version: number(power, "version") as u32,
            table_base: number(power, "table_base"),
            values: readings(&power["values"]),
            limits: readings(&power["limits"]),
            temperatures: readings(&power["temperatures"]),
        }),
    );
    stub_with_status(&sources.dram, readings(&json["dram"]));
    stub_with_status(&sources.board, readings(&json["board"]));
    stub_with_status(&sources.board_monitor, readings(&json["board_monitor"]));
    stub_with_status(&sources.board_temp, readings(&json["board_temp"]));
    stub_with_status(&sources.dimm_temp, readings(&json["dimm_temp"]));
}

fn check(scenario: &str) {
    let fixture = fixture();
    let expected = &fixture["scenarios"][scenario];
    assert!(expected.is_object(), "no scenario {scenario} in the fixture");
    let inputs = &expected["inputs"];
    install_wmi(&inputs["wmi"]);
    install_seams(&inputs["seams"]);
    let runtime = runtime_for(inputs);
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
fn every_scenario_in_the_fixture_has_a_test() {
    let fixture = fixture();
    let names: Vec<&str> = fixture["scenarios"].as_object().unwrap().keys().map(String::as_str).collect();
    let tested = [
        "granite_mixed_lone_channel",
        "granite_fgr_two_channels",
        "raphael_empty_record",
        "raphael_single_record",
        "granite_smn_raises",
        "granite_apob_raises",
        "granite_anchored_ccdl",
        "phoenix_no_training",
        "granite_separators_nothing_plausible",
    ];
    let mut names = names;
    names.sort_unstable();
    let mut tested = tested.to_vec();
    tested.sort_unstable();
    assert_eq!(names, tested);
}

#[test]
fn ryzen_9000_lone_channel_in_mixed_refresh_matches_the_python() {
    check("granite_mixed_lone_channel");
}

#[test]
fn ryzen_9000_channels_that_disagree_match_the_python() {
    check("granite_fgr_two_channels");
}

#[test]
fn ryzen_7000_with_an_empty_record_matches_the_python() {
    check("raphael_empty_record");
}

#[test]
fn ryzen_7000_with_one_record_and_a_saturated_channel_matches_the_python() {
    check("raphael_single_record");
}

#[test]
fn an_smn_transport_that_raises_matches_the_python() {
    check("granite_smn_raises");
}

#[test]
fn an_apob_reader_that_raises_matches_the_python() {
    check("granite_apob_raises");
}

#[test]
fn a_run_found_by_the_register_anchor_matches_the_python() {
    check("granite_anchored_ccdl");
}

#[test]
fn a_processor_without_training_support_matches_the_python() {
    check("phoenix_no_training");
}

#[test]
fn firmware_strings_are_stripped_as_python_strips_them() {
    check("granite_separators_nothing_plausible");
}

#[test]
fn the_os_architecture_split_matches_python_re_search() {
    // re.search(r"\((?:x64|x86)\)\s+", text, re.IGNORECASE).end(), in
    // characters, as py -3.13 gave it on each string.
    for (text, python) in [
        ("Microsoft Windows 11 Pro (x64)\u{a0}24H2", Some(31)),
        ("Windows (X86)\u{3000}\u{3000}Build 1", Some(15)),
        ("W (x64)24H2", None),
        ("W (x64)\x1c\x1d 1", Some(10)),
        ("(x64) (x86) 2", Some(6)),
    ] {
        let rust = os_architecture_end(text).map(|end| text[..end].chars().count());
        assert_eq!(rust, python, "{text:?}");
    }
}
