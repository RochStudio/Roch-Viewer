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

import types
import unittest
from unittest import mock

from rochviewer.ui.main import (
    AM5_SUMMARY_TIMING_PRIORITY,
    SHADED_TABS,
    SUMMARY_COLUMN_TAIL,
    TIMINGS_SECTION_ORDER,
    ordered_sections,
    TimingGUI,
    DDR5_MODE_REGISTER_VREF,
    SUMMARY_PAIRS_PER_ROW,
    am5_summary_timing_columns,
    channel_slot_labels,
    summary_system_memory_blocks,
    summary_vref_row_names,
    is_dual_timing,
    summary_compact_ohm,
    summary_rtt_display,
    summary_signal_timings,
    summary_slash_pair,
    summary_column_count,
    summary_column_width,
    summary_system_memory_layout,
    summary_system_memory_names,
    summary_voltage_names,
)


class DualTimingDefinitionTest(unittest.TestCase):
    def test_value_backed_channel_rows_are_dual(self):
        self.assertTrue(is_dual_timing({"value_a": "A", "value_b": "B"}))

    def test_single_value_row_is_not_dual(self):
        self.assertFalse(is_dual_timing({"value": lambda: "shared"}))

    def test_summary_selects_rtt_odt_and_drive_strength_after_cs_odt_b(self):
        rows = [
            {"name": "RTT WR", "Category": "RTT", "value_a": lambda: "A", "value_b": lambda: "B"},
            {"name": "CA ODT A", "Category": "ODT", "value_a": lambda: "A", "value_b": lambda: "B"},
            {"name": "CS ODT B", "Category": "ODT", "value_a": lambda: "A", "value_b": lambda: "B"},
            {"name": "Proc ODT Pu", "Category": "Drive Strength", "value_a": lambda: "A", "value_b": lambda: "B"},
            {"name": "Proc CA DS", "Category": "Drive Strength", "value_a": lambda: "A", "value_b": lambda: "B"},
            {"name": "VREF only", "Category": "VREF"},
        ]

        selected = summary_signal_timings(rows)
        names = [row["name"] for row in selected]

        self.assertEqual(
            names,
            ["RTT WR", "CA ODT A", "CS ODT B", "Proc ODT Pu", "Proc CA DS"],
        )
        self.assertTrue(names.index("Proc ODT Pu") > names.index("CS ODT B"))
        self.assertTrue(all(is_dual_timing(row) for row in selected))

    def _am5_names(self):
        return {
            "CPU", "Cores / Threads", "Model", "BIOS",
            "AGESA", "BCLK", "MCLK", "UCLK", "FCLK", "DRAM Frequency",
            "UCLK:MCLK", "DRAM Ratio", "CR", "Refresh Mode",
            "Memory Capacity",
            "Gear Down Mode", "Power Down Mode", "Nitro Rx/Tx/Ctrl",
        }

    def test_am5_summary_reads_down_three_aligned_columns(self):
        layout = summary_system_memory_layout(self._am5_names())
        self.assertEqual(layout, [
            ["CPU", "Cores / Threads"],
            ["Model", "BIOS"],
            ["DRAM Frequency", "AGESA", "MCLK"],
            # DRAM Ratio leads UCLK:MCLK: the ratio the kit runs at, then how
            # the controller is geared to it.
            ["DRAM Ratio", "BCLK", "FCLK"],
            ["UCLK:MCLK", "Memory Capacity", "UCLK"],
            ["Power Down Mode", "Refresh Mode", "Nitro Rx/Tx/Ctrl"],
            # One row longer than the other two columns, so this one is alone.
            ["Gear Down Mode"],
        ])

    def test_the_memory_block_is_aligned_and_the_identity_rows_are_not(self):
        # Only an aligned row lands on the Summary columns; identity packs
        # tight so a long board name is not chopped into a narrow cell.
        blocks = summary_system_memory_blocks(self._am5_names())
        aligned = {tuple(names): flag for names, flag in blocks}
        self.assertFalse(aligned[("CPU", "Cores / Threads")])
        self.assertFalse(aligned[("Model", "BIOS")])
        self.assertTrue(aligned[("DRAM Frequency", "AGESA", "MCLK")])
        self.assertTrue(aligned[("Power Down Mode", "Refresh Mode",
                                 "Nitro Rx/Tx/Ctrl")])
        # Alone in its row, but still aligned: it has to land on the first
        # column rather than pack to the left edge.
        self.assertTrue(aligned[("Gear Down Mode", None, None)])

    def test_an_aligned_row_keeps_a_hole_for_a_missing_name(self):
        # Dropping it would slide BCLK and FCLK one column to the left, under
        # the wrong timing section.
        blocks = summary_system_memory_blocks(self._am5_names() - {"DRAM Ratio"})
        row = next(names for names, _ in blocks if "BCLK" in names)
        self.assertEqual(row, [None, "BCLK", "FCLK"])
        self.assertEqual(row.index("BCLK"), 1)

    def test_a_row_that_is_alone_still_holds_the_first_column(self):
        # Gear Down Mode has no partners, and packing it tight would let it
        # drift off the column the rows above it sit on.
        blocks = summary_system_memory_blocks(self._am5_names())
        row, aligned = next((names, flag) for names, flag in blocks
                            if "Gear Down Mode" in names)
        self.assertEqual(row, ["Gear Down Mode", None, None])
        self.assertTrue(aligned)

    def test_channel_columns_take_the_slot_name(self):
        modules = [{"slot": "A2"}, {"slot": "B2"}]
        self.assertEqual(channel_slot_labels(modules), {"a": "A2", "b": "B2"})

    def test_two_modules_on_one_channel_keep_the_generic_label(self):
        # The column covers both sticks, so neither slot name would be right.
        modules = [{"slot": "A1"}, {"slot": "A2"}, {"slot": "B2"}]
        self.assertEqual(channel_slot_labels(modules), {"b": "B2"})

    def test_an_unreadable_locator_leaves_the_column_unnamed(self):
        self.assertEqual(channel_slot_labels([{"slot": None}]), {})
        self.assertEqual(channel_slot_labels([]), {})

    def test_intel_summary_layout_does_not_gain_empty_am5_row(self):
        layout = summary_system_memory_layout({"CPU", "MCLK", "UCLK", "Gear Mode"})
        self.assertNotIn(["Gear Down Mode", "Nitro Rx/Tx/Ctrl"], layout)

    def test_intel_summary_keeps_every_clock(self):
        # The three no longer share one row: MCLK sits beside DRAM Frequency,
        # Uncore beside BCLK, UCLK on the last row. What matters is that none
        # of them is dropped.
        layout = summary_system_memory_layout({"Uncore", "MCLK", "UCLK"})
        placed = [name for row in layout for name in row]
        for name in ("Uncore", "MCLK", "UCLK"):
            with self.subTest(name=name):
                self.assertIn(name, placed)

    def test_intel_summary_matches_the_requested_arrangement(self):
        layout = summary_system_memory_layout({
            "CPU", "Cores / Threads", "Model", "BIOS",
            "Microcode", "BCLK", "Uncore", "MCLK", "UCLK", "DRAM Frequency",
            "Gear Mode", "Power Down", "Memory Capacity",
        })
        self.assertEqual(layout, [
            ["CPU", "Cores / Threads"],
            # System Info splits the board the way CPU-Z does, so Summary
            # names both halves -- the same shape the AM5 block already used.
            # Microcode joins them: it is a firmware revision like the BIOS
            # beside it, rather than a CPU fact stranded mid memory block.
            ["Model", "BIOS", "Microcode"],
            ["DRAM Frequency", "BCLK", "MCLK"],
            ["Gear Mode", "Memory Capacity", "Uncore"],
            # The hole Microcode left comes to the foot of the column rather
            # than sitting in the middle of it, so the rows below it moved up
            # one. summary_system_memory_layout drops the None.
            ["Power Down", "UCLK"],
        ])

    def test_intel_summary_clock_rows_sit_on_the_summary_columns(self):
        # DRAM Frequency over tCL, BCLK over tREFI, MCLK over RTT WR. Packed
        # tight these rows each chose their own column positions and stepped
        # in and out against the timing grid below them.
        blocks = summary_system_memory_blocks({
            "CPU", "Cores / Threads", "Model", "BIOS", "Microcode",
            "BCLK", "Uncore", "MCLK", "UCLK", "DRAM Frequency", "Gear Mode",
            "Power Down", "Memory Capacity",
        })
        aligned = {tuple(names) for names, is_aligned in blocks if is_aligned}
        self.assertEqual(aligned, {
            ("DRAM Frequency", "BCLK", "MCLK"),
            ("Gear Mode", "Memory Capacity", "Uncore"),
            # The None is kept rather than dropped: an aligned row holds the
            # column open so UCLK stays over its own column instead of
            # sliding into the middle one.
            ("Power Down", None, "UCLK"),
        })

    def test_no_summary_row_exceeds_the_configured_column_pairs(self):
        # The panel only configures SUMMARY_PAIRS_PER_ROW column pairs; a
        # longer row spills its last entry against the right edge.
        every_name = set(summary_system_memory_names())
        for layout in (summary_system_memory_layout(every_name),
                       summary_system_memory_layout(every_name - {"AGESA"})):
            for row in layout:
                with self.subTest(row=row):
                    self.assertLessEqual(len(row), SUMMARY_PAIRS_PER_ROW)

    def test_summary_allowlist_includes_fclk(self):
        self.assertIn("FCLK", summary_system_memory_names())

    def test_summary_vref_matches_the_skew_tab_minus_the_mode_registers(self):
        skew = [
            "WrDS Up", "WrDS Dn", "RdODT Up", "RdODT Dn",
            "WrDSCmd Up", "WrDSCmd Dn", "WrDSCtl Up", "WrDSCtl Dn",
            "WrDSClk Up", "WrDSClk Dn", "WrDSCke CS Up",
            "DQ VREF", "CA VREF", "CS VREF",
        ]
        rows = [{"Category": "VREF", "name": name} for name in skew]
        rows.append({"Category": "RTT", "name": "RTT WR"})

        self.assertEqual(
            summary_vref_row_names(rows),
            [name for name in skew if name not in DDR5_MODE_REGISTER_VREF],
        )

    def test_summary_vref_keeps_table_order(self):
        rows = [
            {"Category": "VREF", "name": "WrDSClk Up"},
            {"Category": "VREF", "name": "WrDS Up"},
        ]
        self.assertEqual(
            summary_vref_row_names(rows), ["WrDSClk Up", "WrDS Up"]
        )

    def test_summary_vref_keeps_the_mode_registers_when_they_are_all_there_is(self):
        # Arrow Lake drops the up/down block, so these three are the only VREF
        # readings on that platform and must not be filtered away.
        rows = [
            {"Category": "VREF", "name": name}
            for name in DDR5_MODE_REGISTER_VREF
        ]
        self.assertEqual(
            summary_vref_row_names(rows), list(DDR5_MODE_REGISTER_VREF)
        )

    def test_summary_vref_ignores_other_categories(self):
        rows = [
            {"Category": "RTT", "name": "RTT WR"},
            {"Category": "Primary", "name": "tCL"},
        ]
        self.assertEqual(summary_vref_row_names(rows), [])

    def test_am5_summary_uses_requested_timing_priority_then_leftovers(self):
        # tRCDRD leads tRCDWR, and the power-down pair tails the column
        # after tMOD rather than interrupting precharge and mode register.
        priority = [
            "tCL", "tRCDRD", "tRCDWR", "tRP", "tRAS", "tRC", "tWR",
            "tRFCns", "tRFC", "tRFC2", "tRFCsb", "tRRD_L", "tRRD_S", "tWTR_L",
            "tWTR_S", "tRTP", "tFAW", "tCWL",
            "tRDPRE", "tWRPRE", "tMOD", "tCKE", "tXP",
        ]
        rows = [
            {"name": "tRCDRD", "Category": "Primary"},
            {"name": "tREFI", "Category": "Refresh timings"},
            {"name": "Refresh Mode", "Category": "Refresh timings"},
        ] + [
            {"name": name, "Category": "Primary"} for name in reversed(priority)
        ] + [
            {"name": "RTT WR", "Category": "RTT"},
            {"name": "tXP", "Category": "Tertiary"},
        ]

        first, leftover = am5_summary_timing_columns(rows)

        self.assertEqual(first, priority)
        self.assertEqual(leftover, ["tREFI"])
        self.assertNotIn("Refresh Mode", first)
        self.assertNotIn("Refresh Mode", leftover)

    def test_am5_summary_places_trefi_above_the_pinned_groups(self):
        rows = [
            {"name": "tCL", "Category": "Primary"},
            {"name": "tRDRDSCL", "Category": "Tertiary"},
            {"name": "tRDRDSC", "Category": "Tertiary"},
            {"name": "tREFI", "Category": "Refresh timings"},
            {"name": "tRFCns", "Category": "Refresh timings"},
            {"name": "tRFC", "Category": "Refresh timings"},
            {"name": "Refresh Mode", "Category": "Refresh timings"},
        ]

        first, leftover = am5_summary_timing_columns(rows)

        self.assertEqual(first, ["tCL", "tRFCns", "tRFC"])
        self.assertLess(first.index("tRFCns"), first.index("tRFC"))
        # The refresh interval leads, then the pinned read group.
        self.assertEqual(leftover, ["tREFI", "tRDRDSCL", "tRDRDSC"])
        self.assertNotIn("Refresh Mode", leftover)

    def test_am5_summary_places_the_turnarounds_after_the_groups(self):
        rows = [
            {"name": "tCL", "Category": "Primary"},
            {"name": "tRDRDSCL", "Category": "Tertiary"},
            {"name": "tWRWRSCL", "Category": "Tertiary"},
            {"name": "tRDWR", "Category": "Tertiary"},
            {"name": "tWRRD", "Category": "Tertiary"},
            {"name": "tMRD", "Category": "Tertiary"},
            {"name": "tREFI", "Category": "Refresh timings"},
        ]

        first, leftover = am5_summary_timing_columns(rows)

        self.assertEqual(
            leftover,
            ["tREFI", "tRDRDSCL", "tWRWRSCL", "tWRRD", "tRDWR", "tMRD"],
        )
        self.assertLess(leftover.index("tRDRDSCL"), leftover.index("tWRRD"))
        self.assertLess(leftover.index("tWRRD"), leftover.index("tRDWR"))

    def test_am5_summary_leftover_priority_tolerates_missing_names(self):
        rows = [
            {"name": "tRDRDSCL", "Category": "Tertiary"},
            {"name": "tRDWR", "Category": "Tertiary"},
        ]

        _, leftover = am5_summary_timing_columns(rows)

        self.assertEqual(leftover, ["tRDRDSCL", "tRDWR"])  # tREFI absent here


    def test_the_board_row_is_the_model_alone(self):
        # It was Manufacturer and Model. The vendor is not carried on the
        # strip any more -- the model names itself -- so the row is the
        # model and nothing else, and no placement asks for the vendor.
        layout = summary_system_memory_layout({"Manufacturer", "Model", "CPU"})
        row = next(row for row in layout if "Model" in row)
        self.assertEqual(row, ["Model"])
        self.assertNotIn("Manufacturer", summary_system_memory_names())

    def test_the_board_row_survives_a_missing_manufacturer(self):
        # A platform that reports only half the board identity still gets a
        # row, rather than the pair collapsing to nothing.
        layout = summary_system_memory_layout({"Model"})
        self.assertIn(["Model"], layout)
        # AGESA marks the AM5 branch, which places the same two names, so
        # this is the same case on the other platform.
        layout = summary_system_memory_layout({"AGESA", "Model"})
        self.assertIn(["Model"], layout)

    def test_summary_is_three_columns_whether_or_not_rails_exist(self):
        # The rails had a fourth column. They are readings, so they moved to
        # the Sensor Telemetry window; the timing columns keep their width.
        timings = [{"name": "tCL", "Tab": "Timings"}]
        self.assertEqual(summary_column_count(timings), 3)

        timings = timings + [
            {"name": "VDDIO", "Tab": "Sensors", "rail_key": "vddio_mem"},
            {"name": "VSOC", "Tab": "Sensors", "rail_key": "vddcr_soc"},
        ]
        self.assertEqual(summary_voltage_names(timings), ["VDDIO", "VSOC"])
        self.assertEqual(summary_column_count(timings), 3)

    def test_vtt_is_hidden_from_summary_but_stays_on_the_voltages_tab(self):
        # Filtering is on rail_key, so renaming a rail cannot change which rows
        # Summary shows.
        timings = [
            {"name": "VDDIO", "Tab": "Sensors", "rail_key": "vddio_mem"},
            {"name": "VTT", "Tab": "Sensors", "rail_key": "vtt"},
            {"name": "DRAM VDD", "Tab": "Sensors", "rail_key": "dram_vdd"},
        ]
        # Summary stays short; the tab itself still carries every rail.
        self.assertEqual(
            summary_voltage_names(timings), ["VDDIO", "DRAM VDD"]
        )

    def test_summary_filter_survives_a_label_rename(self):
        timings = [
            {"name": "renamed", "Tab": "Sensors", "rail_key": "vtt"},
            {"name": "also renamed", "Tab": "Sensors", "rail_key": "vddio_mem"},
        ]
        self.assertEqual(summary_voltage_names(timings), ["also renamed"])

    def test_am5_summary_tails_the_column_with_power_down(self):
        rows = [
            {"name": name, "Category": "Tertiary"}
            for name in ("tCKE", "tXP", "tMOD", "tRDPRE", "tWRPRE", "tRDRDSCL")
        ] + [
            {"name": "tCL", "Category": "Primary"},
            {"name": "tPHYRDL", "Category": "Tertiary"},
            {"name": "tPHYWRL", "Category": "Tertiary"},
            {"name": "tPHYWRD", "Category": "Tertiary"},
        ]
        first, leftover = am5_summary_timing_columns(rows)
        # Precharge and mode register read together, then the power-down
        # pair closes the column.
        self.assertEqual(
            first[-5:],
            ["tRDPRE", "tWRPRE", "tMOD", "tCKE", "tXP"],
        )
        self.assertNotIn("tPHYRDL", first)
        self.assertNotIn("tPHYRDL", leftover)
        self.assertNotIn("tPHYWRL", leftover)
        self.assertNotIn("tPHYWRD", leftover)

    def test_summary_rtt_display_shows_the_ohms_as_a_bare_number(self):
        # Every row in the block is a resistance, so the column reads as
        # numbers rather than as RZQ ratios with the figure in brackets.
        self.assertEqual(summary_rtt_display("RZQ/6 (40 Ω)"), "40")
        self.assertEqual(summary_rtt_display("RZQ/5 (48 Ω)"), "48")
        self.assertEqual(summary_rtt_display("RZQ/4 (60)"), "60")
        self.assertEqual(summary_rtt_display("RZQ (240)"), "240")
        self.assertEqual(summary_rtt_display("RZQ/0.5 (480)"), "480")
        self.assertEqual(summary_rtt_display("40 Ω"), "40")

    def test_a_termination_that_is_off_reads_as_zero_ohms(self):
        # An unterminated line belongs in the same column as the numbers.
        # "Disabled" is in here because it is the word the DDR4 RTT tables
        # use for code 0; without it the Summary printed "Disabled/Disabled",
        # which does not fit the column and says in nine characters what 0
        # says in one.
        for text in ("RTT_OFF", "Off", "OFF", "Hi-Z", "Disabled", "DISABLED"):
            with self.subTest(text=text):
                self.assertEqual(summary_rtt_display(text, "RTT"), "0")
                self.assertEqual(summary_rtt_display(text, "ODT"), "0")

    def test_a_driver_that_is_off_reads_as_hi_z(self):
        # The opposite state to a termination being off, and it was printing
        # the opposite thing: zero ohms on a driver says shorted to the rail,
        # when what the setting means is that the pin has stopped driving.
        for category in ("Drive Strength", "RON"):
            for text in ("Hi-Z", "HiZ", "Off", "OFF"):
                with self.subTest(category=category, text=text):
                    self.assertEqual(summary_rtt_display(text, category),
                                     "Hi-Z")

    def test_a_driver_with_a_real_impedance_still_reads_as_a_number(self):
        # Only the off state changes; the column stays numeric otherwise.
        self.assertEqual(summary_rtt_display("34.3 Ω", "Drive Strength"),
                         "34.3")
        self.assertEqual(summary_rtt_display("120 Ω", "Drive Strength"), "120")

    def test_an_unknown_category_keeps_the_old_reading(self):
        # The default matters: a row whose category is not passed must not
        # silently change meaning.
        self.assertEqual(summary_rtt_display("Off"), "0")
        self.assertEqual(summary_rtt_display("Off", None), "0")

    def test_a_reserved_code_is_not_turned_into_a_number(self):
        # RFU is not a resistance, so it stays as it is rather than becoming
        # a 0 that would read as a real termination setting.
        self.assertEqual(summary_rtt_display("RFU"), "RFU")
        self.assertEqual(summary_rtt_display("—"), "—")
        self.assertEqual(summary_rtt_display(None), "")

    def test_summary_slash_pair_uses_cha_chb_style(self):
        self.assertEqual(summary_compact_ohm("40 Ω"), "40Ω")
        self.assertEqual(summary_slash_pair("40 Ω", "40 Ω"), "40Ω/40Ω")
        self.assertEqual(summary_slash_pair("35", "37"), "35/37")
        self.assertEqual(summary_slash_pair("Off", "60 Ω"), "Off/60Ω")
        # The two together are what the signal panel draws: both sides go
        # through the display helper first, so the pair is bare numbers.
        self.assertEqual(
            summary_slash_pair(summary_rtt_display("RZQ/6 (40 Ω)"),
                               summary_rtt_display("RZQ/6 (40)")),
            "40/40",
        )
        self.assertEqual(
            summary_slash_pair(summary_rtt_display("RTT_OFF"),
                               summary_rtt_display("RTT_OFF")),
            "0/0",
        )

    def test_summary_allowlist_includes_agesa_and_bclk(self):
        names = summary_system_memory_names()
        self.assertIn("AGESA", names)
        self.assertIn("BCLK", names)
        self.assertIn("Refresh Mode", names)


