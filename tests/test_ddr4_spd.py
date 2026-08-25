"""Cover the DDR4 SPD identity reader and the alignment check guarding it."""

import unittest

from ddr4_spd import (
    EM_DASH,
    SPD_DEVICE_TYPE,
    SPD_DEVICE_TYPE_DDR4,
    SPD_MFG_WEEK,
    SPD_MFG_YEAR,
    SPD_MODULE_MFG_ID,
    SPD_PART_NUMBER,
    SPD_PART_NUMBER_LENGTH,
    SPD_SERIAL_NUMBER,
    decode_identity,
    decode_part_number,
    decode_serial_number,
    read_identity,
)

# Captured from the Z790-P bench (G.Skill F4-3600C14-16GVKA, EEPROMs 0x51 and
# 0x53, both identical): module vendor 04 CD at register 0x40, then a location,
# date and serial that G.Skill left unprogrammed, the part number at 0x49, a
# zero revision, DRAM vendor 80 CE and a zero stepping.
BENCH_BLOCK = (
    bytes.fromhex("04CD" "00" "0000" "00000000")
    + b"F4-3600C14-16GVKA".ljust(SPD_PART_NUMBER_LENGTH, b"\x00")
    + bytes.fromhex("00" "80CE" "00" "00")
)


def bench_values(overrides=None):
    values = {
        SPD_MODULE_MFG_ID + index: byte
        for index, byte in enumerate(BENCH_BLOCK)
    }
    values.update(overrides or {})
    return values


class PartNumberTest(unittest.TestCase):
    """The part number is what proves which half of the SPD is in the window."""

    def test_the_bench_part_number_decodes(self):
        self.assertEqual(
            decode_part_number(bench_values()), "F4-3600C14-16GVKA"
        )

    def test_unprogrammed_tail_bytes_end_the_string(self):
        # 0x00 and 0xFF are padding, not corruption: they stop the string
        # rather than disqualifying the block.
        values = bench_values({SPD_PART_NUMBER + 17: 0xFF})
        self.assertEqual(decode_part_number(values), "F4-3600C14-16GVKA")

    def test_a_non_printable_byte_disqualifies_the_block(self):
        # This is the case that matters: the lower half of the SPD holds
        # timing parameters, which are not ASCII, so a window on the wrong
        # half must not decode as though it were the right one.
        values = bench_values({SPD_PART_NUMBER + 2: 0x0C})
        self.assertEqual(decode_part_number(values), "")

    def test_an_unreadable_byte_is_skipped_rather_than_faked(self):
        values = bench_values({SPD_PART_NUMBER: None})
        self.assertEqual(decode_part_number(values), "4-3600C14-16GVKA")


class SerialNumberTest(unittest.TestCase):
    def test_an_unprogrammed_serial_is_not_a_serial_of_zero(self):
        # Both bench sticks read this way. Printing 00000000 would put a
        # serial on a module that never gave one.
        self.assertEqual(decode_serial_number(bench_values()), EM_DASH)

    def test_an_all_f_block_is_unprogrammed_too(self):
        values = bench_values({
            SPD_SERIAL_NUMBER + index: 0xFF for index in range(4)
        })
        self.assertEqual(decode_serial_number(values), EM_DASH)

    def test_a_programmed_serial_reads_as_its_bytes(self):
        values = bench_values(dict(zip(
            range(SPD_SERIAL_NUMBER, SPD_SERIAL_NUMBER + 4),
            (0x00, 0x00, 0xAB, 0xCD),
        )))
        self.assertEqual(decode_serial_number(values), "0000ABCD")

    def test_an_unreadable_byte_reports_nothing(self):
        values = bench_values({SPD_SERIAL_NUMBER + 1: None})
        self.assertEqual(decode_serial_number(values), EM_DASH)


