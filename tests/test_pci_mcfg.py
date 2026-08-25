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

import struct
import unittest

import pci_mcfg as mcfg


def _table(base=0xE0000000, segment=0, start=0, end=0xFF):
    length = 60
    header = b"MCFG" + struct.pack("<I", length) + bytes(28)
    reserved = bytes(8)
    entry = struct.pack("<QHBBI", base, segment, start, end, 0)
    return header + reserved + entry


class McfgParseTest(unittest.TestCase):
    def test_windows_acpi_provider_signature_uses_multichar_constant_order(self):
        self.assertEqual(mcfg._ACPI_PROVIDER, 0x41435049)

    def test_parses_allocation_and_computes_ecam_address(self):
        entries = mcfg.parse_mcfg(_table())
        self.assertEqual(len(entries), 1)
        entry = entries[0]
        self.assertEqual(entry.base_address, 0xE0000000)
        self.assertEqual(entry.segment, 0)
        self.assertEqual(entry.start_bus, 0)
        self.assertEqual(entry.end_bus, 0xFF)
        self.assertEqual(
            mcfg.ecam_address(entry, bus=2, device=3, function=1, register=0x64),
            0xE0000000 + (2 << 20) + (3 << 15) + (1 << 12) + 0x64,
        )

    def test_rejects_invalid_table(self):
        with self.assertRaises(ValueError):
            mcfg.parse_mcfg(b"BAD!")

    def test_rejects_truncated_declared_length(self):
        table = bytearray(_table())
        struct.pack_into("<I", table, 4, len(table) + 16)
        with self.assertRaisesRegex(ValueError, "truncated"):
            mcfg.parse_mcfg(table)

    def test_rejects_misaligned_allocation_area(self):
        table = bytearray(_table()) + b"X"
        struct.pack_into("<I", table, 4, len(table))
        with self.assertRaisesRegex(ValueError, "misaligned"):
            mcfg.parse_mcfg(table)

    def test_rejects_zero_or_misaligned_ecam_bases(self):
        for base in (0, 0xE0001000):
            with self.subTest(base=base), self.assertRaises(ValueError):
                mcfg.parse_mcfg(_table(base=base))

    def test_probe_reads_bus_zero_vendor_from_matching_segment(self):
        reads = []

        def read_dword(address):
            reads.append(address)
            return 0x14E81022

        result = mcfg.probe_mcfg_vendor(_table(), read_dword)
        self.assertEqual(result.vendor_id, 0x1022)
        self.assertEqual(result.device_id, 0x14E8)
        self.assertEqual(result.config_address, 0xE0000000)
        self.assertEqual(reads, [0xE0000000])

    def test_nonzero_start_bus_is_accounted_for(self):
        table = _table(base=0xD0000000, start=0x40, end=0x7F)
        entry = mcfg.parse_mcfg(table)[0]
        self.assertEqual(
            mcfg.ecam_address(entry, bus=0x41, device=0, function=0, register=0),
            0xD0000000 + (1 << 20),
        )


if __name__ == "__main__":
    unittest.main()
