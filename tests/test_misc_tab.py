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

"""Cover the Misc tab: its rows, its decoding and the tab's registration."""

import inspect
import unittest

from rochviewer.ui import main

from rochviewer.ui.display_values import select_tab_names
from tests.intel_stub import install, restore

intel_timings = None


def setUpModule():
    global intel_timings
    intel_timings = install()


def tearDownModule():
    restore()


MISC_CATEGORIES = ("Power Down", "Command", "ECS", "Features", "Preamble",
                   "Mode Registers")


def _reading(row):
    """One row's displayed value."""
    return row["value"]() if callable(row.get("value")) else None


class MiscRowTest(unittest.TestCase):
    def _tab_rows(self):
        """Former Misc rows now combined into per-module Training."""
        return [t for t in intel_timings.TIMINGS
                if t.get("Tab") == "Training"
                and t.get("source_scope") == "module"
                and not t.get("diagnostic")]

    def _rows(self):
        """The original per-module Misc descriptor rows."""
        names = (
            {"Burst Length", "DQS Interval Timer RT"}
            | {name for name, *_ in intel_timings.MISC_MODE_REGISTER_COMMAND}
            | {name for name, *_ in intel_timings.MISC_MODE_REGISTER_ECS}
            | {name for name, *_ in intel_timings.MISC_MODE_REGISTER_FIELDS}
            | {name for name, *_ in intel_timings.MISC_MODE_REGISTER_STATE}
        )
        return [
            t for t in self._tab_rows()
            if t.get("name") in names and "value" in t
        ]

    def _settings_rows(self):
        """The fixed controller-register rows now combined into PHY."""
        return [t for t in intel_timings.TIMINGS
                if t.get("Tab") == intel_timings.IMC_TAB
                and t.get("source_scope") == "controller"]

    def test_every_row_from_both_reference_blocks_is_present(self):
        rows = self._rows()
        if not rows:
            self.skipTest("Misc tab is not installed on this platform")
        names = [t.get("name") for t in rows]
        expected = (
            ["Burst Length"]
            + [name for name, _, _, _, _
               in intel_timings.MISC_MODE_REGISTER_COMMAND]
            + [name for name, _, _, _, _
               in intel_timings.MISC_MODE_REGISTER_ECS]
            # Preamble and the mode-register state follow ECS at the
            # foot of the left column; Features is the right column's
            # own block and comes after them in the table.
            + [name for name, _, _, _, _
               in intel_timings.MISC_MODE_REGISTER_FIELDS]
            # tWR_MR and tRTP_MR are built here and then moved to the
            # Timings tab, beside the tWR and tRTP they restate, so
            # they are not on this tab to be found.
            + [name for name, _, _, _, _
               in intel_timings.MISC_MODE_REGISTER_STATE
               if name not in ("tWR_MR", "tRTP_MR")]
            + ["DQS Interval Timer RT"]
        )
        if intel_timings.detect_ddr_generation() == "DDR4":
            # The rows DDR4 has no counterpart for, and the ones it has that
            # cannot be reached, are both gated off. What remains is still in
            # the declared order.
            gated = (set(intel_timings.DDR5_ONLY_MISC_ROWS)
                     | set(intel_timings.DDR4_UNREACHABLE_MISC_ROWS))
            self.assertTrue(gated & set(expected))
            expected = [name for name in expected if name not in gated]
        # Presence, not order. The tab is now ordered by section rather than
        # by the sequence the passes that build it happen to run in, so the
        # build order this list describes is no longer the drawn order --
        # test_the_sections_are_drawn_in_the_declared_order covers that.
        self.assertEqual(sorted(names), sorted(expected))
        self.assertEqual(len(names), len(set(names)))

    def test_the_sections_are_drawn_in_the_declared_order(self):
        rows = self._tab_rows()
        if not rows:
            self.skipTest("Misc tab is not installed on this platform")
        seen = []
        for row in rows:
            category = row.get("Category")
            if not seen or seen[-1] != category:
                seen.append(category)
        # Each section appears once and only once: a category showing up in
        # two places means rows of one kind were split across the tab.
        self.assertEqual(len(seen), len(set(seen)), seen)
        seen = [
            name for name, _rows in main.ordered_sections(
                [(name, []) for name in seen], main.SKEW_SECTION_ORDER
            )
        ]
        declared = [name for name in main.SKEW_SECTION_ORDER
                    if name in set(seen)]
        self.assertEqual(seen, declared)

    def test_sections_use_the_requested_columns(self):
        rows = self._rows()
        if not rows:
            self.skipTest("Misc tab is not installed on this platform")
        for row in self._tab_rows():
            with self.subTest(name=row.get("name")):
                self.assertEqual(
                    row.get("Column"),
                    intel_timings.DDR4_TRAINING_TWO_COLUMN_COLUMNS[
                        row.get("Category")
                    ],
                )

    def test_each_misc_column_reads_in_the_requested_order(self):
        requested = {
            "Left": [
                "RTT", "ODT", "RON", "ODT DELAY", "VREF",
                "DLL / LATENCY", "DATA CONTROL", "DFE", "ODTL",
            ],
            "Right": [
                "MPR / WRITE", "Command", "PARITY / CRC",
                "REFRESH / POWER", "PREAMBLE / PPR",
            ],
        }
        for column, expected in requested.items():
            seen = []
            for row in self._tab_rows():
                if row.get("Column") != column:
                    continue
                category = row.get("Category")
                if not seen or seen[-1] != category:
                    seen.append(category)
            with self.subTest(column=column):
                self.assertEqual(seen, [name for name in expected if name in seen])

    def test_every_row_carries_a_value(self):
        # A row with no value renders as an empty line rather than as a
        # missing reading. Most rows read two channels and carry value_a and
        # value_b instead of one value.
        for row in self._rows():
            with self.subTest(name=row.get("name")):
                self.assertIsNotNone(_reading(row))

    def test_no_row_is_frozen_at_import(self):
        # A value resolved while building the table is a snapshot of startup
        # that never moves again, so every row holds a getter.
        for row in self._rows():
            with self.subTest(name=row.get("name")):
                self.assertTrue(callable(row.get("value")))

    def test_each_mode_register_row_reads_both_modules(self):
        # Type-5 fields are evaluated in the selected DIMM context.
        # Equal readings are still two sources and may diverge after training.
        for row in self._rows():
            with self.subTest(name=row.get("name")):
                self.assertTrue(intel_timings.is_dual_timing(row))
                self.assertTrue(callable(row.get("value_a")))
                self.assertTrue(callable(row.get("value_b")))

    def test_each_row_reads_its_own_field(self):
        # Late binding in the building loops would give every row the last
        # field's parameters, and the whole tab would show one number. Tested
        # by feeding one register value and checking the rows disagree, since
        # the bit positions differ even when the register does not.
        module = intel_timings
        saved = module.read_physical_memory_int
        self.addCleanup(
            lambda: setattr(module, "read_physical_memory_int", saved))
        module.read_physical_memory_int = lambda address, size: 0x08104426
        by_name = {row["name"]: _reading(row)
                   for row in self._settings_rows()}
        cke = [name for name, _, _ in intel_timings.MISC_CKE_CONFIG_FIELDS]
        self.assertGreater(len({by_name[name] for name in cke}), 1)
        # The bench's own register: bits 1-4 hold 3 and bits 24-27 hold 8.
        self.assertEqual(by_name["idle_length"], "3")
        self.assertEqual(by_name["ckevalid_length"], "8")

    def test_the_tab_is_offered_once_it_has_rows(self):
        tabs = select_tab_names(intel_timings.TIMINGS)
        if self._rows():
            self.assertIn("Training", tabs)
            self.assertNotIn(intel_timings.MISC_TAB, tabs)
        else:
            self.assertNotIn(intel_timings.MISC_TAB, tabs)

    def test_the_latency_rows_have_their_own_rtl_tab(self):
        if not self._rows():
            self.skipTest("Misc tab is not installed on this platform")
        rtl = [t for t in intel_timings.TIMINGS if t.get("Tab") == "RTL"]
        self.assertEqual(len([t for t in rtl if t.get("name")]), 32)

    def test_the_latency_tab_is_registered(self):
        tabs = select_tab_names(intel_timings.TIMINGS)
        if self._rows():
            self.assertIn(intel_timings.RTL_TAB, tabs)
        else:
            self.assertNotIn(intel_timings.MISC_TAB, tabs)

    def test_rtl_is_split_into_two_channel_columns(self):
        rtl = [t for t in intel_timings.TIMINGS
               if t.get("Tab") == intel_timings.RTL_TAB]
        counts = {column: sum(t.get("Column") == column for t in rtl)
                  for column in ("Left", "Right")}
        self.assertEqual(counts, {"Left": 17, "Right": 17})
        self.assertEqual(
            {t.get("Category") for t in rtl}, {"RTL CHA", "RTL CHB"}
        )

    def test_latency_spacers_do_not_flip_cross_column_shading(self):
        latency = [row for row in intel_timings.TIMINGS
                   if row.get("Tab") == intel_timings.RTL_TAB]
        self.assertTrue(latency)
        # One blank row in each channel separates MC0 from MC1.
        for category in {row.get("Category") for row in latency}:
            rows = [row for row in latency if row.get("Category") == category]
            self.assertEqual(
                sum(not str(row.get("name", "")).strip() for row in rows), 1
            )

    def test_the_cke_fields_do_not_overlap(self):
        # The whole block comes out of one register, so an overrun would make
        # one field silently carry a neighbour's bits -- the CLK Drv Dn trap.
        claimed = {}
        for name, start, length in intel_timings.MISC_CKE_CONFIG_FIELDS:
            for bit in range(start, start + length):
                self.assertLess(bit, 32, f"{name} runs past the register")
                self.assertNotIn(bit, claimed,
                                 f"{name} overlaps {claimed.get(bit)}")
                claimed[bit] = name