class SummaryColumnWidthTest(unittest.TestCase):
    """CustomTkinter fills an even number of pixels and drops the remainder.

    A Summary column of odd width therefore leaves its own last pixel
    unpainted, and because the columns tile with no gutter that pixel reads
    as a hairline running the height of the tab.
    """

    def test_an_odd_column_is_widened_so_its_shading_reaches_the_edge(self):
        self.assertEqual(summary_column_width(177, is_last=False), 178)

    def test_an_even_column_is_left_alone(self):
        self.assertEqual(summary_column_width(202, is_last=False), 202)

    def test_the_last_column_keeps_its_width(self):
        # It ends at the panel edge, with no neighbour to show a seam against.
        self.assertEqual(summary_column_width(177, is_last=True), 177)


class ShadedTabTest(unittest.TestCase):
    """Zebra banding is opt-in per tab, and alternates from an unshaded row."""

    class _Palette:
        HIGHLIGHT_COLOR = ("light", "dark")

    def shade(self, tab_name, data_row):
        return TimingGUI._shade_row(self._Palette(), tab_name, data_row)

    def test_system_info_alternates_starting_unshaded(self):
        self.assertEqual(self.shade("System Info", 0), "transparent")
        self.assertEqual(self.shade("System Info", 1), ("light", "dark"))
        self.assertEqual(self.shade("System Info", 2), "transparent")

    def test_a_tab_that_did_not_ask_for_banding_keeps_the_plain_background(self):
        self.assertNotIn("RTL", SHADED_TABS)
        self.assertEqual(self.shade("RTL", 1), "transparent")

    def test_a_section_built_without_a_tab_name_is_not_shaded(self):
        self.assertEqual(self.shade(None, 1), "transparent")


