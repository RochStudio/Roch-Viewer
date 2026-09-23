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

"""On DDR5 the second column reads module B1, and tCCD comes from MR0."""

import unittest
from unittest import mock

from rochviewer.platform_profiles import LGA1700_DDR4, LGA1700_DDR5
from tests import intel_stub


class ChannelBTest(unittest.TestCase):
    def rows_with_both_sides(self, timings):
        return [t for t in timings.TIMINGS
                if isinstance(t.get("dynamic_params_a"), dict)
                and isinstance(t.get("dynamic_params_b"), dict)]

    def test_ddr5_mode_register_rows_read_the_other_sub_channel(self):
        # These were written with MCHBAR2 as the second side: on DDR5 that is
        # MC1 channel A, the same A1 module as the first column.
        timings = intel_stub.install(LGA1700_DDR5)
        try:
            rows = self.rows_with_both_sides(timings)
            self.assertTrue(rows)
            for row in rows:
                with self.subTest(row=row.get("name")):
                    self.assertEqual(row["dynamic_params_a"]["mchbar"],
                                     timings.MCHBAR)
                    self.assertEqual(row["dynamic_params_b"]["mchbar"],
                                     timings.MCHBAR + 0x800)
        finally:
            intel_stub.restore()

    def test_ddr4_keeps_the_second_controller(self):
        timings = intel_stub.install(LGA1700_DDR4)
        try:
            for row in self.rows_with_both_sides(timings):
                with self.subTest(row=row.get("name")):
                    self.assertEqual(row["dynamic_params_b"]["mchbar"],
                                     timings.MCHBAR2)
        finally:
            intel_stub.restore()


class Ddr5CcdShortTest(unittest.TestCase):
    def ccd_s(self, mr0):
        timings = intel_stub.install(LGA1700_DDR5)
        try:
            with mock.patch.object(timings, "read_mode_register",
                                   return_value=mr0):
                return timings._ddr5_ccd_s()
        finally:
            intel_stub.restore()

    def test_half_the_burst_length(self):
        # 0x20 is the bench's MR0: BL16 with CL 38 in the upper bits.
        self.assertEqual(self.ccd_s(0x20), 8)
        self.assertEqual(self.ccd_s(0x21), 8)
        self.assertEqual(self.ccd_s(0x22), 16)

    def test_on_the_fly_bl32_and_an_unread_mr0_stay_empty(self):
        self.assertIsNone(self.ccd_s(0x23))
        self.assertIsNone(self.ccd_s(None))


if __name__ == "__main__":
    unittest.main()
