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

"""The 64-bit read path, and the field that made it necessary.

tWRPDEN on Core Ultra 200S sits at bits 27..36 of MCHBAR+0xE050 -- it spans
the 32-bit boundary, so it is not present in either dword on its own. The
register was read as two independent dwords for a long time and the row
showed N/A as a result, which looked like a missing register rather than a
missing transport. These cases pin the distinction down with the two real
captures that settled it.
"""

import unittest

from rochviewer.hardware.read import (
    WIDE_READ_BYTES,
    extract_value_from_hex,
    extract_value_from_wide_hex,
)


def qword_hex(low, high):
    """The eight bytes a 64-bit read of an adjacent dword pair returns."""
    return ((high << 32) | low).to_bytes(WIDE_READ_BYTES, "little").hex()


# Measured on a Z890 bench either side of a single BIOS change, everything
# else held constant.
BIOS_90 = (0xD0658204, 0x2206EA02)
BIOS_120 = (0xC0658204, 0x2206EA03)

TWRPDEN_START = 27
TWRPDEN_LENGTH = 10


class WideExtractTest(unittest.TestCase):
    def test_reads_a_field_spanning_the_dword_boundary(self):
        for (low, high), expected in ((BIOS_90, 90), (BIOS_120, 120)):
            with self.subTest(expected=expected):
                self.assertEqual(
                    extract_value_from_wide_hex(
                        qword_hex(low, high), TWRPDEN_START, TWRPDEN_LENGTH
                    ),
                    expected,
                )

    def test_neither_dword_alone_contains_the_value(self):
        # The point of the wide path: a dword-local search cannot find this
        # field at any bit position or width, so its absence was never
        # evidence that the silicon lacked the register.
        for low, high in (BIOS_90, BIOS_120):
            for dword in (low, high):
                hex_str = dword.to_bytes(4, "little").hex()
                for width in range(7, 13):
                    for shift in range(0, 33 - width):
                        field = extract_value_from_hex(hex_str, shift, width)
                        self.assertNotIn(field, (90, 120))

    def test_low_bits_still_come_from_the_low_dword(self):
        low, high = BIOS_90
        self.assertEqual(
            extract_value_from_wide_hex(qword_hex(low, high), 0, 7),
            extract_value_from_hex(low.to_bytes(4, "little").hex(), 0, 7),
        )

    def test_rejects_input_that_is_not_eight_bytes(self):
        with self.assertRaises(ValueError):
            extract_value_from_wide_hex("D0658204", 0, 8)


# The Core Ultra 200S positions, as wired. tWRPDEN is the only one with a
# two-value BIOS test behind it; the other three come from the reference
# layout that agrees with every position this bench established on its own.
EXPECTED_WIDE = {
    "tWRPDEN": (0xE050, 27, 10),
    "tCKCKEH": (0xE050, 37, 5),
    "tSR": (0xE4C0, 45, 6),
    "tXSDLL": (0xE4C0, 51, 13),
}


