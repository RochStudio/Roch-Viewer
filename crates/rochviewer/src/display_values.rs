//! Turning a row into display text, shared by the profiles and the window.
//!
//! Lives here rather than in the window so a platform profile can ask these
//! questions while it is still building its own table, and so tests can read
//! a row exactly as the window would without a window.

use crate::hardware::read::read_timing;
use crate::row::{Channel, Formula, ReadSpec, Row, Value};

pub const EM_DASH: &str = "—";
pub const NOT_AVAILABLE: &str = "N/A";

/// Python's `resolve_display_value`.
pub fn resolve_display_value(value: &Value, unavailable: &str) -> String {
    value.resolve(unavailable)
}

/// Tabs that live in their own window rather than the main tab strip.
pub const WINDOWED_TABS: &[&str] = &["Sensors"];

/// The order the standard tabs appear in.
pub const STANDARD_TAB_ORDER: &[&str] = &[
    "System Info",
    "Timings",
    "Training",
    "IMC",
    "RTL",
    "Voltages",
    "Jedec",
];

/// The standard tabs, omitting empty platform-only pages. Summary is always
/// first.
pub fn select_tab_names(rows: &[Row]) -> Vec<String> {
    let mut names = vec!["Summary".to_string()];
    for tab in STANDARD_TAB_ORDER {
        if rows.iter().any(|row| row.tab == *tab) && !WINDOWED_TABS.contains(tab) {
            names.push(tab.to_string());
        }
    }
    names
}

/// How a platform turns a raw register value into text.
///
/// The Intel profile showed a missing read as "N/A", the AMD and unsupported
/// profiles as an em dash; a lookup table with no entry for the value shows
/// "N/A" on both.
pub fn apply_formula(raw: Option<u64>, formula: Option<&Formula>, unavailable: &str) -> String {
    let Some(value) = raw else {
        return unavailable.to_string();
    };
    match formula {
        Some(Formula::Map(map)) => map.get(&value).cloned().unwrap_or_else(|| NOT_AVAILABLE.into()),
        Some(Formula::Func(f)) => f(value),
        None => value.to_string(),
    }
}

/// Normalise resistance values with one space before Ω.
pub fn summary_compact_ohm(value: &str) -> String {
    let text = value.trim();
    if text.is_empty() {
        return String::new();
    }
    let mut text = text.replace(" ohm", "Ω").replace(" Ohm", "Ω");
    text = text.replace('Ω', " Ω");
    while text.contains("  Ω") {
        text = text.replace("  Ω", " Ω");
    }
    text
}

/// One shared value, or an A/B ordered slash pair on mismatch.
pub fn module_pair_text(value_a: &str, value_b: &str) -> String {
    let left = non_empty_or_dash(summary_compact_ohm(value_a));
    let right = non_empty_or_dash(summary_compact_ohm(value_b));
    if left == right { left } else { format!("{left}/{right}") }
}

fn non_empty_or_dash(text: String) -> String {
    if text.is_empty() { EM_DASH.to_string() } else { text }
}

/// Read one channel of a dual row, as the compact tables do.
pub fn read_side(row: &Row, channel: Channel, unavailable: &str) -> String {
    let side = row.side(channel);
    if let Some(value) = side.and_then(|s| s.value.as_ref()) {
        return value.resolve(EM_DASH);
    }
    if let Some(spec) = side.and_then(|s| s.read) {
        return apply_formula(read_timing(&spec), row.formula.as_ref(), unavailable);
    }
    if let Some(value) = &row.value {
        return value.resolve(EM_DASH);
    }
    NOT_AVAILABLE.to_string()
}

