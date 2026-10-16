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

//! Fail-closed Granite Ridge voltage reader via the RSMU PM-table.
//!
//! This module reuses the exact mailbox flow and the exact approved table
//! version (`0x620105`) already validated in [`super::smu_clocks`]; only the
//! decoded offsets differ.
//!
//! IMPORTANT -- no rail is decoded until its offset has been CONFIRMED on real
//! hardware, because a wrong offset in a memory-tuning tool prints a
//! plausible-looking but false voltage, which is strictly worse than printing
//! nothing. Each entry in [`CONFIRMED_VOLTAGE_OFFSETS`] carries the evidence
//! that identified it; rails without evidence stay out of the map and render
//! as an em dash. To identify more, run `COLLECT_VOLTAGE_REPORT.bat` (see
//! `amd_smu_voltage_probe`) and compare captures.
//!
//! Everything else -- UI rows, ranges, formatting, validation -- is already
//! wired, so landing a new rail is a one-line change here.

use std::collections::BTreeMap;
use std::ops::Deref;
use std::sync::Arc;

use crate::hardware::{HwError, HwResult};
use crate::sensors::voltage_rails::{rail_by_key, validate_voltage};

use super::smu_clocks::{
    ADDRESS_REQUEST_ARGUMENTS, EXPECTED_TABLE_VERSION, PM_TABLE_LENGTH, RSMU_PBO_SCALAR_COMMAND,
    RSMU_TABLE_ADDRESS_COMMAND, RSMU_TABLE_TRANSFER_COMMAND, RsmuAccess, RsmuClockReader, RsmuOptions,
    check_cpu_gate, decode_table_float, shared_rsmu_access,
};
use super::smn::RSMU_TABLE_VERSION_COMMAND;

const ZERO_ARGUMENTS: [u32; 6] = [0, 0, 0, 0, 0, 0];

