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

from ddr5_spd import (
    EM_DASH,
    SPD_DRAM_MFG_ID,
    SPD_DRAM_STEPPING,
    SPD_MFG_WEEK,
    SPD_MFG_YEAR,
    SPD_MODULE_MFG_ID,
    SPD_PART_NUMBER,
    SPD_SERIAL_NUMBER,
    decode_die,
    decode_identity,
    decode_jep106_id,
    decode_manufacture_date,
    decode_serial_number,
    read_identity,
)

# Captured from the bench kit (G.Skill F5-6000J2636G16G, SPD hub 0x51):
# module vendor 04 CD, serial, part number at 0x209 padded with spaces,
# revision 00, DRAM vendor 80 AD, stepping 0x41.
BENCH_BLOCK = (
    bytes.fromhex("04CD002507AA5AE5FA")
    + b"F5-6000J2636G16G".ljust(30, b" ")
    + bytes.fromhex("00" "80AD" "41" "0000000000")
)


def bench_values(overrides=None):
    values = {
        SPD_MODULE_MFG_ID + index: byte
        for index, byte in enumerate(BENCH_BLOCK)
    }
    values.update(overrides or {})
    return values


class Jep106Test(unittest.TestCase):
    def test_bank_comes_from_the_continuation_count(self):
        # 0x80 = parity bit set, continuation count 0 -> bank 1, code 0xAD.
        self.assertEqual(decode_jep106_id(0x80, 0xAD), "SK hynix")
        self.assertEqual(decode_jep106_id(0x00, 0xAD), "SK hynix")
        self.assertEqual(decode_jep106_id(0x80, 0x2C), "Micron Technology")
        self.assertEqual(decode_jep106_id(0x80, 0xCE), "Samsung")
        # Bank 4 (continuation count 3), Nanya.
        self.assertEqual(decode_jep106_id(0x03, 0x0B), "Nanya Technology")

    def test_an_unlisted_vendor_reports_its_raw_id_rather_than_a_guess(self):
        self.assertEqual(decode_jep106_id(0x80, 0x11), "0x8011")

    def test_missing_bytes_report_nothing(self):
        self.assertEqual(decode_jep106_id(None, 0xAD), EM_DASH)
        self.assertEqual(decode_jep106_id(0x80, None), EM_DASH)


class DieDecodeTest(unittest.TestCase):
    def test_hynix_4n_stepping_names_the_die(self):
        self.assertEqual(decode_die("SK hynix", 0x41), "A-die")
        self.assertEqual(decode_die("SK hynix", 0x42), "B-die")
        self.assertEqual(decode_die("SK hynix", 0x4D), "M-die")

    def test_other_hynix_families_are_not_named(self):
        # Outside the 0x4n family the letter rule is not claimed to hold.
        self.assertEqual(decode_die("SK hynix", 0x10), "0x10")
        self.assertEqual(decode_die("SK hynix", 0x40), "0x40")

    def test_other_vendors_report_the_raw_stepping(self):
        self.assertEqual(decode_die("Samsung", 0x41), "0x41")
        self.assertEqual(decode_die("Micron Technology", 0x02), "0x02")

    def test_missing_stepping_reports_nothing(self):
        self.assertEqual(decode_die("SK hynix", None), EM_DASH)


class SerialNumberTest(unittest.TestCase):
    def test_the_serial_is_printed_as_digits_not_converted(self):
        # CPU-Z shows 00004996 for the first module on the DDR5 bench and
        # 00004997 for the second. Read as a number those would be 18838 and
        # 18839, which is not what is printed on either label.
        values = {SPD_SERIAL_NUMBER + i: b
                  for i, b in enumerate((0x00, 0x00, 0x49, 0x96))}
        self.assertEqual(decode_serial_number(values), "00004996")

    def test_every_byte_is_kept_including_leading_zeroes(self):
        values = {SPD_SERIAL_NUMBER + i: b
                  for i, b in enumerate((0x00, 0x0A, 0xB0, 0x0F))}
        self.assertEqual(decode_serial_number(values), "000AB00F")

    def test_a_short_read_reports_nothing(self):
        self.assertEqual(decode_serial_number({SPD_SERIAL_NUMBER: 0x00}),
                         EM_DASH)


class VendorNameTest(unittest.TestCase):
    def test_the_lga1700_module_vendor_decodes_to_its_name(self):
        # 0x866D off the bench kit: continuation count 6 -> bank 7, code 0x6D.
        # SMBIOS calls the same module "V-Color Technology Inc"; this is the
        # name the module carries, and what CPU-Z shows.
        self.assertEqual(decode_jep106_id(0x86, 0x6D), "V-Color Technology")


