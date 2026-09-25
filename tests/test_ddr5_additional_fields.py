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

"""The fields Raptor Lake DDR5 gained beside the reference tools' own lists.

MR2-MR4, Add/Dec tCWL, the refresh-control fields, Write 0, Multi-Cycle and
Add 1 QCLK Delay read both modules and belong on Training; the three PHY
fields are one shared value and belong on IMC. The register fields sit where
the reference executable's own Raptor Lake table puts them.
"""

import unittest

from rochviewer.platform_profiles import LGA1700_DDR4, LGA1700_DDR5
from tests.intel_stub import install, restore

MODE_REGISTER_ROWS = (
    "Write Leveling", "N-Mode", "MPSM", "CS Assertion Duration",
    "Device 15 MPSM", "Internal Write Timing", "Write Leveling LB",
    "Write Leveling UB", "Minimum Refresh Rate",
    "Refresh Interval Rate Indicator", "TUF",
)
PHY_ROWS = ("Weak Lock End Delay", "SCR DLL Enable Timer", "SCR PI Enable Timer")
CWL_ROWS = ("Add tCWL", "Dec tCWL")
REGISTER_ROWS = (
    "Refresh Interval", "Refresh Stagger", "Refresh Stagger Mode",
    "Stolen Refresh", "Refresh Type Display", "tREFI Pulse Stagger",
    "Wake Up On HPM", "Write 0", "Multi-Cycle Command", "Add 1 QCLK Delay",
)


def rows_named(module, names):
    return {row["name"]: row for row in module.TIMINGS if row.get("name") in names}


class Ddr5AdditionalFieldsTest(unittest.TestCase):
    def setUp(self):
        self.module = install(LGA1700_DDR5)
        self.addCleanup(restore)

    def test_mode_registers_read_both_modules_on_training(self):
        # Refresh Interval Rate Indicator only says what the DRAM can do and
        # is left off LGA1700 DDR5's Training; see DDR5_TRAINING_REMOVED.
        shown = tuple(name for name in MODE_REGISTER_ROWS
                      if name not in self.module.DDR5_TRAINING_REMOVED)
        rows = rows_named(self.module, MODE_REGISTER_ROWS)
        self.assertEqual(set(rows), set(shown))
        for name, row in rows.items():
            with self.subTest(name=name):
                self.assertEqual(row["Tab"], "Training")
                self.assertEqual(row["Category"], "Mode Registers")
                self.assertTrue(self.module.is_dual_timing(row))

    def test_the_capability_rows_are_left_off(self):
        names = {row.get("name") for row in self.module.TIMINGS
                 if row.get("Tab") == "Training"}
        for name in ("Wide Range", "Package Output Driver Test Mode",
                     "Refresh Interval Rate Indicator",
                     "SRX/NOP Clock-Sync Support"):
            with self.subTest(name=name):
                self.assertNotIn(name, names)

    def test_long_values_are_worded_briefly(self):
        brief = self.module.brief_ddr5_value
        for long, short in (
                ("0 RZQ OFF", "Off"),
                ("1 tCK - 10 Pattern", "1 tCK"),
                ("1.5 tCK - 010 Pattern", "1.5 tCK"),
                # The read preamble's two 2 tCK settings stay apart.
                ("2 tCK - 0010 Pattern", "2 tCK (0010)"),
                ("2 tCK - 1110 Pattern", "2 tCK (1110)"),
                ("Manual ECS Mode Disabled", "Disabled"),
                ("ECS counts Rows with errors", "Rows"),
                ("Timer Stops at 2048th clocks", "2048 clocks"),
                ("Timer Stops at 1st clocks", "1 clocks"),
                ("40 RZQ/6", "40 RZQ/6")):
            with self.subTest(value=long):
                self.assertEqual(brief(long), short)
        self.assertEqual(brief(7), 7)
        # The write preamble has one 2 tCK setting, so it needs no pattern.
        self.assertEqual(brief("2 tCK - 0010 Pattern", "Write Preamble"),
                         "2 tCK")

    def test_the_rows_read_briefly_and_the_tables_are_untouched(self):
        rows = {row["name"]: row for row in self.module.TIMINGS
                if row.get("Tab") == "Training"}
        self.assertEqual(rows["RTT Nom Rd"]["Formula"][0], "Off")
        self.assertEqual(rows["ECS Mode"]["display_name"], "Manual ECS")
        # The shared tables DDR4 and the other platforms read keep their words.
        self.assertEqual(self.module.RTT_NOM_RD_FORMULA[0], "0 RZQ OFF")
        self.assertEqual(self.module.MISC_POSTAMBLE[1], "1.5 tCK - 010 Pattern")

    def test_the_imc_rows_read_by_their_proper_names(self):
        rows = {row["name"]: row for row in self.module.TIMINGS}
        self.assertEqual(rows["CMD SlewStatlegen"]["display_name"],
                         "CMD Slew Static Leg")
        self.assertEqual(rows["Realtime Memory"]["display_name"],
                         "Realtime Memory Timing")
        switch = self.module.imc_switch_text
        self.assertEqual(switch(1), "Enabled")
        self.assertEqual(switch("0"), "Disabled")
        self.assertEqual(switch("N/A"), "N/A")

    def test_the_mode_register_section_is_one_block(self):
        # The new rows sit ahead of Read DQS Offset Timing, which moves to DQS;
        # after it, Mode Registers and DQS would each be drawn twice.
        seen = []
        for row in self.module.TIMINGS:
            if row.get("Tab") != "Training":
                continue
            if not seen or seen[-1] != row["Category"]:
                seen.append(row["Category"])
        self.assertEqual(len(seen), len(set(seen)), seen)

    def test_cwl_reads_both_modules_on_training(self):
        for name, row in rows_named(self.module, CWL_ROWS).items():
            with self.subTest(name=name):
                self.assertEqual(row["Tab"], "Training")
                self.assertTrue(self.module.is_dual_timing(row))
        self.assertEqual(len(rows_named(self.module, CWL_ROWS)), 2)

    def test_phy_fields_are_one_shared_value_on_imc(self):
        rows = rows_named(self.module, PHY_ROWS)
        self.assertEqual(set(rows), set(PHY_ROWS))
        for name, row in rows.items():
            with self.subTest(name=name):
                self.assertEqual(row["Tab"], "IMC")
                self.assertFalse(self.module.is_dual_timing(row))

    def test_register_fields_read_both_modules_on_training(self):
        rows = rows_named(self.module, REGISTER_ROWS)
        self.assertEqual(set(rows), set(REGISTER_ROWS))
        for name, row in rows.items():
            with self.subTest(name=name):
                self.assertEqual(row["Tab"], "Training")
                self.assertTrue(self.module.is_dual_timing(row))
                self.assertEqual(row["address_b"] - row["address_a"],
                                 self.module.CHANNEL_B_OFFSET)

    def test_add_one_qclk_delay_shares_the_cwl_register(self):
        row = rows_named(self.module, ("Add 1 QCLK Delay",))["Add 1 QCLK Delay"]
        self.assertEqual(row["address_a"], self.module.MCHBAR + 0xE478)
        self.assertEqual(row["parameters_a"], {"bit_start": 12, "bit_length": 1})

    def test_controller_level_settings_read_b1s_controller(self):
        # Error Correction and Self Refresh have a copy in MC1's window; the
        # global Realtime Memory and Memory Scrambler do not.
        rows = rows_named(self.module, ("Error Correction", "Self Refresh",
                                        "Realtime Memory", "Memory Scrambler"))
        self.assertEqual(rows["Error Correction"]["Tab"], "Training")
        self.assertEqual(rows["Self Refresh"]["Tab"], "Training")
        self.assertEqual(rows["Realtime Memory"]["Tab"], "IMC")
        self.assertEqual(rows["Memory Scrambler"]["Tab"], "IMC")


