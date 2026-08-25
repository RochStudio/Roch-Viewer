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

"""No System Info row may carry a value that was typed rather than read.

The tab is entirely identity and configuration, which is exactly the kind of
thing that is easy to paste in from a bench machine and never notice. Two
guards:

  the source, so a literal cannot be introduced in the row table at all; and
  the behaviour, so a row that is wired to a reading actually follows it.

A decode table is not a hardcoded value. "SK hynix", "Z790" and "GDDR6X" are
names for codes read from the hardware, and the tests below change the code
and require the row to change with it.
"""

import ast
import os
import unittest
from unittest import mock

from rochviewer.intel import intel_timings
from rochviewer.gpu import nvidia_gpu

SOURCE = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
                      "rochviewer", "intel", "intel_timings.py")

# Rows defined in the table literal carry Tab "Main" and are moved onto System
# Info by a later pass; see _install_system_info_order.
SOURCE_TAB = "Main"


def _row_dicts():
    """Every row dict literal in intel_timings.py, as AST nodes."""
    with open(SOURCE, encoding="utf-8") as handle:
        tree = ast.parse(handle.read())
    for node in ast.walk(tree):
        if not isinstance(node, ast.Dict):
            continue
        keys = {
            key.value: value
            for key, value in zip(node.keys, node.values)
            if isinstance(key, ast.Constant)
        }
        if "name" in keys and "value" in keys:
            yield keys


class SourceTest(unittest.TestCase):
    def test_no_system_info_row_has_a_literal_value(self):
        # A row may hold a constant -- most of this tab cannot change while
        # the machine runs, so it is read once at import rather than on every
        # draw. What it may not do is hold a constant that never came from the
        # machine: get_cpu_name() is a reading, "i9-14900KS" is a transcript.
        offenders = []
        for keys in _row_dicts():
            tab = keys.get("Tab")
            if not isinstance(tab, ast.Constant) or tab.value != SOURCE_TAB:
                continue
            value = keys["value"]
            name = keys["name"].value if isinstance(keys["name"], ast.Constant) else "?"
            if isinstance(value, ast.Constant) and value.value is not None:
                offenders.append((name, value.value))
        self.assertEqual(offenders, [])

    def test_the_guard_would_catch_a_pasted_value(self):
        # The check above passes trivially if the AST walk finds nothing, so
        # prove it finds the rows it is supposed to be policing.
        names = {
            keys["name"].value
            for keys in _row_dicts()
            if isinstance(keys.get("Tab"), ast.Constant)
            and keys["Tab"].value == SOURCE_TAB
            and isinstance(keys["name"], ast.Constant)
        }
        for expected in ("CPU", "Manufacturer", "Model", "BIOS", "Microcode"):
            self.assertIn(expected, names)


class RowMissing(Exception):
    """The row is not on the tab, because the hardware behind it is absent."""


def _row_value(name):
    """One System Info row from the Intel table, whatever platform this is.

    The Intel table, not the active one. These tests patch Intel readers and
    then require the row to follow -- which says something about the Intel
    row wiring, not about the machine they run on. Reading the active table
    tied them to the dispatcher instead: on an AM5 bench it is filled from
    the AMD profile, so the stubs reached nothing and the assertions compared
    AMD rows against Intel expectations. Five of them failed there while
    every value they printed was correct for that machine -- the CPU really
    is Granite Ridge, the modules really are F5-6000J2636G16G.

    Read this way they run and pass on any platform, which is better than
    skipping: the wiring is checked on every bench rather than only on the
    author's. It survived CI because CI has no hardware, so the rows were
    absent and _require skipped on a missing row.
    """
    for row in intel_timings.TIMINGS:
        if row.get("Tab") == intel_timings.SYSTEM_INFO_TAB and row["name"] == name:
            return row["value"]() if callable(row["value"]) else row["value"]
    raise RowMissing(name)


def _require(test, name):
    """One row's value, or skip when this machine does not build that row.

    The rows follow the hardware, which is the property these tests exist to
    check -- so on a machine with no DIMM answering and no NVIDIA card, the
    row being absent is the behaviour rather than a failure. Distinguished
    from a genuine regression by skipping only where the row is missing
    entirely: a row that exists and reads the wrong thing still fails.
    """
    try:
        return _row_value(name)
    except RowMissing:
        test.skipTest("%s is not built on this machine: no hardware for it"
                      % name)