class SettingsSourceSplitTest(unittest.TestCase):
    """Combined Training/IMC preserve the source-scope split."""

    def _settings(self):
        return [row for row in intel_timings.TIMINGS
                if row.get("Tab") == intel_timings.IMC_TAB
                and row.get("source_scope") == "controller"]

    def test_fixed_controller_rows_move_to_phy(self):
        expected = {
            *[name for name, _, _ in intel_timings.MISC_CKE_CONFIG_FIELDS],
            *[name for name, _, _ in intel_timings.MISC_GS_CONFIG_FIELDS],
            *[name for name, _, _, _, _
              in intel_timings.MISC_FEATURE_FIELDS],
            *intel_timings.REFRESH_POLICY_ROWS,
            *[name for name, *_
              in intel_timings.DDR4_ADDITIONAL_COMMAND_FIELDS],
            *[name for name, *_
              in intel_timings.DDR4_ADDITIONAL_POWER_DOWN_FIELDS],
            *[name for name, *_
              in intel_timings.DDR4_ADDITIONAL_PHY_FIELDS],
        }
        rows = {row.get("name"): row for row in self._settings()}
        self.assertLessEqual(expected, set(rows))
        for name in expected:
            with self.subTest(name=name):
                self.assertEqual(rows[name].get("source_scope"), "controller")
                self.assertFalse(intel_timings.is_dual_timing(rows[name]))

    def test_every_settings_row_is_single_source(self):
        for row in self._settings():
            with self.subTest(name=row.get("name")):
                self.assertEqual(row.get("source_scope"), "controller")
                self.assertFalse(intel_timings.is_dual_timing(row))

    def test_shared_sections_use_the_four_column_phy_layout(self):
        expected = intel_timings.PHY_SETTINGS_COLUMNS
        for row in self._settings():
            with self.subTest(name=row.get("name")):
                self.assertIn(row.get("Category"), expected)
                self.assertEqual(row.get("Column"),
                                 expected[row.get("Category")])

    def test_imc_signal_groups_share_one_ordered_column(self):
        expected = ("DATA", "CMD", "CLK", "CTL", "SComp")
        self.assertEqual(
            tuple(name for name in main.IMC_SECTION_ORDER if name in expected),
            expected,
        )
        self.assertTrue(all(
            intel_timings.PHY_SETTINGS_COLUMNS[name] == "Right"
            for name in expected
        ))

    def test_imc_power_down_follows_refresh_in_the_left_column(self):
        order = main.IMC_SECTION_ORDER
        self.assertEqual(order[order.index("Refresh") + 1], "Power Down")
        self.assertEqual(
            intel_timings.PHY_SETTINGS_COLUMNS["Power Down"], "Left"
        )

    def test_every_former_misc_row_has_an_independent_module_source(self):
        rows = [row for row in intel_timings.TIMINGS
                if row.get("Tab") == "Training"
                and row.get("source_scope") == "module"
                and str(row.get("name", "")).strip()]
        for row in rows:
            with self.subTest(name=row.get("name")):
                self.assertTrue(intel_timings.is_dual_timing(row))

    def test_mode_register_pair_keeps_its_verified_source_reader(self):
        row = next(row for row in intel_timings.TIMINGS
                   if row.get("Tab") == "Training"
                   and row.get("name") == "Burst Length")
        reader = row.get("base_reader")
        self.assertTrue(callable(reader))
        # _promote_computed_row captures this same source once for A1 and
        # once for B1; neither side is a copied startup value.
        self.assertIs(row["value_a"].__defaults__[0], reader)
        self.assertIs(row["value_b"].__defaults__[0], reader)

    def test_shared_settings_are_combined_into_phy(self):
        tabs = select_tab_names(intel_timings.TIMINGS)
        self.assertIn(intel_timings.IMC_TAB, tabs)
        self.assertNotIn(intel_timings.SETTINGS_TAB, tabs)