/// rail key -> PM-table byte offset. Populate ONLY with probe-confirmed values.
///
/// Confirmed on MSI B850MPOWER + Ryzen 7 9850X3D + BIOS 1.A21, PM-table
/// 0x620105, by capturing the table twice -- once idle, once under an
/// all-core load -- and comparing. The header decodes as the documented Zen
/// (limit, value) pairs, which validates the layout before any rail is read:
///
/// ```text
///   0x008/0x00C  PPT   limit 162 W    value  44.3 ->  143.7 W
///   0x020/0x024  TDC   limit 120 A    value  12.5 ->   98.6 A
///   0x028/0x02C  THM   limit  95 C    value  43.7 ->   81.8 C
/// ```
///
/// 0x044/0x048/0x04C are the SVI3 rail triplet that follows those pairs.
/// Each entry was additionally checked over 8 back-to-back captures: a real
/// rail either holds steady or drifts by tens of millivolts, never by volts.
/// Cross-referenced against ZenTimings 1.43.0 read at capture time. Every
/// rail ZenTimings showed was matched to the table within 2.5 mV; the ones it
/// could not be matched to are recorded as absent below.
pub const CONFIRMED_VOLTAGE_OFFSETS: &[(&str, u64)] = &[
    // VSOC as actually DELIVERED.  Confirmed by switching the memory profile,
    // which is the only test that separates the SoC offsets:
    //
    //             8200 MT/s   JEDEC 4800
    //   0x0D4 /
    //   0x14C     1.30000     1.02501     VSOC requested  (ZenTimings shows this)
    //   0x0D8     1.20287     1.01555     VSOC delivered  (this rail)
    //
    // ZenTimings 1.43.0 reads the REQUESTED value, which is why it reported a
    // flat 1.3000 V at 8200 while the chip was really delivering 1.203 V -- the
    // one voltage the user independently reports it gets wrong.
    //
    // Do NOT go back to 0x04C.  It was mapped here once because it looked like
    // a noisy live rail bracketed by 0x0D8 and 0x0D4, but every capture behind
    // that reasoning was taken at the same memory profile.  Across profiles it
    // moves the WRONG WAY: 1.24517 V at 8200 -> 1.27690 V at JEDEC, while every
    // real SoC-side value fell.  Whatever 0x04C is, it is not the SoC rail.
    ("vddcr_soc", 0x0D8),
    // VDDCR_VDD as SVI3 telemetry, matching what HWiNFO labels
    // "CPU VDDCR_VDD Voltage (SVI3 TFN)".  Previously read from the board's
    // Super I/O sensor, which measures the same rail at the VRM and therefore
    // disagrees by tens of millivolts.
    //
    // The PM table lays the SVI3 rails out in fixed five-dword groups, and the
    // VRM temperatures pin the group boundaries independently:
    //
    //   VDD group   0x0C0 0x0C4 volts | 0x0C8 0x0CC amps | 0x0D0 = 33.0 C
    //   SOC group   0x0D4 0x0D8 volts | 0x0DC 0x0E0 amps | 0x0E4 = 35.8 C
    //
    // HWiNFO reports the VDDCR_VDD VRM at 32 C and the VDDCR_SOC VRM at 36 C,
    // matching those two offsets. VDDCR_SOC is the SECOND dword of its group
    // (0x0D8, the delivered value), so VDDCR_VDD takes the second of its own.
    ("vddcr_vdd", 0x0C4),
    // ZenTimings: CLDO VDDP 1.0478 V.  0x434 is the only offset in the table at
    // that value.  Confirmed by BIOS change: setting VDDP to 1.10 V moved this
    // offset 1.04776 -> 1.09806, so the rail behind this row is VDDP.
    ("cldo_vddq", 0x434),
    // ZenTimings: VDDG CCD and VDDG IOD both 1.0492 V.  0x40C and 0x414 both
    // read 1.04921 V with zero spread, and nothing else in the table does.
    //
    // The CCD/IOD ORDER was settled by setting them apart in BIOS -- IOD to
    // 1.08 V, CCD to 1.06 V -- and re-reading:
    //
    //   0x40C  1.04921 -> 1.08216   follows VDDG IOD
    //   0x414  1.04921 -> 1.06250   follows VDDG CCD
    //
    // That is the REVERSE of the AMD-convention ordering used while both rails
    // sat at the same value, which is why it was marked unverified until a
    // capture could actually separate them.
    ("vddg_iod", 0x40C),
    ("vddg_ccd", 0x414),
    // ZenTimings: VDD MISC 1.1000 V.  0x0E8 (duplicated at 0x0EC) reads
    // 1.09999 V with zero spread.  These two offsets were previously mistaken
    // for the VDDG pair purely because 1.100 V looked like a typical VDDG value.
    ("cdd_misc", 0x0E8),
    // Deliberately NOT mapped:
    //
    //   dram_vdd / dram_vddq / dram_vpp -- NOT PRESENT IN THIS TABLE AT ALL.
    //     Proven twice.  First, DRAM VDD was raised 1.40 -> 1.45 V and VDDQ
    //     1.40 -> 1.42 V in BIOS, and after the reboot not one dword in the
    //     0x724-byte table changed.  Second, ZenTimings read MEM VDD 1.4550 V,
    //     MEM VDDQ 1.4250 V and MEM VPP 1.8000 V while a capture was taken, and
    //     none of those values appears anywhere in the table.  These are DIMM
    //     PMIC rails; ZenTimings gets them over SMBus from the SPD hub.
    //     Reading them here needs that same SMBus path -- no offset will do it.
    //
    //   vddio_mem -- ABSENT, proven by direct manipulation.  CPU VDDIO was
    //     raised 1.44 -> 1.47 V in BIOS (+30 mV, chosen as ~15x the 2 mV noise
    //     on these offsets) and after the reboot NOT ONE of the 457 dwords rose
    //     by 20-40 mV.  The cluster that looked like VDDIO behaved as follows:
    //
    //       0x0A8/0x0B4/0x0B8/0x0BC   1.3971 -> 1.3971   did not move at all
    //       0x048/0x09C/0x0A0         1.3943 -> 1.3842   moved the WRONG WAY
    //
    //     It is also not a DDR5 PMIC rail: the PMIC exposes only SWA/SWB/SWC,
    //     VIN_Bulk and the 1.8V/1.0V VOUT rails.
    //
    //     The remaining source is the board's Super I/O sensor chip, which is
    //     where HWiNFO's motherboard-section "CPU VDDIO" reading comes from.
    //     That needs an LPC/Super-I/O reader and per-board scaling; no offset
    //     in this table will ever produce it.
    //
    //   vddcr_vdd -- 0x044 looked like the core rail in a single idle/load diff
    //     (1.0876 -> 0.5261 V), but repeated sampling showed it ranging
    //     1.34-4.21 V.  It is not a voltage.  No other offset was isolated as
    //     the core rail.
    //
    //   cpu_vid, cldo_vddq, cdd_misc -- no offset isolated yet.
];

