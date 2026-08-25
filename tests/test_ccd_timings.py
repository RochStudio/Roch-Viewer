"""Cover the tCCD rows and the evidence gate standing in front of them."""

import unittest

from display_values import is_dual_timing, resolve_display_value
from tests import intel_stub
from tests.intel_stub import FIELD_READS, MCHBAR, MCHBAR2, install, restore

intel_timings = None

CCD_NAMES = ("tCCD_L", "tCCD_L_WR", "tCCD_L_WR2")

MR13_NAMES = ("tCCD_L", "tCCD_L_WR", "tCCD_L_WR2")

# tCCD is not a row. DDR5 fixes the different-bank-group delay at 8 nCK and
# no mode register carries it, so the row could only be blank or be filled
# with the constant as though it had been read.
UNSOURCED_NAME = "tCCD"


# A table entry names a mode register in its data byte and points at the slot
# holding that register's contents with its index byte. It does not carry the
# value itself, which is what the earlier reading of this table assumed.
PAYLOAD_SLOT = 0x12


def mr_entry(number, slot=PAYLOAD_SLOT):
    """One row of the controller's mode-register table."""
    return 0x80000000 | ((slot & 0xFF) << 8) | (number & 0xFF)


def mr_table(number, value, slot=PAYLOAD_SLOT, at=0xE648):
    """An entry naming ``number``, plus the payload byte it points at."""
    return {at: mr_entry(number, slot), 0xE200 + slot: value}

# 0xE08C across two controlled BIOS changes on the Z790 target: tCCD 6 /
# tCCD_L 10, then tCCD 8 / tCCD_L 14. Identical, so the 6 and 10 it appeared to
# hold were numbers it always held.
E08C_AT_6_10 = 0x04008A06
E08C_AT_8_14 = 0x04008A06

# TC_RDRD across the same two changes: also identical, which is what refuted it
# as the source of these timings.
TC_RDRD_AT_6_10 = 0x08080488
TC_RDRD_AT_8_14 = 0x08080488


def setUpModule():
    global intel_timings
    intel_timings = install()


def tearDownModule():
    restore()


def row(name):
    return next(r for r in intel_timings.TIMINGS if r.get("name") == name)


def pin_generation(case, generation):
    """Hold the DDR generation still for one test.

    tCCD_L comes out of MR13 on DDR5 and MR6 on DDR4, so which register a
    test is describing has to be stated rather than inherited from whatever
    the bench happens to be running.
    """
    saved = intel_timings.detect_ddr_generation
    case.addCleanup(
        setattr, intel_timings, "detect_ddr_generation", saved)
    intel_timings.detect_ddr_generation = lambda: generation


class EvidenceGateTest(unittest.TestCase):
    """A timing with nothing to read must report nothing.

    Only tCCD is in that position now. DDR5 fixes the different-bank-group
    delay at 8 nCK and no register or mode register carries it, so filling the
    row with the constant would present an assumption as a reading.
    """

    def test_no_field_ships_confirmed(self):
        self.assertEqual(intel_timings.CCD_CONFIRMED_FIELDS, {})

    def test_the_unsourced_row_reports_no_value(self):
        self.assertIsNone(intel_timings.get_ccd_timing(UNSOURCED_NAME))

    def test_the_unsourced_name_has_no_row(self):
        names = {r.get("name") for r in intel_timings.TIMINGS}
        self.assertNotIn(UNSOURCED_NAME, names)

    def test_the_unsourced_name_does_not_read_hardware_at_all(self):
        FIELD_READS.clear()
        self.assertIsNone(intel_timings.get_ccd_timing(UNSOURCED_NAME))
        self.assertEqual(FIELD_READS, [])

    def test_an_unknown_timing_name_reports_no_value(self):
        self.assertIsNone(intel_timings.get_ccd_timing("tNOPE"))


