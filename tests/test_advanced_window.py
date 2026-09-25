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

"""Cover the Advanced window's row list and its search."""

import types
import unittest

from rochviewer.ui.advanced_window import (
    ROW_HEIGHT, VALUE_WRAP, AdvancedWindow, measuring_font_size,
)
from rochviewer.ui.main import TimingGUI


def build_entries(timings):
    """Call the builder with a stand-in for the app it hangs off."""
    stand_in = types.SimpleNamespace(
        ADVANCED_TABS=TimingGUI.ADVANCED_TABS,
        _read_compact_value=lambda timing: f"<{timing.get('name')}>",
    )
    from rochviewer.ui import main
    saved = main.TIMINGS
    main.TIMINGS = timings
    try:
        return TimingGUI.advanced_entries(stand_in)
    finally:
        main.TIMINGS = saved


def matches(filter_text, haystack):
    return AdvancedWindow._matches(
        types.SimpleNamespace(_filter=filter_text), {"haystack": haystack})


class EntryListTest(unittest.TestCase):
    def test_it_covers_the_reading_tabs(self):
        # Voltages included, so the search and the dump cover the rails too.
        # SPD is added on its own, from the modules the SPD tab reads.
        self.assertEqual(TimingGUI.ADVANCED_TABS,
                         ("System Info", "Timings", "Training", "IMC", "RTL",
                          "Voltages"))

    def test_rows_are_grouped_by_tab_in_tab_order(self):
        # Built tab by tab rather than by walking TIMINGS once, so the window
        # groups the way the tab strip reads even though the table does not
        # store the rows in that order.
        entries = build_entries([
            {"name": "tCL", "Tab": "Timings", "Category": "Primary"},
            {"name": "CPU", "Tab": "System Info", "Category": "General"},
            {"name": "RTL MC0 CHA R0", "Tab": "RTL", "Category": "RTL CHA MC0"},
            {"name": "CMD SComp", "Tab": "Training", "Category": "CMD"},
            {"name": "DLL BWSEL", "Tab": "IMC", "Category": "Misc Additional"},
        ])
        self.assertEqual([tab for tab, *_ in entries],
                         ["System Info", "Timings", "Training", "IMC", "RTL"])

    def test_spacer_rows_are_left_out(self):
        # The tables use blank rows to separate blocks. They are layout, and
        # a searchable list has nothing to show for them.
        entries = build_entries([
            {"name": "tCL", "Tab": "Timings", "Category": "Primary"},
            {"name": "", "Tab": "Timings", "Category": "Primary"},
            {"name": "   ", "Tab": "Timings", "Category": "Primary"},
            {"Tab": "Timings", "Category": "Primary"},
        ])
        self.assertEqual([name for _, _, name, *_ in entries], ["tCL"])

    def test_rows_hidden_from_the_tabs_stay_hidden_here(self):
        entries = build_entries([
            {"name": "tCL", "Tab": "Timings", "Category": "Primary"},
            {"name": "Secret", "Tab": "Timings", "Category": "Primary",
             "diagnostic": True},
        ])
        self.assertEqual([name for _, _, name, *_ in entries], ["tCL"])

    def test_rows_from_other_tabs_are_left_out(self):
        entries = build_entries([
            {"name": "tCL", "Tab": "Timings", "Category": "Primary"},
            {"name": "RTL", "Tab": "Summary", "Category": "General"},
            {"name": "VDD", "Tab": "Sensors", "Category": "Rails"},
        ])
        self.assertEqual([name for _, _, name, *_ in entries], ["tCL"])

    def test_opted_in_diagnostics_are_available_for_dump(self):
        entries = build_entries([
            {"name": "MR1 raw", "Tab": "Training", "Category": "RON diagnostics",
             "diagnostic": True, "advanced_only": True},
            {"name": "Hidden", "Tab": "Training", "diagnostic": True},
        ])
        self.assertEqual([name for _, _, name, *_ in entries], ["MR1 raw"])
        self.assertEqual(entries[0][3](), "<MR1 raw>")

    def test_each_row_reads_its_own_timing(self):
        # The readers are built in a loop; without per-row binding every row
        # would report the last timing's value.
        entries = build_entries([
            {"name": "tCL", "Tab": "Timings", "Category": "Primary"},
            {"name": "tRCD", "Tab": "Timings", "Category": "Primary"},
        ])
        self.assertEqual([read() for _, _, _, read, *_ in entries],
                         ["<tCL>", "<tRCD>"])

    def test_the_real_table_produces_rows(self):
        from rochviewer.ui import main
        entries = build_entries(main.TIMINGS)
        self.assertTrue(entries)
        self.assertTrue(all(name.strip() for _, _, name, *_ in entries))


class LabelTest(unittest.TestCase):
    """The window names a row as its tab does; the dump keeps its own name."""

    def test_the_tab_label_rides_with_the_entry(self):
        entries = build_entries([
            {"name": "QCLK Ratio", "display_name": "QCLK Reference",
             "Tab": "System Info", "Category": "Clocks"},
            {"name": "tCL", "Tab": "Timings", "Category": "Primary"},
        ])
        self.assertEqual([entry[4] for entry in entries],
                         ["QCLK Reference", "tCL"])

    def test_the_dump_keeps_the_rows_own_name(self):
        from rochviewer.ui.advanced_window import format_dump

        text = format_dump([("System Info", "Clocks", "QCLK Ratio",
                             lambda: "100.00 MHz", "QCLK Reference")])
        self.assertIn("QCLK Ratio", text)
        self.assertNotIn("QCLK Reference", text)