class MiscDecodeTest(unittest.TestCase):
    def _with_register(self, value):
        module = intel_timings
        saved = module.read_physical_memory_int
        self.addCleanup(lambda: setattr(module, "read_physical_memory_int", saved))
        module.read_physical_memory_int = lambda address, size: value

    def test_a_field_is_masked_to_its_own_width(self):
        # 0x08104426 is the bench's live CKE register.
        self._with_register(0x08104426)
        self.assertEqual(intel_timings._misc_number(0xE0B8, 1, 4), "3")
        self.assertEqual(intel_timings._misc_number(0xE0B8, 24, 4), "8")
        self.assertEqual(intel_timings._misc_number(0xE0B8, 31, 1), "0")

    def test_an_absent_register_reads_na_rather_than_zero(self):
        # A missing read must not be reported as a disabled feature.
        self._with_register(None)
        self.assertEqual(intel_timings._misc_number(0xE0B8, 1, 4), "N/A")
        self.assertEqual(intel_timings._misc_switch(0x3E00, 0, 1, False), "N/A")
        self._with_register(0xFFFFFFFF)
        self.assertEqual(intel_timings._misc_switch(0x3E00, 0, 1, False), "N/A")

    def test_a_plain_switch_reads_the_bit_directly(self):
        self._with_register(0x1)
        self.assertEqual(intel_timings._misc_switch(0x3E00, 0, 1, False),
                         "Enabled")
        self._with_register(0x0)
        self.assertEqual(intel_timings._misc_switch(0x3E00, 0, 1, False),
                         "Disabled")

    def test_the_disable_bit_is_inverted(self):
        # The map names this field dis_pt_it, so a set bit turns the timeout
        # off and a clear bit means it is running.
        self._with_register(1 << 6)
        self.assertEqual(intel_timings._misc_switch(0xE028, 6, 1, True),
                         "Disabled")
        self._with_register(0)
        self.assertEqual(intel_timings._misc_switch(0xE028, 6, 1, True),
                         "Enabled")

    def test_only_the_page_close_timeout_inverts(self):
        inverted = {name for name, _, _, _, flag
                    in intel_timings.MISC_FEATURE_FIELDS if flag}
        self.assertEqual(inverted, {"Page Close Idle Timeout"})

    def _with_mode_register(self, payload):
        module = intel_timings
        saved = (module._mode_register_pointer, module.read_physical_memory_int)

        def restore_all():
            (module._mode_register_pointer,
             module.read_physical_memory_int) = saved

        self.addCleanup(restore_all)
        self.asked = []

        def pointer(data_byte, command=0, offset_base=0xE200, base=None):
            self.asked.append((data_byte, command))
            return 0xFEDCE211

        module._mode_register_pointer = pointer
        module.read_physical_memory_int = lambda address, size: payload

    def test_the_preamble_fields_decode_the_live_mr8(self):
        # 0x88 is the bench's MR8.
        self._with_mode_register(0x88)
        read = intel_timings._misc_mode_register_value(
            0x08, 0, 3, intel_timings.MISC_READ_PREAMBLE)
        self.assertEqual(read, "1 tCK - 10 Pattern")
        write = intel_timings._misc_mode_register_value(
            0x08, 3, 2, intel_timings.MISC_WRITE_PREAMBLE)
        self.assertEqual(write, "2 tCK - 0010 Pattern")
        self.assertEqual(
            intel_timings._misc_mode_register_value(
                0x08, 6, 1, intel_timings.MISC_POSTAMBLE),
            "0.5 tCK - 0 Pattern")
        self.assertEqual(
            intel_timings._misc_mode_register_value(
                0x08, 7, 1, intel_timings.MISC_POSTAMBLE),
            "1.5 tCK - 010 Pattern")
        self.assertEqual(self.asked[-1], (0x08, 0))

    def test_preamble_training_names_its_state_rather_than_switching(self):
        # The cleared bit is a mode the DRAM is in, not a feature that is off,
        # so it reads Normal Mode and never Disabled. 0x90 is the bench's MR2.
        self._with_mode_register(0x90)
        self.assertEqual(
            intel_timings._misc_mode_register_value(
                0x02, 0, 1, intel_timings.MISC_READ_PREAMBLE_TRAINING),
            "Normal Mode")
        self._with_mode_register(0x91)
        self.assertEqual(
            intel_timings._misc_mode_register_value(
                0x02, 0, 1, intel_timings.MISC_READ_PREAMBLE_TRAINING),
            "Read Preamble Training")

    def test_twr_mr_decodes_the_full_ddr5_range(self):
        # The high codes are valid DDR5 write-recovery settings. Code 11 is
        # the 114-clock setting used by the reference machine; it must not be
        # mistaken for a reserved encoding.
        self._with_mode_register(0x0B)
        self.assertEqual(
            intel_timings._misc_mode_register_value(
                0x06, 0, 4, intel_timings.MR_TWR),
            "114")
        self.assertEqual(
            [intel_timings.MR_TWR[code] for code in range(9, 15)],
            ["102", "108", "114", "120", "126", "132"])
        self.assertEqual(intel_timings.MR_TWR[15], "Reserved")

    def test_tcl_mr_decodes_the_ddr5_mr0_field(self):
        # MR0 0x20 carries code 8 in A6:A2, the reference machine's CL38.
        self._with_mode_register(0x20)
        self.assertEqual(
            intel_timings._misc_mode_register_value(
                0x00, 2, 5, intel_timings.MR_TCL),
            "38")
        self.assertEqual(intel_timings.MR_TCL[0], "22")
        self.assertEqual(intel_timings.MR_TCL[31], "84")

    def test_fine_granularity_refresh_uses_the_short_label(self):
        self.assertEqual(intel_timings.REFRESH_MODE_FORMULA[1], "FGR")

    def test_a_field_without_a_table_shows_its_number(self):
        # No table means a count or an index, not a switch. It read as
        # Enabled/Disabled once, which turned the ECS error register index --
        # four bits selecting a record -- into "Disabled" at index 0.
        self._with_mode_register(0x90)
        self.assertEqual(
            intel_timings._misc_mode_register_value(0x0E, 0, 4, None), "0")
        self._with_mode_register(0x93)
        self.assertEqual(
            intel_timings._misc_mode_register_value(0x0E, 0, 4, None), "3")

    def test_every_switch_field_carries_the_table_that_says_so(self):
        # The other half of the rule above: a field that means Enabled or
        # Disabled has to say so in a table, because no table now means a
        # number.
        for group in (intel_timings.MISC_MODE_REGISTER_FIELDS,
                      intel_timings.MISC_MODE_REGISTER_COMMAND,
                      intel_timings.MISC_MODE_REGISTER_STATE,
                      intel_timings.MISC_MODE_REGISTER_ECS):
            for name, _number, _start, length, decode in group:
                if length == 1:
                    with self.subTest(name=name):
                        self.assertIsNotNone(decode)

    def test_the_features_read_in_the_requested_order(self):
        order = [name for name, _, _, _, _
                 in intel_timings.MISC_FEATURE_FIELDS]
        self.assertEqual(order, [
            "Realtime Memory", "Power Down", "Error Correction",
            "Self Refresh", "Memory Scrambler", "Row Hammer",
            "Page Close Idle Timeout",
        ])

    def test_an_unnamed_code_is_shown_rather_than_guessed(self):
        # write_pre has no entry for code 0 and read_pre none past 4. A code
        # the reference tables skip is a number, not a wrong pattern name.
        self._with_mode_register(0x00)
        self.assertEqual(
            intel_timings._misc_mode_register_value(
                0x08, 3, 2, intel_timings.MISC_WRITE_PREAMBLE),
            "0")
        self._with_mode_register(0x07)
        self.assertEqual(
            intel_timings._misc_mode_register_value(
                0x08, 0, 3, intel_timings.MISC_READ_PREAMBLE),
            "7")

    def test_a_missing_table_entry_is_not_reported_as_a_setting(self):
        module = intel_timings
        saved = module._mode_register_pointer
        self.addCleanup(
            lambda: setattr(module, "_mode_register_pointer", saved))
        module._mode_register_pointer = (
            lambda data_byte, command=0, offset_base=0xE200, base=None: None)
        self.assertEqual(
            module._misc_mode_register_value(0x08, 0, 3,
                                             module.MISC_READ_PREAMBLE),
            "N/A")

    def test_the_two_clock_read_patterns_are_kept_distinct(self):
        # Codes 1 and 2 are both two clocks with different patterns. Folding
        # them together would look like a tidy-up and lose a real difference.
        table = intel_timings.MISC_READ_PREAMBLE
        self.assertNotEqual(table[1], table[2])
        self.assertEqual(len(set(table.values())), len(table))

    def test_burst_length_decodes_mr0(self):
        # 0x1C is the bench's MR0: BL16 in the low two bits, CL 36 above.
        self._with_mode_register(0x1C)
        self.assertEqual(
            intel_timings._misc_mode_register_value(
                *intel_timings.MISC_BURST_LENGTH_FIELD),
            "BL16")
        self.assertEqual(self.asked[-1], (0x00, 0))

    def test_the_burst_length_names_come_from_the_reference_table(self):
        # Code 1 is the on-the-fly eight-burst and code 2 the optional
        # thirty-two. Reading the JEDEC order off the top of one's head gives
        # BL32 at code 1, which would look right and be wrong.
        self.assertEqual(intel_timings.MISC_BURST_LENGTHS, {
            0: "BL16",
            1: "BC8 OTF",
            2: "BL32 (Optional)",
            3: "BL32 OTF (Optional)",
        })

    def test_burst_length_reads_mr0_through_the_pointer_path(self):
        # Matching the table entry by index and taking its data byte finds
        # nothing for MR0, which is why this row once read N/A. The lookup
        # must ask for mode register 0 and follow the pointer.
        module = intel_timings
        saved = module._mode_register_pointer
        self.addCleanup(
            lambda: setattr(module, "_mode_register_pointer", saved))
        module._mode_register_pointer = (
            lambda data_byte, command=0, offset_base=0xE200, base=None: None)
        self.assertEqual(
            module._misc_mode_register_value(
                *module.MISC_BURST_LENGTH_FIELD),
            "N/A")


