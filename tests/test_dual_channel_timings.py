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

"""Cover the Timings tab reading both DIMM channels.

The two columns are the two installed modules, and how far apart their
registers sit depends on the memory generation. DDR5 puts two channels inside
one controller, 0x800 across, and a module drives one of them. DDR4 has no
sub-channels: its second module is on the second controller, one window up,
and the 0x800 block there is a channel that was never trained.

Both have been got wrong in turn, and neither is obvious from the ordinary
timing registers, which hold the same values in every combination on a matched
kit. Only a per-DIMM trained result tells the candidates apart: the DFE taps on
DDR5, the RTL latencies on DDR4.
"""

import unittest
from unittest import mock

from platform_profiles import LGA1700_DDR4, LGA1700_DDR5, LGA1851
from tests.intel_stub import MCHBAR, MCHBAR2, MC_WINDOW, READS, install, restore

intel_timings = None


def setUpModule():
    global intel_timings
    intel_timings = install()


def tearDownModule():
    restore()


# Rows on the Timings tab that come from the global MCHBAR region rather than a
# per-controller block, so they have no second channel to show.
GLOBAL_REGISTER_ROWS = ("Refresh Mode",)


def timings_rows():
    return [row for row in intel_timings.TIMINGS if row.get("Tab") == "Timings"]


def register_rows():
    """Every row in the table, by name, wherever its tab put it.

    Register layout is a hardware fact and does not move when a row is shown
    somewhere else, so the tests that pin bit positions look here rather than
    at one tab.
    """
    return {row.get("name"): row for row in intel_timings.TIMINGS}


def field(row):
    """One row's ``(offset from MCHBAR, bit_start, bit_length)``.

    A row shown in both channels keeps its reading under the ``_a`` keys; a
    single-value row keeps it under the plain ones. The bits are the same
    either way.
    """
    address = row.get("address_a", row.get("address"))
    parameters = row.get("parameters_a", row.get("parameters"))
    return (address - MCHBAR, parameters["bit_start"],
            parameters["bit_length"])


class ChannelBAddressTest(unittest.TestCase):
    """The mirror only claims an address it can derive from a channel-A one."""

    def test_the_offset_follows_the_memory_generation(self):
        # DDR5 puts the second module on a sub-channel of the same controller,
        # 0x800 across. DDR4 has no sub-channels and puts it on the second
        # controller. Each was established from per-DIMM trained results on its
        # own bench: DFE taps on DDR5, RTL latencies on DDR4.
        self.assertEqual(intel_timings.channel_b_offset(LGA1700_DDR5), 0x800)
        self.assertEqual(intel_timings.channel_b_offset(LGA1851), 0x800)
        self.assertEqual(
            intel_timings.channel_b_offset(LGA1700_DDR4), MCHBAR2 - MCHBAR
        )

    def test_the_ddr4_stub_mirrors_onto_the_second_controller(self):
        # The stub is a Z790 DDR4 board. Reading +0x800 there lands on a
        # channel the controller never trained: tCL 5 against the real 18, and
        # a tCWL low enough to drive the computed tWTR rows negative.
        self.assertEqual(intel_timings.CHANNEL_B_OFFSET, MCHBAR2 - MCHBAR)
        self.assertEqual(
            intel_timings._channel_b_address(MCHBAR + 0xE070),
            MCHBAR2 + 0xE070,
        )

    def test_address_mirrors_one_channel_across(self):
        self.assertEqual(
            intel_timings._channel_b_address(MCHBAR + 0xE070),
            MCHBAR + intel_timings.CHANNEL_B_OFFSET + 0xE070,
        )

    def test_lowest_window_address_mirrors(self):
        self.assertEqual(
            intel_timings._channel_b_address(MCHBAR),
            MCHBAR + intel_timings.CHANNEL_B_OFFSET,
        )

    def test_address_already_in_channel_b_is_not_mirrored_again(self):
        self.assertIsNone(
            intel_timings._channel_b_address(
                MCHBAR + intel_timings.CHANNEL_B_OFFSET + 0xE020
            )
        )

    def test_address_below_the_window_has_no_twin(self):
        self.assertIsNone(intel_timings._channel_b_address(MCHBAR - 1))

    def test_unrelated_address_has_no_twin(self):
        self.assertIsNone(intel_timings._channel_b_address(0xFEE00000))

    def test_missing_or_non_integer_address_has_no_twin(self):
        for value in (None, "0xFEDCE070", 3.5, True):
            with self.subTest(value=value):
                self.assertIsNone(intel_timings._channel_b_address(value))


