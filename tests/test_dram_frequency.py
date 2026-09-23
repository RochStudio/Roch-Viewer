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

"""DRAM Frequency does not follow the measured BCLK's wander."""

import unittest
from unittest import mock

from rochviewer.intel import intel_timings


def speed_at(bclk, ratio=40, multiplier=1, gear=1):
    """get_speed on the pre-Arrow-Lake path with 0x5E04 and BCLK stubbed.

    Ratio 40 at the 100 MHz QCLK multiplier in Gear 2 is DDR5-8000.
    """
    fields = {0: ratio, 8: multiplier, 12: gear}

    def read_timing(address, bit_start=0, bit_length=32, read_type=None):
        return fields[bit_start]

    with mock.patch.object(intel_timings, "is_arrow_lake_platform",
                           return_value=False), \
            mock.patch.object(intel_timings, "get_bclk", return_value=bclk), \
            mock.patch.object(intel_timings, "read_timing", read_timing):
        return intel_timings.get_speed()


class DramFrequencyTest(unittest.TestCase):
    def test_the_bclk_wander_does_not_move_the_frequency(self):
        for bclk in (99.98, 99.99, 100.0, 100.02):
            with self.subTest(bclk=bclk):
                self.assertEqual(speed_at(bclk), "8000.0 MHz")

    def test_a_real_bclk_change_still_shows(self):
        self.assertEqual(speed_at(102.5), "8200.0 MHz")


if __name__ == "__main__":
    unittest.main()
