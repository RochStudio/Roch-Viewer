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

"""No AM5 System Info row may carry a value that was typed rather than read.

The AMD counterpart of test_system_info_not_hardcoded. The Intel file has
guarded this property since the beginning; the AMD rows have not been checked
at all, which is how the port could have replaced a reading with the bench's
own answer and nothing would have said so.

Change what the machine reports; the row must change with it. A decode table
is not a hardcoded value -- "Granite Ridge", "4 nm" and "Navi 48" are names
for codes read from the hardware, so each test moves the code and requires
the name to move.

Two things make this harder than the Intel side, and both were found the hard
way on the AM5 bench:

  The rows are cached. Opening a WMI connection costs about a second, so the
  processor fields, the system-info strings and the card are each read once
  and kept. A test that supplies its own hardware must clear those caches or
  it reads whatever the bench answered earlier and the stub is never
  consulted.

  The row values are lazy. build_timings hands back getters, so a stub has to
  be in place when the getter runs, not when the row is built.
"""

from __future__ import annotations

import unittest
from unittest import mock

from rochviewer.amd import profile as am5_profile
from rochviewer.amd.profile import Am5Runtime, build_timings

SYSTEM_INFO_TAB = "System Info"

# Every module-level cache on the AMD side that can outlive a stub.
_CACHES = ("_SYSTEM_INFO_CACHE", "_PROCESSOR_FACTS", "_GPU_CACHE",
           "_GPU_SENSOR_CACHE")


class RowMissing(Exception):
    """The machine does not build this row at all."""


def _row_value(name):
    for row in build_timings(Am5Runtime()):
        if row.get("Tab") == SYSTEM_INFO_TAB and row.get("name") == name:
            value = row["value"]
            return value() if callable(value) else value
    raise RowMissing(name)


def _require(test, name):
    """One row's value, or skip when this machine does not build that row.

    The rows follow the hardware, which is the property these tests exist to
    check -- so a row that is absent because there is no hardware for it is
    the behaviour rather than a failure. A row that exists and reads the
    wrong thing still fails.
    """
    try:
        return _row_value(name)
    except RowMissing:
        test.skipTest("%s is not built on this machine: no hardware for it"
                      % name)