class HalfIsUsedTest(unittest.TestCase):
    """A tab half that is gridded but empty is not a second column.

    Misc grids a right half like every other tab and puts nothing in it. The
    layout asked only whether the half was managed, so it took the
    two-column branch: the left was sized to its own content and the rest of
    the tab went to an empty frame, and the row shading stopped a third of
    the way across.
    """

    def _half(self, managed="grid", children=("section",)):
        return types.SimpleNamespace(
            winfo_manager=lambda: managed,
            winfo_children=lambda: list(children),
        )

    def test_a_half_with_sections_is_used(self):
        from rochviewer.ui.main import half_is_used

        self.assertTrue(half_is_used(self._half()))

    def test_a_managed_but_empty_half_is_not(self):
        from rochviewer.ui.main import half_is_used

        self.assertFalse(half_is_used(self._half(children=())))

    def test_an_unmanaged_half_is_not(self):
        from rochviewer.ui.main import half_is_used

        self.assertFalse(half_is_used(self._half(managed="")))

    def test_a_missing_half_is_not(self):
        from rochviewer.ui.main import half_is_used

        self.assertFalse(half_is_used(None))

    def test_something_that_is_not_a_widget_is_not(self):
        # The placeholder a platform-only tab leaves behind answers neither
        # call, and a tab that cannot be measured should keep its width
        # rather than take the layout down.
        from rochviewer.ui.main import half_is_used

        self.assertFalse(half_is_used(object()))

    def test_a_half_that_raises_is_not(self):
        from rochviewer.ui.main import half_is_used

        def boom():
            raise RuntimeError("destroyed")

        self.assertFalse(half_is_used(types.SimpleNamespace(
            winfo_manager=boom, winfo_children=lambda: ["x"])))