class PromotionRuleTest(unittest.TestCase):
    """The promotion helpers, exercised on synthetic rows."""

    def test_standard_row_gains_both_sides_and_keeps_its_bitfield(self):
        row = {
            "name": "tFAKE", "Tab": "Timings", "Category": "Primary",
            "address": MCHBAR + 0xE008,
            "parameters": {"bit_start": 3, "bit_length": 7},
            "read_type": "standard",
        }
        self.assertTrue(intel_timings._promote_standard_row(row))
        self.assertEqual(row["address_a"], MCHBAR + 0xE008)
        self.assertEqual(
            row["address_b"],
            MCHBAR + intel_timings.CHANNEL_B_OFFSET + 0xE008,
        )
        self.assertEqual(row["parameters_a"], {"bit_start": 3, "bit_length": 7})
        self.assertEqual(row["parameters_b"], {"bit_start": 3, "bit_length": 7})

    def test_each_side_gets_its_own_parameters_dict(self):
        row = {
            "address": MCHBAR + 0xE008,
            "parameters": {"bit_start": 3, "bit_length": 7},
        }
        intel_timings._promote_standard_row(row)
        row["parameters_a"]["bit_start"] = 99
        self.assertEqual(row["parameters_b"]["bit_start"], 3)

    def test_row_without_a_bitfield_is_left_alone(self):
        row = {"address": MCHBAR + 0xE008, "parameters": {}}
        self.assertFalse(intel_timings._promote_standard_row(row))
        self.assertNotIn("address_b", row)

    def test_row_outside_the_window_is_left_alone(self):
        row = {
            "address": 0xFEE00000,
            "parameters": {"bit_start": 0, "bit_length": 4},
        }
        self.assertFalse(intel_timings._promote_standard_row(row))
        self.assertNotIn("address_b", row)

    def test_dynamic_row_searches_the_second_controller(self):
        row = {
            "read_type": "dynamic",
            "dynamic_params": {"offset_start": 0xE600, "mchbar": MCHBAR},
        }
        self.assertTrue(intel_timings._promote_dynamic_row(row))
        self.assertEqual(row["dynamic_params_a"]["mchbar"], MCHBAR)
        self.assertEqual(row["dynamic_params_b"]["mchbar"],
                         MCHBAR + intel_timings.CHANNEL_B_OFFSET)
        self.assertEqual(row["dynamic_params_b"]["offset_start"], 0xE600)

    def test_dynamic_row_not_anchored_to_mc0_is_left_alone(self):
        row = {"read_type": "dynamic", "dynamic_params": {"mchbar": MCHBAR2}}
        self.assertFalse(intel_timings._promote_dynamic_row(row))
        self.assertNotIn("dynamic_params_b", row)

    def test_computed_row_calls_its_getter_once_per_controller(self):
        seen = []
        row = {}
        intel_timings._promote_computed_row(row, lambda base: seen.append(base))
        row["value_a"]()
        row["value_b"]()
        self.assertEqual(
            seen, [MCHBAR, MCHBAR + intel_timings.CHANNEL_B_OFFSET]
        )

    def test_a_general_row_on_the_timings_tab_stays_single(self):
        row = {
            "name": "Shared Setting", "Tab": "Timings", "Category": "General",
            "address": MCHBAR + 0xE008,
            "parameters": {"bit_start": 0, "bit_length": 4},
            "read_type": "standard",
        }
        intel_timings.TIMINGS.append(row)
        self.addCleanup(intel_timings.TIMINGS.remove, row)

        intel_timings._install_dual_channel_timings()

        self.assertFalse(intel_timings.is_dual_timing(row))

    def test_rerunning_the_pass_does_not_disturb_promoted_rows(self):
        before = {
            row["name"]: (row.get("address_a"), row.get("address_b"))
            for row in timings_rows()
        }
        intel_timings._install_dual_channel_timings()
        after = {
            row["name"]: (row.get("address_a"), row.get("address_b"))
            for row in timings_rows()
        }
        self.assertEqual(before, after)


