# Roch Viewer -- a read-only memory-controller and timing viewer.
# Copyright (C) 2026 Roch Studio
#
# This program is free software: you can redistribute it and/or modify
# it under the terms of the GNU General Public License as published by
# the Free Software Foundation, either version 3 of the License, or
# (at your option) any later version.
#
# This program is distributed in the hope that it will be useful,
# but WITHOUT ANY WARRANTY; without even the implied warranty of
# MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
# GNU General Public License for more details.
#
# You should have received a copy of the GNU General Public License
# along with this program.  If not, see <https://www.gnu.org/licenses/>.

"""The DDR5 Summary's labels, units and the rows it leaves to Timings."""

import unittest
from unittest import mock

from rochviewer.intel import intel_timings
from rochviewer.platform_profiles import LGA1700_DDR4, LGA1700_DDR5
from rochviewer.ui.main import (
    INTEL_DDR5_SUMMARY_ADDED, INTEL_DDR5_SUMMARY_OMITTED,
    intel_summary_timing_columns,
)


def on(platform):
    """Resolve the platform afresh as ``platform`` for the with-block."""
    return mock.patch.multiple(
        intel_timings, _LGA1700_DDR5_ACTIVE=None,
        active_platform=lambda: platform)


def rows(*names, category="Tertiary"):
    return [{"name": name, "Category": category, "Tab": "Timings"}
            for name in names]


class SummaryRowsTest(unittest.TestCase):
    TIMINGS = (rows("tCL", "tRTP", "tWR", "tCWL", category="Primary")
               + rows("tREFI", "tREFIx9", "tCKE", "tWRWR_dd", "tMOD",
                      "tRDPRE", "tWRPRE")
               + rows("tXP", category="Power down"))

    def ddr5_columns(self):
        return intel_summary_timing_columns(
            self.TIMINGS, omitted=INTEL_DDR5_SUMMARY_OMITTED,
            added=INTEL_DDR5_SUMMARY_ADDED)

    def test_ddr5_leaves_these_rows_to_timings(self):
        first, tertiary = self.ddr5_columns()
        for name in ("tRDPRE", "tWRPRE", "tREFIx9", "tMOD"):
            with self.subTest(name=name):
                self.assertNotIn(name, first + tertiary)
        self.assertIn("tRTP", first)

    def test_ddr5_puts_txp_under_tcke(self):
        _first, tertiary = self.ddr5_columns()
        self.assertEqual(tertiary[tertiary.index("tCKE") + 1], "tXP")
        self.assertEqual(tertiary.count("tXP"), 1)

    def test_other_platforms_are_unchanged(self):
        _first, tertiary = intel_summary_timing_columns(self.TIMINGS)
        for name in ("tRDPRE", "tWRPRE", "tREFIx9", "tMOD"):
            with self.subTest(name=name):
                self.assertIn(name, tertiary)
        self.assertNotIn("tXP", tertiary)


class DramRateUnitTest(unittest.TestCase):
    def frequency_on(self, platform):
        with on(platform), mock.patch.object(
                intel_timings, "get_speed", return_value="8000.0 MHz"):
            return intel_timings.get_dram_frequency()

    def test_ddr5_reads_as_a_transfer_rate(self):
        # 8000 transfers a second on a 4000 MHz clock.
        self.assertEqual(self.frequency_on(LGA1700_DDR5), "8000 MT/s")

    def test_ddr4_is_unchanged(self):
        self.assertEqual(self.frequency_on(LGA1700_DDR4), "8000 MHz")


class DisplayNameTest(unittest.TestCase):
    """DDR5's labels come from one table, applied once; the rows keep names."""

    def labels_on(self, platform):
        timings = [{"name": name} for name in (
            "QCLK Ratio", "tCCD", "tCCD_L", "ECS Mode", "CMD SlewStatlegen",
            "Realtime Memory")]
        with mock.patch.object(intel_timings, "TIMINGS", timings),                 on(platform):
            intel_timings._present_ddr5_rows()
        return {timing["name"]: timing.get("display_name")
                for timing in timings}

    def test_ddr5_labels(self):
        self.assertEqual(self.labels_on(LGA1700_DDR5), {
            "QCLK Ratio": "QCLK Reference",
            "tCCD": "tCCD_S",
            "tCCD_L": None,
            "ECS Mode": "Manual ECS",
            "CMD SlewStatlegen": "CMD Slew Static Leg",
            "Realtime Memory": "Realtime Memory Timing",
        })

    def test_ddr4_is_unchanged(self):
        self.assertEqual(set(self.labels_on(LGA1700_DDR4).values()), {None})

    def test_the_slew_bit_reads_as_a_switch_wherever_it_is(self):
        timings = [{"name": "CMD SlewStatlegen", "Tab": "IMC",
                    "value": lambda: 1}]
        with mock.patch.object(intel_timings, "TIMINGS", timings),                 on(LGA1700_DDR5):
            intel_timings._present_ddr5_rows()
        self.assertEqual(timings[0]["value"](), "Enabled")


class SteadyClockFormatTest(unittest.TestCase):
    """BCLK and Ring read the same from one start to the next on DDR5."""

    def bclk_on(self, platform, khz):
        with on(platform), mock.patch.object(
                intel_timings, "read_timing", return_value=khz):
            return intel_timings.get_bclk_rd()

    def ring_on(self, platform, khz, ratio=50):
        with on(platform), \
                mock.patch.object(intel_timings, "_ring_ratio_value",
                                  return_value=ratio), \
                mock.patch.object(intel_timings, "read_timing",
                                  return_value=khz):
            return intel_timings.get_ring_freq()

    def test_ddr5_bclk_is_always_two_places(self):
        self.assertEqual(self.bclk_on(LGA1700_DDR5, 100000), "100.00 MHz")
        self.assertEqual(self.bclk_on(LGA1700_DDR5, 99976), "99.98 MHz")

    def test_ddr5_ring_is_whole_megahertz(self):
        self.assertEqual(self.ring_on(LGA1700_DDR5, 100000), "5000 MHz")
        self.assertEqual(self.ring_on(LGA1700_DDR5, 99976), "4999 MHz")

    def test_ddr4_is_unchanged(self):
        self.assertEqual(self.bclk_on(LGA1700_DDR4, 99976), "99.976 MHz")
        self.assertEqual(self.ring_on(LGA1700_DDR4, 99976), "4998.8 MHz")


class CapacityUnitTest(unittest.TestCase):
    def capacity_on(self, platform):
        system = mock.Mock(TotalPhysicalMemory=str(32 * 1024 ** 3))
        with on(platform), mock.patch.object(
                intel_timings, "_wmi_static", return_value=[system]):
            return intel_timings.get_total_physical_memory()

    def test_ddr5_spaces_the_unit(self):
        self.assertEqual(self.capacity_on(LGA1700_DDR5), "32 GB")

    def test_ddr4_is_unchanged(self):
        self.assertEqual(self.capacity_on(LGA1700_DDR4), "32GB")


if __name__ == "__main__":
    unittest.main()