class ArrowLakeWiringTest(unittest.TestCase):
    def test_wide_fields_are_declared_where_they_were_measured(self):
        from rochviewer.intel.intel_timings import ARROW_LAKE_POWER_DOWN_WIDE

        self.assertEqual(ARROW_LAKE_POWER_DOWN_WIDE, EXPECTED_WIDE)

    def test_twrpden_is_no_longer_listed_as_unreadable(self):
        from rochviewer.intel.intel_timings import (
            ARROW_LAKE_POWER_DOWN_UNKNOWN,
        )

        self.assertNotIn("tWRPDEN", ARROW_LAKE_POWER_DOWN_UNKNOWN)

    def test_every_wide_field_actually_needs_the_wide_read(self):
        # A field that fits entirely above bit 32 can be reached by pointing
        # at the upper dword instead, which is how the base table handles
        # these. The wide read is only justified where the field would
        # otherwise be unreachable or the position is stated against the
        # 64-bit register, so each entry must at least extend past bit 31.
        for name, (_, start, length) in EXPECTED_WIDE.items():
            with self.subTest(field=name):
                self.assertGreater(start + length, 32)

    def test_no_two_rows_claim_the_same_bits(self):
        # tXPDLL was reading 0xE050 bits 14..20, which is where this platform
        # keeps tCPDED, so both rows printed the same number. Overlap between
        # a wide field and a standard one at the same offset is the shape of
        # that bug, and it should not reappear.
        from rochviewer.intel.intel_timings import (
            ARROW_LAKE_POWER_DOWN,
            ARROW_LAKE_POWER_DOWN_WIDE,
        )

        claimed = {}
        for source in (ARROW_LAKE_POWER_DOWN, ARROW_LAKE_POWER_DOWN_WIDE):
            for name, (offset, start, length) in source.items():
                for bit in range(start, start + length):
                    owner = claimed.setdefault((offset, bit), name)
                    self.assertEqual(
                        owner,
                        name,
                        "0x%X bit %d claimed by both %s and %s"
                        % (offset, bit, owner, name),
                    )

    def test_txpdll_reads_nothing_rather_than_tcpded(self):
        from rochviewer.intel.intel_timings import (
            ARROW_LAKE_POWER_DOWN,
            ARROW_LAKE_POWER_DOWN_UNKNOWN,
            ARROW_LAKE_POWER_DOWN_WIDE,
        )

        self.assertIn("tXPDLL", ARROW_LAKE_POWER_DOWN_UNKNOWN)
        self.assertNotIn("tXPDLL", ARROW_LAKE_POWER_DOWN)
        self.assertNotIn("tXPDLL", ARROW_LAKE_POWER_DOWN_WIDE)

    def test_the_channel_mirror_carries_the_read_width(self):
        # A wide row is mirrored onto the second controller for display. The
        # mirror used to stamp both sides "standard" regardless, so the table
        # showed tWRPDEN as 26 -- the field cut off at the dword boundary --
        # while the register table read 90. The row and its two sides must
        # agree on width.
        from rochviewer.intel.intel_timings import (
            ARROW_LAKE_POWER_DOWN_WIDE,
            TIMINGS,
        )

        wide_names = set(ARROW_LAKE_POWER_DOWN_WIDE)
        seen = set()
        for timing in TIMINGS:
            name = timing.get("name")
            if name not in wide_names or timing.get("read_type") != "wide":
                continue
            seen.add(name)
            for side in ("a", "b"):
                key = "read_type_%s" % side
                if key in timing:
                    self.assertEqual(timing[key], "wide", "%s %s" % (name, key))

        if seen:
            self.assertTrue(wide_names >= seen)

    def test_declared_widths_stay_inside_the_register(self):
        for name, (_, start, length) in EXPECTED_WIDE.items():
            with self.subTest(field=name):
                self.assertLessEqual(start + length, 64)


if __name__ == "__main__":
    unittest.main()


class UnmappedRegisterTest(unittest.TestCase):
    """An all-ones dword is the absence of a device, not a value.

    0x2CE8 is unmapped on Core Ultra 200S. Reading it succeeds and returns
    0xFFFFFFFF, so the eleven drive-strength rows decoding it each showed 255
    -- a plausible level that no register ever stated. Rows that read nothing
    have to say so.
    """

    def test_a_fully_set_dword_reads_as_nothing(self):
        from rochviewer.hardware import read as R

        real = R.read_physical_memory
        R.read_physical_memory = lambda addr, size=4: b"\xff" * size
        try:
            self.assertIsNone(R.read_timing(address=0xFEDC2CE8, bit_start=0,
                                            bit_length=8))
        finally:
            R.read_physical_memory = real

    def test_a_narrow_field_of_ones_is_still_a_value(self):
        # Scoped to the whole dword on purpose: tXSR legitimately reads 0x3FF
        # out of a register holding 0x000003FF, and blanking that would lose a
        # real reading to a rule aimed at a different problem.
        from rochviewer.hardware import read as R

        real = R.read_physical_memory
        R.read_physical_memory = lambda addr, size=4: (0x000003FF).to_bytes(
            size, "little")
        try:
            self.assertEqual(
                R.read_timing(address=0xFEDCE4C0, bit_start=0, bit_length=13),
                0x3FF,
            )
        finally:
            R.read_physical_memory = real