class Ddr4MiscRowTest(unittest.TestCase):
    """Only the rows DDR4 has no counterpart for come off on DDR4."""

    def test_the_ddr5_only_rows_are_the_ones_ddr4_cannot_have(self):
        # Checked field by field against JESD79-4. Anything DDR4 does carry
        # -- fine granularity refresh, Qoff, TDQS, DM, gear-down, the DRAM's
        # WR/RTP -- is not on this list, whether or not it can be read.
        gated = set(intel_timings.DDR5_ONLY_MISC_ROWS)
        self.assertIn("Read Postamble", gated)
        self.assertIn("ECS Mode", gated)
        for kept in ("Refresh tRFC Mode", "Data Output Disable",
                     "TDQS Enable", "DM Enable", "CS Geardown",
                     "tWR_MR", "tRTP_MR", "Burst Length"):
            self.assertNotIn(kept, gated)

    def test_the_preambles_are_unreachable_rather_than_absent(self):
        # DDR4 has all three -- MR4 A10, A11 and A12, one clock or two, the
        # tRPRE and tWPRE a BIOS exposes. They come off the tab because the
        # bit positions are unconfirmed, not because the fields do not exist,
        # and the two lists have to keep saying different things.
        absent = set(intel_timings.DDR5_ONLY_MISC_ROWS)
        unreachable = set(intel_timings.DDR4_UNREACHABLE_MISC_ROWS)
        self.assertEqual(
            unreachable,
            {"Read Preamble Training", "Read Preamble", "Write Preamble"},
        )
        self.assertFalse(absent & unreachable)

    def test_the_postambles_are_absent_and_not_merely_unreachable(self):
        # DDR4 fixes both at 0.5 tCK with no register behind them, so they
        # belong on the other list however the preamble question resolves.
        unreachable = set(intel_timings.DDR4_UNREACHABLE_MISC_ROWS)
        for name in ("Read Postamble", "Write Postamble"):
            with self.subTest(name=name):
                self.assertNotIn(name, unreachable)
                self.assertIn(name, intel_timings.DDR5_ONLY_MISC_ROWS)

    def test_every_gated_row_is_a_row_the_tab_actually_declares(self):
        declared = {name for name, _, _, _, _ in
                    intel_timings.MISC_MODE_REGISTER_FIELDS
                    + intel_timings.MISC_MODE_REGISTER_STATE
                    + intel_timings.MISC_MODE_REGISTER_COMMAND
                    + intel_timings.MISC_MODE_REGISTER_ECS}
        declared.add("DQS Interval Timer RT")
        self.assertLessEqual(set(intel_timings.DDR5_ONLY_MISC_ROWS), declared)

    def test_the_whole_ecs_block_goes_since_ddr4_has_no_on_die_ecc(self):
        ecs = {name for name, _, _, _, _
               in intel_timings.MISC_MODE_REGISTER_ECS}
        self.assertTrue(ecs)
        self.assertLessEqual(ecs, set(intel_timings.DDR5_ONLY_MISC_ROWS))


