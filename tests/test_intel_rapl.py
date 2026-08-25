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

"""Cover the MCHBAR RAPL decode and the rate it is turned into."""

import unittest

from rochviewer.intel import intel_rapl
from rochviewer.intel.intel_rapl import (
    ENERGY_STATUS,
    MCHBAR,
    RAPL_POWER_UNIT,
    decode_units,
    energy_rate,
    read_power,
    validate_power,
)

# What the Z790 bench reports: power 1/8 W, energy 1/2^14 J, time 1/2^10 s.
BENCH_UNITS = 0x000A0E03
ENERGY_UNIT = 1.0 / (1 << 14)


class UnitDecodeTest(unittest.TestCase):
    def test_the_bench_unit_register(self):
        units = decode_units(BENCH_UNITS)
        self.assertAlmostEqual(units["power"], 0.125)
        self.assertAlmostEqual(units["energy"], ENERGY_UNIT)
        self.assertAlmostEqual(units["time"], 1.0 / 1024)

    def test_each_field_is_read_from_its_own_bits(self):
        units = decode_units(0x000F1F0F)
        self.assertAlmostEqual(units["power"], 1.0 / (1 << 15))
        self.assertAlmostEqual(units["energy"], 1.0 / (1 << 31))
        self.assertAlmostEqual(units["time"], 1.0 / (1 << 15))


class EnergyRateTest(unittest.TestCase):
    def test_the_bench_idle_reading(self):
        # 716,957 counts a second at 61.035 uJ is the 43.8 W measured against
        # HWiNFO's 42.7 W.
        watts = energy_rate(0, 716957, 1.0, ENERGY_UNIT)
        self.assertAlmostEqual(watts, 43.8, places=1)

    def test_a_wrapped_counter_is_still_a_rate(self):
        # 32 bits at a few hundred watts wraps in about twenty minutes, so
        # this is a normal event and not an error.
        # 0xFFFFFF00 -> 0x000000FF is 0x100 counts to the wrap plus 0xFF after
        # it, so 511, not the 255 the end value alone suggests.
        watts = energy_rate(0xFFFFFF00, 0x000000FF, 1.0, ENERGY_UNIT)
        self.assertAlmostEqual(watts, 511 * ENERGY_UNIT, places=6)
        self.assertGreater(watts, 0)

    def test_no_elapsed_time_is_not_a_rate(self):
        self.assertIsNone(energy_rate(0, 1000, 0.0, ENERGY_UNIT))

    def test_an_impossible_draw_is_dropped(self):
        self.assertIsNone(validate_power(5000.0))
        self.assertIsNone(validate_power(-1.0))
        self.assertIsNone(validate_power(None))
        self.assertEqual(validate_power(43.8), 43.8)


class ReadPowerTest(unittest.TestCase):
    def setUp(self):
        intel_rapl._LAST.clear()
        self.addCleanup(intel_rapl._LAST.clear)

    def _reader(self, energy):
        def read_dword(address):
            if address == MCHBAR + RAPL_POWER_UNIT:
                return BENCH_UNITS
            if address == MCHBAR + ENERGY_STATUS["package"]:
                return energy[0]
            return 0
        return read_dword

    def test_the_first_sample_is_a_baseline_not_a_reading(self):
        energy = [1000]
        clock = [100.0]
        self.assertIsNone(read_power(
            "package", self._reader(energy), lambda: clock[0]))

    def test_the_second_sample_gives_watts(self):
        energy = [0]
        clock = [100.0]
        reader = self._reader(energy)
        read_power("package", reader, lambda: clock[0])
        energy[0] = 716957
        clock[0] = 101.0
        watts = read_power("package", reader, lambda: clock[0])
        self.assertAlmostEqual(watts, 43.8, places=1)

    def test_a_stale_baseline_is_refused(self):
        # The counter wraps in minutes; subtracting from a sample taken before
        # the window was closed would give one confident wrong number.
        energy = [0]
        clock = [100.0]
        reader = self._reader(energy)
        read_power("package", reader, lambda: clock[0])
        energy[0] = 716957
        clock[0] = 100.0 + intel_rapl.MAX_SAMPLE_AGE_S + 1
        self.assertIsNone(read_power("package", reader, lambda: clock[0]))

    def test_an_unreadable_register_reports_nothing(self):
        self.assertIsNone(read_power("package", lambda _a: None, lambda: 1.0))
        self.assertIsNone(read_power("package", lambda _a: 0xFFFFFFFF,
                                     lambda: 1.0))

    def test_an_unknown_domain_reports_nothing(self):
        self.assertIsNone(read_power("gpu", self._reader([0]), lambda: 1.0))

    def test_each_domain_keeps_its_own_baseline(self):
        # Sharing one would make whichever row polled second read the other's
        # delta.
        values = {"package": 0, "cores": 0}

        def read_dword(address):
            if address == MCHBAR + RAPL_POWER_UNIT:
                return BENCH_UNITS
            for domain, offset in ENERGY_STATUS.items():
                if address == MCHBAR + offset:
                    return values[domain]
            return 0

        clock = [100.0]
        read_power("package", read_dword, lambda: clock[0])
        read_power("cores", read_dword, lambda: clock[0])
        values["package"] = 716957
        values["cores"] = 559000
        clock[0] = 101.0
        self.assertAlmostEqual(
            read_power("package", read_dword, lambda: clock[0]), 43.8, places=1)
        self.assertAlmostEqual(
            read_power("cores", read_dword, lambda: clock[0]), 34.1, places=1)


if __name__ == "__main__":
    unittest.main()
