import os
import struct
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from rochviewer.sensors.voltage_rails import (
    CANDIDATE_MAX_VOLTS,
    CANDIDATE_MIN_VOLTS,
    RAILS,
    RAILS_BY_KEY,
    format_volts,
    is_candidate_voltage,
    validate_voltage,
)
from rochviewer.amd.smu_clocks import OFFSET_FCLK, OFFSET_MCLK, OFFSET_UCLK
# The voltage probe is not in this repository: it writes to the SMU
# mailbox and is a bench instrument gated to one exact CPU. The tests
# that exercised it went with it; what is left covers the rail table,
# which is the part a reader depends on.
from rochviewer.amd.smu_voltages import (
    CONFIRMED_VOLTAGE_OFFSETS,
    decode_voltage_float,
    decode_voltages,
)


def as_dword(value):
    return struct.unpack("<I", struct.pack("<f", float(value)))[0]


class VoltageRailTest(unittest.TestCase):
    def test_rail_keys_and_labels_are_unique(self):
        keys = [rail.key for rail in RAILS]
        labels = [rail.label for rail in RAILS]
        self.assertEqual(len(keys), len(set(keys)))
        self.assertEqual(len(labels), len(set(labels)))
        self.assertEqual(len(RAILS_BY_KEY), len(RAILS))

    def test_every_rail_has_a_sane_range(self):
        for rail in RAILS:
            self.assertLess(rail.min_volts, rail.max_volts, rail.label)
            self.assertGreaterEqual(rail.min_volts, CANDIDATE_MIN_VOLTS, rail.label)
            self.assertLessEqual(rail.max_volts, CANDIDATE_MAX_VOLTS, rail.label)

    def test_requested_rail_groups_are_all_present(self):
        groups = {rail.group for rail in RAILS}
        self.assertEqual(groups, {"Core", "SoC", "Fabric", "Memory", "Misc"})

    def test_rails_render_in_the_requested_order(self):
        self.assertEqual(
            [rail.label for rail in RAILS],
            [
                "VDDCR_VDD", "VDDCR_SOC", "VDDP",
                "VDDG CCD", "VDDG IOD", "VDDIO", "VTT",
                "DRAM VDD", "DRAM VDDQ", "DRAM VPP", "VDD_MISC",
            ],
        )

    def test_dram_vpp_band_clears_its_1v8_nominal(self):
        vpp = RAILS_BY_KEY["dram_vpp"]
        validate_voltage(vpp, 1.80)
        self.assertTrue(is_candidate_voltage(1.80))

    def test_validate_voltage_rejects_out_of_range(self):
        soc = RAILS_BY_KEY["vddcr_soc"]
        self.assertAlmostEqual(validate_voltage(soc, 1.20), 1.20)
        with self.assertRaises(ValueError):
            validate_voltage(soc, 1.90)
        with self.assertRaises(ValueError):
            validate_voltage(soc, 0.10)
        with self.assertRaises(ValueError):
            validate_voltage(soc, float("nan"))

    def test_format_volts_uses_millivolt_resolution(self):
        self.assertEqual(format_volts(1.4), "1.400 V")
        self.assertEqual(format_volts(1.23456), "1.235 V")

    def test_is_candidate_voltage_bounds(self):
        self.assertTrue(is_candidate_voltage(1.10))
        self.assertTrue(is_candidate_voltage(CANDIDATE_MIN_VOLTS))
        self.assertTrue(is_candidate_voltage(CANDIDATE_MAX_VOLTS))
        self.assertFalse(is_candidate_voltage(0.05))
        self.assertFalse(is_candidate_voltage(3.3))
        self.assertFalse(is_candidate_voltage(float("inf")))
        self.assertFalse(is_candidate_voltage("nope"))