class ModeRegisterDecodeTest(unittest.TestCase):
    """The bench's MR2 0x90, MR3 0x04 and MR4 0x12, as the reference tool read them."""

    EXPECTED = {
        "Write Leveling": (0x90, "Normal Mode"),
        "N-Mode": (0x90, "2N Mode"),
        "MPSM": (0x90, "Disabled"),
        "CS Assertion Duration": (0x90, "Single Cycle"),
        "Device 15 MPSM": (0x90, "Disabled"),
        "Internal Write Timing": (0x90, "Enabled"),
        "Write Leveling LB": (0x04, "-4 tCK"),
        "Write Leveling UB": (0x04, "0 tCK"),
        "Minimum Refresh Rate": (0x12, "tREFI x1"),
        "Refresh Interval Rate Indicator": (0x12, "Not implemented"),
        "TUF": (0x12, "No Change"),
    }

    def test_each_field_decodes_the_bench_register(self):
        module = install(LGA1700_DDR5)
        self.addCleanup(restore)
        fields = {name: rest for name, *rest
                  in module.MISC_MODE_REGISTER_STATE}
        for name, (raw, shown) in self.EXPECTED.items():
            number, start, length, decode = fields[name]
            value = (raw >> start) & ((1 << length) - 1)
            with self.subTest(name=name):
                self.assertEqual(decode.get(value, str(value)), shown)


class Ddr4KeepsItsOwnFieldsTest(unittest.TestCase):
    def test_ddr4_has_every_group_and_none_of_the_ddr5_mode_registers(self):
        module = install(LGA1700_DDR4)
        self.addCleanup(restore)
        # DDR4 has a Write Leveling of its own, in MR1 and its own section;
        # none of the DDR5 MR2-MR4 decodes may appear beside it.
        ddr5_decodes = {
            name for name, row in rows_named(module, MODE_REGISTER_ROWS).items()
            if row.get("Category") == "Mode Registers"
        }
        self.assertEqual(ddr5_decodes, set())
        for names in (PHY_ROWS, CWL_ROWS, REGISTER_ROWS):
            with self.subTest(names=names[0]):
                self.assertEqual(set(rows_named(module, names)), set(names))