class ModeRegisterTest(unittest.TestCase):
    """tCCD_L comes from MR13, which is where the DRAM keeps it.

    Two mistakes stood in front of this row. The first sweep concluded
    nothing carried these timings: it swept for the cycle count across a
    BIOS change, but MR13 stores a 4-bit JEDEC code, so the search was for
    the wrong representation. The reading that replaced it then matched the
    table entry by its index and returned its data byte, which is the lookup
    inside out -- the data byte names the register and the index points at
    the payload. That gave 2 where MR13 holds 8, and tCCD_L 10 rather
    than 16.
    """

    def setUp(self):
        self.addCleanup(setattr, intel_stub, "response", None)
        pin_generation(self, "DDR5")

    def _table(self, entries, base=MCHBAR):
        def response(address, bit_start, bit_length):
            value = entries.get(address - base, 0)
            return (value >> bit_start) & ((1 << bit_length) - 1)

        intel_stub.response = response

    def test_the_table_is_searched_for_the_requested_register(self):
        self._table(mr_table(0x0D, 0x08))
        self.assertEqual(intel_timings.read_mode_register(0x0D), 0x08)

    def test_the_entry_names_the_register_and_points_at_the_value(self):
        # The distinction the old reading got backwards. Here the entry names
        # MR13 and points at slot 0x12; the value lives there, not in the
        # entry. Reading the entry's own bytes would give 0x12 or 0x0D.
        self._table(mr_table(0x0D, 0x08))
        self.assertEqual(intel_timings.read_mode_register(0x0D), 0x08)
        self.assertNotIn(intel_timings.read_mode_register(0x0D), (0x12, 0x0D))

    def test_a_register_the_table_does_not_list_reports_nothing(self):
        self._table(mr_table(0x0D, 0x08))
        self.assertIsNone(intel_timings.read_mode_register(0x0A))

    def test_an_entry_without_the_prefix_is_not_a_mode_register(self):
        # The table shares its range with other data; the prefix is what says
        # a dword is one of these entries.
        self._table({0xE648: 0x1234120D, 0xE212: 0x08})
        self.assertIsNone(intel_timings.read_mode_register(0x0D))

    def test_the_bench_code_decodes_to_the_three_timings(self):
        # MR13 holds 0x08 on the Z790 bench, identically on both controllers.
        self._table(mr_table(0x0D, 0x08))
        self.assertEqual(intel_timings.get_ccd_timing("tCCD_L"), 16)
        self.assertEqual(intel_timings.get_ccd_timing("tCCD_L_WR"), 32)
        self.assertEqual(intel_timings.get_ccd_timing("tCCD_L_WR2"), 64)

    def test_the_encoding_holds_across_the_range(self):
        # JESD79-5: 8 + code, 16 + 2*code, 32 + 4*code.
        for code, expected in ((0, (8, 16, 32)),
                               (6, (14, 28, 56)),
                               (14, (22, 44, 88))):
            with self.subTest(code=code):
                self._table(mr_table(0x0D, code))
                self.assertEqual(
                    tuple(intel_timings.get_ccd_timing(n) for n in MR13_NAMES),
                    expected,
                )

    def test_the_reserved_code_is_not_a_timing(self):
        self._table(mr_table(0x0D, 0x0F))
        for name in MR13_NAMES:
            with self.subTest(name=name):
                self.assertIsNone(intel_timings.get_ccd_timing(name))

    def test_only_the_low_nibble_carries_the_timing(self):
        # The rest of MR13 is other settings and must not shift the answer.
        self._table(mr_table(0x0D, 0xF8))
        self.assertEqual(intel_timings.get_ccd_timing("tCCD_L"), 16)

    def test_it_reads_the_controller_it_was_given(self):
        self._table(mr_table(0x0D, 0x06), base=MCHBAR2)
        self.assertEqual(intel_timings.get_ccd_timing("tCCD_L", MCHBAR2), 14)

    def test_an_absent_table_reports_nothing_rather_than_guessing(self):
        self._table({})
        for name in MR13_NAMES:
            with self.subTest(name=name):
                self.assertIsNone(intel_timings.get_ccd_timing(name))


