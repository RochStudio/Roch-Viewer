//! The AMD decoders against the Python ones, over generated inputs.
//!
//! A generator script ran the v1.1.0 Python decoders -- the UMC
//! `decode_channel` and `decode_misc_settings`, both APOB training-block
//! layouts, and the two tCCD_L searches -- over inputs drawn from a fixed
//! seed, and wrote each input beside what Python returned. These run the Rust
//! decoders over the same inputs and require the same answers, key for key.
//!
//! The fixture is frozen: the Python it records is retired. The generator,
//! `crates/rochviewer/src/amd/parity/dump_python_decoders.py`, is in git
//! history at commit 81dbf6b, to be run from the root of a v1.1.0 checkout.

use serde_json::{Map, Value as Json, json};

use super::apob::{
    TrainingValue, TrainingValues, decode_granite_ridge_training_block, decode_raphael_training_block,
    find_ccdl_run, find_ccdl_wr,
};
use super::timings::{DecodedChannel, Registers, UmcValue, decode_channel, decode_misc_settings};

const FIXTURE: &str = include_str!("parity/decoder_parity.json");

fn fixture() -> Json {
    serde_json::from_str(FIXTURE).expect("the decoder fixture parses")
}

fn hex(text: &str) -> Vec<u8> {
    (0..text.len()).step_by(2).map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap()).collect()
}

/// A decoded value as the dump script wrote it: floats as `{"float": repr}`.
fn umc_json(value: &Option<UmcValue>) -> Json {
    match value {
        None => Json::Null,
        Some(UmcValue::Int(number)) => json!(number),
        Some(UmcValue::Bool(flag)) => json!(flag),
        Some(UmcValue::Text(text)) => json!(text),
        Some(UmcValue::Float(number)) => json!({ "float": crate::pyfmt::float_str(*number) }),
    }
}

fn channel_json(decoded: Option<&DecodedChannel>) -> Json {
    match decoded {
        None => Json::Null,
        Some(decoded) => Json::Object(decoded.0.iter().map(|(key, value)| (key.to_string(), umc_json(value))).collect()),
    }
}

fn training_json(values: Option<&TrainingValues>) -> Json {
    match values {
        None => Json::Null,
        Some(values) => Json::Object(
            values
                .iter()
                .map(|(key, value)| {
                    let value = match value {
                        TrainingValue::Text(text) => json!(text),
                        TrainingValue::Raw(byte) => json!(byte),
                    };
                    (key.to_string(), value)
                })
                .collect::<Map<_, _>>(),
        ),
    }
}

/// Every case whose Rust answer is not the Python one, then a failure
/// listing them.
fn require_all(kind: &str, problems: Vec<String>) {
    assert!(problems.is_empty(), "{kind}: {} case(s) differ\n{}", problems.len(), problems.join("\n"));
}

#[test]
fn decode_channel_matches_the_python_on_random_snapshots() {
    let fixture = fixture();
    let mut problems = Vec::new();
    for (index, case) in fixture["umc"].as_array().unwrap().iter().enumerate() {
        let regs: Registers = case["regs"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(key, value)| {
                (u32::from_str_radix(key.trim_start_matches("0x"), 16).unwrap(), value.as_u64().unwrap() as u32)
            })
            .collect();
        let decoded = channel_json(decode_channel(&regs).as_ref());
        if decoded != case["decoded"] {
            problems.push(format!("case {index}:\n  rust   {decoded}\n  python {}", case["decoded"]));
        }
        let misc: Map<String, Json> = decode_misc_settings(&regs)
            .into_iter()
            .map(|(key, value)| (key.to_string(), value.map(Json::String).unwrap_or(Json::Null)))
            .collect();
        if Json::Object(misc.clone()) != case["misc"] {
            problems.push(format!("case {index} misc:\n  rust   {misc:?}\n  python {}", case["misc"]));
        }
    }
    require_all("decode_channel", problems);
}

#[test]
fn the_ryzen_9000_training_block_matches_the_python() {
    let fixture = fixture();
    let mut problems = Vec::new();
    for (index, case) in fixture["granite_blocks"].as_array().unwrap().iter().enumerate() {
        let block = hex(case["block"].as_str().unwrap());
        let decoded = training_json(decode_granite_ridge_training_block(&block).as_ref());
        if decoded != case["decoded"] {
            problems.push(format!("case {index}:\n  rust   {decoded}\n  python {}", case["decoded"]));
        }
    }
    require_all("decode_granite_ridge_training_block", problems);
}

#[test]
fn the_ryzen_7000_training_block_matches_the_python() {
    let fixture = fixture();
    let mut problems = Vec::new();
    for (index, case) in fixture["raphael_blocks"].as_array().unwrap().iter().enumerate() {
        let block = hex(case["block"].as_str().unwrap());
        let extended = case["extended"].as_str().map(hex);
        let decoded = training_json(decode_raphael_training_block(&block, extended.as_deref()).as_ref());
        if decoded != case["decoded"] {
            problems.push(format!("case {index}:\n  rust   {decoded}\n  python {}", case["decoded"]));
        }
    }
    require_all("decode_raphael_training_block", problems);
}

#[test]
fn the_tccd_l_searches_match_the_python() {
    let fixture = fixture();
    let mut problems = Vec::new();
    for (index, case) in fixture["ccdl"].as_array().unwrap().iter().enumerate() {
        let table = hex(case["table"].as_str().unwrap());
        let run = find_ccdl_run(&table).map(|(a, b, c)| json!([a, b, c])).unwrap_or(Json::Null);
        if run != case["run"] {
            problems.push(format!("case {index} run: rust {run}, python {}", case["run"]));
        }
        let anchor = |key: &str| case[key].as_u64().map(|value| value as u32);
        let wr = find_ccdl_wr(&table, anchor("tccdl"), anchor("tccdl_wr2")).map(|wr| json!(wr)).unwrap_or(Json::Null);
        if wr != case["wr"] {
            problems.push(format!("case {index} wr: rust {wr}, python {}", case["wr"]));
        }
    }
    require_all("find_ccdl_run / find_ccdl_wr", problems);
}
