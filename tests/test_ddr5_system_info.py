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

"""The rows LGA1700 DDR5's System Info adds, drops and rewrites."""

import unittest
from unittest import mock

from rochviewer.gpu.nvidia_gpu import pcie_link_text
from rochviewer.intel import intel_timings
from rochviewer.intel.intel_timings import (
    DDR5_GPU_ROWS, DDR5_SYSTEM_INFO_REMOVED, SYSTEM_INFO_ORDER,
    core_types_text,
)
from rochviewer.platform_profiles import LGA1700_DDR4, LGA1700_DDR5


def on(platform):
    """Resolve the platform afresh as ``platform`` for the with-block."""
    return mock.patch.multiple(
        intel_timings, _LGA1700_DDR5_ACTIVE=None,
        active_platform=lambda: platform)


class CoreTypesTest(unittest.TestCase):
    def test_a_hybrid_part_counts_each_class(self):
        # A 14600KF as sold: six P-cores with two threads, eight E-cores.
        cores = [(1, True)] * 6 + [(0, False)] * 8
        self.assertEqual(core_types_text(cores), "6P + 8E")

    def test_e_cores_off_leaves_two_thread_cores_that_are_all_p_cores(self):
        # This bench: E-cores disabled in the BIOS, so every core shares one
        # class, and each runs two threads, which no E-core does.
        self.assertEqual(core_types_text([(0, True)] * 6), "6P + 0E")

    def test_one_class_without_smt_is_not_guessed(self):
        self.assertIsNone(core_types_text([(0, False)] * 6))
        self.assertIsNone(core_types_text([]))


class PcieLinkTest(unittest.TestCase):
    def test_written_as_gpu_z_writes_it(self):
        self.assertEqual(pcie_link_text(5, 16), "PCIe 5.0 x16")
        self.assertEqual(pcie_link_text(4, 8), "PCIe 4.0 x8")


class UncoreRatioTest(unittest.TestCase):
    def ratio_on(self, platform):
        with on(platform), mock.patch.object(
                intel_timings, "_ring_ratio_value", return_value=50):
            return intel_timings.get_uncore_ratio()

    def test_ddr5_writes_the_whole_number(self):
        self.assertEqual(self.ratio_on(LGA1700_DDR5), "50 x")

    def test_ddr4_is_unchanged(self):
        self.assertEqual(self.ratio_on(LGA1700_DDR4), "50.0 x")


class SectionsTest(unittest.TestCase):
    def test_ddr5_drops_the_moving_ratio_and_the_socket_label(self):
        self.assertEqual(DDR5_SYSTEM_INFO_REMOVED, ("Core Ratio", "CPU Package"))

    def test_the_added_rows_have_their_places(self):
        order = list(SYSTEM_INFO_ORDER)
        self.assertEqual(order[order.index("Cores / Threads") + 1],
                         "Core Types")
        names = [label for label, _field in DDR5_GPU_ROWS]
        self.assertEqual(names, ["PCIe Link", "VBIOS Version"])
        self.assertEqual(
            order[order.index("Resizable BAR") + 1:
                  order.index("Driver Version")],
            names)


if __name__ == "__main__":
    unittest.main()
