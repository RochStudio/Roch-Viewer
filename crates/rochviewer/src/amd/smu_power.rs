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

//! Fail-closed Granite Ridge PPT/TDC/EDC reader via the RSMU PM-table.
//!
//! READ-ONLY. Unlike ZenStates, which writes these limits through the SMU,
//! this module only decodes them. No write command exists here and none of
//! the RSMU write messages are reachable from this path -- the reader inherits
//! the same permitted commands as the clock and voltage readers: 0x05
//! version, 0x04 address, 0x03 transfer, and 0x6D, the PBO-scalar getter.
//! All four are reads.
//!
//! Offsets follow the same evidence rule as [`super::smu_voltages`]: nothing
//! is decoded until it has been matched against a known-good reference on
//! real hardware.

use std::collections::BTreeMap;
use std::ops::Deref;
use std::sync::Arc;

use crate::hardware::{HwError, HwResult};

use super::power_metrics::{metric_by_key, validate_power};
use super::smu_clocks::{RsmuAccess, RsmuOptions, check_cpu_gate, decode_table_float, shared_rsmu_access};
use super::smu_voltages::RsmuVoltageReader;

/// One metric's `(value offset, limit offset)`. Either may be absent.
pub type PowerOffsets = (Option<u64>, Option<u64>);

/// metric key -> (value offset, limit offset).
///
/// Confirmed on MSI B850MPOWER + Ryzen 7 9850X3D + BIOS 1.A21, PM-table
/// 0x620105, by matching ZenStates 2.0.0 (AM5) against the table and checking
/// that the paired value offset moves under an all-core load:
///
/// ```text
///   PPT  limit 0x008 = 162 W exactly   value 0x00C   41.0 ->  143.7 W
///   TDC  limit 0x020 = 120 A exactly   value 0x024    9.7 ->   98.6 A
///   EDC  limit 0x0FC = 180 A exactly   value 0x100   38.0 ->  100.0 A
/// ```
///
/// The limits match the ZenStates page exactly and hold steady under load,
/// while each paired offset tracks the load -- the documented Zen (limit,
/// value) pairing. 0x028/0x02C is the same shape for the thermal limit, which
/// reads 90.0 C here; see [`CONFIRMED_TEMPERATURE_OFFSETS`].
pub const CONFIRMED_POWER_OFFSETS: &[(&str, PowerOffsets)] = &[
    ("ppt", (Some(0x00C), Some(0x008))),
    ("tdc", (Some(0x024), Some(0x020))),
    ("edc", (Some(0x100), Some(0x0FC))),
    // scalar -- deliberately absent, and it is not an oversight.  0x060 and
    // 0x064 both hold exactly 1.0000, which is far too common a float to claim
    // on a value match.  The scalar does not need the table at all: it has its
    // own SMU getter, which is how ZenStates reads it, so RsmuPowerReader asks
    // firmware directly rather than guessing at an offset.  See
    // RsmuVoltageReader::pbo_scalar_in_run.
];

