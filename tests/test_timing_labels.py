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

"""Cover the DDR5 timing row names and the paired tRFCns value."""

import contextlib
import unittest
from unittest import mock

from rochviewer.platform_profiles import AM5, LGA1700_DDR4, LGA1700_DDR5, LGA1851
from tests.intel_stub import install, restore

intel_timings = None


def setUpModule():
    global intel_timings
    intel_timings = install(LGA1700_DDR5)


def tearDownModule():
    restore()


@contextlib.contextmanager
def table_for(platform):
    """The Intel table as it is built for ``platform``.

    install() saves the modules it replaces into one dict, so calling it
    while a fixture is already up would save the stubs over the real modules
    and the outer restore would put stubs back. The outer fixture is torn
    down first and rebuilt after: siblings, never nested.
    """
    restore()
    try:
        yield install(platform)
    finally:
        restore()
        global intel_timings
        intel_timings = install(LGA1700_DDR5)


class Ddr5LabelTest(unittest.TestCase):
    def test_ddr5_names_the_all_bank_interval_trfc2(self):
        for platform in (LGA1700_DDR5, LGA1851):
            with self.subTest(platform=platform):
                self.assertEqual(
                    intel_timings.ddr5_timing_label(platform, "tRFC"), "tRFC2")

    def test_ddr4_keeps_trfc(self):
        # DDR4 has one refresh interval and no per-bank refresh at all, so
        # tRFC2 would name something the modules do not have.
        self.assertEqual(
            intel_timings.ddr5_timing_label(LGA1700_DDR4, "tRFC"), "tRFC")

    def test_the_derived_nanosecond_rows_are_named_alike_on_both(self):
        # A derived row restating a raw one in nanoseconds is the same row on
        # either generation, so it carries one name on both. These used to be
        # renamed on DDR5 only, which left DDR4 reading "tRFC (ns)" against
        # the DDR5 board's "tRFCns" for the identical calculation.
        for platform in (LGA1700_DDR4, LGA1700_DDR5, LGA1851):
            for name in ("tRFCns", "tREFIns"):
                with self.subTest(platform=platform, name=name):
                    self.assertEqual(
                        intel_timings.ddr5_timing_label(platform, name), name)

    def test_only_trfc_is_still_renamed_per_platform(self):
        # tRFC really is a different register on DDR5. Nothing else is.
        self.assertEqual(set(intel_timings.DDR5_TIMING_LABELS), {"tRFC"})

    def test_an_unlisted_row_is_never_renamed(self):
        # tRFCns and tREFIns are in here deliberately: they are the declared
        # names now, carried unchanged onto DDR5 rather than renamed onto it.
        for name in ("tCL", "tRFCpb", "tREFI", "tRP", "tRFCns", "tREFIns"):
            with self.subTest(name=name):
                self.assertEqual(
                    intel_timings.ddr5_timing_label(LGA1700_DDR5, name), name)

    def test_am5_is_not_an_intel_ddr5_platform(self):
        # It has its own profile and its own tRFC2; this map must not reach it.
        self.assertNotIn(AM5, intel_timings.DDR5_TIMING_PLATFORMS)

    def test_a_ddr5_table_carries_the_renamed_rows(self):
        # Built for DDR5 rather than read from whatever the bench is, so this
        # runs everywhere. It used to branch on the ambient platform, and the
        # fixture was always DDR4 -- so the DDR5 half never ran on any
        # machine, including the one the rename was written for.
        with table_for(LGA1700_DDR5) as built:
            names = {t.get("name") for t in built.TIMINGS}
        self.assertIn("tRFC2", names)
        self.assertIn("tRFCns", names)
        self.assertNotIn("tRFC", names)

    def test_a_ddr4_table_keeps_the_original_rows(self):
        with table_for(LGA1700_DDR4) as built:
            names = {t.get("name") for t in built.TIMINGS}
        self.assertIn("tRFC", names)
        self.assertNotIn("tRFC2", names)

    def test_the_renamed_rows_keep_what_reads_them(self):
        # The rename runs last precisely so the passes that match rows by name
        # have already attached addresses and dual-channel getters. A row that
        # lost either would render blank under its new name.
        #
        # This skipped on every machine there has ever been: it read the
        # ambient table and the fixture was hardcoded to DDR4, so "tRFC2" was
        # never in it. The skip named the platform, which is what made the
        # dead test visible. It builds its own DDR5 table now and runs.
        with table_for(LGA1700_DDR5) as built:
            by_name = {t.get("name"): t for t in built.TIMINGS}
        # Named before it is used, so a table missing the rename says so
        # rather than raising KeyError from the line below.
        self.assertIn("tRFC2", by_name, "the DDR5 table did not rename tRFC")
        trfc2 = by_name["tRFC2"]
        self.assertIsNotNone(trfc2.get("address"))
        self.assertIsNotNone(trfc2.get("address_a"))
        self.assertIsNotNone(trfc2.get("address_b"))
        trfcns = by_name["tRFCns"]
        self.assertTrue(callable(trfcns.get("value")))
        self.assertTrue(callable(trfcns.get("value_a")))