class Ddr4ModeRegisterPlacementTest(unittest.TestCase):
    """The captured Z790-A shadows decode and split by A2/B2 uniqueness."""

    CAPTURED_A = {
        0: 0x0D70, 1: 0x0003, 2: 0x08F0, 3: 0x0400,
        4: 0x0008, 5: 0x00C0, 6: 0x1021,
    }
    CAPTURED_B = {**CAPTURED_A, 6: 0x1020}

    EXPECTED_A = {
        "CAS Latency": "19",
        "Read Burst Type": "Sequential",
        "Test Mode": "Normal",
        "DLL Reset": "Yes",
        "DLL Enable": "Enabled",
        "Additive Latency": "0 (AL disabled)",
        "Write Leveling": "Disabled",
        "Low Power ASR": "Auto Self Refresh",
        "Write CRC": "Disabled",
        "MPR Page Select": "Page 0",
        "MPR Operation": "Normal",
        "Per DRAM Addr": "Disabled",
        "Temp Sensor Readout": "Disabled",
        "Write CMD Latency": "6 nCK",
        "MPR Read Format": "Serial",
        "Max Power Down": "Disabled",
        "Temp Refresh Range": "Normal",
        "Temp Ctrl Refresh": "Enabled",
        "Internal Vref Mon": "Disabled",
        "Soft PPR": "Disabled",
        "CS to CMD Latency": "Disabled",
        "Self Refresh Abort": "Disabled",
        "Read Preamble Train": "Disabled",
        "Read Preamble": "1 nCK",
        "Write Preamble": "1 nCK",
        "Hard PPR": "Disabled",
        "CA Parity Latency": "Disabled",
        "CRC Error Clear": "Clear",
        "CA Parity Err Status": "Clear",
        "ODT Buffer (PD)": "Activated",
        "CA Parity Persist Err": "Disabled",
        "Write DBI": "Disabled",
        "Read DBI": "Disabled",
        "VrefDQ Train Value": "33",
        "VrefDQ Train Range": "Range 1",
        "VrefDQ Train Enable": "Disabled",
    }

    def setUp(self):
        module = intel_timings
        saved = module._ddr4_mode_register
        self.addCleanup(setattr, module, "_ddr4_mode_register", saved)
        module._ddr4_mode_register = lambda number, base=None: (
            self.CAPTURED_B if base == module.CHANNEL_B else self.CAPTURED_A
        ).get(number)

    def _rows(self):
        return {
            row["name"]: row for row in intel_timings.TIMINGS
            if row.get("name") in
                intel_timings.DDR4_TRAINING_MODE_REGISTER_ROWS
        }

    def test_training_uses_function_names_instead_of_mr_numbers(self):
        rows = [row for row in intel_timings.TIMINGS
                if row.get("Tab") == "Training"]
        self.assertFalse(
            {"MR0 / MR1", "MR2 / MR3", "MR4", "MR5 / MR6"}
            & {row.get("Category") for row in rows}
        )

    def test_every_new_reference_field_is_visible_on_training(self):
        rows = self._rows()
        self.assertEqual(set(rows), set(self.EXPECTED_A))
        self.assertEqual(
            {name: row["value_a"]() for name, row in rows.items()},
            self.EXPECTED_A,
        )

    def test_every_per_dimm_mode_register_field_uses_the_module_selector(self):
        rows = self._rows()
        training = {name for name, row in rows.items()
                    if row.get("Tab") == "Training"}
        self.assertEqual(
            training, set(intel_timings.DDR4_TRAINING_MODE_REGISTER_ROWS)
        )
        row = self._rows()["VrefDQ Train Value"]
        self.assertEqual((row["value_a"](), row["value_b"]()), ("33", "32"))

    def test_every_mode_register_field_keeps_independent_a2_b2_sources(self):
        rows = self._rows()
        for name, row in rows.items():
            with self.subTest(name=name):
                self.assertEqual(row.get("Tab"), "Training")
                self.assertEqual(row.get("source_scope"), "module")
                self.assertTrue(intel_timings.is_dual_timing(row))
                self.assertTrue(callable(row.get("value_a")))
                self.assertTrue(callable(row.get("value_b")))

    def test_a_later_difference_remains_separate_on_training(self):
        self.CAPTURED_B[0] = 0x0D40
        self.addCleanup(self.CAPTURED_B.__setitem__, 0, 0x0D70)
        row = self._rows()["CAS Latency"]
        self.assertEqual((row["value_a"](), row["value_b"]()), ("19", "18"))


