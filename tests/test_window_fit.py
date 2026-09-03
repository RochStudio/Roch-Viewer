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

"""The window is sized to the widest tab, not to the narrowest.

The startup size was fitted to Summary, which is the narrowest tab. Timings
and Misc are wider, and grew wider as rows were added, so the right-hand
channel of a dual row was cut off mid value and decoded strings simply ended.
"""

import os
import struct
import types
import inspect
import unittest

from rochviewer.ui.main import TimingGUI


def icon_widths():
    """The sizes icon.ico actually stores, or [] when it is not there."""
    path = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
                        "icon.ico")
    if not os.path.exists(path):
        return []
    with open(path, "rb") as handle:
        data = handle.read()
    return [struct.unpack_from("<BBBBHHII", data, 6 + i * 16)[0] or 256
            for i in range(struct.unpack_from("<H", data, 4)[0])]


def frame(width, managed=True):
    """A stand-in for a half. ``managed`` mirrors winfo_manager()."""
    return types.SimpleNamespace(
        winfo_reqwidth=lambda width=width: width,
        winfo_manager=lambda managed=managed: "grid" if managed else "",
    )


def gui(grid_frames):
    stand_in = types.SimpleNamespace(
        grid_frames=grid_frames, TAB_CHROME_WIDTH=TimingGUI.TAB_CHROME_WIDTH
    )
    return TimingGUI.required_tab_width(stand_in)


class RequiredWidthTest(unittest.TestCase):
    def test_it_takes_the_widest_tab_not_the_first(self):
        # Summary narrow, Misc wide: the answer has to be Misc's.
        width = gui({
            "Summary": {"Left": frame(320), "Right": frame(320)},
            "Misc": {"Left": frame(433), "Right": frame(433)},
        })
        self.assertEqual(width, 866 + TimingGUI.TAB_CHROME_WIDTH)

    def test_both_halves_of_a_tab_are_counted(self):
        self.assertEqual(gui({"Timings": {"Left": frame(399),
                                          "Right": frame(399)}}),
                         798 + TimingGUI.TAB_CHROME_WIDTH)

    def test_a_tab_held_as_a_list_is_measured_too(self):
        # Not every tab stores its halves in a dict.
        self.assertEqual(gui({"Skew": [frame(400), frame(400)]}),
                         800 + TimingGUI.TAB_CHROME_WIDTH)

    def test_an_ungridded_placeholder_is_not_counted(self):
        # System Info keeps a right-hand placeholder it never grids,
        # so callers can expect both keys. Counted, it made the
        # window wider than any tab actually draws.
        width = gui({"System Info": {"Left": frame(598),
                                     "Right": frame(200, managed=False)}})
        self.assertEqual(width, 598 + TimingGUI.TAB_CHROME_WIDTH)

    def test_an_unbuilt_or_empty_tab_contributes_nothing(self):
        self.assertEqual(gui({}), 0)
        self.assertEqual(gui({"Timings": {"Left": None, "Right": None}}), 0)

    def test_a_frame_that_cannot_be_measured_is_skipped(self):
        broken = types.SimpleNamespace()
        width = gui({"Timings": {"Left": broken, "Right": frame(400)}})
        self.assertEqual(width, 400 + TimingGUI.TAB_CHROME_WIDTH)