class SpdEntryTest(unittest.TestCase):
    def entries(self, modules, channels=("A1", "B1")):
        stand_in = types.SimpleNamespace(
            _spd_modules=modules,
            _spd_system_values={"system_capacity": "32 GB"},
            _channel_headers=lambda _channel, a, b: channels,
        )
        stand_in._spd_field_text = (
            lambda module, key: TimingGUI._spd_field_text(stand_in, module, key))
        return TimingGUI._advanced_spd_entries(stand_in)

    def test_a_module_the_headings_do_not_name_keeps_a_column(self):
        # One slot matches and the other is named differently: the second
        # module takes the free column rather than being left out.
        modules = [{"slot": "A1", "part_number": "KIT-A"},
                   {"slot": "B1", "part_number": "KIT-B"}]
        part = next(read for _t, category, _n, read, label
                    in self.entries(modules, ("A1", "B2"))
                    if label == "Part Number")
        self.assertEqual(part(), ("KIT-A", "KIT-B"))

    def test_each_module_takes_its_slots_column(self):
        modules = [
            {"slot": "B1", "part_number": "KIT-B", "address": 0x52,
             "profiles": [{"name": "XMP-8000", "cl": 38}]},
            {"slot": "A1", "part_number": "KIT-A", "address": 0x50,
             "profiles": [{"name": "XMP-8000", "cl": 40}]},
        ]
        by_label = {}
        for tab, category, _name, read, label in self.entries(modules):
            self.assertEqual(tab, "SPD")
            by_label.setdefault((category, label), read)
        self.assertEqual(by_label[("Identity", "Part Number")](),
                         ("KIT-A", "KIT-B"))
        self.assertEqual(by_label[("Identity", "SPD Address")](),
                         ("0x50", "0x52"))
        self.assertEqual(by_label[("Module", "Capacity")](),
                         ("32 GB", "32 GB"))
        self.assertEqual(by_label[("Profile 1", "Profile")](),
                         ("XMP-8000", "XMP-8000"))
        self.assertEqual(by_label[("Profile 1", "CAS Latency")](), ("40", "38"))
        self.assertEqual(by_label[("Profile 2", "Profile")](), ("—", "—"))

    def test_before_the_read_every_value_is_a_dash(self):
        for _tab, _category, _name, read, _label in self.entries([]):
            self.assertEqual(read(), ("—", "—"))

    def test_every_dump_name_is_its_own(self):
        names = [entry[2] for entry in self.entries([])]
        self.assertEqual(len(names), len(set(names)))


class SearchTest(unittest.TestCase):
    def test_an_empty_search_matches_everything(self):
        self.assertTrue(matches("", "timings primary tcl"))

    def test_a_word_matches_anywhere_in_the_row(self):
        self.assertTrue(matches("tcl", "timings primary tcl"))
        self.assertTrue(matches("primary", "timings primary tcl"))
        self.assertTrue(matches("timings", "timings primary tcl"))

    def test_every_word_must_match(self):
        # Two words narrow rather than widen, which is what makes searching
        # "skew vref" useful when both tabs carry a VREF block.
        self.assertTrue(matches("skew vref", "skew vref dq vrefup"))
        self.assertFalse(matches("skew vref", "misc features row hammer"))
        self.assertFalse(matches("skew vref", "timings primary tcl"))

    def test_the_tab_name_is_searchable(self):
        self.assertTrue(matches("imc", "imc power down idle length"))

    def test_matching_ignores_case(self):
        self.assertTrue(matches("vref", "skew vref ca vref"))
        self.assertTrue(matches("CA", "skew vref ca vref".lower()))


if __name__ == "__main__":
    unittest.main()


class ValueHeightTest(unittest.TestCase):
    """A value gets a second line only when it actually needs one."""

    def test_the_measuring_font_is_sized_in_pixels(self):
        # Negative means pixels, which is what customtkinter renders a label
        # in. Positive would mean points, and Tk scales those by the display
        # factor -- 1.333 here -- so values measured a third wider than they
        # draw and rows claimed a second line they left empty.
        self.assertEqual(measuring_font_size(("Consolas", 12)), -12)
        self.assertEqual(measuring_font_size(("Consolas", -12)), -12)

    def test_an_unreadable_font_spec_measures_nothing(self):
        self.assertIsNone(measuring_font_size(("Consolas",)))
        self.assertIsNone(measuring_font_size(None))

    def _height(self, measured):
        stand_in = types.SimpleNamespace(
            _value_font=types.SimpleNamespace(measure=lambda _text: measured)
        )
        return AdvancedWindow._value_height(stand_in, "any")

    def test_a_value_inside_the_column_stays_one_line(self):
        # The board model at its real rendered width. This is the case that
        # regressed: it fits, and used to be given two lines anyway.
        self.assertEqual(self._height(224), ROW_HEIGHT)
        self.assertEqual(self._height(VALUE_WRAP), ROW_HEIGHT)

    def test_a_value_wider_than_the_column_gets_a_second_line(self):
        # The OS string, which genuinely wraps.
        self.assertEqual(self._height(427), ROW_HEIGHT * 2)
        self.assertEqual(self._height(VALUE_WRAP + 1), ROW_HEIGHT * 2)

    def test_no_font_means_the_default_height(self):
        stand_in = types.SimpleNamespace(_value_font=None)
        self.assertEqual(
            AdvancedWindow._value_height(stand_in, "any"), ROW_HEIGHT
        )