class SectionLayoutTest(unittest.TestCase):
    """Power down was the tab's catch-all; each row sits with its own kind now."""

    def _sections(self):
        sections = {}
        for row in timings_rows():
            sections.setdefault(row.get("Category"), []).append(row.get("name"))
        return sections

    def test_rows_moved_out_of_power_down(self):
        sections = self._sections()
        for name, category in intel_timings.TIMINGS_SECTION_MOVES.items():
            with self.subTest(name=name):
                self.assertIn(name, sections.get(category, []))
                self.assertNotIn(name, sections.get("Power down", []))

    def test_power_down_keeps_only_entry_and_exit_timings(self):
        # What is left is the thing the section is named for.
        left = set(self._sections().get("Power down", []))
        self.assertEqual(left, {
            "tWRPDEN", "tRDPDEN", "tPRPDEN", "tAONPD", "tCPDED", "tCKE",
            "tXP", "tXPDLL", "tXSDLL", "tXSR", "tCKCKEH", "tPPD", "tSR",
        })

    def test_refresh_sits_above_tertiary_in_the_same_column(self):
        from main import TIMINGS_SECTION_ORDER

        columns = intel_timings.TIMINGS_TAB_COLUMNS
        self.assertEqual(columns["Refresh timings"], columns["Tertiary"])
        self.assertLess(TIMINGS_SECTION_ORDER.index("Refresh timings"),
                        TIMINGS_SECTION_ORDER.index("Tertiary"))

    def test_no_section_is_ordered_twice(self):
        # A category listed twice sorts by its first appearance and reads as
        # if the second placement were doing something.
        from main import TIMINGS_SECTION_ORDER

        self.assertEqual(len(TIMINGS_SECTION_ORDER),
                         len(set(TIMINGS_SECTION_ORDER)))

    def test_each_section_draws_as_one_block(self):
        # The tab starts a new block every time the category changes as it
        # walks the rows, so a category whose rows are not contiguous renders
        # as two headings with the same name. Re-categorising by name left
        # Refresh timings appearing three times and two others twice.
        for column in ("Left", "Right"):
            seen, previous = [], None
            for row in timings_rows():
                if row.get("Column") != column:
                    continue
                category = row.get("Category")
                if category != previous:
                    with self.subTest(column=column, category=category):
                        self.assertNotIn(
                            category, seen,
                            "%s draws as more than one block" % category)
                    seen.append(category)
                    previous = category

    def test_rows_keep_their_order_inside_a_section(self):
        # Grouping closes the gaps between a section's rows; it must not
        # shuffle them, or the reading order of every section changes.
        names = [row.get("name") for row in timings_rows()
                 if row.get("Category") == "Power down"]
        self.assertEqual(names[:4],
                         ["tWRPDEN", "tRDPDEN", "tPRPDEN", "tAONPD"])

    def test_every_moved_row_names_a_row_that_exists(self):
        names = {row.get("name") for row in timings_rows()}
        for name in intel_timings.TIMINGS_SECTION_MOVES:
            with self.subTest(name=name):
                self.assertIn(name, names)

    def test_a_row_that_restates_another_sits_directly_under_it(self):
        # Each of these says the same thing as the row above in different
        # units or from a different source, so reading it anywhere else on
        # the tab means hunting for what it is a restatement of.
        # Declared names: tREFI (ns) is what the row is called in the table
        # and DDR5_TIMING_LABELS draws it as tREFIns.
        names = [row.get("name") for row in timings_rows()]
        for restated, restatement in (("tWR", "tWR_MR"), ("tRTP", "tRTP_MR"),
                                      ("tREFI", "tREFI (ns)")):
            with self.subTest(name=restatement):
                self.assertIn(restatement, names)
                self.assertEqual(names[names.index(restated) + 1],
                                 restatement)