# The IMC rows whose register each module's controller has a copy of.
PER_CHANNEL_IMC_ROWS = (
    "Idle Length", "Power Down Enable", "Self Refresh Enable",
    "CMD Stretch", "N:1 Ratio", "Power Down", "Row Hammer",
    "Page Close Idle Timeout", "Error Correction", "Self Refresh",
    "PBR Disable", "Refresh HP WM", "Raise Block Wait", "Allow 2cyc B2B LPDDR",
)
SHARED_IMC_ROWS = ("Realtime Memory", "Memory Scrambler", "DLL Code PI", "Dq Vref Up")


class Ddr5PerChannelMoveTest(unittest.TestCase):
    """DDR5: a register with B1's own copy reads both modules on Training."""

    def setUp(self):
        self.module = install(LGA1700_DDR5)
        self.addCleanup(restore)

    def test_per_channel_rows_read_both_modules_on_training(self):
        rows = rows_named(self.module, PER_CHANNEL_IMC_ROWS)
        self.assertEqual(set(rows), set(PER_CHANNEL_IMC_ROWS))
        for name, row in rows.items():
            with self.subTest(name=name):
                self.assertEqual(row["Tab"], "Training")
                self.assertEqual(row["source_scope"], "module")
                self.assertTrue(self.module.is_dual_timing(row))

    def test_shared_rows_stay_single_on_imc(self):
        for name, row in rows_named(self.module, SHARED_IMC_ROWS).items():
            with self.subTest(name=name):
                self.assertEqual(row["Tab"], "IMC")
                self.assertFalse(self.module.is_dual_timing(row))

    def test_a_computed_register_row_reads_b1_at_its_base(self):
        module = self.module
        saved = module.read_physical_memory_int
        self.addCleanup(lambda: setattr(module, "read_physical_memory_int", saved))
        asked = []
        module.read_physical_memory_int = (
            lambda address, size: asked.append(address) or 0)
        rows_named(module, ("Idle Length",))["Idle Length"]["value_b"]()
        self.assertEqual(
            asked, [module.CHANNEL_B + module.MISC_CKE_CONFIG_OFFSET])
        self.assertEqual(module.CHANNEL_B, module.MCHBAR2)

    def test_imc_puts_phy_control_and_features_under_vref(self):
        for row in self.module.TIMINGS:
            if row.get("Tab") == "IMC" and row.get("Category") in (
                    "VREF", "PHY Control", "Features"):
                with self.subTest(name=row["name"]):
                    self.assertEqual(row["Column"], "Left")

    def test_each_imc_section_is_drawn_once(self):
        seen = []
        for row in self.module.TIMINGS:
            if row.get("Tab") != "IMC":
                continue
            if not seen or seen[-1] != row["Category"]:
                seen.append(row["Category"])
        self.assertEqual(len(seen), len(set(seen)), seen)

    def test_multi_cycle_command_reads_as_a_switch(self):
        row = rows_named(self.module, ("Multi-Cycle Command",))["Multi-Cycle Command"]
        self.assertEqual(row.get("Formula"), {0: "Disabled", 1: "Enabled"})


class Ddr4IsUntouchedTest(unittest.TestCase):
    """DDR4 keeps the IMC and Training it was built with."""

    def setUp(self):
        self.module = install(LGA1700_DDR4)
        self.addCleanup(restore)

    def test_controller_rows_stay_single_on_imc(self):
        for name, row in rows_named(self.module, PER_CHANNEL_IMC_ROWS).items():
            with self.subTest(name=name):
                self.assertEqual(row["Tab"], "IMC")
                self.assertFalse(self.module.is_dual_timing(row))

    def test_phy_control_keeps_the_second_imc_column(self):
        rows = [row for row in self.module.TIMINGS
                if row.get("Tab") == "IMC" and row.get("Category") == "PHY Control"]
        self.assertTrue(rows)
        self.assertTrue(all(row["Column"] == "Right" for row in rows))

    def test_add_one_qclk_and_multi_cycle_are_as_they_were(self):
        rows = rows_named(self.module, ("Add 1 QCLK Delay", "Multi-Cycle Command"))
        self.assertEqual(rows["Add 1 QCLK Delay"]["address"], self.module.MCHBAR + 0xE278)
        self.assertNotIn("Formula", rows["Multi-Cycle Command"])

    def test_training_keeps_two_columns(self):
        columns = {row["Column"] for row in self.module.TIMINGS
                   if row.get("Tab") == "Training"}
        self.assertEqual(columns, {"Left", "Right"})


if __name__ == "__main__":
    unittest.main()
