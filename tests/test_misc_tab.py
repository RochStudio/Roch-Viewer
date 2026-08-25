"""Cover the Misc tab: its rows, its decoding and the tab's registration."""

import inspect
import unittest

import main

from display_values import select_tab_names
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
        """Everything on the tab, latency blocks included."""
        return [t for t in intel_timings.TIMINGS
                if t.get("Tab") == intel_timings.MISC_TAB]

    def _rows(self):
        """Only the Misc sections -- the latency rows are read live."""
        return [t for t in self._tab_rows()
                if t.get("Category") in MISC_CATEGORIES]

    def test_every_row_from_both_reference_blocks_is_present(self):
        rows = self._rows()
        if not rows:
            self.skipTest("Misc tab is not installed on this platform")
        names = [t.get("name") for t in rows]
        expected = (
            [name for name, _, _ in intel_timings.MISC_CKE_CONFIG_FIELDS]
            + [name for name, _, _ in intel_timings.MISC_GS_CONFIG_FIELDS]
            + ["Burst Length"]
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
            + [name for name, _, _, _, _ in intel_timings.MISC_FEATURE_FIELDS]
        )
        if intel_timings.detect_ddr_generation() == "DDR4":
            # The rows DDR4 has no counterpart for, and the ones it has that
            # cannot be reached, are both gated off. What remains is still in
            # the declared order.
            gated = (set(intel_timings.DDR5_ONLY_MISC_ROWS)
                     | set(intel_timings.DDR4_UNREACHABLE_MISC_ROWS))
            self.assertTrue(gated & set(expected))
            expected = [name for name in expected if name not in gated]
        self.assertEqual(names, expected)

    def test_every_section_is_in_the_one_column(self):
        rows = self._rows()
        if not rows:
            self.skipTest("Misc tab is not installed on this platform")
        # The tab draws as a single column. It held two, which suited it
        # while it was short; at 83 rows the split was doing less than it
        # looks, because the halves have to be levelled to the same height
        # for the shading to cross the tab whole and the shorter one was
        # padded with blank rows to get there.
        for row in self._tab_rows():
            with self.subTest(name=row.get("name")):
                self.assertEqual(row.get("Column"), "Left")

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

    def test_no_row_reads_two_channels(self):
        # These are controller settings that read the same on both channels,
        # and a second column left the value column too narrow for what this
        # tab holds -- the ECS and preamble strings were cut off.
        for row in self._rows():
            with self.subTest(name=row.get("name")):
                self.assertIsNone(row.get("value_a"))
                self.assertIsNone(row.get("value_b"))

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
        by_name = {row["name"]: _reading(row) for row in self._rows()}
        cke = [name for name, _, _ in intel_timings.MISC_CKE_CONFIG_FIELDS]
        self.assertGreater(len({by_name[name] for name in cke}), 1)
        # The bench's own register: bits 1-4 hold 3 and bits 24-27 hold 8.
        self.assertEqual(by_name["idle_length"], "3")
        self.assertEqual(by_name["ckevalid_length"], "8")

    def test_the_tab_is_offered_once_it_has_rows(self):
        tabs = select_tab_names(intel_timings.TIMINGS)
        if self._rows():
            self.assertIn(intel_timings.MISC_TAB, tabs)
        else:
            self.assertNotIn(intel_timings.MISC_TAB, tabs)

    def test_the_latency_rows_moved_onto_this_tab(self):
        if not self._rows():
            self.skipTest("Misc tab is not installed on this platform")
        categories = {t.get("Category") for t in self._tab_rows()}
        self.assertIn("Latency CHA", categories)
        self.assertIn("Latency CHB", categories)

    def test_the_latency_tab_is_gone_once_they_have_moved(self):
        tabs = select_tab_names(intel_timings.TIMINGS)
        if self._rows():
            self.assertNotIn(intel_timings.RTL_TAB, tabs)
        else:
            # Nothing to merge into, so they keep their own tab rather than
            # landing on one named Misc that holds only latencies.
            self.assertNotIn(intel_timings.MISC_TAB, tabs)

    def test_the_latency_blocks_stack_rather_than_sitting_side_by_side(self):
        # They are pinned by channel in the renderer, not by their rows, so
        # collapsing the tab to one column had to reach that pin as well --
        # Misc asked for a single column and still drew Latency CHB beside
        # it. The rows say Left; main.py's single_column check is what makes
        # the renderer agree.
        columns = {t.get("Category"): t.get("Column")
                   for t in self._tab_rows()}
        if "Latency CHA" not in columns:
            self.skipTest("no latency rows on this platform")
        self.assertEqual(columns["Latency CHA"], "Left")
        self.assertEqual(columns["Latency CHB"], "Left")
        source = inspect.getsource(main.TimingGUI.load_all_tabs_content)
        self.assertIn("single_column = not any(", source)
        self.assertIn("if single_column:", source)

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
    """The refresh arbitration controls, moved off Timings onto Misc.

    They are policy the controller applies around refresh, not intervals a
    memory profile sets, and Timings had grown past what its two columns hold.
    """

    def _row(self, name):
        for row in intel_timings.TIMINGS:
            if row.get("name") == name:
                return row
        return None

    def test_every_moved_row_landed_in_the_misc_refresh_section(self):
        for name in intel_timings.REFRESH_POLICY_ROWS:
            with self.subTest(name=name):
                row = self._row(name)
                self.assertIsNotNone(row, "%s is missing" % name)
                self.assertEqual(row.get("Tab"), intel_timings.MISC_TAB)
                self.assertEqual(row.get("Category"), "Refresh")

    def test_the_moved_rows_show_one_value_like_the_rest_of_the_tab(self):
        # Misc has no channel columns: these registers do have a channel-B
        # twin holding the same value, and a second column leaves the value
        # column too narrow for the text this tab carries.
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


if __name__ == "__main__":
    unittest.main()