class ModeRegisterTimingRowTest(unittest.TestCase):
    """The mode-register copies of controller timings, and where they sit."""

    def _timings(self):
        return [t for t in intel_timings.TIMINGS if t.get("Tab") == "Timings"]

    def test_each_copy_names_a_row_it_restates(self):
        # The placement rule derives the anchor by stripping the suffix, so a
        # copy whose base row does not exist would silently fall to the end of
        # the table instead of sitting under anything.
        names = {t.get("name") for t in self._timings()}
        for copy in intel_timings.MODE_REGISTER_TIMING_ROWS:
            base = copy[: -len(intel_timings.MODE_REGISTER_TIMING_SUFFIX)]
            with self.subTest(row=copy):
                self.assertTrue(copy.endswith(
                    intel_timings.MODE_REGISTER_TIMING_SUFFIX))
                if copy in names:
                    self.assertIn(base, names)

    def test_a_copy_sits_directly_under_what_it_restates(self):
        names = [t.get("name") for t in self._timings()]
        for copy in intel_timings.MODE_REGISTER_TIMING_ROWS:
            if copy not in names:
                continue
            base = copy[: -len(intel_timings.MODE_REGISTER_TIMING_SUFFIX)]
            with self.subTest(row=copy):
                self.assertEqual(names[names.index(base) + 1], copy)

    def test_a_copy_takes_the_section_of_the_row_it_restates(self):
        # Not a fixed "Secondary". tWR and tRTP are both secondaries, which is
        # what the move used to assume; tCCD_L is in Other Timings, and a copy
        # filed under Secondary while sitting between two Other Timings rows
        # puts a row in a section its neighbours are not in.
        rows = {t.get("name"): t for t in self._timings()}
        for copy in intel_timings.MODE_REGISTER_TIMING_ROWS:
            if copy not in rows:
                continue
            base = copy[: -len(intel_timings.MODE_REGISTER_TIMING_SUFFIX)]
            with self.subTest(row=copy):
                self.assertEqual(rows[copy]["Category"],
                                 rows[base]["Category"])
                self.assertEqual(rows[copy]["Column"], rows[base]["Column"])

    def test_every_copy_reads_both_controllers(self):
        rows = {t.get("name"): t for t in self._timings()}
        for copy in intel_timings.MODE_REGISTER_TIMING_ROWS:
            if copy in rows:
                with self.subTest(row=copy):
                    self.assertTrue(intel_timings.is_dual_timing(rows[copy]))

    def test_none_of_them_reaches_the_summary(self):
        # The row each restates is already listed there, so a copy only
        # lengthens the column.
        from rochviewer.ui.main import SUMMARY_EXCLUDED_TIMING_NAMES

        for copy in intel_timings.MODE_REGISTER_TIMING_ROWS:
            with self.subTest(row=copy):
                self.assertIn(copy, SUMMARY_EXCLUDED_TIMING_NAMES)

    def test_the_ccd_l_copy_shares_the_timing_row_s_own_table(self):
        # Derived from DDR4_TCCD_L rather than retyped, so the Misc copy and
        # the Timings row cannot drift into disagreeing about what a code
        # means. Only the rendering differs -- a Misc row is text.
        self.assertEqual(
            intel_timings.DDR4_MR_CCD_L,
            {code: str(value)
             for code, value in intel_timings.DDR4_TCCD_L.items()},
        )

    def test_the_ddr4_cwl_codes_are_the_jedec_ones(self):
        # DDR4's CWL codes are not contiguous: 12 steps to 14, skipping 13.
        # Deriving them as an offset from the code would be wrong from 4 up.
        self.assertEqual(
            intel_timings.DDR4_MR_CWL,
            {0: "9", 1: "10", 2: "11", 3: "12",
             4: "14", 5: "16", 6: "18", 7: "20"},
        )