class Ddr4ModeRegisterTest(unittest.TestCase):
    """DDR4 keeps tCCD_L in MR6, out of the shadow at 0xE5A0.

    The 0xE600 table DDR5 reads is never filled on DDR4, so MR13 answers
    nothing there. MR6 bits [12:10] read 4 on the bench, which decodes to 8
    -- the number the BIOS shows for tCCD_L.
    """

    # MR6 = 0x101C: VrefDQ 28, range 0, tCCD_L code 4.
    BENCH_MR6 = 0x101C

    def setUp(self):
        self.addCleanup(setattr, intel_stub, "response", None)
        pin_generation(self, "DDR4")

    def _shadow(self, mr6, base=MCHBAR):
        saved = intel_timings.read_physical_memory_int
        self.addCleanup(
            setattr, intel_timings, "read_physical_memory_int", saved)
        where = base + intel_timings.DDR4_MODE_REGISTER_BASE + 12
        intel_timings.read_physical_memory_int = (
            lambda address, size=4: mr6 if address == where else 0)

    def test_the_bench_register_gives_the_timing_the_bios_shows(self):
        self._shadow(self.BENCH_MR6)
        self.assertEqual(intel_timings.get_ccd_timing("tCCD_L"), 8)

    def test_the_encoding_holds_across_its_range(self):
        for code, expected in enumerate((4, 5, 6, 7, 8)):
            with self.subTest(code=code):
                self._shadow(0x1C | (code << 10))
                self.assertEqual(intel_timings.get_ccd_timing("tCCD_L"),
                                 expected)

    def test_an_undefined_code_is_not_a_timing(self):
        self._shadow(0x1C | (5 << 10))
        self.assertIsNone(intel_timings.get_ccd_timing("tCCD_L"))

    def test_the_write_variants_have_no_ddr4_register(self):
        # DDR5 derives them from the same MR13 nibble. DDR4 defines no such
        # field, so deriving them here would print a DDR5 figure.
        self._shadow(self.BENCH_MR6)
        for name in ("tCCD_L_WR", "tCCD_L_WR2"):
            with self.subTest(name=name):
                self.assertIsNone(intel_timings.get_ccd_timing(name))

    def test_it_reads_the_controller_it_was_given(self):
        # Only the second controller's shadow holds the bench value; the
        # first reads zero, which is a different code and a different timing.
        self._shadow(self.BENCH_MR6, base=MCHBAR2)
        self.assertEqual(intel_timings.get_ccd_timing("tCCD_L", MCHBAR2), 8)
        self.assertEqual(intel_timings.get_ccd_timing("tCCD_L", MCHBAR), 4)

    def test_the_ddr5_table_is_not_consulted_at_all(self):
        self._shadow(self.BENCH_MR6)
        FIELD_READS.clear()
        intel_timings.get_ccd_timing("tCCD_L")
        self.assertEqual(FIELD_READS, [])


class RefutedMappingTest(unittest.TestCase):
    """Pin the measurements that withdrew both claims."""

    def test_the_scheduler_register_did_not_move_across_the_bios_changes(self):
        self.assertEqual(TC_RDRD_AT_6_10, TC_RDRD_AT_8_14)

    def test_the_scheduler_register_never_held_any_of_the_settings(self):
        # Neither 6/10 nor 8/14 appears in the .sg or .dg field.
        self.assertEqual(TC_RDRD_AT_8_14 & 0x7F, 8)
        self.assertEqual((TC_RDRD_AT_8_14 >> 8) & 0x7F, 4)

    def test_the_looked_promising_register_did_not_move_either(self):
        self.assertEqual(E08C_AT_6_10, E08C_AT_8_14)

    def test_that_register_held_the_first_pair_by_coincidence(self):
        # It reads 6 and 10 at both settings, so agreeing once meant nothing.
        self.assertEqual(E08C_AT_8_14 & 0x7F, 6)
        self.assertEqual((E08C_AT_8_14 >> 8) & 0x7F, 10)

    def test_no_candidate_survived(self):
        self.assertEqual(intel_timings.CCD_CANDIDATE_FIELDS, {})

    def test_the_effective_delays_are_still_shown_under_their_own_names(self):
        names = [r.get("name") for r in intel_timings.TIMINGS]
        for name in ("tRDRD_sg", "tRDRD_dg", "tWRWR_sg"):
            with self.subTest(name=name):
                self.assertIn(name, names)


