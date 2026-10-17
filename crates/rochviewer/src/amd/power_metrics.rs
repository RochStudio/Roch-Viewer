// Roch Viewer -- a read-only memory-controller and timing viewer.
// Copyright (C) 2026 Roch Studio
//
// This file follows ZenStates-Core and ZenTimings by irusanov
// (https://github.com/irusanov), both GPL-3.0. Register numbers, bit fields
// and the bounds applied to decoded values were taken from or checked against
// that work, and the comments below say where. Copyright in those parts
// remains with their authors; this file is distributed under the same licence
// they are, which is what makes that use permitted.
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

//! Power/current limit definitions for the AM5 Power view.
//!
//! Read-only counterpart to the ZenStates limits page: the same PPT / TDC /
//! EDC / Scalar figures, but this project never writes them.
//!
//! Deliberately free of any hardware access so the UI row builder and the
//! tests can use it without the driver.

use crate::amd::py_float::PyFloat;

/// One displayable limit plus the range that makes a reading believable.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PowerMetric {
    pub key: &'static str,
    pub label: &'static str,
    pub unit: &'static str,
    pub min_value: f64,
    pub max_value: f64,
    pub decimals: usize,
}

const fn metric(
    key: &'static str,
    label: &'static str,
    unit: &'static str,
    min_value: f64,
    max_value: f64,
    decimals: usize,
) -> PowerMetric {
    PowerMetric { key, label, unit, min_value, max_value, decimals }
}

/// Rendered top to bottom in this order, matching the ZenStates Power page.
pub const METRICS: &[PowerMetric] = &[
    metric("ppt", "PPT", "W", 1.0, 1000.0, 1),
    metric("tdc", "TDC", "A", 1.0, 1000.0, 1),
    metric("edc", "EDC", "A", 1.0, 1000.0, 1),
    metric("scalar", "Scalar", "x", 0.1, 10.0, 2),
];

/// Python's `METRICS_BY_KEY[key]`.
pub fn metric_by_key(key: &str) -> Option<&'static PowerMetric> {
    METRICS.iter().find(|metric| metric.key == key)
}

/// The reading, or why it is not believable.
pub fn validate_power(metric: &PowerMetric, value: f64) -> Result<f64, String> {
    if !value.is_finite() {
        return Err(format!("{} is not finite", metric.label));
    }
    if !(metric.min_value <= value && value <= metric.max_value) {
        return Err(format!(
            "{} {:.3} is outside {:.2}-{:.2}",
            metric.label, value, metric.min_value, metric.max_value
        ));
    }
    Ok(value)
}

/// Render "current / limit unit", or just the limit when there is no value.
///
/// ZenStates shows the configured limit alone. Showing the live draw next to
/// it costs nothing here and is what a tuner actually watches, so both appear
/// when the value offset is known.
pub fn format_power(metric: &PowerMetric, value: Option<f64>, limit: Option<f64>) -> Option<String> {
    let decimals = metric.decimals;
    match (value, limit) {
        (None, None) => None,
        (None, Some(limit)) => Some(format!("{:.decimals$} {}", PyFloat(limit), metric.unit)),
        (Some(value), None) => Some(format!("{:.decimals$} {}", PyFloat(value), metric.unit)),
        (Some(value), Some(limit)) => {
            Some(format!("{:.decimals$} / {:.decimals$} {}", PyFloat(value), PyFloat(limit), metric.unit))
        }
    }
}

#[cfg(test)]
mod tests {
    // The Python tests for this module live in test_amd_smu_power.py and are
    // ported with the rest of that file in amd::smu_power.

    use super::*;

    #[test]
    fn validation_message_matches_the_python() {
        let ppt = metric_by_key("ppt").unwrap();
        assert_eq!(validate_power(ppt, 5000.0).unwrap_err(), "PPT 5000.000 is outside 1.00-1000.00");
        assert_eq!(validate_power(ppt, f64::NAN).unwrap_err(), "PPT is not finite");
    }
}