class ChromeTest(unittest.TestCase):
    """The app's own title bar and footer, and what they cost the tabs."""

    def test_the_startup_size_is_the_one_that_was_asked_for(self):
        # Pinned rather than derived. The title bar and footer sit inside the
        # window rather than in a frame around it, so 850 has to carry them
        # as well as the tabs: the two take 54px, and Summary needs 644.
        self.assertEqual(TimingGUI.WINDOW_WIDTH, 710)
        self.assertEqual(TimingGUI.WINDOW_HEIGHT, 790)
        chrome = TimingGUI.TITLE_BAR_HEIGHT + TimingGUI.FOOTER_HEIGHT
        self.assertEqual(chrome, 54)
        # No assertion that Summary clears its 644px here any more: at this
        # height it does not, and scrolls by 17px. That is the chosen size,
        # not a defect, so the test records the size rather than a fit it no
        # longer has.

    def test_the_fitted_width_pass_compares_against_the_asked_for_size(self):
        # It runs before the window is mapped, where winfo_width() answers
        # with Tk's 200x200 default. Compared against that, every tab looked
        # too wide and the window grew past the pinned size every time.
        source = inspect.getsource(TimingGUI._widen_to_fit_tabs)
        self.assertIn('getattr(self, "_window_width"', source)
        self.assertIn("self._window_width = target", source)
        self.assertIn("_window_width = window_width",
                      inspect.getsource(TimingGUI.setup_window_geometry))

    def test_the_logo_picks_a_size_it_can_actually_draw(self):
        # Tk cannot scale an image up, so the entry chosen has to be at or
        # above the size wanted whenever the file has one. A tie broke
        # downward before: asking 20 of a file holding 16 and 24 took the 16
        # and drew a logo four pixels short of its slot.
        stored = [16, 24, 32, 48, 64, 128, 256]
        for want, expected in ((16, 16), (20, 24), (24, 24), (32, 32),
                               (40, 48)):
            with self.subTest(want=want):
                self.assertEqual(TimingGUI.choose_icon_size(stored, want),
                                 expected)

    def test_it_falls_back_to_the_largest_when_nothing_is_big_enough(self):
        # Better a logo drawn small than no logo at all.
        self.assertEqual(TimingGUI.choose_icon_size([16, 24], 300), 24)

    def test_the_title_bar_asks_for_a_size_the_icon_actually_stores(self):
        # An exact match is the only case with no scaling at all, and the
        # strip is 30px so 24 is the largest that fits with room either side.
        self.assertEqual(TimingGUI.LOGO_SIZE, 24)
        self.assertLess(TimingGUI.LOGO_SIZE, TimingGUI.TITLE_BAR_HEIGHT)
        widths = icon_widths()
        if not widths:
            self.skipTest("icon.ico is not beside the module")
        self.assertIn(TimingGUI.LOGO_SIZE, widths)
        self.assertEqual(
            TimingGUI.choose_icon_size(widths, TimingGUI.LOGO_SIZE),
            TimingGUI.LOGO_SIZE)

    def test_the_chrome_takes_its_colours_from_the_palette(self):
        # These two used to be asserted as a matching pair on BRAND_COLOR.
        # They no longer match: the title is plain text and the footer link is
        # branded, because the red marks what is selected or interactive and a
        # static title competes with that. What still holds, and is the point
        # of the test, is that neither writes a colour literal -- two literals
        # drift, palette entries cannot.
        title = inspect.getsource(TimingGUI.build_title_bar)
        footer = inspect.getsource(TimingGUI.build_footer)
        self.assertIn("text_color=self.TEXT_COLOR", title)
        self.assertNotIn("text_color=self.BRAND_COLOR", title)
        self.assertIn("text_color=self.BRAND_COLOR", footer)
        for name, source in (("title bar", title), ("footer", footer)):
            with self.subTest(chrome=name):
                self.assertNotRegex(source, r'text_color=\("?#')

    def test_the_selected_tab_carries_readable_text_on_the_red(self):
        # The strip went from blue to the app's red. TEXT_COLOR is near-black
        # in light mode, which the old blue was light enough to carry and
        # #B91C1C is not: it measured 2.8:1, under the 4.5:1 floor. White is
        # 6.5:1 there and 12.9:1 on the dark red.
        source = inspect.getsource(TimingGUI.setup_appearance)
        self.assertIn('self.TAB_SELECTED_TEXT_COLOR = ("#FFFFFF", "#FFFFFF")',
                      source)
        self.assertIn('self.TAB_SELECTED_COLOR = ("#B91C1C", "#5D1A1A")',
                      source)

    def test_the_tab_strip_is_the_only_thing_on_the_selected_colour(self):
        # The Light/Dark pair used to share that colour and needed the same
        # readable text on it. It is one plain button now, drawn on the
        # unselected colour like Telemetry and Advanced beside it, so the
        # strip is the only place the selected red is a background.
        source = inspect.getsource(TimingGUI.create_widgets)
        self.assertEqual(source.count("_selected_text_color"), 1)
        tools = inspect.getsource(TimingGUI.build_tab_strip_tools)
        self.assertIn("fg_color=self.TAB_UNSELECTED_COLOR", tools)
        self.assertNotIn("TAB_SELECTED_COLOR", tools)

    def test_the_theme_control_is_a_single_button(self):
        # A pair spent half its width naming the mode you are not in.
        tools = inspect.getsource(TimingGUI.build_tab_strip_tools)
        self.assertNotIn("CTkSegmentedButton", tools)
        self.assertIn("self.appearance_button", tools)
        self.assertNotIn("appearance_selector", inspect.getsource(TimingGUI))

    def test_the_tools_are_measured_onto_the_tab_strip(self):
        # TAB_STRIP_HEIGHT is what the tabview is asked for, not what the
        # strip draws as -- 36 against 26 here -- so the offset is measured
        # rather than assumed, and on <Configure> rather than after_idle,
        # which runs before there is a size to measure.
        source = inspect.getsource(TimingGUI.align_tab_strip_tools)
        self.assertIn("strip.winfo_rooty() - self.tabview.winfo_rooty()",
                      source)
        self.assertIn('self.root.bind("<Configure>"',
                      inspect.getsource(TimingGUI.build_tab_strip_tools))

    def test_an_unused_half_is_taken_out_of_the_grid(self):
        # An empty CTkFrame still asks for the toolkit's default 200px, so
        # the grid handed Misc's unused half a 56px slice for holding
        # nothing and the row shading stopped short of it. grid_remove keeps
        # the configuration, so the half comes back if the tab splits again.
        source = inspect.getsource(TimingGUI._stretch_tab_halves)
        self.assertIn("right.grid_remove()", source)
        self.assertIn("grid_columnconfigure(1, minsize=0, weight=0)", source)

    def test_summary_is_the_tab_drawn_without_a_scrollbar(self):
        # It gives up no width to a gutter it never uses. The other four are
        # longer than any window and keep theirs. Whether it still fits is
        # measured against the drawn window in test_unscrolled_fit_live --
        # without a scrollbar, content past the bottom is not reachable.
        self.assertEqual(TimingGUI.UNSCROLLED_TABS, ("Summary",))

    def test_the_footer_links_to_the_handle_it_names(self):
        self.assertEqual(TimingGUI.TWITTER_URL,
                         "https://x.com/MateoPCTech")
        self.assertTrue(
            TimingGUI.TWITTER_URL.endswith(
                TimingGUI.TWITTER_HANDLE.lstrip("@")),
            "the link and the text it shows have to name the same account")

    def test_the_footer_is_packed_before_the_tabs_claim_the_height(self):
        # Packed after the expanding tabview, a bottom-side footer is pushed
        # off the window entirely rather than sitting under it.
        source = inspect.getsource(TimingGUI.create_widgets)
        self.assertLess(source.index("self.build_footer()"),
                        source.index("self.tabview = ctk.CTkTabview("))