class GridPadxTest(unittest.TestCase):
    """Tk reports padx two ways, and only one of them was being handled.

    The Summary strip grids its cells with a (left, right) pair. Measuring
    the cell without that pad set a column minsize the grid then grew past to
    fit it, so the strip sat several pixels right of the timing columns it is
    supposed to start on, and no widening could close the gap.
    """

    def test_a_pair_is_summed(self):
        from rochviewer.ui.main import _grid_padx

        self.assertEqual(_grid_padx({"padx": (0, 8)}), 8)
        self.assertEqual(_grid_padx({"padx": (4, 4)}), 8)

    def test_a_single_value_counts_for_both_sides(self):
        from rochviewer.ui.main import _grid_padx

        self.assertEqual(_grid_padx({"padx": 4}), 8)

    def test_a_missing_or_unreadable_pad_is_zero(self):
        # grid_info comes back from Tk as strings on some builds, and a row
        # that cannot be measured must not take the whole strip down.
        from rochviewer.ui.main import _grid_padx

        self.assertEqual(_grid_padx({}), 0)
        self.assertEqual(_grid_padx({"padx": "6"}), 12)
        self.assertEqual(_grid_padx({"padx": None}), 0)
        self.assertEqual(_grid_padx({"padx": "not a number"}), 0)


