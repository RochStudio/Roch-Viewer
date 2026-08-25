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

"""Ask the running machine whether its two gear witnesses agree.

Every other Intel test runs against tests/intel_stub, which reports a DDR4
fixture. That is the right default -- the suite has to pass on a machine that
is not the target -- but it means a cross-check between two registers can
only ever compare the fixture with itself. This module reads the real
hardware, and skips everywhere it cannot.

The bit positions this relies on were measured rather than guessed; see
SCHEDULER_GEAR2_BIT in intel_timings for the round trip that placed them.

What this catches, and what it does not. It catches the gear row drifting
away from the scheduler -- a moved field at 0x5E04, a broken decode. It
cannot catch a wrong bit assignment in SC_GS_CFG, because in Gear 2 both
bit 31 and bit 0 read 1, so pointing GEAR2 at either one agrees with the
row and this still passes. Checked by mutation, not assumed. Pinning the
positions themselves needs both measured register values, which is what
SchedulerGearWitnessTest in test_timing_labels does; every mutation of the
two constants fails there.
"""

import unittest

from rochviewer.platform_profiles import LGA1700_DDR5, LGA1851

DDR5_PLATFORMS = (LGA1700_DDR5, LGA1851)


def real_intel_timings():
    """The real module, or None when this machine cannot supply one.

    Imported inside the call rather than at module scope: on a machine with
    no driver the import reads physical memory while building its table, and
    collection should not pay that or fail on it.
    """
    try:
        from rochviewer.intel import intel_timings
    except Exception:
        return None
    # Another module may still have a stub installed. The fixture reports
    # DDR4, so the platform check below rejects it along with real DDR4.
    return intel_timings


class GearWitnessTest(unittest.TestCase):
    def setUp(self):
        self.module = real_intel_timings()
        if self.module is None:
            self.skipTest("intel_timings will not import here")
        if self.module.active_platform() not in DDR5_PLATFORMS:
            self.skipTest("not an Intel DDR5 platform")

    def test_the_scheduler_agrees_with_the_gear_row(self):
        # 0x5E04 bits 12-13 against SC_GS_CFG bits 15 and 31. If a future map
        # moves the field the row reads, this is what notices: a duplicate
        # row would print the same wrong answer twice.
        scheduler = self.module.scheduler_gear_mode()
        if scheduler is None:
            self.skipTest("scheduler register unreadable -- no driver?")
        self.assertEqual(self.module.get_gear_mode_value(),
                         "Gear Mode %d" % scheduler)

    def test_the_scheduler_register_is_actually_claimed(self):
        # 0xFFFFFFFF is an unclaimed window, and scheduler_gear_mode reports
        # None for it. Distinguish "no answer because no hardware" from "no
        # answer because both flags read the same", which would be a real
        # disagreement worth seeing rather than a skip.
        raw = self.module.read_physical_memory_int(
            self.module.MCHBAR + self.module.SCHEDULER_CONFIG_OFFSET, 4)
        if raw is None:
            self.skipTest("no driver")
        self.assertNotEqual(int(raw), 0xFFFFFFFF)
        gear2 = int(raw) >> self.module.SCHEDULER_GEAR2_BIT & 1
        gear4 = int(raw) >> self.module.SCHEDULER_GEAR4_BIT & 1
        self.assertNotEqual(
            gear2, gear4,
            "SC_GS_CFG 0x%08X sets both gear flags or neither" % raw)


if __name__ == "__main__":
    unittest.main()