class PairedRefreshValueTest(unittest.TestCase):
    """tRFCns carries both intervals on DDR5 and one on DDR4."""

    def _getter(self, platform, all_bank, per_bank, mclk="4000 Mhz"):
        module = intel_timings
        saved = (module.active_platform, module.get_mclk,
                 module._read_finalized_timing)

        def restore_all():
            (module.active_platform, module.get_mclk,
             module._read_finalized_timing) = saved

        self.addCleanup(restore_all)
        module.active_platform = lambda: platform
        module.get_mclk = lambda: mclk
        readings = {
            module.ddr5_timing_label(platform, "tRFC"): all_bank,
            "tRFCpb": per_bank,
        }
        module._read_finalized_timing = lambda name, base=None: readings.get(name)
        return module.get_trfc_ns

    def test_ddr5_shows_the_all_bank_then_the_same_bank_interval(self):
        # 480 and 390 ticks at MCLK 4000 are the bench's own numbers. The
        # all-bank interval then the per-bank one, unit named once at the end,
        # which is the shape the AM5 profile uses for the same pair.
        getter = self._getter(LGA1700_DDR5, 480, 390)
        self.assertEqual(getter(), "120/98 (ns)")

    def test_ddr4_shows_one_interval(self):
        getter = self._getter(LGA1700_DDR4, 480, 390)
        self.assertEqual(getter(), "120 ns")

    def test_a_board_without_the_per_bank_interval_still_reports(self):
        # Half an answer beats none, and an absent per-bank read is not an
        # error on a board that does not carry one.
        getter = self._getter(LGA1700_DDR5, 480, None)
        self.assertEqual(getter(), "120 ns")

    def test_no_mclk_means_no_reading(self):
        getter = self._getter(LGA1700_DDR5, 480, 390, mclk=None)
        self.assertIsNone(getter())

    def test_no_interval_means_no_reading(self):
        getter = self._getter(LGA1700_DDR5, None, 390)
        self.assertIsNone(getter())


class SchedulerGearWitnessTest(unittest.TestCase):
    """The gear row read two ways, and what happens when they disagree.

    Values are the ones measured across a Gear 2 -> Gear 4 -> Gear 2 round
    trip on the LGA1700 DDR5 bench. Both transitions moved bits 15 and 31 of
    SC_GS_CFG and nothing else in that register, which is what places them.
    """

    GEAR2_REGISTER = 0x8FC00029
    GEAR4_REGISTER = 0x0FC08029

    def _with_register(self, raw):
        return mock.patch.object(
            intel_timings, "read_physical_memory_int", return_value=raw)

    def test_the_measured_registers_decode_to_the_gear_they_were_read_under(self):
        for raw, gear in ((self.GEAR2_REGISTER, 2), (self.GEAR4_REGISTER, 4)):
            with self.subTest(raw=hex(raw)), self._with_register(raw):
                self.assertEqual(intel_timings.scheduler_gear_mode(), gear)

    def test_exactly_one_flag_moved_between_the_two(self):
        # If both bits belonged to the same field, or one of them were
        # something else that happened to move, this would not hold.
        flipped = self.GEAR2_REGISTER ^ self.GEAR4_REGISTER
        self.assertEqual(
            [bit for bit in range(32) if flipped >> bit & 1],
            [intel_timings.SCHEDULER_GEAR4_BIT,
             intel_timings.SCHEDULER_GEAR2_BIT])

    def test_bit_zero_is_not_the_gear_flag(self):
        # It was the obvious candidate before the measurement -- the only set
        # bit below the known fields -- and it did not move. Pinned so the
        # guess does not get reintroduced.
        self.assertEqual(self.GEAR2_REGISTER & 1, self.GEAR4_REGISTER & 1)
        self.assertNotIn(0, (intel_timings.SCHEDULER_GEAR2_BIT,
                             intel_timings.SCHEDULER_GEAR4_BIT))

    def test_it_says_nothing_rather_than_guessing(self):
        # Neither flag set, both set, an absent driver, an unclaimed
        # register. A witness that answers anyway is not a witness.
        for raw in (0x00000000, 0x80008000, None, 0xFFFFFFFF):
            with self.subTest(raw=raw), self._with_register(raw):
                self.assertIsNone(intel_timings.scheduler_gear_mode())

    # Whether the two witnesses actually agree is asked of real hardware, in
    # test_gear_witness_live: this module runs against the stub, where both
    # answers would be the fixture's rather than the machine's.


class JedecSpellingTest(unittest.TestCase):
    """Rank and bank-group suffixes are separated, the way JEDEC writes them.

    tRRD_L and tWTR_L are JESD79's own spelling, and the reference tools use
    the underscore for the turnaround group too. Without it tRDRDsg reads as
    one word and the suffix that says which bank group it covers disappears
    into the name.
    """

    TURNAROUNDS = ("tRDRD", "tRDWR", "tWRRD", "tWRWR")
    SUFFIXES = ("sg", "dg", "dr", "dd")

    def _names(self):
        return {row.get("name") for row in intel_timings.TIMINGS}

    def test_every_turnaround_separates_its_suffix(self):
        names = self._names()
        for stem in self.TURNAROUNDS:
            for suffix in self.SUFFIXES:
                with self.subTest(name=stem + suffix):
                    self.assertIn("%s_%s" % (stem, suffix), names)
                    self.assertNotIn(stem + suffix, names)

    def test_the_long_and_short_pairs_separate_theirs(self):
        names = self._names()
        for stem in ("tRRD", "tWTR"):
            for suffix in ("L", "S"):
                with self.subTest(name=stem + suffix):
                    self.assertIn("%s_%s" % (stem, suffix), names)
                    self.assertNotIn(stem + suffix, names)


if __name__ == "__main__":
    unittest.main()
