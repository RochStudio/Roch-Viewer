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

import unittest
from unittest import mock

from rochviewer import system_identity as identity


class MicrocodeTest(unittest.TestCase):
    """Windows packs the revision into the high half of a QWORD."""

    def test_a_packed_qword_gives_the_upper_half(self):
        self.assertEqual(identity.decode_microcode(0x13300000000), "0x133")

    def test_a_plain_revision_is_left_alone(self):
        self.assertEqual(identity.decode_microcode(0xB404035), "0xB404035")

    def test_a_qword_with_an_empty_upper_half_falls_back_to_the_lower(self):
        self.assertEqual(identity.decode_microcode(0x1_0000_B404035 & 0xFFFFFFFF),
                         "0xB404035")

    def test_bytes_are_read_little_endian(self):
        self.assertEqual(
            identity.decode_microcode((0x133).to_bytes(8, "little")), "0x133"
        )

    def test_anything_else_reads_as_nothing(self):
        # A row that cannot be read shows an em dash; it does not invent one.
        self.assertIsNone(identity.decode_microcode("0x133"))
        self.assertIsNone(identity.decode_microcode(None))

    def test_an_unreadable_registry_is_not_an_error(self):
        with mock.patch.object(identity.winreg, "OpenKey",
                               side_effect=OSError("no such key")):
            self.assertIsNone(identity.microcode())


class MemoryTypeTest(unittest.TestCase):
    def test_the_ddr5_code_is_decoded(self):
        self.assertEqual(identity.SMBIOS_MEMORY_TYPES[0x22], "DDR5")

    def test_ddr4_and_ddr3_are_decoded_too(self):
        self.assertEqual(identity.SMBIOS_MEMORY_TYPES[0x1A], "DDR4")
        self.assertEqual(identity.SMBIOS_MEMORY_TYPES[0x18], "DDR3")


class LpcioTest(unittest.TestCase):
    """The vendor comes from the reader that answered, not the chip name."""

    def _profile(self, module_name, chip_name):
        reader = mock.Mock()
        reader.chip_name = chip_name
        type(reader).__module__ = module_name
        return {"reader": reader}

    def _name(self, profile):
        with mock.patch.dict(
            "sys.modules",
            {"rochviewer.sensors.board_sensors": mock.Mock(
                board_sensor_profile=lambda: profile)},
        ):
            return identity.lpcio_name()

    def test_a_nuvoton_reader_names_nuvoton(self):
        self.assertEqual(
            self._name(self._profile("superio_lpc", "NCT6687D")),
            "Nuvoton NCT6687D",
        )

    def test_an_ite_reader_names_ite(self):
        self.assertEqual(
            self._name(self._profile("ite_superio", "IT8696E")), "ITE IT8696E"
        )

    def test_an_unknown_reader_still_names_the_chip(self):
        self.assertEqual(self._name(self._profile("other", "X1234")), "X1234")

    def test_no_profile_reads_as_nothing(self):
        self.assertIsNone(self._name(None))

    def test_a_profile_with_no_chip_reads_as_nothing(self):
        self.assertIsNone(self._name(self._profile("superio_lpc", None)))



if __name__ == "__main__":
    unittest.main()