/// The offset of one confirmed rail.
pub fn confirmed_voltage_offset(key: &str) -> Option<u64> {
    CONFIRMED_VOLTAGE_OFFSETS.iter().find(|(name, _)| *name == key).map(|(_, offset)| *offset)
}

#[derive(Clone, Debug, PartialEq)]
pub struct SmuVoltages {
    pub version: u32,
    pub table_base: u64,
    /// rail key -> volts (only confirmed, in-range rails)
    pub values: BTreeMap<&'static str, f64>,
}

/// Decode one PM-table dword as a little-endian float in volts.
pub fn decode_voltage_float(raw_dword: u32) -> Result<f64, String> {
    decode_table_float(raw_dword, "voltage", true)
}

/// Decode confirmed rails from a transferred table.
///
/// `read_dword` takes an absolute physical address. Rails that decode out of
/// range are dropped rather than displayed, keeping the module fail-closed.
pub fn decode_voltages(
    read_dword: impl Fn(u64) -> HwResult<u32>,
    base: u64,
    offsets: Option<&[(&'static str, u64)]>,
) -> BTreeMap<&'static str, f64> {
    let offsets = offsets.unwrap_or(CONFIRMED_VOLTAGE_OFFSETS);
    let mut values = BTreeMap::new();
    for (key, offset) in offsets {
        let Some(rail) = rail_by_key(key) else {
            continue;
        };
        let Ok(raw) = read_dword(base + offset) else {
            continue;
        };
        let Ok(volts) = decode_voltage_float(raw) else {
            continue;
        };
        if let Ok(volts) = validate_voltage(rail, volts) {
            values.insert(*key, volts);
        }
    }
    values
}

/// Read confirmed voltage rails from the approved PM-table version only.
///
/// Inherits the validated mailbox timing, mutex and selector-restore handling
/// from [`RsmuClockReader`] (through `Deref`) so the privileged sequence stays
/// identical.
pub struct RsmuVoltageReader {
    clock: RsmuClockReader,
}

impl Deref for RsmuVoltageReader {
    type Target = RsmuClockReader;

    fn deref(&self) -> &RsmuClockReader {
        &self.clock
    }
}

impl RsmuVoltageReader {
    pub fn new(access: Arc<dyn RsmuAccess>) -> Self {
        RsmuVoltageReader { clock: RsmuClockReader::new(access) }
    }

    pub fn with_options(access: Arc<dyn RsmuAccess>, options: RsmuOptions) -> Result<Self, String> {
        Ok(RsmuVoltageReader { clock: RsmuClockReader::with_options(access, options)? })
    }

    /// Run the approved mailbox sequence, then hand the table to `decode`.
    ///
    /// `decode(read_dword, base)` is called once, after the version gate and
    /// the transfer command, and must return the caller's result. Anything it
    /// returns as an error is treated as a failed read. Returns
    /// `(version, base, result)` or `None`; `last_error` explains a `None`.
    ///
    /// Shared by every PM-table consumer so the privileged sequence -- vendor
    /// check, selector capture, version gate, bounded transfer, guaranteed
    /// selector restore -- exists in exactly one place.
    pub fn read_transferred_table<T>(
        &self,
        decode: impl FnOnce(&dyn Fn(u64) -> HwResult<u32>, u64) -> HwResult<T>,
    ) -> Option<(u32, u64, T)> {
        self.reset_run();
        let run = || -> HwResult<(u32, u64, T)> {
            self.arm_run_deadline()?;
            self.command(RSMU_TABLE_VERSION_COMMAND, &ZERO_ARGUMENTS)?;
            let version = self.access.read_arg0()?;
            if version != EXPECTED_TABLE_VERSION {
                return Err(HwError(format!(
                    "PM-table version 0x{version:06X} is not the approved 0x{EXPECTED_TABLE_VERSION:06X}"
                )));
            }
            let base = self.request_table_base()?;
            self.command(RSMU_TABLE_TRANSFER_COMMAND, &ZERO_ARGUMENTS)?;
            let read_dword = |address: u64| self.access.read_phys_dword(address);
            Ok((version, base, decode(&read_dword, base)?))
        };
        match self.in_mailbox_session(run) {
            Ok(result) => Some(result),
            Err(error) => {
                if self.last_error().is_empty() {
                    self.set_last_error(format!("RSMU PM-table read failed: {error}"));
                }
                None
            }
        }
    }

    /// The confirmed rails, or `None` with `last_error` saying why. `offsets`
    /// of `None` is [`CONFIRMED_VOLTAGE_OFFSETS`].
    pub fn read_voltages(&self, offsets: Option<&[(&'static str, u64)]>) -> Option<SmuVoltages> {
        self.set_last_error(String::new());
        let offsets = offsets.unwrap_or(CONFIRMED_VOLTAGE_OFFSETS);
        if offsets.is_empty() {
            self.set_last_error("No PM-table voltage offset has been confirmed for this platform".into());
            return None;
        }

        let decode = |read_dword: &dyn Fn(u64) -> HwResult<u32>, base: u64| {
            let values = decode_voltages(read_dword, base, Some(offsets));
            if values.is_empty() {
                return Err(HwError::new("no rail decoded inside its valid range"));
            }
            Ok(values)
        };

        let Some((version, base, values)) = self.read_transferred_table(decode) else {
            if self.last_error().is_empty() {
                self.set_last_error("RSMU voltage read failed".into());
            }
            return None;
        };
        Some(SmuVoltages { version, table_base: base, values })
    }

    /// Ask the SMU for the PBO scalar. Valid only inside an active run.
    ///
    /// Named for the constraint: it must be called from the `decode` callback
    /// of [`RsmuVoltageReader::read_transferred_table`], so it rides on that
    /// run's mailbox session -- selector captured, mutex held, deadline armed
    /// -- rather than opening a second privileged sequence for one number.
    ///
    /// The scalar is not in the PM table. ZenStates reads it the same way,
    /// through the getter at [`RSMU_PBO_SCALAR_COMMAND`]. Firmware that does
    /// not accept the message answers with a non-OK status, which fails out
    /// of the command, so an unrecognised command leaves the row blank
    /// instead of decoding whatever happened to be in arg0.
    pub fn pbo_scalar_in_run(&self) -> HwResult<f64> {
        self.command(RSMU_PBO_SCALAR_COMMAND, &ZERO_ARGUMENTS)?;
        decode_table_float(self.access.read_arg0()?, "scalar", true).map_err(HwError)
    }

    /// Ask firmware for the PM-table base and bound-check it.
    fn request_table_base(&self) -> HwResult<u64> {
        self.command(RSMU_TABLE_ADDRESS_COMMAND, &ADDRESS_REQUEST_ARGUMENTS)?;
        let arg0 = self.access.read_arg0()? as u64;
        let arg1 = self.access.read_arg1()? as u64;
        let base = arg0 | (arg1 << 32);
        if base == 0 || base == u64::MAX {
            return Err(HwError::new("firmware returned an invalid PM-table base"));
        }
        if base > (1 << 48) {
            return Err(HwError::new("PM-table base exceeds phys address width"));
        }
        if base + PM_TABLE_LENGTH > (1 << 48) {
            return Err(HwError::new("PM-table range is not mappable"));
        }
        Ok(base)
    }
}

/// Convenience entry used by Am5Runtime: `None` off Ryzen 9000 or when the
/// read fails.
pub fn read_smu_voltages(cpu_name: &str) -> Option<SmuVoltages> {
    if CONFIRMED_VOLTAGE_OFFSETS.is_empty() {
        return None;
    }
    if !check_cpu_gate(cpu_name).is_empty() {
        return None;
    }
    let access = shared_rsmu_access().ok()?;
    RsmuVoltageReader::new(access).read_voltages(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::amd::smn::tests::FakeMutex;
    use crate::amd::smu_clocks::tests::{FakeMailbox, as_dword};
    use crate::amd::smu_clocks::{OFFSET_FCLK, OFFSET_MCLK, OFFSET_UCLK};
    use crate::sensors::voltage_rails::{CANDIDATE_MAX_VOLTS, CANDIDATE_MIN_VOLTS, RAILS, format_volts, is_candidate_voltage};

    // The voltage probe is not in this repository: it writes to the SMU
    // mailbox and is a bench instrument gated to one exact CPU. The tests
    // that exercised it went with it; what is left covers the rail table,
    // which is the part a reader depends on.

    fn rail(key: &str) -> &'static crate::sensors::voltage_rails::VoltageRail {
        rail_by_key(key).unwrap()
    }

    fn offsets() -> Vec<u64> {
        CONFIRMED_VOLTAGE_OFFSETS.iter().map(|(_, offset)| *offset).collect()
    }

    fn assert_almost_equal(actual: f64, expected: f64, places: i32) {
        assert!(
            crate::pyfmt::round_half_even(actual - expected, places) == 0.0,
            "{actual} != {expected} to {places} places"
        );
    }

    // -- VoltageRailTest -----------------------------------------------------

    #[test]
    fn rail_keys_and_labels_are_unique() {
        for (i, a) in RAILS.iter().enumerate() {
            for b in &RAILS[i + 1..] {
                assert_ne!(a.key, b.key);
                assert_ne!(a.label, b.label);
            }
        }
        assert!(RAILS.iter().all(|r| rail_by_key(r.key) == Some(r)));
    }

    #[test]
    fn every_rail_has_a_sane_range() {
        for rail in RAILS {
            assert!(rail.min_volts < rail.max_volts, "{}", rail.label);
            assert!(rail.min_volts >= CANDIDATE_MIN_VOLTS, "{}", rail.label);
            assert!(rail.max_volts <= CANDIDATE_MAX_VOLTS, "{}", rail.label);
        }
    }

    #[test]
    fn requested_rail_groups_are_all_present() {
        let groups: std::collections::BTreeSet<&str> = RAILS.iter().map(|r| r.group).collect();
        assert_eq!(groups, ["Core", "SoC", "Fabric", "Memory", "Misc"].into_iter().collect());
    }

    #[test]
    fn rails_render_in_the_requested_order() {
        assert_eq!(
            RAILS.iter().map(|r| r.label).collect::<Vec<_>>(),
            vec![
                "VDDCR_VDD", "VDDCR_SOC", "VDDP",
                "VDDG CCD", "VDDG IOD", "VDDIO", "VTT",
                "DRAM VDD", "DRAM VDDQ", "DRAM VPP", "VDD_MISC",
            ]
        );
    }

    #[test]
    fn dram_vpp_band_clears_its_1v8_nominal() {
        assert!(validate_voltage(rail("dram_vpp"), 1.80).is_ok());
        assert!(is_candidate_voltage(1.80));
    }

    #[test]
    fn validate_voltage_rejects_out_of_range() {
        let soc = rail("vddcr_soc");
        assert_almost_equal(validate_voltage(soc, 1.20).unwrap(), 1.20, 7);
        assert!(validate_voltage(soc, 1.90).is_err());
        assert!(validate_voltage(soc, 0.10).is_err());
        assert!(validate_voltage(soc, f64::NAN).is_err());
    }

    #[test]
    fn format_volts_uses_millivolt_resolution() {
        assert_eq!(format_volts(1.4), "1.400 V");
        assert_eq!(format_volts(1.23456), "1.235 V");
    }

    #[test]
    fn is_candidate_voltage_bounds() {
        assert!(is_candidate_voltage(1.10));
        assert!(is_candidate_voltage(CANDIDATE_MIN_VOLTS));
        assert!(is_candidate_voltage(CANDIDATE_MAX_VOLTS));
        assert!(!is_candidate_voltage(0.05));
        assert!(!is_candidate_voltage(3.3));
        assert!(!is_candidate_voltage(f64::INFINITY));
        // Python also refused the string "nope"; a non-number cannot reach
        // this function in Rust.
    }

    // -- VoltageDecodeTest ---------------------------------------------------

    #[test]
    fn confirmed_offsets_match_the_b850mpower_capture() {
        // Pinned to the idle/load diff on MSI B850MPOWER + 9850X3D, BIOS 1.A21,
        // PM-table 0x620105. Changing one of these means re-running the probe.
        let map: BTreeMap<&str, u64> = CONFIRMED_VOLTAGE_OFFSETS.iter().copied().collect();
        let expected: BTreeMap<&str, u64> = [
            ("vddcr_soc", 0x0D8),
            ("vddcr_vdd", 0x0C4),
            ("cldo_vddq", 0x434),
            ("vddg_iod", 0x40C),
            ("vddg_ccd", 0x414),
            ("cdd_misc", 0x0E8),
        ]
        .into_iter()
        .collect();
        assert_eq!(map, expected);
        assert_eq!(map.len(), CONFIRMED_VOLTAGE_OFFSETS.len());
    }

    #[test]
    fn svi3_rails_sit_in_their_own_five_dword_groups() {
        // The VRM temperatures pin the group boundaries: 0x0D0 reads 33.0 C
        // against HWiNFO's VDDCR_VDD VRM 32 C, and 0x0E4 reads 35.8 C against
        // its VDDCR_SOC VRM 36 C. Each rail is the second dword of its group.
        assert_eq!(confirmed_voltage_offset("vddcr_vdd"), Some(0x0C4));
        assert_eq!(confirmed_voltage_offset("vddcr_soc"), Some(0x0D8));
        let vdd_group = confirmed_voltage_offset("vddcr_vdd").unwrap();
        let soc_group = confirmed_voltage_offset("vddcr_soc").unwrap();
        assert_eq!(soc_group - vdd_group, 0x14);
    }

    #[test]
    fn vddg_order_matches_the_bios_separation_test() {
        // Settled by setting IOD to 1.08 V and CCD to 1.06 V:
        //   0x40C  1.04921 -> 1.08216   IOD
        //   0x414  1.04921 -> 1.06250   CCD
        // This is the reverse of the AMD-convention order assumed while both
        // rails read the same value.
        assert_eq!(confirmed_voltage_offset("vddg_iod"), Some(0x40C));
        assert_eq!(confirmed_voltage_offset("vddg_ccd"), Some(0x414));
    }

    #[test]
    fn soc_rail_is_the_delivered_value_not_the_request() {
        // 0x0D4/0x14C carry the REQUESTED VSOC (1.30000 at 8200, 1.02501 at
        // JEDEC) and are what ZenTimings displays. 0x0D8 is what is actually
        // delivered (1.20287 / 1.01555) and is the number worth showing.
        let soc = confirmed_voltage_offset("vddcr_soc").unwrap();
        assert_eq!(soc, 0x0D8);
        assert!(![0x0D4, 0x14C].contains(&soc));
    }

    #[test]
    fn vddio_is_never_mapped_to_the_pm_table() {
        // Proven by direct manipulation: CPU VDDIO 1.44 -> 1.47 V moved nothing
        // in the table by 20-40 mV. The cluster that resembled it either held
        // exactly (0x0A8 = 1.3971 both times) or moved the wrong way
        // (0x048: 1.3943 -> 1.3842). VDDIO comes from the Super I/O chip.
        assert_eq!(confirmed_voltage_offset("vddio_mem"), None);
        for offset in [0x048, 0x09C, 0x0A0, 0x0A8, 0x0B4, 0x0B8, 0x0BC] {
            assert!(!offsets().contains(&offset));
        }
    }

    #[test]
    fn offset_0x04c_is_not_treated_as_the_soc_rail() {
        // Rose 1.24517 -> 1.27690 V when the profile dropped from 8200 to
        // JEDEC, the opposite direction to every real SoC-side value.
        assert!(!offsets().contains(&0x04C));
    }

    #[test]
    fn vdd_misc_offsets_are_not_reused_as_the_vddg_pair() {
        // 0x0E8/0x0EC read 1.100 V and were once mistaken for VDDG. ZenTimings
        // 1.43.0 shows VDD MISC 1.1000 V and VDDG 1.0492 V, so 1.100 V belongs
        // to VDD MISC and the VDDG pair lives at 0x40C/0x414.
        assert_eq!(confirmed_voltage_offset("cdd_misc"), Some(0x0E8));
        assert!(![0x0E8, 0x0EC].contains(&confirmed_voltage_offset("vddg_ccd").unwrap()));
        assert!(![0x0E8, 0x0EC].contains(&confirmed_voltage_offset("vddg_iod").unwrap()));
    }

    #[test]
    fn confirmed_offsets_survive_a_memory_profile_change() {
        // Values read at 8200 MT/s and again at JEDEC 4800, cross-checked
        // against ZenTimings 1.43.0 at both points. A mapping that only holds
        // at one operating point is not confirmed.
        for (key, at_8200, at_jedec) in [
            ("vddcr_soc", 1.20287, 1.01555),
            ("cldo_vddq", 1.04776, 0.79920),
            ("vddg_ccd", 1.04921, 0.90310),
            ("vddg_iod", 1.04921, 0.90310),
            ("cdd_misc", 1.09999, 1.10000),
        ] {
            assert!(validate_voltage(rail(key), at_8200).is_ok(), "{key}");
            assert!(validate_voltage(rail(key), at_jedec).is_ok(), "{key}");
        }
    }

    #[test]
    fn dimm_pmic_rails_are_never_mapped_to_this_table() {
        // Proven by BIOS change: DRAM VDD 1.40 -> 1.45 V and VDDQ 1.40 -> 1.42 V
        // left every dword in the table untouched. They are DIMM PMIC rails and
        // cannot come from the SMU PM table at any offset.
        for key in ["dram_vdd", "dram_vddq", "dram_vpp"] {
            assert_eq!(confirmed_voltage_offset(key), None);
        }
    }

    #[test]
    fn offset_0x044_is_not_treated_as_the_core_rail() {
        // It passed a single idle/load diff but ranges 1.34-4.21 V when sampled
        // repeatedly, so it is not a voltage. Regression guard against
        // re-adding it from that one misleading observation.
        assert!(!offsets().contains(&0x044));
    }

    #[test]
    fn every_confirmed_offset_names_a_real_rail() {
        for (key, _) in CONFIRMED_VOLTAGE_OFFSETS {
            assert!(rail_by_key(key).is_some(), "{key}");
        }
    }

    #[test]
    fn unresolved_rails_stay_out_of_the_map() {
        // These could not be told apart in the capture; they must render as an
        // em dash rather than borrow a neighbouring rail's offset.
        for key in ["vddio_mem", "vtt"] {
            assert_eq!(confirmed_voltage_offset(key), None);
        }
    }

    #[test]
    fn captured_values_decode_inside_their_declared_ranges() {
        // The raw dwords observed at each confirmed offset in the idle capture.
        for (key, raw, expected) in [
            ("vddcr_soc", 0x3F99_F7C5u32, 1.2029),
            ("cldo_vddq", 0x3F86_1D28, 1.0478),
            ("vddg_ccd", 0x3F86_4C5D, 1.0492),
            ("vddg_iod", 0x3F86_4C5D, 1.0492),
            ("cdd_misc", 0x3F8C_CC55, 1.1000),
        ] {
            let volts = decode_voltage_float(raw).unwrap();
            assert_almost_equal(volts, expected, 3);
            // Must survive the rail's own range check, not just decode.
            assert!(validate_voltage(rail(key), volts).is_ok(), "{key}");
        }
    }

    #[test]
    fn observed_rail_swing_stays_inside_the_declared_ranges() {
        // Widest values seen across the idle, loaded and 8-sample captures.
        // If a range were too tight the rail would silently blank out.
        for (key, lo, hi) in [
            ("vddcr_soc", 1.1356, 1.2631),
            ("cldo_vddq", 1.0478, 1.0478),
            ("vddg_ccd", 1.0492, 1.0492),
            ("vddg_iod", 1.0492, 1.0492),
            ("cdd_misc", 1.1000, 1.1000),
        ] {
            assert!(validate_voltage(rail(key), lo).is_ok(), "{key}");
            assert!(validate_voltage(rail(key), hi).is_ok(), "{key}");
        }
    }

    #[test]
    fn decode_voltage_float_round_trips() {
        assert_almost_equal(decode_voltage_float(as_dword(1.35)).unwrap(), 1.35, 5);
    }

    #[test]
    fn decode_voltage_float_rejects_non_finite() {
        assert_eq!(decode_voltage_float(as_dword(f32::INFINITY)).unwrap_err(), "non-finite voltage float");
    }

    #[test]
    fn decode_voltages_keeps_only_in_range_rails() {
        let table: BTreeMap<u64, u32> = [(0x10, as_dword(1.40)), (0x14, as_dword(9.90))].into_iter().collect();
        let values =
            decode_voltages(|address| Ok(table[&address]), 0, Some(&[("vddio_mem", 0x10), ("vddcr_soc", 0x14)]));
        assert_almost_equal(values["vddio_mem"], 1.40, 5);
        assert!(!values.contains_key("vddcr_soc"));
    }

    #[test]
    fn decode_voltages_ignores_unknown_rail_keys() {
        let values = decode_voltages(|_| Ok(as_dword(1.10)), 0, Some(&[("not_a_rail", 0x10)]));
        assert!(values.is_empty());
    }

    #[test]
    fn decode_voltages_survives_a_failing_read() {
        let values = decode_voltages(|_| Err(HwError::new("physical read refused")), 0, Some(&[("vddio_mem", 0x10)]));
        assert!(values.is_empty());
    }

    // -- The clock offsets this file's Python imported, kept in view ---------

    #[test]
    fn clock_offsets_do_not_collide_with_any_rail() {
        for offset in [OFFSET_FCLK, OFFSET_UCLK, OFFSET_MCLK] {
            assert!(!offsets().contains(&offset));
        }
    }

    // -- The full mailbox run, on the clock tests' fake mailbox --------------

    fn voltage_reader(mailbox: Arc<FakeMailbox>) -> RsmuVoltageReader {
        let options = RsmuOptions { mutex: Some(Box::new(FakeMutex::default())), ..RsmuOptions::default() };
        RsmuVoltageReader::with_options(mailbox, options).unwrap()
    }

    #[test]
    fn a_full_run_decodes_the_confirmed_rails() {
        let mailbox = Arc::new(FakeMailbox::new(
            EXPECTED_TABLE_VERSION,
            0x2000_0000,
            &[(0x0D8, 0x3F99_F7C5), (0x434, 0x3F86_1D28)],
        ));
        let result = voltage_reader(mailbox.clone()).read_voltages(None).unwrap();
        assert_eq!(result.version, EXPECTED_TABLE_VERSION);
        assert_eq!(result.table_base, 0x2000_0000);
        assert_eq!(result.values.keys().copied().collect::<Vec<_>>(), vec!["cldo_vddq", "vddcr_soc"]);
        assert_eq!(mailbox.commands(), vec![0x05, 0x04, 0x03]);
    }

    #[test]
    fn another_table_version_is_refused_before_the_transfer() {
        let mailbox = Arc::new(FakeMailbox::new(0x540108, 0x2000_0000, &[]));
        let reader = voltage_reader(mailbox.clone());
        assert!(reader.read_voltages(None).is_none());
        assert_eq!(
            reader.last_error(),
            "RSMU PM-table read failed: PM-table version 0x540108 is not the approved 0x620105"
        );
        assert_eq!(mailbox.commands(), vec![0x05]);
    }

    #[test]
    fn nothing_in_range_is_a_failed_read() {
        let mailbox = Arc::new(FakeMailbox::new(EXPECTED_TABLE_VERSION, 0x2000_0000, &[]));
        let reader = voltage_reader(mailbox);
        assert!(reader.read_voltages(None).is_none());
        assert_eq!(reader.last_error(), "RSMU PM-table read failed: no rail decoded inside its valid range");
        assert!(reader.read_voltages(Some(&[])).is_none());
        assert_eq!(reader.last_error(), "No PM-table voltage offset has been confirmed for this platform");
    }

    #[test]
    fn read_smu_voltages_is_gated_to_ryzen_9000() {
        assert!(read_smu_voltages("AMD Ryzen 7 7800X3D 8-Core Processor").is_none());
    }
}