if __name__ == "__main__":
    unittest.main()


class RowBandTest(unittest.TestCase):
    """Both kinds of section answer the band question the same way.

    A tab puts dual-channel sections in one column and single-value ones in
    the other. While the two branches computed the position differently, every
    row on Skew came out one shade on the left and the other on the right.
    """

    def test_the_heading_holds_a_band_position_on_a_continuous_tab(self):
        # The heading sits at the section's offset, so its first data row is
        # the position after it.
        self.assertEqual(TimingGUI.row_band(1, True, 0), 2)
        self.assertEqual(TimingGUI.row_band(1, True, 1), 3)

    def test_a_stacked_tab_has_no_heading_row_to_count(self):
        self.assertEqual(TimingGUI.row_band(0, False, 0), 0)
        self.assertEqual(TimingGUI.row_band(6, False, 2), 8)

    def test_the_next_section_carries_on_from_the_last(self):
        # Offset advances by the heading plus the rows drawn, so the first row
        # of the next section is the position after the last row of this one.
        rows_drawn = 6
        following = 1 + 1 + rows_drawn
        self.assertEqual(TimingGUI.row_band(following, True, 0),
                         TimingGUI.row_band(1, True, rows_drawn - 1) + 2)

    def test_opposite_columns_stay_in_step(self):
        # Skew's left column is dual and its right single, both starting at
        # offset 1. Facing rows must land on the same band.
        for data_row in range(8):
            with self.subTest(data_row=data_row):
                dual = TimingGUI.row_band(1, True, data_row)
                single = TimingGUI.row_band(1, True, data_row)
                self.assertEqual(dual % 2, single % 2)