class FollowsTheHardwareTest(unittest.TestCase):
    """Change what the machine reports; the row must change with it."""

    def setUp(self):
        intel_timings._clear_identity_caches()
        nvidia_gpu._CACHE[:] = []
        self.addCleanup(nvidia_gpu._CACHE.clear)
        self.addCleanup(intel_timings._clear_identity_caches)

    def test_the_cpu_rows_follow_the_cpuid_model(self):
        seen = []
        for model in (0xB7, 0x97):
            intel_timings._clear_identity_caches()
            with mock.patch.object(intel_timings, "_cpu_family_model",
                                   return_value=(6, model)):
                seen.append(_require(self, "Code Name"))
        self.assertEqual(seen, ["Raptor Lake", "Alder Lake"])

    def test_the_memory_type_row_follows_smbios(self):
        import types

        seen = []
        for code in (34, 26):
            intel_timings._clear_identity_caches()
            with mock.patch.object(
                intel_timings, "_wmi_static",
                side_effect=lambda name, code=code: (
                    [types.SimpleNamespace(SMBIOSMemoryType=code)]
                    if name == "Win32_PhysicalMemory" else []
                ),
            ):
                seen.append(_require(self, "Type"))
        self.assertEqual(seen, ["DDR5", "DDR4"])

    def _generation(self, generation):
        return mock.patch.object(
            intel_timings, "detect_ddr_generation", return_value=generation)

    def test_the_module_rows_follow_the_spd(self):
        other = {
            "part_number": "F5-6000J3038F16G", "dram_manufacturer": "Samsung",
            "dram_die": "B-die", "manufacture_date": "31 / 2023",
            "serial_number": "0000ABCD", "module_manufacturer": "G.Skill",
        }
        with self._generation("DDR5"), \
                mock.patch("rochviewer.memory.ddr5_spd.read_identity", return_value=[other]):
            self.assertEqual(_require(self, "Part Number"), "F5-6000J3038F16G")
            self.assertEqual(_require(self, "DRAM Die"), "B-die")
            self.assertEqual(_require(self, "Manufactured"), "31 / 2023")
            self.assertEqual(_require(self, "Serial Number"), "0000ABCD")

    def test_ddr4_takes_the_serial_and_date_from_its_own_reader(self):
        # The two fields nothing else on the machine carries, and the two the
        # DDR4 block is asked for. The DDR5 reader must not be consulted: its
        # page-select is a write, and on DDR4 that lands on the SPD array.
        module = {"serial_number": "0000ABCD", "manufacture_date": "31 / 2023"}
        with self._generation("DDR4"), \
                mock.patch("rochviewer.memory.ddr4_spd.read_identity", return_value=[module]), \
                mock.patch("rochviewer.memory.ddr5_spd.read_identity",
                           side_effect=AssertionError) as ddr5:
            self.assertEqual(_require(self, "Serial Number"), "0000ABCD")
            self.assertEqual(_require(self, "Manufactured"), "31 / 2023")
            ddr5.assert_not_called()

    def test_ddr4_does_not_take_the_die_from_a_blank_stepping(self):
        # The DDR4 block carries a stepping byte, and it reads 0x00 on the
        # bench kit. Sourcing the die from it would turn a "B-die" the
        # part-number table gets right into a useless raw byte.
        module = {"dram_die": "0x00", "dram_manufacturer": "0x0000",
                  "part_number": "NOT-THE-REAL-SKU"}
        with self._generation("DDR4"), \
                mock.patch("rochviewer.memory.ddr4_spd.read_identity", return_value=[module]):
            self.assertNotEqual(_require(self, "DRAM Die"), "0x00")
            self.assertNotEqual(_require(self, "Part Number"), "NOT-THE-REAL-SKU")

    def test_the_table_backed_gpu_rows_fail_closed_on_another_card(self):
        # ROPs/TMUs and the code-name SKU are the two values on this tab that
        # nothing reports, so they come from a table keyed on the device ID.
        # The property that keeps that honest is that an unlisted card gets
        # nothing rather than the listed card's numbers.
        def card(device):
            return mock.patch.object(
                nvidia_gpu, "_adapter_identity",
                return_value={"vendor_id": nvidia_gpu.NVIDIA_VENDOR_ID, "device_id": device,
                              "revision": 0xA1, "subsystem_vendor_id": 0x1458},
            )

        with card(0x2786), mock.patch.object(nvidia_gpu, "_Nvapi",
                                             side_effect=OSError), \
                mock.patch.object(nvidia_gpu, "_nvml_query",
                                  return_value={"architecture": 8}):
            self.assertEqual(_require(self, "ROPs / TMUs"), "64 / 184")
            self.assertEqual(_require(self, "GPU Code Name"), "AD104-250")

        nvidia_gpu._CACHE[:] = []
        with card(0x2C05), mock.patch.object(nvidia_gpu, "_Nvapi",
                                             side_effect=OSError), \
                mock.patch.object(nvidia_gpu, "_nvml_query",
                                  return_value={}):
            self.assertIsNone(_require(self, "ROPs / TMUs"))
            self.assertIsNone(_require(self, "GPU Code Name"))


if __name__ == "__main__":
    unittest.main()