class TimingsSectionOrderTest(unittest.TestCase):
    """The Timings tab's sections read in a chosen order, not profile order."""

    # Four columns, not two. The platforms share one order but disagree on
    # which column a section belongs to -- Power down is on the left for AM5
    # and the right for Intel -- so a single LEFT/RIGHT pair cannot describe
    # it, and pretending otherwise is what this class used to do.
    AM5_LEFT = (
        "Primary", "Secondary", "CAS to CAS", "Power down", "Stagger",
        "Preamble / postamble", "Mode register",
    )
    AM5_RIGHT = (
        "Refresh timings", "Turnaround", "Read to read", "Write to write",
        "PHY",
    )
    INTEL_LEFT = ("Primary", "Secondary", "Other Timings", "Command")
    INTEL_RIGHT = ("Refresh timings", "Tertiary", "Power down")

    SKEW_ONLY = ("RTT", "ODT", "Drive Strength")

    def order(self, names):
        sections = [(name, []) for name in names]
        return [name for name, _ in
                ordered_sections(sections, TIMINGS_SECTION_ORDER)]

    def test_the_am5_left_column_reads_in_the_requested_order(self):
        self.assertEqual(self.order(reversed(self.AM5_LEFT)),
                         list(self.AM5_LEFT))

    def test_the_am5_right_column_reads_in_the_requested_order(self):
        self.assertEqual(self.order(reversed(self.AM5_RIGHT)),
                         list(self.AM5_RIGHT))

    def test_the_intel_left_column_keeps_its_order(self):
        self.assertEqual(self.order(reversed(self.INTEL_LEFT)),
                         list(self.INTEL_LEFT))

    def test_the_intel_right_column_keeps_its_order(self):
        # The AM5 reshuffle must not drag Intel's Power down to the top of
        # its column, which is exactly what moving the name nearly did.
        self.assertEqual(self.order(reversed(self.INTEL_RIGHT)),
                         list(self.INTEL_RIGHT))

    def test_every_named_section_belongs_to_a_column_somewhere(self):
        placed = (set(self.AM5_LEFT) | set(self.AM5_RIGHT)
                  | set(self.INTEL_LEFT) | set(self.INTEL_RIGHT)
                  | set(self.SKEW_ONLY))
        self.assertEqual(set(TIMINGS_SECTION_ORDER), placed)

    def test_a_section_added_later_still_appears(self):
        # Unlisted categories keep their own order and follow the listed ones
        # rather than being dropped from the tab.
        self.assertEqual(
            self.order(["Stagger", "New Group", "Primary", "Another"]),
            ["Primary", "Stagger", "New Group", "Another"],
        )

    def test_refresh_leads_the_intel_right_column(self):
        self.assertEqual(
            self.order(["Power down", "Tertiary", "Refresh timings"]),
            ["Refresh timings", "Tertiary", "Power down"],
        )

    def test_the_intel_right_column_leads_with_tertiary(self):
        # Unlisted sections sort to the foot of their column, which put Power
        # down above Tertiary until both were named.
        self.assertEqual(
            self.order(["Power down", "Tertiary"]),
            ["Tertiary", "Power down"],
        )

    def test_the_timings_tab_is_shaded(self):
        self.assertIn("Timings", SHADED_TABS)

    def test_the_skew_tab_is_shaded(self):
        # Skew is a dense multi-section table like Timings, and was the one
        # tab of that shape still reading as an unbroken block.
        self.assertIn("Skew", SHADED_TABS)

    def test_the_dense_dual_channel_tabs_are_continuous(self):
        from rochviewer.ui.main import CONTINUOUS_SECTION_TABS

        self.assertEqual(CONTINUOUS_SECTION_TABS,
                         frozenset({"Timings", "Skew", "Misc"}))

    def test_the_signal_tail_follows_the_last_vref_level(self):
        from rochviewer.ui.main import (SUMMARY_SIGNAL_TAIL_ANCHOR, SUMMARY_SIGNAL_TAIL_ROWS,
                          insert_summary_rows_after)

        placed = insert_summary_rows_after(
            ["WrDSClk Dn", SUMMARY_SIGNAL_TAIL_ANCHOR],
            SUMMARY_SIGNAL_TAIL_ANCHOR, SUMMARY_SIGNAL_TAIL_ROWS)
        self.assertEqual(
            placed,
            ["WrDSClk Dn", SUMMARY_SIGNAL_TAIL_ANCHOR]
            + list(SUMMARY_SIGNAL_TAIL_ROWS))

    def test_the_signal_tail_is_not_a_vref_row(self):
        # It rides in the VREF name list but does not come from that category.
        # summary_vref_row_names reads the Skew tab's VREF rows so the two
        # displays cannot drift; a tail row appearing in there would mean it
        # had been added to the wrong place.
        from rochviewer.ui import main
        from rochviewer.ui.main import SUMMARY_SIGNAL_TAIL_ROWS, summary_vref_row_names

        vref = summary_vref_row_names(main.TIMINGS)
        for name in SUMMARY_SIGNAL_TAIL_ROWS:
            with self.subTest(name=name):
                self.assertNotIn(name, vref)

    def test_the_signal_tail_names_rows_that_exist(self):
        from rochviewer.ui import main
        from rochviewer.ui.main import SUMMARY_SIGNAL_TAIL_ROWS

        present = {timing.get("name") for timing in main.TIMINGS}
        if "DLL BWSEL" not in present:
            self.skipTest("platform has no MISC Additional block")
        for name in SUMMARY_SIGNAL_TAIL_ROWS:
            with self.subTest(name=name):
                self.assertIn(name, present)

    def test_the_dfe_bias_rows_follow_the_rtl_block(self):
        # Two paths render Summary RTL and only one is live on a given board.
        # Adding the rows to the other one is a change nobody sees, which is
        # exactly what happened first: this pins them to the path that runs.
        from rochviewer.ui.main import (SUMMARY_DFE_BIAS_ROWS, SUMMARY_RTL_ROWS,
                          insert_summary_rtl_after)

        placed = insert_summary_rtl_after(["tWRWR_dd", "tRDPRE"], "tWRWR_dd")
        self.assertEqual(
            placed,
            ["tWRWR_dd"] + list(SUMMARY_RTL_ROWS) + list(SUMMARY_DFE_BIAS_ROWS)
            + ["tRDPRE"],
        )

    def test_a_dfe_entry_pairs_one_row_rather_than_two(self):
        # The RTL form names two rows; a DFE row already carries both
        # channels, so it names one and the pair comes from its own sides.
        from rochviewer.ui.main import SUMMARY_DFE_BIAS_ROWS, SUMMARY_RTL_ROWS, is_summary_pair

        for entry in SUMMARY_DFE_BIAS_ROWS:
            with self.subTest(entry=entry):
                self.assertTrue(is_summary_pair(entry))
                self.assertEqual(len(entry), 2)
        for entry in SUMMARY_RTL_ROWS:
            with self.subTest(entry=entry):
                self.assertTrue(is_summary_pair(entry))
                self.assertEqual(len(entry), 3)

    def test_the_dfe_rows_name_rows_that_exist(self):
        from rochviewer.ui import main
        from rochviewer.ui.main import SUMMARY_DFE_BIAS_ROWS

        present = {timing.get("name") for timing in main.TIMINGS}
        if not any(name.startswith("DFE Tap") for name in present):
            self.skipTest("platform has no DFE block")
        for label, name in SUMMARY_DFE_BIAS_ROWS:
            with self.subTest(name=name):
                self.assertIn(name, present)

    def test_summary_leaves_the_whole_ccd_group_to_the_timings_tab(self):
        # All four come off one mode-register nibble. Listing part of the
        # group would read as a deliberate selection rather than what it is.
        from rochviewer.ui.main import SUMMARY_EXCLUDED_TIMING_NAMES

        # tCCD is in here too now that it is a row. It is the one member of
        # the group that is not programmable, but it is still a
        # column-to-column delay and the group is excluded as a group -- half
        # of it on the Summary would read as a deliberate selection.
        for name in ("tCCD", "tCCD_L", "tCCD_L_WR", "tCCD_L_WR2"):
            with self.subTest(name=name):
                self.assertIn(name, SUMMARY_EXCLUDED_TIMING_NAMES)

    def test_summary_drops_the_rows_that_restate_a_row_it_already_lists(self):
        # tWR_MR restates tWR and tRTP_MR restates tRTP.
        # On Timings each sits directly under the row it refers to and the
        # pairing is the point; on the Summary the referent is already there
        # and these only lengthen the column.
        from rochviewer.intel import intel_timings
        from rochviewer.ui.main import (SUMMARY_EXCLUDED_TIMING_NAMES,
                          intel_summary_timing_columns)

        primary, tertiary = intel_summary_timing_columns(intel_timings.TIMINGS)
        listed = [n for n in primary + tertiary if isinstance(n, str)]
        for name, referent in (("tWR_MR", "tWR"), ("tRTP_MR", "tRTP")):
            with self.subTest(name=name):
                self.assertIn(name, SUMMARY_EXCLUDED_TIMING_NAMES)
                self.assertNotIn(name, listed)
                # The row it restates does stay: dropping both would lose the
                # timing from the Summary altogether.
                self.assertIn(referent, listed)

    def test_the_refresh_cycle_rows_follow_the_write_recovery(self):
        from rochviewer.intel import intel_timings
        from rochviewer.ui.main import intel_summary_timing_columns

        primary, _tertiary = intel_summary_timing_columns(intel_timings.TIMINGS)
        names = {row.get("name") for row in intel_timings.TIMINGS}
        # tRFCns first -- the same derived row under the same name on both
        # generations -- then whichever spelling this platform uses for the
        # all-bank interval, then the per-bank one. Written from the names
        # that exist rather than fixed to DDR5's, because the suite's stub
        # loads a DDR4 fixture and DDR4 neither renames tRFC nor has a
        # per-bank interval at all.
        expected = [name for name in
                    ("tRFCns", "tRFC2", "tRFC", "tRFCpb")
                    if name in names]
        self.assertTrue(expected, "no refresh cycle row to place")
        start = primary.index("tWR") + 1
        self.assertEqual(primary[start:start + len(expected)], expected)

    def test_only_the_spelling_this_platform_uses_is_listed(self):
        # Both spellings are offered and the one that does not exist here
        # resolves to nothing, so the column never carries a name with no row
        # behind it -- which would show as a gap rather than as a reading.
        from rochviewer.intel import intel_timings
        from rochviewer.ui.main import intel_summary_timing_columns

        primary, tertiary = intel_summary_timing_columns(intel_timings.TIMINGS)
        listed = [n for n in primary + tertiary if isinstance(n, str)]
        names = {row.get("name") for row in intel_timings.TIMINGS}
        for name in listed:
            with self.subTest(name=name):
                self.assertIn(name, names)
        # The pair is mutually exclusive: one spelling, never both. Only
        # tRFC has two -- the ns rows stopped being renamed per platform, so
        # ("tRFCns", "tRFC (ns)") was left here asserting against a name that
        # can no longer exist, which is an assertion that cannot fail.
        for a, b in (("tRFC2", "tRFC"),):
            with self.subTest(pair=(a, b)):
                self.assertFalse(a in listed and b in listed)

    def test_every_excluded_name_is_a_row_that_exists(self):
        # An excluded name with no row behind it excludes nothing, and reads
        # like the row was dealt with when it was never there.
        from rochviewer.intel import intel_timings
        from rochviewer.ui.main import SUMMARY_EXCLUDED_TIMING_NAMES

        names = {row.get("name") for row in intel_timings.TIMINGS}
        for name in SUMMARY_EXCLUDED_TIMING_NAMES:
            with self.subTest(name=name):
                self.assertIn(name, names)

    def test_the_exclusion_list_governs_both_summary_columns(self):
        # It was applied to the primary/secondary column only, so excluding a
        # Tertiary row did nothing and the row stayed on the Summary with no
        # sign of why. tREFI is the exception and is meant to be: the Tertiary
        # column pins it at its head deliberately.
        from rochviewer.intel import intel_timings
        from rochviewer.ui.main import (
            SUMMARY_EXCLUDED_TIMING_NAMES, intel_summary_timing_columns,
        )

        primary, tertiary = intel_summary_timing_columns(intel_timings.TIMINGS)
        shown = set(primary) | set(tertiary)
        for name in SUMMARY_EXCLUDED_TIMING_NAMES:
            if name == "tREFI":
                continue
            with self.subTest(name=name):
                self.assertNotIn(name, shown)

    def test_a_tertiary_row_can_be_kept_off_the_summary(self):
        # The case that failed: a Tertiary name in the exclusion list.
        from rochviewer.intel import intel_timings
        from rochviewer.ui.main import intel_summary_timing_columns

        rows = [t for t in intel_timings.TIMINGS
                if t.get("Category") == "Tertiary" and t.get("name")]
        self.assertTrue(rows, "no Tertiary rows to check against")
        with mock.patch("rochviewer.ui.main.SUMMARY_EXCLUDED_TIMING_NAMES",
                        (rows[0]["name"],)):
            _primary, tertiary = intel_summary_timing_columns(
                intel_timings.TIMINGS
            )
        self.assertNotIn(rows[0]["name"], tertiary)

    def test_the_excluded_names_are_names_rows_actually_have(self):
        # The exclusion once read "tCCDL" and "tCCDL WR", which nothing has
        # ever been called, so it matched nothing and the rows showed anyway.
        # A name here that no row carries is silently doing nothing.
        from rochviewer.intel import intel_timings
        from rochviewer.ui import main
        from rochviewer.ui.main import SUMMARY_EXCLUDED_TIMING_NAMES

        # Both tables: the list is shared, and the two platforms spell the
        # command-rate row differently -- AM5 shortens it to CR. Checking
        # only the loaded profile would call the other one's spelling a typo.
        present = {timing.get("name") for timing in main.TIMINGS}
        present |= {timing.get("name") for timing in intel_timings.TIMINGS}
        for name in SUMMARY_EXCLUDED_TIMING_NAMES:
            with self.subTest(name=name):
                self.assertIn(name, present)

    def test_a_continuous_tab_is_never_also_padded(self):
        # It already lines its columns up by construction, so padding would
        # add blank rows to a layout that does not need them.
        from rochviewer.ui.main import CONTINUOUS_SECTION_TABS, PAIRED_SECTION_TABS

        self.assertFalse(CONTINUOUS_SECTION_TABS & PAIRED_SECTION_TABS)

    def test_padding_is_only_ever_asked_of_a_shaded_tab(self):
        # The spacers carry band colours, so padding an unshaded tab would
        # add rows that shade nothing.
        from rochviewer.ui.main import PAIRED_SECTION_TABS

        self.assertTrue(PAIRED_SECTION_TABS <= SHADED_TABS)

    def test_a_continuous_tab_is_shaded(self):
        # The section headings take their colour from the band sequence, so a
        # continuous tab that was not shaded would band nothing.
        from rochviewer.ui.main import CONTINUOUS_SECTION_TABS

        self.assertTrue(CONTINUOUS_SECTION_TABS <= SHADED_TABS)