class ManufactureDateTest(unittest.TestCase):
    def test_the_lga1700_kit_decodes_to_what_cpuz_reports(self):
        # V-Color TMXFL1680838KWK, read from both hubs on the DDR5 bench:
        # 0x203 = 0x25, 0x204 = 0x04, where CPU-Z shows Week/Year 04 / 25.
        self.assertEqual(decode_manufacture_date(0x25, 0x04), "04 / 2025")

    def test_both_bytes_are_bcd_rather_than_binary(self):
        # 0x24 is week 24, not 36; a binary read would say the latter.
        self.assertEqual(decode_manufacture_date(0x25, 0x24), "24 / 2025")
        self.assertEqual(decode_manufacture_date(0x23, 0x53), "53 / 2023")

    def test_an_unprogrammed_module_reports_nothing(self):
        self.assertEqual(decode_manufacture_date(0x00, 0x00), EM_DASH)
        self.assertEqual(decode_manufacture_date(0xFF, 0xFF), EM_DASH)
        self.assertEqual(decode_manufacture_date(None, None), EM_DASH)

    def test_a_week_past_the_calendar_is_not_reported_as_a_date(self):
        self.assertEqual(decode_manufacture_date(0x25, 0x54), EM_DASH)
        # Nibbles that are not digits are not a BCD week either.
        self.assertEqual(decode_manufacture_date(0x25, 0x1A), EM_DASH)


class DecodeIdentityTest(unittest.TestCase):
    def test_the_bench_block_decodes_to_the_kit_on_the_bench(self):
        identity = decode_identity(bench_values())
        self.assertEqual(identity["part_number"], "F5-6000J2636G16G")
        self.assertEqual(identity["dram_manufacturer"], "SK hynix")
        self.assertEqual(identity["dram_stepping"], 0x41)
        self.assertEqual(identity["dram_die"], "A-die")
        self.assertEqual(identity["manufacture_date"], "07 / 2025")
        self.assertEqual(identity["serial_number"], "AA5AE5FA")

    def test_the_date_comes_from_the_block_rather_than_a_constant(self):
        identity = decode_identity(bench_values({
            SPD_MFG_YEAR: 0x22, SPD_MFG_WEEK: 0x51,
        }))
        self.assertEqual(identity["manufacture_date"], "51 / 2022")

    def test_a_samsung_module_reports_its_stepping_byte(self):
        identity = decode_identity(bench_values({
            SPD_DRAM_MFG_ID + 1: 0xCE, SPD_DRAM_STEPPING: 0x11,
        }))
        self.assertEqual(identity["dram_manufacturer"], "Samsung")
        self.assertEqual(identity["dram_die"], "0x11")

    def test_a_short_read_does_not_raise(self):
        identity = decode_identity({SPD_PART_NUMBER: ord("F")})
        self.assertEqual(identity["part_number"], "F")
        self.assertEqual(identity["dram_manufacturer"], EM_DASH)
        self.assertEqual(identity["dram_die"], EM_DASH)
        self.assertEqual(identity["manufacture_date"], EM_DASH)


class FakeReader:
    def __init__(self, hubs, driver_open=True):
        self.hubs = hubs
        self._driver_open = driver_open
        self.probed = []

    def is_driver_open(self):
        return self._driver_open

    def probe_address(self, address, controller=0x00):
        self.probed.append((controller, address))
        return (controller, address) in self.hubs

    def read_spd(self, address, offset, length, controller=0x00):
        return bench_values()


class ReadIdentityTest(unittest.TestCase):
    """The DDR5 path. Generation is stated rather than sniffed from the bench.

    These describe what an SPD5 hub answers, so they say DDR5 outright: run on
    a DDR4 machine the reader refuses before it reaches the bus, which is the
    behaviour Ddr4RefusalTest covers.
    """

    def test_every_answering_hub_is_decoded(self):
        reader = FakeReader({(0x00, 0x51), (0x00, 0x53)})
        found = read_identity(reader_factory=lambda: reader,
                              generation="DDR5")
        self.assertEqual(len(found), 2)
        self.assertEqual(
            [entry["address"] for entry in found], [0x51, 0x53]
        )
        self.assertEqual(found[0]["dram_die"], "A-die")

    def test_the_second_controller_is_not_probed_once_dimms_answered(self):
        reader = FakeReader({(0x00, 0x51)})
        read_identity(reader_factory=lambda: reader, generation="DDR5")
        self.assertEqual({c for c, _ in reader.probed}, {0x00})

    def test_ddr4_is_refused_without_touching_the_bus(self):
        # The page selector this path uses is a write, and on DDR4 the same
        # transaction lands on the SPD array rather than on a hub's page
        # register. It must not reach the bus at all.
        reader = FakeReader({(0x00, 0x51)})
        self.assertEqual(
            read_identity(reader_factory=lambda: reader, generation="DDR4"),
            [],
        )
        self.assertEqual(reader.probed, [])

    def test_no_driver_means_no_identity_rather_than_an_error(self):
        reader = FakeReader({(0x00, 0x51)}, driver_open=False)
        self.assertEqual(read_identity(reader_factory=lambda: reader), [])

    def test_a_reader_that_raises_reports_no_identity(self):
        def broken():
            raise OSError("no driver")

        self.assertEqual(read_identity(reader_factory=broken), [])


if __name__ == "__main__":
    unittest.main()
