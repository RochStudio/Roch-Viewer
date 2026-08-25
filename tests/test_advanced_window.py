"""Cover the Advanced window's row list and its search."""

import types
import unittest

from advanced_window import (
    ROW_HEIGHT, VALUE_WRAP, AdvancedWindow, measuring_font_size,
)
from main import TimingGUI


def build_entries(timings):
    """Call the builder with a stand-in for the app it hangs off."""
    stand_in = types.SimpleNamespace(
        ADVANCED_TABS=TimingGUI.ADVANCED_TABS,
        _read_compact_value=lambda timing: f"<{timing.get('name')}>",
    )
    import main
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
    def test_it_covers_the_four_reading_tabs(self):
        self.assertEqual(TimingGUI.ADVANCED_TABS,
                         ("System Info", "Timings", "Skew", "Misc"))

    def test_rows_are_grouped_by_tab_in_tab_order(self):
        # Built tab by tab rather than by walking TIMINGS once, so the window
        # groups the way the tab strip reads even though the table does not
        # store the rows in that order.
        entries = build_entries([
            {"name": "tCL", "Tab": "Timings", "Category": "Primary"},
            {"name": "CPU", "Tab": "System Info", "Category": "General"},
            {"name": "CMD SComp", "Tab": "Skew", "Category": "CMD"},
            {"name": "idle_length", "Tab": "Misc", "Category": "Power Down"},
        ])
        self.assertEqual([tab for tab, _, _, _ in entries],
                         ["System Info", "Timings", "Skew", "Misc"])

    def test_spacer_rows_are_left_out(self):
        # The tables use blank rows to separate blocks. They are layout, and
        # a searchable list has nothing to show for them.
        entries = build_entries([
            {"name": "tCL", "Tab": "Timings", "Category": "Primary"},
            {"name": "", "Tab": "Timings", "Category": "Primary"},
            {"name": "   ", "Tab": "Timings", "Category": "Primary"},
            {"Tab": "Timings", "Category": "Primary"},
        ])
        self.assertEqual([name for _, _, name, _ in entries], ["tCL"])

    def test_rows_hidden_from_the_tabs_stay_hidden_here(self):
        entries = build_entries([
            {"name": "tCL", "Tab": "Timings", "Category": "Primary"},
            {"name": "Secret", "Tab": "Timings", "Category": "Primary",
             "diagnostic": True},
        ])
        self.assertEqual([name for _, _, name, _ in entries], ["tCL"])

    def test_rows_from_other_tabs_are_left_out(self):
        entries = build_entries([
            {"name": "tCL", "Tab": "Timings", "Category": "Primary"},
            {"name": "RTL", "Tab": "Summary", "Category": "General"},
            {"name": "VDD", "Tab": "Sensors", "Category": "Rails"},
        ])
        self.assertEqual([name for _, _, name, _ in entries], ["tCL"])

    def test_each_row_reads_its_own_timing(self):
        # The readers are built in a loop; without per-row binding every row
        # would report the last timing's value.
        entries = build_entries([
            {"name": "tCL", "Tab": "Timings", "Category": "Primary"},
            {"name": "tRCD", "Tab": "Timings", "Category": "Primary"},
        ])
        self.assertEqual([read() for _, _, _, read in entries],
                         ["<tCL>", "<tRCD>"])

    def test_the_real_table_produces_rows(self):
        import main
        entries = build_entries(main.TIMINGS)
        self.assertTrue(entries)
        self.assertTrue(all(name.strip() for _, _, name, _ in entries))


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
        self.assertTrue(matches("misc", "misc power down idle_length"))

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