class DecodeIdentityTest(unittest.TestCase):
    def test_the_bench_block_decodes_to_what_the_kit_is(self):
        found = decode_identity(bench_values())
        self.assertEqual(found["part_number"], "F4-3600C14-16GVKA")
        self.assertEqual(found["module_manufacturer"], "G.Skill")
        self.assertEqual(found["dram_manufacturer"], "Samsung")
        # Unprogrammed on this kit, and reported as such.
        self.assertEqual(found["serial_number"], EM_DASH)
        self.assertEqual(found["manufacture_date"], EM_DASH)

    def test_a_programmed_date_decodes(self):
        values = bench_values({SPD_MFG_YEAR: 0x23, SPD_MFG_WEEK: 0x31})
        self.assertEqual(decode_identity(values)["manufacture_date"],
                         "31 / 2023")

    def test_a_block_that_fails_the_alignment_check_decodes_to_nothing(self):
        # The lower half in the window: the device type byte lands where the
        # part number would start, and nothing here is ASCII.
        values = {
            SPD_PART_NUMBER + index: 0x0C
            for index in range(SPD_PART_NUMBER_LENGTH)
        }
        self.assertIsNone(decode_identity(values))

    def test_a_short_run_of_printable_bytes_is_not_a_part_number(self):
        values = {SPD_PART_NUMBER: 0x41, SPD_PART_NUMBER + 1: 0x42}
        self.assertIsNone(decode_identity(values))

    def test_the_lower_half_is_recognisable_by_its_device_type(self):
        # Not used to decide anything -- the part number does that -- but the
        # constant is what makes the negative case checkable by hand.
        self.assertEqual(SPD_DEVICE_TYPE, 0x02)
        self.assertEqual(SPD_DEVICE_TYPE_DDR4, 0x0C)


class FakeReader:
    """Answers one address, from the bench block, and records what was asked."""

    def __init__(self, answering=(0x51,), values=None, driver_open=True):
        self.answering = set(answering)
        self.values = bench_values() if values is None else values
        self._driver_open = driver_open
        self.reads = []
        self.writes = []

    def is_driver_open(self):
        return self._driver_open

    def probe_address(self, address, controller_offset=0x00):
        return address in self.answering

    def read_byte(self, address, register, controller_offset=0x00):
        self.reads.append((address, register))
        return self.values.get(register)

    def write_byte(self, *args, **kwargs):
        self.writes.append((args, kwargs))
        raise AssertionError("the DDR4 SPD reader must never write")

    def select_spd_page(self, *args, **kwargs):
        self.writes.append((args, kwargs))
        raise AssertionError("the DDR4 SPD reader must never select a page")


class ReadIdentityTest(unittest.TestCase):
    def test_an_answering_module_is_decoded(self):
        reader = FakeReader()
        found = read_identity(reader_factory=lambda: reader)
        self.assertEqual(len(found), 1)
        self.assertEqual(found[0]["part_number"], "F4-3600C14-16GVKA")
        self.assertEqual(found[0]["address"], 0x51)

    def test_nothing_is_written_to_the_bus(self):
        # The whole reason this module exists rather than reusing the DDR5
        # one: selecting a page is a write, and on DDR4 that write lands on
        # the SPD array itself.
        reader = FakeReader()
        read_identity(reader_factory=lambda: reader)
        self.assertEqual(reader.writes, [])

    def test_no_driver_means_no_identity_rather_than_an_error(self):
        reader = FakeReader(driver_open=False)
        self.assertEqual(read_identity(reader_factory=lambda: reader), [])

    def test_a_reader_that_raises_reports_no_identity(self):
        def broken():
            raise OSError("no driver")

        self.assertEqual(read_identity(reader_factory=broken), [])

    def test_an_address_whose_block_does_not_align_is_dropped(self):
        reader = FakeReader(values={SPD_PART_NUMBER: 0x0C})
        self.assertEqual(read_identity(reader_factory=lambda: reader), [])

    def test_only_the_identity_registers_are_read(self):
        reader = FakeReader()
        read_identity(reader_factory=lambda: reader)
        asked = {register for _, register in reader.reads}
        self.assertTrue(asked)
        self.assertLessEqual(asked, set(range(SPD_MODULE_MFG_ID, 0x61)))


if __name__ == "__main__":
    unittest.main()