class DecTcwlTest(unittest.TestCase):
    """The write-leveling decrement, and the part of it that is inferred.

    The register is established -- the reference tool reads exactly three
    registers this project does not, and the code around this one emits
    exactly three field descriptors against a group holding exactly three
    fields. The field width is not: the other two read zero, so the whole
    register is the value and any width from 3 bits up reads the same.
    """

    def _row(self):
        return next((row for row in intel_timings.TIMINGS
                     if row.get("name") == intel_timings.DEC_TCWL_ROW), None)

    def test_it_reads_the_register_the_reference_tool_reads(self):
        row = self._row()
        self.assertIsNotNone(row)
        self.assertEqual(row["address_a"] - MCHBAR,
                         intel_timings.DEC_TCWL_OFFSET)

    def test_it_shows_both_channels(self):
        # The whole point of carrying it: this is a trained per-channel
        # value, and the reference tool draws only one of the two. A row
        # that collapsed to a single column would show less, not the same.
        row = self._row()
        self.assertTrue(intel_timings.is_dual_timing(row))
        self.assertEqual(row["address_b"] - row["address_a"],
                         intel_timings.channel_b_offset())

    def test_it_sits_under_the_timing_it_adjusts(self):
        names = [row.get("name") for row in timings_rows()]
        self.assertEqual(names[names.index("tCWL") + 1],
                         intel_timings.DEC_TCWL_ROW)

    def test_the_width_covers_the_bound_the_control_declares(self):
        # Four bits, from the reference control's own maximum of 15. Pinned
        # because it is the one inferred part: a wider real field only
        # matters if DEC_TCWL ever exceeds 15, and then this truncates.
        row = self._row()
        self.assertEqual(row["parameters_a"]["bit_start"], 0)
        self.assertEqual(row["parameters_a"]["bit_length"], 4)
        self.assertGreaterEqual((1 << row["parameters_a"]["bit_length"]) - 1, 15)

    def test_arrow_lake_says_nothing_rather_than_reading_the_offset(self):
        # That platform moved several registers in this block, so the same
        # offset there would read a plausible number meaning something else.
        # Run the installer as Arrow Lake would and read the row back.
        row = self._row()
        saved = dict(row)
        try:
            with mock.patch.object(intel_timings, "is_arrow_lake_platform",
                                   return_value=True):
                intel_timings._install_arrow_lake_power_down_rows()
            self.assertEqual(row.get("value"), "N/A")
            self.assertNotIn("address", row)
            self.assertNotIn("parameters", row)
        finally:
            row.clear()
            row.update(saved)
        # And back to a real reading once the row is restored.
        self.assertEqual(self._row()["address_a"] - MCHBAR,
                         intel_timings.DEC_TCWL_OFFSET)


class TimingsLivenessTest(unittest.TestCase):
    def test_no_row_renders_a_reading_taken_at_import(self):
        # Judged by what the renderer reaches for: a dual row draws from its
        # two sides and never looks at a stale `value` left on the dict.
        for row in timings_rows():
            name = row.get("name")
            if not name or not name.strip():
                continue
            if intel_timings.is_dual_timing(row):
                keys = ("value_a", "value_b")
            else:
                if row.get("read_type") == "dynamic" and "dynamic_params" in row:
                    continue
                if row.get("address") is not None and "parameters" in row:
                    continue
                keys = ("value",)
            for key in keys:
                reading = row.get(key)
                if reading is None or reading == "":
                    continue
                with self.subTest(name=name, key=key):
                    self.assertTrue(
                        callable(reading),
                        "%s renders a stored %r" % (name, reading))