/// sensor key -> PM-table offset, in degrees Celsius.
///
/// Confirmed on MSI B850MPOWER + Ryzen 7 9850X3D:
///
/// ```text
///   0x02C  CPU (Tctl)   43.7 -> 81.8 C under an all-core load; paired with the
///                       95 C thermal limit at 0x028, the documented Zen
///                       (limit, value) shape.
///   0x0D0  VDDCR_VDD VRM  33.0 C against HWiNFO's 32 C
///   0x0E4  VDDCR_SOC VRM  35.8 C against HWiNFO's 36 C
/// ```
///
/// The two VRM sensors are what pinned the SVI3 voltage group boundaries, so
/// they are the same evidence that placed VDDCR_VDD at 0x0C4.
///
/// ```text
///   0x1A8  IOD Average    38.92 against HWiNFO's 38.1 minimum on a cooled
///                       idle, rising 4.0 under an all-core load against its
///                       3.3. The weakest of the three: its loaded peak read
///                       43.5 against 41.4, further out than the sampling bias
///                       below explains. It is the only candidate in the table
///                       with that behaviour, and it sits below the hotspot,
///                       which it must.
///   0x458  IOD Hotspot    42.21 against 41.8 cooled, 46.1 against 45.8 loaded,
///                       rising 3.5 against 4.0.
///   0x700  L3             29.80 against 29.2 cooled, 46.2 against 46.0 loaded,
///                       rising 16.3 against 16.8 -- a climb nothing else in
///                       the table comes near, which is what makes this one
///                       unmistakable.
/// ```
///
/// All three were matched on the rise as well as on both ends, because
/// absolute values alone picked the wrong offset once: 0x458 peaks within 0.1
/// of the L3 maximum and was briefly taken for it, until the idle end showed
/// it climbing 3.5 where L3 climbs 16.8.
///
/// The comparison is calibrated by the sensors already confirmed. Read in the
/// same episode, they land at Tctl -0.70, VDD VRM -0.20, SOC VRM -0.10 and
/// MISC VRM 0.00 of HWiNFO's own figures, so a candidate inside that band is
/// agreeing as closely as a known-good row does. A peak read against a fresh
/// reset runs about a degree high, since this samples at ~35 Hz and HWiNFO
/// polls every couple of seconds.
///
/// ```text
///   0x0F8  VDD_MISC VRM   confirmed three times against HWiNFO's own row:
///                       32.00 against 32.0 at idle, 35.00 against 35.0 at the
///                       peak of an all-core load, and 35.00 again on a second
///                       load. It is the flattest sensor in the table, which is
///                       why one match would not have been enough.
/// ```
pub const CONFIRMED_TEMPERATURE_OFFSETS: &[(&str, u64)] = &[
    ("cpu", 0x02C),
    ("vdd_vrm", 0x0D0),
    ("soc_vrm", 0x0E4),
    ("misc_vrm", 0x0F8),
    ("iod_average", 0x1A8),
    ("iod_hotspot", 0x458),
    ("l3", 0x700),
    // The limit Tctl is measured against, the paired half of 0x02C. Read
    // rather than assumed: this bench reports 90.0, and HWiNFO's "Thermal
    // Limit 45.3%" is 40.8/90 to the decimal. A hardcoded 95 would have shown
    // five degrees of headroom that are not there.
    ("cpu_limit", 0x028),
];

// A silicon sensor outside this band is a decode error, not a reading.
pub const TEMPERATURE_MIN_C: f64 = 0.0;
pub const TEMPERATURE_MAX_C: f64 = 125.0;

/// Readings keyed by metric or sensor name.
pub type Readings = BTreeMap<&'static str, f64>;

#[derive(Clone, Debug, PartialEq)]
pub struct SmuPower {
    pub version: u32,
    pub table_base: u64,
    /// metric key -> current reading
    pub values: Readings,
    /// metric key -> configured limit
    pub limits: Readings,
    /// sensor key -> degrees Celsius
    pub temperatures: Readings,
}

/// Decode one PM-table dword as a little-endian float.
pub fn decode_power_float(raw_dword: u32) -> Result<f64, String> {
    decode_table_float(raw_dword, "power", true)
}

fn decode_one(
    read_dword: &dyn Fn(u64) -> HwResult<u32>,
    base: u64,
    metric: &super::power_metrics::PowerMetric,
    offset: Option<u64>,
) -> Option<f64> {
    let offset = offset?;
    let raw = read_dword(base + offset).ok()?;
    validate_power(metric, decode_power_float(raw).ok()?).ok()
}

