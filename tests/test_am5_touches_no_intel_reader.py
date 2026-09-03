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

"""Reading the AM5 rows must not reach an Intel memory-controller reader.

The dispatcher says this is a safety boundary, and for a year it was true of
importing and false of reading. Three System Info rows reached into the Intel
package for helpers that were never Intel -- the ECAM lookup, the Super I/O
profile, and the DDR generation. The third was the one that bit: asking for
it imported intel_timings, and that module builds its table as its body runs,
so about 2,560 dwords were read out of the hardcoded MCHBAR window on a board
where 0xFEDC0000 belongs to the FCH.

Nothing was visibly wrong, which is the point. Every AM5 reading was correct
the whole time, so only counting the reads shows it.
"""

from __future__ import annotations

import unittest

from rochviewer.platform_profiles import AM5
from rochviewer.timings import ACTIVE_PLATFORM

# The Intel window: MCHBAR and the second controller's aperture above it.
MCHBAR_START = 0xFEDC0000
MCHBAR_END = 0xFEDE0000

# The whole package, not a list of the readers in it. There was an exception
# here while the Super I/O sensors were called intel_board_sensors: the AM5
# board temperatures came out of the Intel package, harmlessly but really.
# Moving them to rochviewer.sensors.board_sensors left nothing to except, and
# a rule with no exceptions is the one worth asserting.
INTEL_PACKAGE = "rochviewer.intel"


def _read_every_row():
    """Build and read every row this platform has, returning the row count."""
    from rochviewer.timings import TIMINGS

    for row in TIMINGS:
        value = row.get("value")
        try:
            value() if callable(value) else value
        except Exception:
            # A row that cannot read is not what this test is about.
            pass
    return len(TIMINGS)


class NoIntelReaderOnAm5Test(unittest.TestCase):
    def setUp(self):
        if ACTIVE_PLATFORM != AM5:
            self.skipTest("not an AM5 platform: %s" % ACTIVE_PLATFORM)

    def test_reading_the_rows_imports_nothing_from_the_intel_package(self):
        """Take the package out of sys.modules, read, and see if it comes back.

        Asserting it is simply absent would be a test of whatever else has run
        in this process: the Intel test modules import it on purpose, and in
        the full suite it is already there. Removing it first makes the
        question the right one -- does *reading the AM5 rows* pull it in --
        and gives the same answer whatever ran before.
        """
        import sys

        removed = [name for name in list(sys.modules)
                   if name == INTEL_PACKAGE
                   or name.startswith(INTEL_PACKAGE + ".")]
        saved = {name: sys.modules.pop(name) for name in removed}

        # The parent package holds each submodule as an attribute too, and
        # "from rochviewer.intel import x" reads it there without consulting
        # sys.modules. intel_stub learned this the hard way.
        package = saved.get(INTEL_PACKAGE)
        attributes = {}
        if package is not None:
            for name in removed:
                attribute = name.rsplit(".", 1)[-1]
                if attribute != INTEL_PACKAGE and hasattr(package, attribute):
                    attributes[attribute] = getattr(package, attribute)

        def restore():
            sys.modules.update(saved)
            for attribute, module in attributes.items():
                setattr(package, attribute, module)

        self.addCleanup(restore)

        _read_every_row()

        came_back = sorted(
            name for name in sys.modules
            if name == INTEL_PACKAGE or name.startswith(INTEL_PACKAGE + ".")
        )
        self.assertEqual(
            came_back, [],
            "reading the AM5 rows imported %s. Importing an Intel module runs "
            "its body, which for the timing table means reading the hardcoded "
            "MCHBAR -- find the row that asked and give it a platform-neutral "
            "home instead." % ", ".join(came_back))

    def test_nothing_reads_the_intel_mchbar_window(self):
        # The count, not the import, is the thing that matters: a future
        # helper could reach the same addresses without importing any of the
        # modules named above.
        import rochviewer.hardware.read as hw

        seen = []
        original = hw.map_physical_address

        def watched(address, size=4):
            if MCHBAR_START <= int(address) < MCHBAR_END:
                seen.append(int(address))
            return original(address, size)

        hw.map_physical_address = watched
        self.addCleanup(setattr, hw, "map_physical_address", original)

        rows = _read_every_row()
        self.assertGreater(rows, 0, "no rows were built to read")
        self.assertEqual(
            seen, [],
            "%d read(s) landed in the Intel MCHBAR window while reading the "
            "AM5 rows, first at 0x%08X. On this board that address space is "
            "the FCH's." % (len(seen), seen[0] if seen else 0))


class GenerationDetectionIsPlatformNeutralTest(unittest.TestCase):
    """The helper that started it, checked on any platform.

    Reaching it must not import the Intel table on any machine -- this runs
    on the Intel bench too, where the table is imported anyway and the
    boundary would go unnoticed.
    """

    def test_it_lives_outside_the_intel_package(self):
        from rochviewer.memory import ddr_generation

        self.assertFalse(ddr_generation.__name__.startswith("rochviewer.intel"))

    def test_the_spd_module_reaches_it_without_the_intel_table(self):
        import inspect

        from rochviewer.memory import ddr5_spd

        source = inspect.getsource(ddr5_spd.installed_generation)
        self.assertNotIn("rochviewer.intel", source)
        self.assertIn("ddr_generation", source)


if __name__ == "__main__":
    unittest.main()