class ReferenceAdditionsTest(unittest.TestCase):
    """The rows the reference tools' POWERDOWN group carried and we did not.

    Keyed on the register each one reads rather than on the section it sits
    in: they were added under Power down and have since been sorted into the
    sections that describe them, which does not change what they read.
    """

    ADDED = {
        # name: (register, bit start, bit length)
        "tSR": (0xE4C4, 20, 6),
        "tOSCO": (0xE494, 0, 8),
        "tPREMRR": (0xE494, 8, 7),
        "tMRR": (0xE494, 22, 7),
        "tRFM": (0xE40C, 0, 11),
    }

    def _rows(self):
        return {row.get("name"): row for row in timings_rows()}

    def test_each_one_reads_the_field_the_reference_map_names(self):
        rows = self._rows()
        for name, (offset, start, length) in self.ADDED.items():
            with self.subTest(name=name):
                row = rows.get(name)
                self.assertIsNotNone(row, "%s is not on the tab" % name)
                self.assertEqual(row["address_a"] - MCHBAR, offset)
                self.assertEqual(row["parameters_a"]["bit_start"], start)
                self.assertEqual(row["parameters_a"]["bit_length"], length)

    def test_tsr_reads_the_upper_half_of_its_register(self):
        # The map writes it as bit 52 of a 64-bit read at 0xE4C0. Taken at
        # face value against a 32-bit read that shifts past the end and
        # reports 0; it is bits 20-25 of 0xE4C4.
        row = self._rows()["tSR"]
        self.assertEqual(row["address_a"] - MCHBAR, 0xE4C4)
        self.assertEqual(row["parameters_a"]["bit_start"], 20)

    def test_they_joined_the_tab_without_displacing_anything(self):
        names = [name for name in self._rows() if name]
        for existing in ("tXSR", "tCKE", "OREF_RI"):
            self.assertIn(existing, names)
        self.assertEqual(len(set(names)), len(names))

    def test_no_row_is_named_for_a_field_that_does_not_exist(self):
        # tPREMRW was asked for and neither reference tool defines it. The row
        # wanted was tPREMRR.
        names = self._rows()
        self.assertNotIn("tPREMRW", names)
        self.assertIn("tPREMRR", names)

    def test_the_refresh_arbitration_fields_tile_0xe438(self):
        # OREF_RI holds bits 0-7 and tREFIx9 bits 24-31; the reference tool's
        # six names fill the gap exactly. A layout that only half fitted would
        # still read plausible numbers, so what pins it is that all six match
        # its dump on this bench at once -- 6, 7, 0, 1, 2, 5.
        rows = register_rows()
        expected = {
            "Refresh HP WM": (8, 4), "Refresh panic WM": (12, 4),
            "CounttREFIWhileRefEnOff": (16, 1), "HPRefOnMRS": (17, 1),
            "SRX_Ref_Debits": (18, 2), "RAISE_BLK_WAIT": (20, 4),
        }
        for name, (start, length) in expected.items():
            with self.subTest(name=name):
                row = rows.get(name)
                self.assertIsNotNone(row, "%s is missing" % name)
                self.assertEqual(field(row), (0xE438, start, length))

    def test_the_fields_of_0xe438_do_not_overlap(self):
        # Including the two rows that were already there: an overlap would
        # mean one of the seven is reading someone else's bits.
        spans = []
        for row in register_rows().values():
            if row.get("address_a", row.get("address", 0)) - MCHBAR != 0xE438:
                continue
            _offset, start, length = field(row)
            spans.append((start, start + length))
        spans.sort()
        self.assertEqual(len(spans), 8, "expected the whole register covered")
        for (_start, end), (next_start, _next_end) in zip(spans, spans[1:]):
            self.assertLessEqual(end, next_start, "fields overlap: %s" % spans)
        self.assertEqual(spans[0][0], 0)
        self.assertEqual(spans[-1][1], 32)

    def test_the_per_bank_refresh_controls_share_trfcpb_register(self):
        # PBR is per-bank refresh, so its controls sit in the register tRFCpb
        # already reads, below its bits 10-20 with the ABR release above.
        rows = register_rows()
        expected = {
            "PBR Disable": (0, 1), "PBR OOO Disable": (1, 1),
            "PBR Disable on hot": (3, 1), "PBR Exit on idle": (4, 6),
            "Refresh ABR release": (21, 4),
        }
        for name, (start, length) in expected.items():
            with self.subTest(name=name):
                row = rows.get(name)
                self.assertIsNotNone(row, "%s is missing" % name)
                self.assertEqual(field(row), (0xE488, start, length))

    def test_the_dll_codes_share_the_bwsel_register(self):
        # 0x01BC tiles as CODEPI 0-5, CODEWL 6-11, BWSEL 12-17. These three
        # are Skew rows, not Timings ones, so they are looked up in the whole
        # table rather than through the tab-scoped helper.
        rows = {row.get("name"): row for row in intel_timings.TIMINGS}
        for name, start in (("DLL_CODEPI", 0), ("DLL_CODEWL", 6),
                            ("DLL BWSEL", 12)):
            with self.subTest(name=name):
                row = rows.get(name)
                self.assertIsNotNone(row, "%s is missing" % name)
                self.assertEqual(row["address"] - MCHBAR, 0x01BC)
                self.assertEqual(row["parameters"]["bit_start"], start)
                self.assertEqual(row["parameters"]["bit_length"], 6)

    def test_tmrrmrw_reads_the_field_beside_tmrr(self):
        # Identified here before it was wanted, at 0xE494 bits 15-21, and
        # carried now that it was asked for. It reads 90 against the reference
        # tool's 90, and sits with the tMRR it pairs with rather than in the
        # power-down section its register neighbours land in.
        row = self._rows().get("tMRRMRW")
        self.assertIsNotNone(row)
        self.assertEqual(row["address_a"] - MCHBAR, 0xE494)
        self.assertEqual(row["parameters_a"]["bit_start"], 15)
        self.assertEqual(row["parameters_a"]["bit_length"], 7)
        self.assertEqual(row["Category"], "Command")

    def test_tdllk_steps_every_second_code(self):
        # Same MR13 nibble as tCCD_L, different formula: 8 and 9 both give
        # 2048. A per-code step would make 9 read 2304 and look plausible.
        module = intel_timings
        saved = module.read_mode_register
        self.addCleanup(
            lambda: setattr(module, "read_mode_register", saved))
        for code, expected in ((0, 1024), (1, 1024), (8, 2048), (9, 2048),
                               (14, 2816)):
            module.read_mode_register = lambda number, base=None, c=code: c
            with self.subTest(code=code):
                self.assertEqual(module.get_dllk_timing(), expected)

    def test_the_reserved_code_is_not_a_lock_time(self):
        module = intel_timings
        saved = module.read_mode_register
        self.addCleanup(
            lambda: setattr(module, "read_mode_register", saved))
        module.read_mode_register = lambda number, base=None: 0x0F
        self.assertIsNone(module.get_dllk_timing())
        module.read_mode_register = lambda number, base=None: None
        self.assertIsNone(module.get_dllk_timing())


