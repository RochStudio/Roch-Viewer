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


"""The fixture must hand the machine back exactly as it found it.

A stub that does not fully restore does not announce itself. It makes later
modules read the fixture under the real module's name, and the visible effect
is a test that passes alone and skips in the suite -- which reads as "this
bench is not supported" rather than "the fixture is still installed".

That is what happened: the live gear witness, the one test that checks the
measured GEAR2/GEAR4 bit positions against real hardware, skipped in every
full run on the machine those bits were measured on.

Two things kept the stub alive after restore. sys.modules was cleaned but the
parent package still held the module as an attribute, and ``from
rochviewer.intel import intel_timings`` reads that attribute. And
active_platform is lru_cached, so even the right module could keep the
fixture's answer.
"""

import sys
import unittest

from rochviewer.platform_profiles import LGA1700_DDR4, LGA1700_DDR5
from tests.intel_stub import install, restore


def platform_now():
    """What a fresh caller sees, reached the way real code reaches it."""
    from rochviewer.intel import intel_timings
    return intel_timings.active_platform()


class RestoreTest(unittest.TestCase):
    def setUp(self):
        self.before = platform_now()

    def test_the_platform_comes_back(self):
        install(LGA1700_DDR4)
        try:
            self.assertEqual(platform_now(), LGA1700_DDR4)
        finally:
            restore()
        self.assertEqual(platform_now(), self.before)

    def test_it_comes_back_from_a_ddr5_fixture_too(self):
        install(LGA1700_DDR5)
        try:
            self.assertEqual(platform_now(), LGA1700_DDR5)
        finally:
            restore()
        self.assertEqual(platform_now(), self.before)

    def test_the_package_attribute_is_not_left_pointing_at_the_stub(self):
        # The specific leak. sys.modules was being cleaned and this was not,
        # so the name still resolved to the fixture.
        install()
        stub = sys.modules["rochviewer.intel.intel_timings"]
        restore()
        package = sys.modules.get("rochviewer.intel")
        self.assertIsNot(getattr(package, "intel_timings", None), stub)

    def test_repeated_cycles_do_not_drift(self):
        for platform in (LGA1700_DDR4, LGA1700_DDR5, LGA1700_DDR4):
            install(platform)
            self.assertEqual(platform_now(), platform)
            restore()
            self.assertEqual(platform_now(), self.before)


if __name__ == "__main__":
    unittest.main()