/// Read a row for display. `selected` narrows a dual row to one channel;
/// `None` shows both.
pub fn read_value(row: &Row, selected: Option<Channel>, unavailable: &str) -> String {
    if row.is_dual() {
        if let Some(channel) = selected {
            return read_side(row, channel, unavailable);
        }
        let value_a = read_side(row, Channel::A, unavailable);
        let value_b = read_side(row, Channel::B, unavailable);
        if row.lone_channel_alone {
            // A controller setting read on one channel only -- one DIMM, or
            // both on channel A -- is that channel's setting, not a pair with
            // a missing half: "Disabled", not "Disabled/-".
            let a_missing = value_a.trim() == EM_DASH;
            let b_missing = value_b.trim() == EM_DASH;
            if b_missing && !a_missing {
                return summary_compact_ohm(&value_a);
            }
            if a_missing && !b_missing {
                return summary_compact_ohm(&value_b);
            }
        }
        return module_pair_text(&value_a, &value_b);
    }

    match row.read {
        Some(ReadSpec::Dynamic(params)) => {
            let Some(mut raw) = read_timing(&ReadSpec::Dynamic(params)) else {
                return NOT_AVAILABLE.to_string();
            };
            if row.name == "tWR" {
                raw &= 0xF;
            }
            return apply_formula(Some(raw), row.formula.as_ref(), unavailable);
        }
        Some(spec @ ReadSpec::Field(_)) => {
            return apply_formula(read_timing(&spec), row.formula.as_ref(), unavailable);
        }
        None => {}
    }
    if let Some(value) = &row.value {
        return value.resolve(EM_DASH);
    }
    if let Some(default) = &row.default_value {
        return default.clone();
    }
    NOT_AVAILABLE.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::row::{FieldRead, Side};
    use crate::testing::FakeHardware;

    #[test]
    fn tabs_keep_their_order_and_drop_empty_and_windowed() {
        let rows = vec![
            Row::new("a", "c", "RTL", "Left"),
            Row::new("b", "c", "System Info", "Left"),
            Row::new("c", "c", "Sensors", "Left"),
        ];
        assert_eq!(select_tab_names(&rows), vec!["Summary", "System Info", "RTL"]);
    }

    #[test]
    fn compact_ohm() {
        assert_eq!(summary_compact_ohm("40 ohm"), "40 Ω");
        assert_eq!(summary_compact_ohm("40Ω"), "40 Ω");
        assert_eq!(summary_compact_ohm("40  Ω"), "40 Ω");
        assert_eq!(summary_compact_ohm("  "), "");
    }

    #[test]
    fn pair_text_collapses_matches() {
        assert_eq!(module_pair_text("34 ohm", "34 Ω"), "34 Ω");
        assert_eq!(module_pair_text("34", "48"), "34/48");
        assert_eq!(module_pair_text("", "48"), "—/48");
    }

    #[test]
    fn formulas() {
        let map = Formula::map(&[(0, "Disabled"), (1, "Enabled")]);
        assert_eq!(apply_formula(Some(1), Some(&map), "N/A"), "Enabled");
        assert_eq!(apply_formula(Some(7), Some(&map), "N/A"), "N/A");
        assert_eq!(apply_formula(None, Some(&map), EM_DASH), EM_DASH);
        assert_eq!(apply_formula(Some(5), None, "N/A"), "5");
        let double = Formula::func(|v| (v * 2).to_string());
        assert_eq!(apply_formula(Some(5), Some(&double), "N/A"), "10");
    }

    #[test]
    fn dual_rows_read_both_channels() {
        let _hw = FakeHardware::new()
            .with_dword(0x100, 40)
            .with_dword(0x900, 42)
            .install();
        let row = Row::new("tCL", "Primary", "Timings", "Left").with_sides(
            Side::read(ReadSpec::Field(FieldRead::new(0x100, 0, 8))).named("ChA"),
            Side::read(ReadSpec::Field(FieldRead::new(0x900, 0, 8))).named("ChB"),
        );
        assert_eq!(read_value(&row, None, "N/A"), "40/42");
        assert_eq!(read_value(&row, Some(Channel::B), "N/A"), "42");
    }

    #[test]
    fn lone_channel_stands_alone() {
        let mut row = Row::new("RTT", "RTT", "Training", "Left").with_sides(
            Side::value("Disabled".into()),
            Side::value(Value::lazy(|| None)),
        );
        assert_eq!(read_value(&row, None, "N/A"), "Disabled/—");
        row.lone_channel_alone = true;
        assert_eq!(read_value(&row, None, "N/A"), "Disabled");
    }

    #[test]
    fn single_rows_fall_back_in_python_order() {
        let row = Row::new("x", "c", "t", "Left").with_value("fixed");
        assert_eq!(read_value(&row, None, "N/A"), "fixed");
        let mut empty = Row::new("y", "c", "t", "Left");
        assert_eq!(read_value(&empty, None, "N/A"), "N/A");
        empty.default_value = Some("0".into());
        assert_eq!(read_value(&empty, None, "N/A"), "0");
        // a register read with no driver is unavailable in the platform's words
        let read = Row::new("z", "c", "t", "Left").with_field(FieldRead::new(0x10, 0, 4));
        assert_eq!(read_value(&read, None, EM_DASH), EM_DASH);
    }
}
