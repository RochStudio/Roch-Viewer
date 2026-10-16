//! A developer fixture: the window's rows from a dump instead of the
//! hardware.
//!
//! Set `ROCHVIEWER_FIXTURE=path.json` on a debug build and the backend is
//! built from the file -- `{"platform": "lga1700-ddr5", "rows": [{tab,
//! category, column, name, label, dual, live, value, ...}]}`, which is what
//! the Python build's table dump writes. A dual row's channels come from
//! `value_a` / `value_b` when the dump carries them; otherwise both sides
//! hold the dumped text, so the all-modules view reads exactly as dumped.
//!
//! Optional keys the window reads are honoured when present: `dim`,
//! `name_a`, `name_b`, `hide_when_blank`, `diagnostic`, `advanced_only`,
//! `show_on_summary`, `lone_channel_alone`, `display_name`.
//!
//! Release builds never look at the variable.

use std::sync::Arc;

use serde_json::Value as Json;

use rochviewer::display_values::{EM_DASH, NOT_AVAILABLE};
use rochviewer::platform_profiles::Platform;
use rochviewer::row::{Extra, Row, Side, Value};
use rochviewer::timings::Backend;

pub const FIXTURE_VAR: &str = "ROCHVIEWER_FIXTURE";

/// The backend from `ROCHVIEWER_FIXTURE`, when it is set and reads.
pub fn backend_from_env() -> Option<Backend> {
    if !cfg!(debug_assertions) {
        return None;
    }
    let path = std::env::var_os(FIXTURE_VAR)?;
    match std::fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|text| backend_from_json(&text)) {
        Ok(backend) => {
            eprintln!("Rows from fixture {} ({} rows)", path.to_string_lossy(), backend.rows.len());
            Some(backend)
        }
        Err(error) => {
            eprintln!("Fixture {} not used: {error}", path.to_string_lossy());
            None
        }
    }
}

pub fn platform_from_id(id: &str) -> Platform {
    [Platform::Lga1700Ddr4, Platform::Lga1700Ddr5, Platform::Lga1851, Platform::Am5]
        .into_iter()
        .find(|p| p.id() == id)
        .unwrap_or(Platform::Unsupported)
}

fn text(value: Option<&Json>) -> Option<String> {
    match value? {
        Json::Null => None,
        Json::String(s) => Some(s.clone()),
        other => Some(other.to_string()),
    }
}

fn flag(value: Option<&Json>) -> bool {
    value.and_then(Json::as_bool).unwrap_or(false)
}

pub fn backend_from_json(source: &str) -> Result<Backend, String> {
    let json: Json = serde_json::from_str(source).map_err(|e| e.to_string())?;
    let platform = platform_from_id(json.get("platform").and_then(Json::as_str).unwrap_or(""));
    let rows = json.get("rows").and_then(Json::as_array).ok_or("no rows")?;
    let rows = rows.iter().map(row_from_json).collect();
    let unavailable = if platform.is_intel() { NOT_AVAILABLE } else { EM_DASH };
    Ok(Backend { platform, rows, unavailable })
}

fn row_from_json(item: &Json) -> Row {
    let field = |key: &str| text(item.get(key)).unwrap_or_default();
    let mut row = Row::new(field("name"), field("category"), field("tab"), field("column"));
    let label = text(item.get("display_name")).or_else(|| text(item.get("label")));
    if let Some(label) = label.filter(|label| *label != row.name) {
        row.display_name = Some(label);
    }
    let value = field("value");
    if flag(item.get("dual")) {
        let a = text(item.get("value_a")).unwrap_or_else(|| value.clone());
        let b = text(item.get("value_b")).unwrap_or_else(|| value.clone());
        let mut side_a = Side::value(Value::text(a));
        let mut side_b = Side::value(Value::text(b));
        side_a.name = text(item.get("name_a"));
        side_b.name = text(item.get("name_b"));
        row = row.with_sides(side_a, side_b);
    } else {
        row = row.with_value(value);
    }
    row.live = flag(item.get("live"));
    row.lone_channel_alone = flag(item.get("lone_channel_alone"));
    if let Some(dim) = item.get("dim").and_then(Json::as_bool) {
        row.dim = Some(Arc::new(move || dim));
    }
    for key in ["hide_when_blank", "diagnostic", "advanced_only"] {
        if flag(item.get(key)) {
            row.extra.insert(key.to_string(), Extra::Bool(true));
        }
    }
    if let Some(show) = item.get("show_on_summary").and_then(Json::as_bool) {
        row.extra.insert("show_on_summary".into(), Extra::Bool(show));
    }
    if let Some(name) = text(item.get("parameter_name")) {
        row.extra.insert("parameter_name".into(), Extra::Str(name));
    }
    if let Some(parent) = text(item.get("parent")) {
        row.extra.insert("Parent".into(), Extra::Str(parent));
    }
    row
}

#[cfg(test)]
mod tests {
    use super::*;

    const DUMP: &str = r#"{"platform": "lga1700-ddr5", "rows": [
        {"tab": "Timings", "category": "Primary", "column": "Left", "name": "tCL", "label": "tCL",
         "dual": true, "live": false, "value": "38/40", "value_a": "38", "value_b": "40"},
        {"tab": "Training", "category": "RTT", "column": "Left", "name": "RTT Wr", "label": "RTT Wr",
         "dual": true, "live": false, "value": "40 RZQ/6"},
        {"tab": "Voltages", "category": "CPU", "column": "Left", "name": "Vcore snapshot", "label": "Vcore",
         "dual": false, "live": false, "value": "1.276 V", "show_on_summary": true, "dim": true}
    ]}"#;

    #[test]
    fn a_dump_becomes_rows_the_window_reads() {
        let backend = backend_from_json(DUMP).unwrap();
        assert_eq!(backend.platform, Platform::Lga1700Ddr5);
        assert_eq!(backend.unavailable, NOT_AVAILABLE);
        let tcl = &backend.rows[0];
        assert!(tcl.is_dual());
        assert_eq!(backend.read_value(tcl, None), "38/40");
        assert_eq!(backend.read_value(tcl, Some(rochviewer::row::Channel::B)), "40");
        // Without separate sides the dumped text is both: "40 RZQ/6" is one
        // value, not a pair to split.
        assert_eq!(backend.read_value(&backend.rows[1], None), "40 RZQ/6");
        let vcore = &backend.rows[2];
        assert_eq!(vcore.label(), "Vcore");
        assert!(vcore.is_dimmed());
        assert!(matches!(vcore.extra.get("show_on_summary"), Some(Extra::Bool(true))));
    }
}