class EmptyModeRegisterTableTest(unittest.TestCase):
    """A table that was never programmed must not answer lookups.

    DDR4 leaves all 128 entries at zero, and a zero entry matches register 0
    with command 0 pointing at payload index 0 -- so the search for MR0
    succeeded and Burst Length reported BL16, which DDR4 does not have.
    """

    def _with_table(self, entry):
        module = intel_timings
        saved = module.read_timing
        self.addCleanup(lambda: setattr(module, "read_timing", saved))
        module._MODE_REGISTER_TABLE_POPULATED.clear()
        self.addCleanup(module._MODE_REGISTER_TABLE_POPULATED.clear)
        module.read_timing = lambda **kwargs: entry

    def test_an_all_zero_table_answers_nothing(self):
        self._with_table(0)
        self.assertFalse(
            intel_timings._mode_register_table_populated(intel_timings.MCHBAR))
        self.assertIsNone(
            intel_timings._mode_register_pointer(0x00,
                                                 base=intel_timings.MCHBAR))

    def test_burst_length_is_not_invented_out_of_an_empty_table(self):
        self._with_table(0)
        self.assertEqual(
            intel_timings._misc_mode_register_value(
                *intel_timings.MISC_BURST_LENGTH_FIELD),
            "N/A")

    def test_a_programmed_table_still_resolves(self):
        # Data byte 0x00, payload index 0x11, command 0.
        self._with_table(0x00001100)
        self.assertTrue(
            intel_timings._mode_register_table_populated(intel_timings.MCHBAR))
        self.assertEqual(
            intel_timings._mode_register_pointer(0x00,
                                                 base=intel_timings.MCHBAR),
            intel_timings.MCHBAR + 0xE211)

    def test_an_unreadable_table_is_not_cached_as_empty(self):
        # Asked before the driver is up every entry reads None. Caching that
        # would leave the mode registers dead for the rest of the session.
        self._with_table(None)
        self.assertFalse(
            intel_timings._mode_register_table_populated(intel_timings.MCHBAR))
        self.assertEqual(intel_timings._MODE_REGISTER_TABLE_POPULATED, {})


class GenerationGateTest(unittest.TestCase):
    """Which rows each memory generation drops, without building the tab."""

    def _shown(self, generation):
        rows = [{"name": name} for name in (
            "tCL",                      # on both
            "ECS Mode",                 # DDR5 only
            "Read Preamble",            # DDR4 has it, unreachable here
            "Refresh tRFC Mode",        # DDR4 only
        )]
        return [row["name"] for row in
                intel_timings._misc_rows_for_generation(rows, generation)]

    def test_ddr5_drops_the_row_the_timings_tab_already_shows(self):
        # On DDR5 both this and the Timings Refresh Mode row resolve to MR4
        # bit 4 through the same shadow window -- the same bit under two
        # names. The Timings row is the one that stays, above the tRFC rows
        # it applies to.
        shown = self._shown("DDR5")
        self.assertNotIn("Refresh tRFC Mode", shown)
        self.assertEqual(shown, ["tCL", "ECS Mode", "Read Preamble"])

    def test_ddr4_keeps_it_because_there_it_says_something_else(self):
        # DDR4 reads it from MR3[8:6], what the DRAM was commanded, while the
        # Timings row reads DDR_PTM_CTL[3:2], what the controller decided.
        # Those two can disagree.
        shown = self._shown("DDR4")
        self.assertIn("Refresh tRFC Mode", shown)
        self.assertEqual(shown, ["tCL", "Refresh tRFC Mode"])

    def test_the_two_gates_name_no_row_in_common(self):
        # A row on both lists would be dropped everywhere, which is never
        # what either list means.
        self.assertFalse(set(intel_timings.DDR4_ONLY_MISC_ROWS)
                         & set(intel_timings.DDR5_ONLY_MISC_ROWS))
        self.assertFalse(set(intel_timings.DDR4_ONLY_MISC_ROWS)
                         & set(intel_timings.DDR4_UNREACHABLE_MISC_ROWS))