/// Decode the confirmed PM-table temperature sensors, in Celsius. `offsets`
/// of `None` is [`CONFIRMED_TEMPERATURE_OFFSETS`].
pub fn decode_temperatures(
    read_dword: &dyn Fn(u64) -> HwResult<u32>,
    base: u64,
    offsets: Option<&[(&'static str, u64)]>,
) -> Readings {
    let offsets = offsets.unwrap_or(CONFIRMED_TEMPERATURE_OFFSETS);
    let mut values = Readings::new();
    for (key, offset) in offsets {
        let Ok(raw) = read_dword(base + offset) else {
            continue;
        };
        let Ok(celsius) = decode_table_float(raw, "temperature", true) else {
            continue;
        };
        if (TEMPERATURE_MIN_C..=TEMPERATURE_MAX_C).contains(&celsius) {
            values.insert(*key, celsius);
        }
    }
    values
}

/// Decode confirmed metrics from a transferred table: `(values, limits)`.
///
/// Anything that decodes outside its declared range is dropped rather than
/// displayed, keeping the module fail-closed. `offsets` of `None` is
/// [`CONFIRMED_POWER_OFFSETS`].
pub fn decode_power(
    read_dword: &dyn Fn(u64) -> HwResult<u32>,
    base: u64,
    offsets: Option<&[(&'static str, PowerOffsets)]>,
) -> (Readings, Readings) {
    let offsets = offsets.unwrap_or(CONFIRMED_POWER_OFFSETS);
    let (mut values, mut limits) = (Readings::new(), Readings::new());
    for (key, (value_offset, limit_offset)) in offsets {
        let Some(metric) = metric_by_key(key) else {
            continue;
        };
        if let Some(value) = decode_one(read_dword, base, metric, *value_offset) {
            values.insert(*key, value);
        }
        if let Some(limit) = decode_one(read_dword, base, metric, *limit_offset) {
            limits.insert(*key, limit);
        }
    }
    (values, limits)
}

/// Read confirmed power limits from the approved PM-table version only.
///
/// The voltage reader's mailbox session, through `Deref`.
pub struct RsmuPowerReader {
    voltage: RsmuVoltageReader,
}

impl Deref for RsmuPowerReader {
    type Target = RsmuVoltageReader;

    fn deref(&self) -> &RsmuVoltageReader {
        &self.voltage
    }
}

impl RsmuPowerReader {
    pub fn new(access: Arc<dyn RsmuAccess>) -> Self {
        RsmuPowerReader { voltage: RsmuVoltageReader::new(access) }
    }

    pub fn with_options(access: Arc<dyn RsmuAccess>, options: RsmuOptions) -> Result<Self, String> {
        Ok(RsmuPowerReader { voltage: RsmuVoltageReader::with_options(access, options)? })
    }

    /// The confirmed metrics, or `None` with `last_error` saying why.
    /// `offsets` of `None` is [`CONFIRMED_POWER_OFFSETS`].
    pub fn read_power(&self, offsets: Option<&[(&'static str, PowerOffsets)]>) -> Option<SmuPower> {
        self.set_last_error(String::new());
        let offsets = offsets.unwrap_or(CONFIRMED_POWER_OFFSETS);
        if offsets.is_empty() {
            self.set_last_error("No PM-table power offset has been confirmed for this platform".into());
            return None;
        }

        let decode = |read_dword: &dyn Fn(u64) -> HwResult<u32>, base: u64| {
            let (mut values, limits) = decode_power(read_dword, base, Some(offsets));
            if values.is_empty() && limits.is_empty() {
                return Err(HwError::new("no metric decoded inside its valid range"));
            }
            // The scalar is asked for last, after the table values are already
            // in hand, so a firmware that will not answer the getter costs the
            // scalar row and nothing else.
            if let Some(scalar) = self.read_scalar() {
                values.insert("scalar", scalar);
            }
            // Temperatures ride along on the same transfer rather than costing
            // a second mailbox sequence.
            Ok((values, limits, decode_temperatures(read_dword, base, None)))
        };

        let Some((version, base, (values, limits, temperatures))) = self.read_transferred_table(decode) else {
            if self.last_error().is_empty() {
                self.set_last_error("RSMU power read failed".into());
            }
            return None;
        };
        Some(SmuPower { version, table_base: base, values, limits, temperatures })
    }

    /// PBO scalar, from the SMU getter rather than the PM table.
    ///
    /// Any failure -- a firmware that rejects the message, a timeout, a value
    /// outside the metric's range -- blanks the scalar and leaves every other
    /// reading on the run intact.
    fn read_scalar(&self) -> Option<f64> {
        let metric = metric_by_key("scalar")?;
        validate_power(metric, self.pbo_scalar_in_run().ok()?).ok()
    }
}

/// Convenience entry used by Am5Runtime: `None` off Ryzen 9000 or when the
/// read fails.
pub fn read_smu_power(cpu_name: &str) -> Option<SmuPower> {
    if CONFIRMED_POWER_OFFSETS.is_empty() {
        return None;
    }
    if !check_cpu_gate(cpu_name).is_empty() {
        return None;
    }
    let access = shared_rsmu_access().ok()?;
    RsmuPowerReader::new(access).read_power(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::amd::power_metrics::{METRICS, format_power};
    use crate::amd::smn::tests::FakeMutex;
    use crate::amd::smu_clocks::tests::as_dword;
    use crate::amd::smu_clocks::{PERMITTED_COMMANDS, RSMU_PBO_SCALAR_COMMAND};
    use std::sync::Mutex;

    fn metric(key: &str) -> &'static crate::amd::power_metrics::PowerMetric {
        metric_by_key(key).unwrap()
    }

    fn assert_almost_equal(actual: f64, expected: f64, places: i32) {
        assert!(
            crate::pyfmt::round_half_even(actual - expected, places) == 0.0,
            "{actual} != {expected} to {places} places"
        );
    }

    fn table_reader(table: &[(u64, u32)]) -> impl Fn(u64) -> HwResult<u32> + use<> {
        let table: BTreeMap<u64, u32> = table.iter().copied().collect();
        move |address| table.get(&address).copied().ok_or_else(|| HwError(format!("KeyError: {address}")))
    }

    // -- PowerMetricTest -------------------------------------------------------

    #[test]
    fn metrics_render_in_the_zenstates_order() {
        assert_eq!(METRICS.iter().map(|m| m.label).collect::<Vec<_>>(), vec!["PPT", "TDC", "EDC", "Scalar"]);
        assert_eq!(METRICS.iter().map(|m| m.unit).collect::<Vec<_>>(), vec!["W", "A", "A", "x"]);
    }

    #[test]
    fn metric_keys_are_unique() {
        for (i, a) in METRICS.iter().enumerate() {
            assert!(METRICS[i + 1..].iter().all(|b| b.key != a.key));
        }
        assert!(METRICS.iter().all(|m| metric_by_key(m.key) == Some(m)));
    }

    #[test]
    fn validate_power_rejects_out_of_range() {
        let ppt = metric("ppt");
        assert_almost_equal(validate_power(ppt, 162.0).unwrap(), 162.0, 7);
        assert!(validate_power(ppt, 0.0).is_err());
        assert!(validate_power(ppt, 5000.0).is_err());
        assert!(validate_power(ppt, f64::NAN).is_err());
    }

    #[test]
    fn format_shows_current_over_limit() {
        let ppt = metric("ppt");
        assert_eq!(format_power(ppt, Some(41.0), Some(162.0)).as_deref(), Some("41.0 / 162.0 W"));
        assert_eq!(format_power(ppt, None, Some(162.0)).as_deref(), Some("162.0 W"));
        assert_eq!(format_power(ppt, Some(41.0), None).as_deref(), Some("41.0 W"));
        assert_eq!(format_power(ppt, None, None), None);
    }

    #[test]
    fn scalar_uses_two_decimals() {
        assert_eq!(format_power(metric("scalar"), None, Some(1.0)).as_deref(), Some("1.00 x"));
    }

    // -- PowerDecodeTest -------------------------------------------------------

    #[test]
    fn confirmed_offsets_match_the_zenstates_reference() {
        // PPT 162 W, TDC 120 A, EDC 180 A on MSI B850MPOWER + 9850X3D.
        assert_eq!(
            CONFIRMED_POWER_OFFSETS,
            &[
                ("ppt", (Some(0x00C), Some(0x008))),
                ("tdc", (Some(0x024), Some(0x020))),
                ("edc", (Some(0x100), Some(0x0FC))),
            ]
        );
    }

    #[test]
    fn scalar_is_never_claimed_as_a_table_offset() {
        // 0x060 and 0x064 both hold exactly 1.0, which is too common a float to
        // claim on a value match alone. The scalar comes from the SMU getter
        // instead, so it must not reappear here as a guessed offset.
        assert!(CONFIRMED_POWER_OFFSETS.iter().all(|(key, _)| *key != "scalar"));
    }

    #[test]
    fn every_confirmed_key_names_a_real_metric() {
        for (key, _) in CONFIRMED_POWER_OFFSETS {
            assert!(metric_by_key(key).is_some(), "{key}");
        }
    }

    #[test]
    fn decode_power_float_rejects_non_finite() {
        assert_eq!(decode_power_float(as_dword(f32::INFINITY)).unwrap_err(), "non-finite power float");
    }

    #[test]
    fn decode_power_returns_captured_values_and_limits() {
        let read = table_reader(&[
            (0x008, as_dword(162.0)), (0x00C, as_dword(41.0244)),
            (0x020, as_dword(120.0)), (0x024, as_dword(9.6954)),
            (0x0FC, as_dword(180.0)), (0x100, as_dword(38.0)),
        ]);
        let (values, limits) = decode_power(&read, 0, None);
        assert_eq!(limits, Readings::from([("ppt", 162.0), ("tdc", 120.0), ("edc", 180.0)]));
        assert_almost_equal(values["ppt"], 41.0244, 3);
        assert_almost_equal(values["tdc"], 9.6954, 3);
        assert_almost_equal(values["edc"], 38.0, 3);
    }

    #[test]
    fn loaded_values_stay_inside_their_ranges() {
        // Readings observed under an all-core load; if a range were too tight
        // the metric would blank out exactly when it matters.
        for (key, value) in [("ppt", 143.6983), ("tdc", 98.5523), ("edc", 100.0)] {
            assert!(validate_power(metric(key), value).is_ok(), "{key}");
        }
    }

    #[test]
    fn out_of_range_reading_is_dropped_not_shown() {
        let read = table_reader(&[(0x008, as_dword(162.0)), (0x00C, as_dword(9e9))]);
        let (values, limits) = decode_power(&read, 0, Some(&[("ppt", (Some(0x00C), Some(0x008)))]));
        assert_eq!(limits, Readings::from([("ppt", 162.0)]));
        assert!(!values.contains_key("ppt"));
    }

    #[test]
    fn a_failing_read_does_not_raise() {
        let read = |_address: u64| -> HwResult<u32> { Err(HwError::new("physical read refused")) };
        let (values, limits) = decode_power(&read, 0, Some(&[("ppt", (Some(0x00C), Some(0x008)))]));
        assert!(values.is_empty() && limits.is_empty());
    }

    #[test]
    fn unknown_metric_keys_are_ignored() {
        let read = |_address: u64| -> HwResult<u32> { Ok(as_dword(1.0)) };
        let (values, limits) = decode_power(&read, 0, Some(&[("not_a_metric", (Some(0x10), Some(0x14)))]));
        assert!(values.is_empty() && limits.is_empty());
    }

    // -- PboScalarTest: the scalar comes from RSMU 0x6D, not from a PM-table
    //    offset.

    struct FakeAccess {
        arg0: u32,
        response: u32,
        commands: Mutex<Vec<u32>>,
        arguments: Mutex<Vec<Vec<u32>>>,
    }

    impl FakeAccess {
        fn new(arg0: u32, response: u32) -> Arc<Self> {
            Arc::new(FakeAccess { arg0, response, commands: Mutex::new(Vec::new()), arguments: Mutex::new(Vec::new()) })
        }
    }

    impl RsmuAccess for FakeAccess {
        fn is_driver_open(&self) -> bool {
            true
        }
        fn read_vendor(&self) -> HwResult<u32> {
            Ok(0x1022)
        }
        fn capture_selector(&self) -> HwResult<()> {
            Ok(())
        }
        fn selector_captured(&self) -> bool {
            true
        }
        fn read_response(&self) -> HwResult<u32> {
            Ok(self.response)
        }
        fn clear_response(&self) -> HwResult<()> {
            Ok(())
        }
        fn write_arguments(&self, values: &[u32]) -> HwResult<()> {
            self.arguments.lock().unwrap().push(values.to_vec());
            Ok(())
        }
        fn issue_command(&self, command: u32) -> HwResult<()> {
            if !PERMITTED_COMMANDS.contains(&command) {
                return Err(HwError(format!("command 0x{command:02X} is not permitted")));
            }
            self.commands.lock().unwrap().push(command);
            Ok(())
        }
        fn read_arg0(&self) -> HwResult<u32> {
            Ok(self.arg0)
        }
        fn read_arg1(&self) -> HwResult<u32> {
            Ok(0)
        }
        fn restore_selector(&self) -> HwResult<()> {
            Ok(())
        }
        fn read_phys_dword(&self, _address: u64) -> HwResult<u32> {
            Ok(0)
        }
    }

    fn options() -> RsmuOptions {
        RsmuOptions { mutex: Some(Box::new(FakeMutex::default())), sleep: Box::new(|_| {}), ..RsmuOptions::default() }
    }

    fn voltage_reader(access: Arc<FakeAccess>) -> RsmuVoltageReader {
        RsmuVoltageReader::with_options(access, options()).unwrap()
    }

    fn power_reader(access: Arc<FakeAccess>) -> RsmuPowerReader {
        RsmuPowerReader::with_options(access, options()).unwrap()
    }

    #[test]
    fn the_getter_issues_the_scalar_command_with_no_arguments() {
        let access = FakeAccess::new(as_dword(1.0), 1);
        let scalar = voltage_reader(access.clone()).pbo_scalar_in_run().unwrap();
        assert_eq!(*access.commands.lock().unwrap(), vec![0x6D]);
        assert_eq!(*access.arguments.lock().unwrap(), vec![vec![0, 0, 0, 0, 0, 0]]);
        assert_almost_equal(scalar, 1.0, 7);
    }

    #[test]
    fn arg0_is_decoded_as_a_float() {
        let access = FakeAccess::new(as_dword(3.5), 1);
        assert_almost_equal(voltage_reader(access).pbo_scalar_in_run().unwrap(), 3.5, 7);
    }

    #[test]
    fn a_rejected_command_raises_rather_than_decoding_arg0() {
        // A firmware that does not know the message answers non-OK; the row
        // must go blank instead of showing whatever arg0 held.
        let access = FakeAccess::new(as_dword(1.0), 0xFF);
        assert_eq!(
            voltage_reader(access).pbo_scalar_in_run().unwrap_err().0,
            "RSMU command 0x6D returned 0x000000FF"
        );
    }

    #[test]
    fn the_scalar_command_is_permitted_and_nothing_else_is() {
        assert_eq!(RSMU_PBO_SCALAR_COMMAND, 0x6D);
        let mut permitted = PERMITTED_COMMANDS.to_vec();
        permitted.sort_unstable();
        assert_eq!(permitted, vec![0x03, 0x04, 0x05, 0x6D]);
        // Neighbours of the getter are setters in the same message block.
        for forbidden in [0x6C, 0x6E, 0x00, 0xFF] {
            assert!(!PERMITTED_COMMANDS.contains(&forbidden));
        }
    }

    #[test]
    fn a_scalar_outside_its_range_is_dropped() {
        assert_eq!(power_reader(FakeAccess::new(as_dword(99.0), 1)).read_scalar(), None);
    }

    #[test]
    fn a_failing_getter_does_not_take_the_other_metrics_down() {
        assert_eq!(power_reader(FakeAccess::new(as_dword(1.0), 0xFF)).read_scalar(), None);
    }

    // -- PowerIsReadOnlyTest ---------------------------------------------------

    #[test]
    fn module_exposes_no_write_path() {
        let source = include_str!("smu_power.rs");
        let shipped = source.split("#[cfg(test)]").next().unwrap();
        // ZenStates writes these limits; this project must only decode them.
        for forbidden in ["write_arguments", "issue_command", "SetPhysLong", "set_phys_long"] {
            assert!(!shipped.contains(forbidden), "{forbidden}");
        }
    }

    // -- The full run, beyond the Python tests ---------------------------------

    struct TableAccess {
        inner: Arc<FakeAccess>,
        mailbox: crate::amd::smu_clocks::tests::FakeMailbox,
    }

    impl RsmuAccess for TableAccess {
        fn is_driver_open(&self) -> bool {
            true
        }
        fn read_vendor(&self) -> HwResult<u32> {
            Ok(0x1022)
        }
        fn capture_selector(&self) -> HwResult<()> {
            self.mailbox.capture_selector()
        }
        fn selector_captured(&self) -> bool {
            self.mailbox.selector_captured()
        }
        fn read_response(&self) -> HwResult<u32> {
            self.mailbox.read_response()
        }
        fn clear_response(&self) -> HwResult<()> {
            self.mailbox.clear_response()
        }
        fn write_arguments(&self, values: &[u32]) -> HwResult<()> {
            self.mailbox.write_arguments(values)
        }
        fn issue_command(&self, command: u32) -> HwResult<()> {
            self.mailbox.issue_command(command)
        }
        fn read_arg0(&self) -> HwResult<u32> {
            // The scalar getter answers in arg0 like everything else.
            if self.mailbox.commands().last() == Some(&RSMU_PBO_SCALAR_COMMAND) {
                return self.inner.read_arg0();
            }
            self.mailbox.read_arg0()
        }
        fn read_arg1(&self) -> HwResult<u32> {
            self.mailbox.read_arg1()
        }
        fn restore_selector(&self) -> HwResult<()> {
            self.mailbox.restore_selector()
        }
        fn read_phys_dword(&self, address: u64) -> HwResult<u32> {
            self.mailbox.read_phys_dword(address)
        }
    }

    #[test]
    fn a_full_run_carries_values_limits_scalar_and_temperatures() {
        let access = Arc::new(TableAccess {
            inner: FakeAccess::new(as_dword(2.0), 1),
            mailbox: crate::amd::smu_clocks::tests::FakeMailbox::new(
                crate::amd::smu_clocks::EXPECTED_TABLE_VERSION,
                0x3000_0000,
                &[
                    (0x008, as_dword(162.0)), (0x00C, as_dword(41.0)),
                    (0x028, as_dword(90.0)), (0x02C, as_dword(43.5)),
                    (0x700, as_dword(200.0)),
                ],
            ),
        });
        let power = RsmuPowerReader::with_options(access, options()).unwrap().read_power(None).unwrap();
        assert_eq!(power.limits, Readings::from([("ppt", 162.0)]));
        assert_eq!(power.values, Readings::from([("ppt", 41.0), ("scalar", 2.0)]));
        // 0.0 C reads as a reading (the band includes it); 200 C does not.
        assert_eq!(power.temperatures["cpu"], 43.5);
        assert_eq!(power.temperatures["cpu_limit"], 90.0);
        assert!(!power.temperatures.contains_key("l3"));
    }

    #[test]
    fn read_smu_power_is_gated_to_ryzen_9000() {
        assert!(read_smu_power("AMD Ryzen 7 7800X3D 8-Core Processor").is_none());
    }
}