class InstalledTableTest(unittest.TestCase):
    """The table the GUI actually renders."""

    def test_the_tab_has_rows(self):
        self.assertGreater(len(timings_rows()), 50)

    def test_every_per_controller_row_reads_both_channels(self):
        single = [
            row.get("name") for row in timings_rows()
            if not intel_timings.is_dual_timing(row)
            and row.get("name") not in GLOBAL_REGISTER_ROWS
        ]
        self.assertEqual(single, [])

    def test_the_global_row_stays_single(self):
        # Refresh mode comes from DDR_PTM_CTL in the global register region,
        # so there is no second-channel copy of it to show.
        rows = [
            row for row in timings_rows()
            if row.get("name") in GLOBAL_REGISTER_ROWS
        ]
        self.assertTrue(rows)
        for row in rows:
            with self.subTest(row=row.get("name")):
                self.assertFalse(intel_timings.is_dual_timing(row))

    def test_the_global_row_stays_narrow_enough_for_a_channel_column(self):
        # It now shares a section with per-channel rows, so its text renders in
        # the A1 column, and _align_dual_columns widens that column across the
        # whole tab to fit the longest entry. The labels have to stay in the
        # same league as the timings beside them.
        widest_timing = max(
            len(str(intel_timings.apply_formula(65535, None))), len("144 ns")
        )
        for label in intel_timings.REFRESH_MODE_LABELS.values():
            with self.subTest(label=label):
                self.assertLessEqual(len(label), widest_timing * 3)

    def test_every_mirrored_pair_differs_by_exactly_one_channel(self):
        for row in timings_rows():
            if "address_a" not in row:
                continue
            with self.subTest(row=row.get("name")):
                self.assertEqual(row["address_b"] - row["address_a"],
                                 intel_timings.CHANNEL_B_OFFSET)
                self.assertEqual(row["parameters_a"], row["parameters_b"])

    def test_both_sides_of_a_pair_read_their_own_channel(self):
        offset = intel_timings.CHANNEL_B_OFFSET
        for row in timings_rows():
            if "address_a" not in row:
                continue
            with self.subTest(row=row.get("name")):
                # Side A is always in the first controller's first channel.
                # Where side B lands depends on the generation, so it is
                # checked against the offset rather than a fixed window.
                self.assertTrue(MCHBAR <= row["address_a"] < MCHBAR2)
                self.assertFalse((row["address_a"] - MCHBAR) & offset)
                self.assertTrue((row["address_b"] - MCHBAR) & offset)
                self.assertEqual(row["address_b"] - row["address_a"], offset)

    def test_columns_are_named_for_the_populated_slots(self):
        # The stub's modules sit in DIMMA2 and DIMMB2, so those are the names.
        # A fixed "A1"/"B1" would have claimed sockets nothing is plugged into.
        labelled = [row for row in timings_rows() if "name_a" in row]
        self.assertTrue(labelled)
        for row in labelled:
            with self.subTest(row=row.get("name")):
                self.assertEqual(row["name_a"], "A2")
                self.assertEqual(row["name_b"], "B2")
                # main.py blanks the header above the name gutter on this token.
                self.assertEqual(row["parameter_name"], "Name")

    def test_primary_timings_are_present_and_dual(self):
        by_name = {row.get("name"): row for row in timings_rows()}
        for name in ("tCL", "tRCD", "tRP", "tRAS", "tRC"):
            with self.subTest(name=name):
                self.assertIn(name, by_name)
                self.assertTrue(intel_timings.is_dual_timing(by_name[name]))

    def test_derived_rows_are_dual_even_without_an_address(self):
        by_name = {row.get("name"): row for row in timings_rows()}
        for name in ("tWR", "tRFC (ns)", "CR", "tWTR_L", "tWTR_S"):
            with self.subTest(name=name):
                self.assertIn(name, by_name)
                self.assertIn("value_b", by_name[name])

    def test_a_derived_row_reads_the_channel_it_was_asked_for(self):
        trc = next(row for row in timings_rows() if row.get("name") == "tRC")

        READS.clear()
        trc["value_a"]()
        side_a = {address for address in READS if address}

        READS.clear()
        trc["value_b"]()
        side_b = {address for address in READS if address}

        # tRC is tRAS + tRP, so each side reads those two registers on its own
        # channel. The platform-detection register is read either way and is
        # not part of the timing, so it is excluded.
        detect = MCHBAR + 0x13D10
        channel_b = MCHBAR + intel_timings.CHANNEL_B_OFFSET
        self.assertIn(MCHBAR + 0xE004, side_a)
        self.assertIn(MCHBAR + 0xE000, side_a)
        self.assertIn(channel_b + 0xE004, side_b)
        self.assertIn(channel_b + 0xE000, side_b)
        self.assertNotIn(channel_b + 0xE004, side_a - {detect})
        self.assertNotIn(MCHBAR + 0xE004, side_b - {detect})

    # Skew carries channel columns too, on the rows whose registers were
    # shown to have a channel-B twin. Misc does not: its registers do have
    # twins, but they hold the same values and a second column left the value
    # column too narrow for the text this tab shows.
    CHANNEL_COLUMN_TABS = ("Timings", "Skew")

    def test_no_other_tab_was_given_channel_columns(self):
        for row in intel_timings.TIMINGS:
            if row.get("Tab") in self.CHANNEL_COLUMN_TABS:
                continue
            with self.subTest(tab=row.get("Tab"), row=row.get("name")):
                self.assertNotEqual(
                    row.get("name_a"), intel_timings.CHANNEL_A_LABEL
                )

    def test_unnamed_slots_fall_back_to_the_controller(self):
        # A board whose firmware does not name its sockets still gets a usable
        # header, rather than a slot number invented for it.
        import dimm_inventory

        original = dimm_inventory.read_modules
        dimm_inventory.read_modules = lambda *a, **k: [{"slot": None}]
        self.addCleanup(setattr, dimm_inventory, "read_modules", original)

        self.assertEqual(intel_timings._channel_labels(), ("ChA", "ChB"))


if __name__ == "__main__":
    unittest.main()