class BlankRetryTest(unittest.TestCase):
    """A fixed row that lost its one read is queued to be read again.

    UCLK:MCLK needs the clock block and MCLK to both answer in the same
    instant, so a poll running against the same mailbox could blank it -- and
    nothing read it again, so the em dash stayed for the life of the window.
    """

    class _Label:
        def __init__(self, text):
            self._text = text

        def cget(self, _option):
            return self._text

    def setUp(self):
        self.gui = types.SimpleNamespace(
            live_value_labels=[], _blank_labels=[],
            _is_blank=TimingGUI._is_blank,
        )

    def register(self, timing, text):
        label = self._Label(text)
        TimingGUI._register_live_value(self.gui, timing, label)
        return label

    def test_a_blank_fixed_row_is_queued_for_one_more_read(self):
        label = self.register({"name": "UCLK:MCLK"}, "—")
        self.assertEqual(self.gui._blank_labels, [({"name": "UCLK:MCLK"}, label)])
        self.assertEqual(self.gui.live_value_labels, [])

    def test_a_fixed_row_that_read_is_left_alone(self):
        self.register({"name": "UCLK:MCLK"}, "1:2")
        self.assertEqual(self.gui._blank_labels, [])
        self.assertEqual(self.gui.live_value_labels, [])

    def test_a_live_row_goes_to_the_live_list_even_when_blank(self):
        # It will be re-read every tick anyway; queueing it twice would read
        # the same mailbox twice for one value.
        label = self.register({"name": "PPT", "live": True}, "—")
        self.assertEqual(self.gui.live_value_labels, [({"name": "PPT", "live": True}, label)])
        self.assertEqual(self.gui._blank_labels, [])

    def test_whitespace_around_the_dash_still_counts_as_blank(self):
        self.register({"name": "UCLK:MCLK"}, " — ")
        self.assertEqual(len(self.gui._blank_labels), 1)


class SummaryColumnTailTest(unittest.TestCase):
    """The tail is a membership set; the order comes from one list only.

    Held as two ordered lists they disagreed about where tCKE and tXP go,
    and the tail is applied second, so it silently won.
    """

    def test_every_tail_row_is_in_the_priority_order(self):
        self.assertTrue(SUMMARY_COLUMN_TAIL <= set(AM5_SUMMARY_TIMING_PRIORITY))

    def test_the_tail_is_the_last_thing_in_the_priority_order(self):
        ordered = [name for name in AM5_SUMMARY_TIMING_PRIORITY
                   if name in SUMMARY_COLUMN_TAIL]
        self.assertEqual(
            ordered, ["tRDPRE", "tWRPRE", "tMOD", "tCKE", "tXP"]
        )
        self.assertEqual(list(AM5_SUMMARY_TIMING_PRIORITY)[-len(ordered):],
                         ordered)


if __name__ == "__main__":
    unittest.main()