class RefreshPolicyMoveTest(unittest.TestCase):
    """The refresh arbitration controls, moved into shared PHY.

    They are policy the controller applies around refresh, not intervals a
    memory profile sets, and Timings had grown past what its two columns hold.
    """

    def _row(self, name):
        for row in intel_timings.TIMINGS:
            if row.get("name") == name:
                return row
        return None

    def test_every_moved_row_landed_in_the_phy_refresh_section(self):
        for name in intel_timings.REFRESH_POLICY_ROWS:
            with self.subTest(name=name):
                row = self._row(name)
                self.assertIsNotNone(row, "%s is missing" % name)
                self.assertEqual(row.get("Tab"), intel_timings.IMC_TAB)
                self.assertEqual(row.get("Category"), "Refresh")

    def test_the_moved_rows_show_one_value_like_the_rest_of_the_tab(self):
        # These are fixed controller-register rows, not type-5
        # fields evaluated in a selected DIMM's mode-register context.
        for name in intel_timings.REFRESH_POLICY_ROWS:
            with self.subTest(name=name):
                row = self._row(name)
                self.assertFalse(intel_timings.is_dual_timing(row))
                self.assertIn("address", row)
                self.assertIn("parameters", row)

    def test_the_whole_of_0xe488_moved_together(self):
        # All four PBR controls share that register, so leaving one on
        # Timings split a single register across two tabs.
        for name in ("PBR Disable", "PBR OOO Disable", "PBR Disable on hot",
                     "PBR Exit on idle"):
            with self.subTest(name=name):
                self.assertIn(name, intel_timings.REFRESH_POLICY_ROWS)

    def test_the_additional_refresh_fields_match_the_reference_map(self):
        expected = {
            "Refresh Interval": (0xE444, 0, 13),
            "Refresh Stagger En": (0xE444, 15, 1),
            "Refresh Stagger Mode": (0xE444, 16, 1),
            "Disable Stolen Refresh": (0xE444, 13, 1),
            "Enable Refresh Type Display": (0xE444, 14, 1),
            "tREFI Pulse Stagger Dis": (0xE444, 17, 1),
            "Wake Up On HPM": (0xE444, 19, 13),
        }
        for name, (offset, start, length) in expected.items():
            with self.subTest(name=name):
                row = self._row(name)
                self.assertIsNotNone(row)
                self.assertEqual(row["address"], intel_timings.MCHBAR + offset)
                self.assertEqual(
                    row["parameters"],
                    {"bit_start": start, "bit_length": length},
                )

    def test_the_upper_scheduler_fields_request_a_wide_read(self):
        expected = {"Write 0": 49, "MultiCycCmd": 51}
        for name, start in expected.items():
            with self.subTest(name=name):
                row = self._row(name)
                self.assertIsNotNone(row)
                self.assertEqual(row["address"],
                                 intel_timings.MCHBAR + 0xE088)
                self.assertEqual(row["parameters"],
                                 {"bit_start": start, "bit_length": 1})
                self.assertEqual(row["read_type"], "wide")

    def test_the_additional_phy_fields_match_live_verified_offsets(self):
        expected = {
            "WEAKLOCKENDLY": (0x01AC, 8, 5),
            "SCR DLL En Timer Value": (0x2D1C, 13, 10),
            "SCR PIEN Timer Value": (0x2D20, 0, 11),
        }
        for name, (offset, start, length) in expected.items():
            with self.subTest(name=name):
                row = self._row(name)
                self.assertIsNotNone(row)
                self.assertEqual(row.get("Tab"), intel_timings.IMC_TAB)
                self.assertEqual(row.get("Category"), "MISC Additional")
                self.assertEqual(row["address"], intel_timings.MCHBAR + offset)
                self.assertEqual(row["parameters"],
                                 {"bit_start": start, "bit_length": length})

    def test_add_one_qclk_delay_completes_the_power_down_block(self):
        row = self._row("Add 1 QCLK Delay")
        self.assertIsNotNone(row)
        self.assertEqual(row.get("Tab"), intel_timings.IMC_TAB)
        self.assertEqual(row.get("Category"), "Power Down")
        self.assertEqual(row["address"], intel_timings.MCHBAR + 0xE278)
        self.assertEqual(row["parameters"],
                         {"bit_start": 12, "bit_length": 1})


class VainDdr4BlockCoverageTest(unittest.TestCase):
    """Every named row in Vain's DDR4 MR and power-down blocks exists."""

    MODE_REGISTER_ROWS = {
        "Burst Length", "CAS Latency", "Read Burst Type", "Test Mode",
        "DLL Reset", "DLL Enable", "RON", "Additive Latency",
        "Write Leveling", "RTT Nom", "TDQS Enable",
        "Data Output Disable", "tCWL_MR", "Low Power ASR", "RTT Wr",
        "Write CRC", "MPR Page Select", "MPR Operation", "CS Geardown",
        "Per DRAM Addr", "Temp Sensor Readout", "Refresh tRFC Mode",
        "Write CMD Latency", "MPR Read Format", "Max Power Down",
        "Temp Refresh Range", "Temp Ctrl Refresh", "Internal Vref Mon",
        "Soft PPR", "CS to CMD Latency", "Self Refresh Abort",
        "Read Preamble Train", "Read Preamble", "Write Preamble",
        "Hard PPR", "CA Parity Latency", "CRC Error Clear",
        "CA Parity Err Status", "ODT Buffer (PD)", "RTT Park",
        "CA Parity Persist Err", "DM Enable", "Write DBI", "Read DBI",
        "VrefDQ Train Value", "VrefDQ Train Range",
        "VrefDQ Train Enable", "tCCD_L_MR",
    }
    POWER_DOWN_ROWS = {
        "powerdown_enable", "powerdown_latency", "powerdown_length",
        "selfrefresh_enable", "selfrefresh_latency", "selfrefresh_length",
        "ckevalid_enable", "ckevalid_length", "idle_enable", "idle_length",
        "Add 1 QCLK Delay", "DLL_CODEPI", "DLL_CODEWL", "DLL BWSEL",
        "BWSEL LO Threshold", "QX Count", "RX VREF", "RcvEn PI",
    }

    def test_all_48_vain_mode_register_rows_are_present(self):
        names = {row.get("name") for row in intel_timings.TIMINGS}
        self.assertEqual(len(self.MODE_REGISTER_ROWS), 48)
        self.assertLessEqual(self.MODE_REGISTER_ROWS, names)

    def test_all_18_vain_power_down_rows_are_present(self):
        names = {row.get("name") for row in intel_timings.TIMINGS}
        self.assertEqual(len(self.POWER_DOWN_ROWS), 18)
        self.assertLessEqual(self.POWER_DOWN_ROWS, names)


if __name__ == "__main__":
    unittest.main()