class ConfirmedReadTest(unittest.TestCase):
    """Behaviour once a field is confirmed, driven through the map."""

    def setUp(self):
        self.addCleanup(
            setattr, intel_timings, "CCD_CONFIRMED_FIELDS",
            intel_timings.CCD_CONFIRMED_FIELDS,
        )
        # Exercised through tCCD, the row this map still governs: the others
        # now come from MR13 and take that path first.
        intel_timings.CCD_CONFIRMED_FIELDS = {"tCCD": (0xE08C, 8)}
        self.addCleanup(setattr, intel_stub, "response", None)
        pin_generation(self, "DDR5")

    def test_a_confirmed_field_is_read_from_its_register_and_bits(self):
        FIELD_READS.clear()
        intel_timings.get_ccd_timing("tCCD")
        self.assertEqual(FIELD_READS[-1], (MCHBAR + 0xE08C, 8, 7))

    def test_a_confirmed_field_reads_the_controller_it_was_given(self):
        FIELD_READS.clear()
        intel_timings.get_ccd_timing("tCCD", MCHBAR2)
        self.assertEqual(FIELD_READS[-1][0], MCHBAR2 + 0xE08C)

    def test_a_confirmed_field_decodes_from_its_own_bits(self):
        # 14 at bits 8-14, the value a confirmed field would have to report.
        register = 14 << 8
        intel_stub.response = (
            lambda address, bit_start, bit_length:
            (register >> bit_start) & ((1 << bit_length) - 1)
        )
        self.assertEqual(intel_timings.get_ccd_timing("tCCD"), 14)

    def test_an_unreadable_register_still_reports_no_value(self):
        intel_stub.response = lambda address, bit_start, bit_length: None
        self.assertIsNone(intel_timings.get_ccd_timing("tCCD"))

    def test_a_mode_register_row_ignores_this_map(self):
        # MR13 is the source for those, so a stale entry here must not win.
        intel_timings.CCD_CONFIRMED_FIELDS = {"tCCD_L": (0xE08C, 8)}
        intel_stub.response = lambda address, bit_start, bit_length: 0
        self.assertIsNone(intel_timings.get_ccd_timing("tCCD_L"))


class InstalledRowTest(unittest.TestCase):
    def test_every_row_is_on_the_timings_tab(self):
        for name in CCD_NAMES:
            with self.subTest(name=name):
                self.assertEqual(row(name).get("Tab"), "Timings")

    def test_the_mode_register_rows_are_gone(self):
        names = [r.get("name") for r in intel_timings.TIMINGS]
        for stale in ("tCCDL", "tCCDL WR"):
            with self.subTest(name=stale):
                self.assertNotIn(stale, names)

    def test_no_row_still_decodes_through_a_mode_register_table(self):
        for name in CCD_NAMES:
            with self.subTest(name=name):
                self.assertNotIn("Formula", row(name))
                self.assertNotIn("dynamic_params", row(name))

    def test_every_row_reads_both_channels(self):
        for name in CCD_NAMES:
            with self.subTest(name=name):
                self.assertTrue(is_dual_timing(row(name)))

    def test_the_rows_sit_together_after_their_anchor(self):
        # A section draws its rows in table order, so being in the right
        # category is not enough: left among the secondaries they would have
        # rendered at the head of their new section rather than beside the
        # row they belong with.
        names = [
            r.get("name") for r in intel_timings.TIMINGS
            if r.get("Tab") == "Timings"
            and r.get("Category") == intel_timings.CCD_CATEGORY
        ]
        start = names.index(intel_timings.CCD_ANCHOR) + 1
        self.assertEqual(names[start:start + len(CCD_NAMES)], list(CCD_NAMES))

    def test_the_values_are_read_lazily_rather_than_at_import(self):
        self.assertTrue(callable(row("tCCD_L")["value"]))


if __name__ == "__main__":
    unittest.main()