class VoltageDecodeTest(unittest.TestCase):
    def test_confirmed_offsets_match_the_b850mpower_capture(self):
        # Pinned to the idle/load diff on MSI B850MPOWER + 9850X3D, BIOS 1.A21,
        # PM-table 0x620105. Changing one of these means re-running the probe.
        self.assertEqual(
            CONFIRMED_VOLTAGE_OFFSETS,
            {
                "vddcr_soc": 0x0D8,
                "vddcr_vdd": 0x0C4,
                "cldo_vddq": 0x434,
                "vddg_iod": 0x40C,
                "vddg_ccd": 0x414,
                "cdd_misc": 0x0E8,
            },
        )

    def test_svi3_rails_sit_in_their_own_five_dword_groups(self):
        # The VRM temperatures pin the group boundaries: 0x0D0 reads 33.0 C
        # against HWiNFO's VDDCR_VDD VRM 32 C, and 0x0E4 reads 35.8 C against
        # its VDDCR_SOC VRM 36 C. Each rail is the second dword of its group.
        self.assertEqual(CONFIRMED_VOLTAGE_OFFSETS["vddcr_vdd"], 0x0C4)
        self.assertEqual(CONFIRMED_VOLTAGE_OFFSETS["vddcr_soc"], 0x0D8)
        vdd_group = CONFIRMED_VOLTAGE_OFFSETS["vddcr_vdd"]
        soc_group = CONFIRMED_VOLTAGE_OFFSETS["vddcr_soc"]
        self.assertEqual(soc_group - vdd_group, 0x14)

    def test_vddg_order_matches_the_bios_separation_test(self):
        # Settled by setting IOD to 1.08 V and CCD to 1.06 V:
        #   0x40C  1.04921 -> 1.08216   IOD
        #   0x414  1.04921 -> 1.06250   CCD
        # This is the reverse of the AMD-convention order assumed while both
        # rails read the same value.
        self.assertEqual(CONFIRMED_VOLTAGE_OFFSETS["vddg_iod"], 0x40C)
        self.assertEqual(CONFIRMED_VOLTAGE_OFFSETS["vddg_ccd"], 0x414)

    def test_soc_rail_is_the_delivered_value_not_the_request(self):
        # 0x0D4/0x14C carry the REQUESTED VSOC (1.30000 at 8200, 1.02501 at
        # JEDEC) and are what ZenTimings displays. 0x0D8 is what is actually
        # delivered (1.20287 / 1.01555) and is the number worth showing.
        self.assertEqual(CONFIRMED_VOLTAGE_OFFSETS["vddcr_soc"], 0x0D8)
        self.assertNotIn(CONFIRMED_VOLTAGE_OFFSETS["vddcr_soc"], (0x0D4, 0x14C))

    def test_vddio_is_never_mapped_to_the_pm_table(self):
        # Proven by direct manipulation: CPU VDDIO 1.44 -> 1.47 V moved nothing
        # in the table by 20-40 mV. The cluster that resembled it either held
        # exactly (0x0A8 = 1.3971 both times) or moved the wrong way
        # (0x048: 1.3943 -> 1.3842). VDDIO comes from the Super I/O chip.
        self.assertNotIn("vddio_mem", CONFIRMED_VOLTAGE_OFFSETS)
        for offset in (0x048, 0x09C, 0x0A0, 0x0A8, 0x0B4, 0x0B8, 0x0BC):
            self.assertNotIn(offset, CONFIRMED_VOLTAGE_OFFSETS.values())

    def test_offset_0x04c_is_not_treated_as_the_soc_rail(self):
        # Rose 1.24517 -> 1.27690 V when the profile dropped from 8200 to
        # JEDEC, the opposite direction to every real SoC-side value.
        self.assertNotIn(0x04C, CONFIRMED_VOLTAGE_OFFSETS.values())

    def test_vdd_misc_offsets_are_not_reused_as_the_vddg_pair(self):
        # 0x0E8/0x0EC read 1.100 V and were once mistaken for VDDG. ZenTimings
        # 1.43.0 shows VDD MISC 1.1000 V and VDDG 1.0492 V, so 1.100 V belongs
        # to VDD MISC and the VDDG pair lives at 0x40C/0x414.
        self.assertEqual(CONFIRMED_VOLTAGE_OFFSETS["cdd_misc"], 0x0E8)
        self.assertNotIn(
            CONFIRMED_VOLTAGE_OFFSETS["vddg_ccd"], (0x0E8, 0x0EC)
        )
        self.assertNotIn(
            CONFIRMED_VOLTAGE_OFFSETS["vddg_iod"], (0x0E8, 0x0EC)
        )

    def test_confirmed_offsets_survive_a_memory_profile_change(self):
        # Values read at 8200 MT/s and again at JEDEC 4800, cross-checked
        # against ZenTimings 1.43.0 at both points. A mapping that only holds
        # at one operating point is not confirmed.
        for key, at_8200, at_jedec in (
            ("vddcr_soc", 1.20287, 1.01555),
            ("cldo_vddq", 1.04776, 0.79920),
            ("vddg_ccd", 1.04921, 0.90310),
            ("vddg_iod", 1.04921, 0.90310),
            ("cdd_misc", 1.09999, 1.10000),
        ):
            rail = RAILS_BY_KEY[key]
            validate_voltage(rail, at_8200)
            validate_voltage(rail, at_jedec)

    def test_dimm_pmic_rails_are_never_mapped_to_this_table(self):
        # Proven by BIOS change: DRAM VDD 1.40 -> 1.45 V and VDDQ 1.40 -> 1.42 V
        # left every dword in the table untouched. They are DIMM PMIC rails and
        # cannot come from the SMU PM table at any offset.
        for key in ("dram_vdd", "dram_vddq", "dram_vpp"):
            self.assertNotIn(key, CONFIRMED_VOLTAGE_OFFSETS)

    def test_offset_0x044_is_not_treated_as_the_core_rail(self):
        # It passed a single idle/load diff but ranges 1.34-4.21 V when sampled
        # repeatedly, so it is not a voltage. Regression guard against
        # re-adding it from that one misleading observation.
        self.assertNotIn(0x044, CONFIRMED_VOLTAGE_OFFSETS.values())

    def test_every_confirmed_offset_names_a_real_rail(self):
        for key in CONFIRMED_VOLTAGE_OFFSETS:
            self.assertIn(key, RAILS_BY_KEY)

    def test_unresolved_rails_stay_out_of_the_map(self):
        # These could not be told apart in the capture; they must render as an
        # em dash rather than borrow a neighbouring rail's offset.
        for key in ("vddio_mem", "vtt"):
            self.assertNotIn(key, CONFIRMED_VOLTAGE_OFFSETS)

    def test_captured_values_decode_inside_their_declared_ranges(self):
        # The raw dwords observed at each confirmed offset in the idle capture.
        captured = {
            "vddcr_soc": (0x3F99F7C5, 1.2029),
            "cldo_vddq": (0x3F861D28, 1.0478),
            "vddg_ccd": (0x3F864C5D, 1.0492),
            "vddg_iod": (0x3F864C5D, 1.0492),
            "cdd_misc": (0x3F8CCC55, 1.1000),
        }
        for key, (raw, expected) in captured.items():
            volts = decode_voltage_float(raw)
            self.assertAlmostEqual(volts, expected, places=3, msg=key)
            # Must survive the rail's own range check, not just decode.
            validate_voltage(RAILS_BY_KEY[key], volts)

    def test_observed_rail_swing_stays_inside_the_declared_ranges(self):
        # Widest values seen across the idle, loaded and 8-sample captures.
        # If a range were too tight the rail would silently blank out.
        for key, lo, hi in (
            ("vddcr_soc", 1.1356, 1.2631),
            ("cldo_vddq", 1.0478, 1.0478),
            ("vddg_ccd", 1.0492, 1.0492),
            ("vddg_iod", 1.0492, 1.0492),
            ("cdd_misc", 1.1000, 1.1000),
        ):
            rail = RAILS_BY_KEY[key]
            validate_voltage(rail, lo)
            validate_voltage(rail, hi)

    def test_decode_voltage_float_round_trips(self):
        self.assertAlmostEqual(decode_voltage_float(as_dword(1.35)), 1.35, places=5)

    def test_decode_voltage_float_rejects_non_finite(self):
        with self.assertRaises(ValueError):
            decode_voltage_float(as_dword(float("inf")))

    def test_decode_voltages_keeps_only_in_range_rails(self):
        table = {0x10: as_dword(1.40), 0x14: as_dword(9.90)}
        values = decode_voltages(
            lambda address: table[address],
            0,
            {"vddio_mem": 0x10, "vddcr_soc": 0x14},
        )
        self.assertIn("vddio_mem", values)
        self.assertAlmostEqual(values["vddio_mem"], 1.40, places=5)
        self.assertNotIn("vddcr_soc", values)

    def test_decode_voltages_ignores_unknown_rail_keys(self):
        values = decode_voltages(
            lambda address: as_dword(1.10), 0, {"not_a_rail": 0x10}
        )
        self.assertEqual(values, {})

    def test_decode_voltages_survives_a_failing_read(self):
        def read(address):
            raise OSError("physical read refused")

        self.assertEqual(decode_voltages(read, 0, {"vddio_mem": 0x10}), {})


if __name__ == "__main__":
    unittest.main()