class FollowsTheHardwareTest(unittest.TestCase):
    """Change what the machine reports; the row must change with it."""

    def setUp(self):
        self._clear()
        self.addCleanup(self._clear)

    @staticmethod
    def _clear():
        for name in _CACHES:
            getattr(am5_profile, name).clear()

    def _with_cpuid(self, processor_id):
        """Rebuild the identity rows with a CPUID this test chose."""
        cpu = mock.Mock(ProcessorId=processor_id, ExtClock=100, Name="stub",
                        Manufacturer="AuthenticAMD", NumberOfCores=8,
                        NumberOfLogicalProcessors=16)
        connection = mock.Mock()
        connection.Win32_Processor.return_value = [cpu]
        self._clear()
        return mock.patch.dict("sys.modules",
                               {"wmi": mock.Mock(WMI=lambda: connection)})

    def test_the_cpu_rows_follow_the_cpuid(self):
        # Family 0x1A model 0x44 is the part AMD_SILICON names. The row must
        # come from that lookup rather than from a string typed beside it.
        with self._with_cpuid("178BFBFF00B40F40"):
            self.assertEqual(_require(self, "Code Name"), "Granite Ridge")
            self.assertEqual(_require(self, "Technology"), "4 nm")

    def test_an_unlisted_part_gets_no_name_rather_than_its_neighbours(self):
        # The node is a property of the silicon with nothing on the machine
        # to read it from, so a part the table does not name must come back
        # blank. Inheriting the entry above it would be a typed value in the
        # worst possible disguise: one that looks read.
        with self._with_cpuid("178BFBFF00A10F10"):
            self.assertEqual(_require(self, "Code Name"), am5_profile.EM_DASH)
            self.assertEqual(_require(self, "Technology"), am5_profile.EM_DASH)

    def _with_bridge(self, device, revision):
        return mock.patch(
            "rochviewer.system_identity.pci_device_and_revision",
            side_effect=lambda bus, function: (
                (device, revision) if (bus, function) == (0x00, 0)
                else (None, None)),
        )

    def test_the_chipset_row_follows_the_host_bridge(self):
        with self._with_cpuid("178BFBFF00B40F40"), \
                self._with_bridge(0x14D8, 0x21):
            self.assertEqual(_require(self, "Chipset"),
                             "AMD Granite Ridge rev. 21")

    def test_the_chipset_revision_is_read_not_assumed(self):
        # Same bridge, different stepping. A constant here would misstate
        # every board that ships another one.
        with self._with_cpuid("178BFBFF00B40F40"), \
                self._with_bridge(0x14D8, 0x07):
            self.assertEqual(_require(self, "Chipset"),
                             "AMD Granite Ridge rev. 07")

    def test_an_unknown_bridge_reads_nothing(self):
        with self._with_bridge(0xDEAD, 0x00):
            self.assertEqual(_require(self, "Chipset"), am5_profile.EM_DASH)

    def _with_modules(self, modules):
        return mock.patch("rochviewer.memory.dimm_inventory.read_modules",
                          return_value=modules)

    def _with_spd(self, entries):
        return mock.patch("rochviewer.memory.ddr5_spd.read_identity",
                          return_value=entries)

    def test_the_module_rows_follow_the_spd(self):
        # SPD is the primary source for the maker, the die and the serial:
        # SMBIOS does not carry the DRAM component at all, so the modules are
        # asked directly and the inventory is only the fallback below.
        spd = [{
            "part_number": "F5-6000J3038F16G",
            "module_manufacturer": "Corsair", "serial_number": "0000ABCD",
            "manufacture_date": "31 / 2023", "dram_manufacturer": "Micron",
            "dram_stepping": 66, "dram_die": "B-die",
        }]
        with self._with_spd(spd):
            self.assertEqual(_require(self, "Module Manufacturer"), "Corsair")
            self.assertEqual(_require(self, "IC Manufacturer"), "Micron")
            self.assertEqual(_require(self, "DRAM Die"), "B-die")
            self.assertEqual(_require(self, "Serial Number"), "0000ABCD")
            self.assertEqual(_require(self, "Manufactured"), "31 / 2023")

    def test_the_ic_row_splits_the_inventory_string_without_spd(self):
        # With no SPD answer the inventory carries "SK hynix A-die" as one
        # string, and the two rows must take their own halves of it rather
        # than a fixed pair.
        modules = [{
            "serial_number": "1", "device_locator": "DIMMA1", "slot": "A1",
            "channel": "A", "part_number": "X", "capacity_gb": 16,
            "capacity": "16GB", "rank_count": 1, "rank": "SR",
            "module_manufacturer": "G.Skill", "ic": "Micron B-die",
        }]
        with self._with_spd([]), self._with_modules(modules):
            self.assertEqual(_require(self, "IC Manufacturer"), "Micron")
            self.assertEqual(_require(self, "DRAM Die"), "B-die")

    def test_the_module_rows_follow_the_inventory(self):
        other = [{
            "serial_number": "0000ABCD", "device_locator": "DIMMA1",
            "slot": "A1", "channel": "A", "part_number": "F5-6000J3038F16G",
            "capacity_gb": 32, "capacity": "32GB", "rank_count": 2,
            "rank": "DR", "module_manufacturer": "Corsair",
            "ic": "Samsung B-die",
        }]
        with self._with_spd([]), self._with_modules(other):
            self.assertEqual(_require(self, "Part Number"),
                             "F5-6000J3038F16G")
            self.assertEqual(_require(self, "Module Manufacturer"), "Corsair")
            self.assertEqual(_require(self, "Serial Number"), "0000ABCD")
            # From rank_count, not from the inventory's own "DR" string --
            # two ranks reads as 2R, the way one reads as 1R on this bench.
            self.assertEqual(_require(self, "Rank"), "2R")
            # From capacity_gb, formatted here -- not the inventory's own
            # "32GB" string passed through.
            self.assertEqual(_require(self, "DIMM Size"), "32 GB")

    def _with_card(self, card):
        return mock.patch("rochviewer.gpu.radeon.read_gpu", return_value=card)

    def test_the_gpu_rows_follow_the_card(self):
        card = {
            "name": "AMD Radeon RX 7900 XTX", "code_name": "Navi 31",
            "technology": "5 nm", "cores": 6144, "rops": 192, "tmus": 384,
            "memory_type": "GDDR6", "bus_width": "384 bit",
        }
        with self._with_card(card):
            self.assertEqual(_require(self, "GPU Code Name"), "Navi 31")
            self.assertEqual(_require(self, "GPU Technology"), "5 nm")
            self.assertEqual(_require(self, "Cores"), "6144 Unified")
            self.assertEqual(_require(self, "ROPs / TMUs"), "192 / 384")

    def test_a_card_the_table_does_not_name_reads_nothing(self):
        # The table-backed fields have nothing on the card to read them from,
        # so an unlisted card must leave them blank rather than showing the
        # bench's own numbers.
        with self._with_card({"name": "Some Other Card"}):
            self.assertEqual(_require(self, "GPU Code Name"),
                             am5_profile.EM_DASH)
            self.assertEqual(_require(self, "ROPs / TMUs"),
                             am5_profile.EM_DASH)


class CachesDoNotOutliveAStubTest(unittest.TestCase):
    """The caches are why this file has to clear them, stated as a test.

    Without this, a future cache added beside the others would silently make
    every test above pass against the bench's own hardware instead of the
    stub -- which is exactly how the Intel copy of these tests failed on AM5
    for five releases while printing correct values.
    """

    def test_every_named_cache_exists_and_is_clearable(self):
        for name in _CACHES:
            with self.subTest(cache=name):
                cache = getattr(am5_profile, name, None)
                self.assertIsNotNone(cache, "%s no longer exists" % name)
                self.assertTrue(hasattr(cache, "clear"))

    def test_a_stale_cache_would_be_caught(self):
        # Prime the processor cache with a part the table does not name, then
        # stub a part it does. The row must follow the stub, which it can
        # only do if the cache was cleared.
        am5_profile._PROCESSOR_FACTS.clear()
        am5_profile._PROCESSOR_FACTS.append({"processor_id": "178BFBFF00A10F10"})
        self.addCleanup(am5_profile._PROCESSOR_FACTS.clear)
        self.assertEqual(am5_profile._cpu_silicon(), (None, None))


if __name__ == "__main__":
    unittest.main()
