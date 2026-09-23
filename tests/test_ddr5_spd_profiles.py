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

"""DDR5 SPD: the JEDEC base block and Intel XMP 3.0, from a real module.

The bytes are the base block and the extension area of a V-Color
TMXFL1680838KWK (DDR5-8000 CL38, 1.45 V) read on an LGA1700 bench. The
identity block at 0x200 is left out: it carries the module's serial number.
Rows of zeros are omitted.
"""

import unittest

from rochviewer.memory.spd_profiles import (
    decode_ddr5_base, decode_ddr5_spd, decode_ddr5_xmp,
)

ROWS = {
    0x000: bytes.fromhex('30 10 12 02 04 00 20 62 00 00 00 00 10 02 05 00'),
    0x010: bytes.fromhex('00 00 00 00 A0 01 F2 03 7A 0D 00 00 00 00 80 3E'),
    0x020: bytes.fromhex('80 3E 80 3E 00 7D 80 BB 30 75 27 01 A0 00 82 00'),
    0x040: bytes.fromhex('00 00 00 00 00 00 88 13 08 88 13 08 20 4E 20 10'),
    0x050: bytes.fromhex('27 10 15 34 20 10 27 10 C4 09 04 4C 1D 0C 00 00'),
    0x280: bytes.fromhex('0C 4A 30 01 15 8A 8C 01 07 00 00 00 00 00 00 00'),
    0x2B0: bytes.fromhex('00 00 00 00 00 00 00 00 00 00 00 00 00 00 5B 11'),
    0x2C0: bytes.fromhex('30 29 29 00 22 FA 00 FA EF 02 00 00 00 1C 25 E0'),
    0x2D0: bytes.fromhex('2E E0 2E 00 7D E0 AB 30 75 27 01 A0 00 82 00 00'),
    0x2F0: bytes.fromhex('00 00 00 00 00 00 00 00 00 00 00 00 00 00 79 89'),
    0x340: bytes.fromhex('45 58 50 4F 11 03 01 01 00 00 29 29 30 00 FA 00'),
    0x350: bytes.fromhex('1C 25 E0 2E E0 2E 00 7D E0 AB 30 75 27 01 A0 00'),
    0x360: bytes.fromhex('82 00 88 13 88 13 20 4E 80 3E CD 37 10 27 C4 09'),
    0x370: bytes.fromhex('4C 1D 00 00 00 00 00 00 00 00 00 00 00 00 00 00'),
    0x3B0: bytes.fromhex('00 00 00 00 00 00 00 00 00 00 00 00 00 00 D8 A7'),
}


def module_bytes():
    values = {offset: 0 for offset in range(0x400)}
    for base, row in ROWS.items():
        for index, value in enumerate(row):
            values[base + index] = value
    return values


def column(record, name):
    return next(p for p in record["profiles"] if p["name"] == name)


class Ddr5JedecTest(unittest.TestCase):
    def test_module_description(self):
        record = decode_ddr5_base(module_bytes())
        self.assertEqual(record["memory_type"], "DDR5")
        self.assertEqual(record["module_type"], "UDIMM")
        self.assertEqual(record["max_bandwidth"], "DDR5-4800 (2400 MHz)")

    def test_the_three_fastest_jedec_bins(self):
        record = decode_ddr5_base(module_bytes())
        self.assertEqual(
            [(p["name"], p["frequency"], p["cl"], p["trcd"], p["trp"],
              p["tras"], p["trc"], p["voltage"]) for p in record["profiles"]],
            [("JEDEC 4800", "2400 MHz", 40, 39, 39, 77, 116, "1.10 V"),
             ("JEDEC 4400", "2200 MHz", 36, 36, 36, 71, 106, "1.10 V"),
             ("JEDEC 4000", "2000 MHz", 32, 32, 32, 64, 96, "1.10 V")],
        )

    def test_not_ddr5_is_not_decoded(self):
        values = module_bytes()
        values[2] = 0x0C
        self.assertEqual(decode_ddr5_base(values), {})


class Xmp3Test(unittest.TestCase):
    def test_profile_one_is_the_kits_rating(self):
        xmp = decode_ddr5_xmp(module_bytes())
        self.assertEqual(len(xmp["profiles"]), 1)
        profile = xmp["profiles"][0]
        self.assertEqual(profile["name"], "XMP-8000")
        self.assertEqual(profile["frequency"], "4000 MHz")
        self.assertEqual(
            (profile["cl"], profile["trcd"], profile["trp"], profile["tras"],
             profile["trc"]),
            (38, 48, 48, 128, 176),
        )
        self.assertEqual(profile["voltage"], "1.45 V")

    def test_extension_names_both_blocks(self):
        self.assertEqual(
            decode_ddr5_xmp(module_bytes())["extension"], "XMP 3.0, EXPO")

    def test_disabled_profiles_are_not_read(self):
        # Profile 3's slot at 0x340 holds this kit's EXPO block. With only
        # profile 1 enabled it must not be read as a second XMP profile.
        values = module_bytes()
        names = [p["name"] for p in decode_ddr5_xmp(values)["profiles"]]
        self.assertEqual(names, ["XMP-8000"])

    def test_no_xmp_header_means_no_xmp(self):
        values = module_bytes()
        values[0x280] = 0
        self.assertEqual(decode_ddr5_xmp(values)["extension"], "EXPO")
        self.assertEqual(decode_ddr5_xmp(values)["profiles"], [])


class Ddr5RecordTest(unittest.TestCase):
    def test_jedec_then_xmp_in_five_columns(self):
        record = decode_ddr5_spd(module_bytes(), {"part_number": "X"})
        self.assertEqual(
            [p["name"] for p in record["profiles"]],
            ["JEDEC 4800", "JEDEC 4400", "JEDEC 4000", "XMP-8000"],
        )
        self.assertEqual(record["part_number"], "X")


if __name__ == "__main__":
    unittest.main()
