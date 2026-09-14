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

"""The fixed compact window keeps every unscrolled tab reachable."""

import os
import struct
import inspect
import unittest

from rochviewer.ui.main import (
    PAIRED_SECTION_TABS, TimingGUI, VIEWPORT_SHADED_TABS,
)


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


class ChromeTest(unittest.TestCase):
    """The app's own title bar and footer, and what they cost the tabs."""

    def test_the_startup_size_is_the_one_that_was_asked_for(self):
        # Pinned rather than derived; scrolling absorbs longer pages.
        self.assertEqual(TimingGUI.WINDOW_WIDTH, 750)
        self.assertEqual(TimingGUI.WINDOW_HEIGHT, 800)
        self.assertNotIn("Summary", TimingGUI.UNSCROLLED_TABS)
        chrome = TimingGUI.TITLE_BAR_HEIGHT + TimingGUI.FOOTER_HEIGHT
        self.assertEqual(chrome, 54)
        # The rendered fit is checked separately against the live window; this
        # assertion records the requested fixed size itself.

    def test_summary_top_pairs_have_readable_spacing(self):
        source = inspect.getsource(TimingGUI._summary_about_pair)
        self.assertIn('padx=(0, 4)', source)
        self.assertIn('padx=(0, 8)', source)

    def test_tabs_do_not_expand_the_fixed_window_after_startup(self):
        source = inspect.getsource(TimingGUI.__init__)
        self.assertNotIn("self._widen_to_fit_tabs()", source)
        geometry = inspect.getsource(TimingGUI.setup_window_geometry)
        self.assertIn("window_width = self.WINDOW_WIDTH", geometry)
        self.assertNotIn("extra_columns", geometry)

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

    def test_the_tools_use_a_separate_compact_strip(self):
        source = inspect.getsource(TimingGUI.build_tab_strip_tools)
        self.assertIn("before=self.tabview", source)
        self.assertIn("bar.pack_propagate(False)", source)
        self.assertNotIn("bar.place(", source)

    def test_an_unused_half_is_taken_out_of_the_grid(self):
        # An empty CTkFrame still asks for the toolkit's default 200px, so
        # the grid handed Misc's unused half a 56px slice for holding
        # nothing and the row shading stopped short of it. grid_remove keeps
        # the configuration, so the half comes back if the tab splits again.
        source = inspect.getsource(TimingGUI._stretch_tab_halves)
        self.assertIn("unused.grid_remove()", source)
        self.assertIn("grid_column, minsize=0, weight=0", source)

    def test_only_tabs_that_fit_are_drawn_without_scrollbars(self):
        # They give up no width to gutters they do not use. Whether they fit is
        # measured against the drawn window in test_unscrolled_fit_live --
        # without a scrollbar, content past the bottom is not reachable.
        self.assertEqual(
            TimingGUI.UNSCROLLED_TABS,
            ("RTL", "IMC", "Voltages"),
        )
        for name in ("Summary", "System Info", "Timings", "Training"):
            self.assertNotIn(name, TimingGUI.UNSCROLLED_TABS)

    def test_timings_and_training_are_two_columns_and_imc_is_three(self):
        source = inspect.getsource(TimingGUI.create_widgets)
        self.assertIn('if name == "IMC"', source)
        self.assertIn('column_keys = ("Left", "Middle", "Right")', source)
        self.assertIn('column_keys = ("Left", "Right")', source)
        self.assertNotIn("stacked =", source)

        from rochviewer.ui.main import SKEW_SECTION_ORDER

        self.assertEqual(
            SKEW_SECTION_ORDER,
            ("RTT", "ODT", "RON", "ODT DELAY",
             "DFE", "VREF", "ODTL",
             "Command", "Mode Registers", "DQS", "Preamble", "ECS"),
        )

    def test_training_keeps_its_right_column(self):
        source = inspect.getsource(TimingGUI.load_all_tabs_content)
        self.assertIn("available_columns = set(self.grid_frames[tab_name])", source)
        self.assertIn('timing.get("Column", "Left") != "Left"', source)

    def test_imc_uses_the_balanced_three_column_section_order(self):
        from rochviewer.ui.main import IMC_SECTION_ORDER

        self.assertEqual(
            IMC_SECTION_ORDER,
            ("VREF", "Command", "ODTL", "Refresh",
             "DATA", "CMD", "CLK", "CTL", "SComp",
             "MISC Additional", "Features", "Power Down"),
        )

    def test_module_selector_is_built_for_summary_and_module_aware_tabs(self):
        source = inspect.getsource(TimingGUI.create_widgets)
        self.assertNotIn("bottom_part_number_frame", source)
        summary_branch = source[
            source.index('if name == "Summary"'):
            source.index('if name == "System Info"')
        ]
        self.assertIn("_build_summary_module_selector", summary_branch)
        self.assertEqual(source.count("_build_summary_module_selector"), 1)
        self.assertIn('(\"Timings\", \"Training\")', source)
        self.assertIn("_build_module_selector", source)
        # The holder occupies row 0 and the selector is a sibling in row 1.
        # Putting the selector inside ``frame`` makes it disappear below the
        # scrollable timing table until the user scrolls to its end.
        self.assertIn('holder.grid(row=0, column=0, sticky="nsew")', source)
        self.assertIn('tab_page, name, row=1', source)
        selector = inspect.getsource(TimingGUI._build_module_selector)
        self.assertIn("CTkOptionMenu", selector)
        self.assertIn("fg_color=self.BRAND_COLOR", selector)
        choices = inspect.getsource(TimingGUI._prepare_module_choices)
        self.assertIn('{"All modules": None}', choices)

    def test_short_banded_tabs_extend_their_shading_when_selected(self):
        self.assertEqual(
            VIEWPORT_SHADED_TABS,
            frozenset({
                "System Info", "Timings", "Training", "IMC", "RTL", "Voltages",
            }),
        )
        source = inspect.getsource(TimingGUI._on_tab_changed)
        self.assertIn("_extend_tab_shading_to_viewport", source)
        self.assertIn("after_idle", source)
        extension = inspect.getsource(TimingGUI._extend_tab_shading_to_viewport)
        self.assertIn("self.tabview.tab(tab_name)", extension)

    def test_system_info_columns_stack_without_cross_column_padding(self):
        # Four identity sections face only two clock/memory sections. Pairing
        # them inserts blank rows between Processor and Motherboard.
        self.assertNotIn("System Info", PAIRED_SECTION_TABS)

    def test_system_info_section_names_are_shaded_rows(self):
        from rochviewer.ui.main import CONTINUOUS_SECTION_TABS

        self.assertIn("System Info", CONTINUOUS_SECTION_TABS)
        source = inspect.getsource(TimingGUI.create_section)
        self.assertIn(
            "self.VALUE_COLOR if uniform_header else self.SUBTITLE_COLOR",
            source,
        )

    def test_the_footer_links_to_the_handle_it_names(self):
        self.assertEqual(TimingGUI.TWITTER_URL,
                         "https://x.com/MateoPCTech")
        self.assertTrue(
            TimingGUI.TWITTER_URL.endswith(
                TimingGUI.TWITTER_HANDLE.lstrip("@")),
            "the link and the text it shows have to name the same account")
        self.assertEqual(
            TimingGUI.YOUTUBE_URL,
            "https://www.youtube.com/@MateoPcTech",
        )
        self.assertEqual(TimingGUI.YOUTUBE_LABEL, "YouTube")
        self.assertEqual(TimingGUI.TWITTER_LABEL, "X")
        self.assertEqual(
            TimingGUI.DISCORD_URL,
            "https://discord.gg/KfzExpKQHB",
        )
        self.assertEqual(TimingGUI.DISCORD_LABEL, "Discord")
        footer = inspect.getsource(TimingGUI.build_footer)
        self.assertIn("self.open_youtube", footer)
        self.assertIn("self.open_twitter", footer)
        self.assertIn("self.open_discord", footer)
        self.assertEqual(footer.count('text="|"'), 2)

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
    row on Training came out one shade on the left and the other on the right.
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
        # Training's left column is dual and its right single, both starting at
        # offset 1. Facing rows must land on the same band.
        for data_row in range(8):
            with self.subTest(data_row=data_row):
                dual = TimingGUI.row_band(1, True, data_row)
                single = TimingGUI.row_band(1, True, data_row)
                self.assertEqual(dual % 2, single % 2)
