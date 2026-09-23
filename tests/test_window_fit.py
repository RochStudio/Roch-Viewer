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
from unittest import mock

import rochviewer.ui.main as main_ui

from rochviewer.ui.main import (
    PAIRED_SECTION_TABS, TimingGUI, VIEWPORT_SHADED_TABS,
    timings_three_column_layout,
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

    def test_summary_owns_the_startup_size(self):
        self.assertEqual(TimingGUI.WINDOW_WIDTH, 775)
        self.assertEqual(TimingGUI.WINDOW_HEIGHT, 775)
        self.assertEqual(
            TimingGUI.window_size_for_tab("Summary"), (775, 775)
        )
        self.assertIn("Summary", TimingGUI.UNSCROLLED_TABS)
        self.assertIn("System Info", TimingGUI.UNSCROLLED_TABS)
        chrome = TimingGUI.TITLE_BAR_HEIGHT + TimingGUI.FOOTER_HEIGHT
        self.assertEqual(chrome, 54)

    def test_each_main_tab_has_its_own_size(self):
        self.assertEqual(TimingGUI.TAB_WINDOW_SIZES, {
            "Summary": (775, 775),
            "System Info": (750, 750),
            "SPD": (750, 750),
            "Timings": (750, 750),
            "Training": (1010, 800),
            "IMC": (750, 1100),
            "RTL": (750, 654),
            "Voltages": (750, 654),
        })
        self.assertEqual(
            TimingGUI.window_size_for_tab("unknown"), (775, 775)
        )

    def test_spd_resolves_the_moved_system_memory_values(self):
        gui = TimingGUI.__new__(TimingGUI)
        rows = [
            {"name": "Channels", "Tab": "SPD", "value": "Dual Channel"},
            {"name": "Memory Capacity", "Tab": "SPD", "value": "32GB"},
        ]
        with mock.patch.object(main_ui, "TIMINGS", rows):
            self.assertEqual(gui._read_spd_system_values(), {
                "system_channels": "Dual Channel",
                "system_capacity": "32GB",
            })

    def test_spd_shows_the_source_backed_module_type(self):
        source = inspect.getsource(TimingGUI._build_spd_tab)
        self.assertIn('(\"Module Type\", \"module_type\")', source)

    def test_a_short_screen_caps_each_tab_height(self):
        self.assertEqual(
            TimingGUI.window_size_for_tab("IMC", screen_height=800),
            (750, 680),
        )

    def test_summary_top_pairs_have_readable_spacing(self):
        source = inspect.getsource(TimingGUI._summary_about_pair)
        self.assertIn('padx=(0, self.COLUMN_GAP)', source)
        self.assertIn('padx=(0, self.DETAIL_COLUMN_GAP)', source)

    def test_tab_changes_apply_their_own_window_size(self):
        source = inspect.getsource(TimingGUI.__init__)
        self.assertNotIn("self._widen_to_fit_tabs()", source)
        geometry = inspect.getsource(TimingGUI.setup_window_geometry)
        self.assertIn("self.window_size_for_tab", geometry)
        self.assertNotIn("extra_columns", geometry)
        changed = inspect.getsource(TimingGUI._on_tab_changed)
        self.assertIn("self._resize_for_tab(tab_name)", changed)

    def test_popouts_are_placed_on_their_requested_side(self):
        root = type("Root", (), {
            "update_idletasks": lambda self: None,
            "winfo_x": lambda self: 900,
            "winfo_y": lambda self: 100,
            "winfo_width": lambda self: 750,
            "winfo_screenwidth": lambda self: 2560,
            "winfo_screenheight": lambda self: 1440,
        })()
        app = type("App", (), {"root": root})()
        place = TimingGUI.adjacent_window_position
        self.assertEqual(place(app, "right", 520, 800), (1658, 100))
        self.assertEqual(place(app, "left", 600, 800), (292, 100))

    def test_popouts_fall_back_to_the_other_side_at_a_screen_edge(self):
        root = type("Root", (), {
            "update_idletasks": lambda self: None,
            "winfo_x": lambda self: 20,
            "winfo_y": lambda self: 700,
            "winfo_width": lambda self: 750,
            "winfo_screenwidth": lambda self: 1920,
            "winfo_screenheight": lambda self: 1080,
        })()
        app = type("App", (), {"root": root})()
        self.assertEqual(
            TimingGUI.adjacent_window_position(app, "left", 600, 800),
            (778, 280),
        )

    def test_tab_switch_does_not_force_a_layout_at_the_old_size(self):
        resize = inspect.getsource(TimingGUI._resize_for_tab)
        self.assertNotIn("update_idletasks()", resize)
        changed = inspect.getsource(TimingGUI._on_tab_changed)
        self.assertIn("after_cancel", changed)
        self.assertIn("_finish_tab_shading", changed)
        shading = inspect.getsource(TimingGUI._extend_tab_shading_to_viewport)
        self.assertIn("self._shading_viewports.get(tab_name)", shading)

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
        self.assertIn("fg_color=self.BG_COLOR", title)
        self.assertNotIn("HEADER_COLOR", title)
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
        # 6.5:1 there, and 4.9:1 on the Refined dark red -- #E0383E, the
        # mockup's, was 4.4:1 and is the hover colour instead.
        source = inspect.getsource(TimingGUI.setup_appearance)
        self.assertIn('self.TAB_SELECTED_TEXT_COLOR = ("#FFFFFF", "#FFFFFF")',
                      source)
        self.assertIn('self.TAB_SELECTED_COLOR = ("#B91C1C", "#D0343A")',
                      source)

    def test_title_bar_uses_clean_windows_style_symbols(self):
        source = inspect.getsource(TimingGUI.build_title_bar)
        self.assertIn('("×", self.root.destroy', source)
        self.assertIn('("−", self.minimize_window', source)
        self.assertNotIn('"✕"', source)
        self.assertNotIn('"–"', source)

    def test_theme_toggle_is_an_icon_left_of_minimize(self):
        title = inspect.getsource(TimingGUI.build_title_bar)
        tools = inspect.getsource(TimingGUI.build_tab_strip_tools)
        self.assertIn("self.appearance_button", title)
        self.assertIn("self.appearance_toggle_icon()", title)
        self.assertNotIn("self.appearance_button", tools)
        self.assertEqual(TimingGUI.LIGHT_MODE_ICON, "☀")
        self.assertEqual(TimingGUI.DARK_MODE_ICON, "☾")
        self.assertGreater(
            title.index("self.appearance_button"),
            title.index("self.minimize_window"),
        )

    def test_the_tab_strip_is_the_only_thing_on_the_selected_colour(self):
        # The title-bar theme icon and the two tab tools do not use the selected
        # red, so the active tab remains the only selected-colour background.
        source = inspect.getsource(TimingGUI.create_widgets)
        self.assertEqual(source.count("_selected_text_color"), 1)
        tools = inspect.getsource(TimingGUI.build_tab_strip_tools)
        self.assertIn("fg_color=self.TAB_UNSELECTED_COLOR", tools)
        self.assertNotIn("TAB_SELECTED_COLOR", tools)

    def test_the_theme_control_is_a_single_button(self):
        title = inspect.getsource(TimingGUI.build_title_bar)
        self.assertNotIn("CTkSegmentedButton", title)
        self.assertEqual(title.count("self.appearance_button ="), 1)
        self.assertNotIn("appearance_selector", inspect.getsource(TimingGUI))

    def test_the_tools_share_the_tab_header(self):
        source = inspect.getsource(TimingGUI.build_tab_strip_tools)
        self.assertIn("self.tabview", source)
        self.assertIn('bar.place(relx=1.0, x=-4, y=10, anchor="ne")', source)
        self.assertNotIn("before=self.tabview", source)

    def test_an_unused_half_is_taken_out_of_the_grid(self):
        # An empty CTkFrame still asks for the toolkit's default 200px, so
        # the grid handed Misc's unused half a 56px slice for holding
        # nothing and the row shading stopped short of it. grid_remove keeps
        # the configuration, so the half comes back if the tab splits again.
        source = inspect.getsource(TimingGUI._stretch_tab_halves)
        self.assertIn("unused.grid_remove()", source)
        self.assertIn("grid_column, minsize=0, weight=0", source)

    def test_imc_uses_equal_width_columns(self):
        source = inspect.getsource(TimingGUI._stretch_tab_halves)
        self.assertIn('name in ("IMC", "RTL") and len(used) == 2', source)

    def test_only_tabs_that_fit_are_drawn_without_scrollbars(self):
        # They give up no width to gutters they do not use. Whether they fit is
        # measured against the drawn window in test_unscrolled_fit_live --
        # without a scrollbar, content past the bottom is not reachable.
        self.assertEqual(
            TimingGUI.UNSCROLLED_TABS,
            ("Summary", "System Info", "SPD", "Timings", "Training", "IMC", "RTL",
             "Voltages"),
        )

    def test_dense_intel_tables_use_three_columns(self):
        source = inspect.getsource(TimingGUI.create_widgets)
        self.assertIn('or training_has_middle', source)
        self.assertIn('column_keys = ("Left", "Middle", "Right")', source)
        self.assertIn('column_keys = ("Left", "Right")', source)
        self.assertNotIn("stacked =", source)

    def test_intel_timings_sections_balance_across_three_columns(self):
        layout = timings_three_column_layout({
            "Primary", "Secondary", "Command", "Refresh timings",
            "Tertiary", "CAS to CAS", "Power down", "Other Timings",
        })
        self.assertEqual(
            [layout[name] for name in (
                "Primary", "Secondary", "Command", "Refresh timings",
                "Tertiary", "CAS to CAS", "Power down", "Other Timings",
            )],
            ["Left", "Left", "Left", "Middle", "Middle",
             "Right", "Right", "Right"],
        )

    def test_am5_timings_sections_use_all_three_columns(self):
        layout = timings_three_column_layout({
            "Primary", "Secondary", "Refresh timings", "CAS to CAS",
            "Power down", "Stagger", "Mode register", "Turnaround",
            "Read to read", "Write to write", "PHY",
            "Preamble / postamble",
        })
        self.assertEqual(layout["Primary"], "Left")
        self.assertEqual(layout["Power down"], "Middle")
        self.assertEqual(layout["Refresh timings"], "Right")

        from rochviewer.ui.main import SKEW_SECTION_ORDER

        self.assertEqual(
            SKEW_SECTION_ORDER,
            ("RTT", "ODT", "RON", "ODT DELAY", "VREF",
             "DLL / LATENCY", "DATA CONTROL", "DFE", "ODTL", "Command",
             "MPR / ACCESS", "PARITY / CRC", "REFRESH / POWER",
             "PREAMBLE / PPR", "MR0 / MR1", "MR2 / MR3", "MR4",
             "MR5 / MR6", "Mode Registers", "DQS", "Preamble", "ECS"),
        )

    def test_training_keeps_its_right_column(self):
        source = inspect.getsource(TimingGUI.load_all_tabs_content)
        self.assertIn("available_columns = set(self.grid_frames[tab_name])", source)
        self.assertIn('timing.get("Column", "Left") != "Left"', source)

    def test_training_sections_share_value_alignment_per_column(self):
        source = inspect.getsource(TimingGUI.create_section)
        self.assertNotIn(
            'section_frame if tab_name == "Training" else parent', source
        )
        self.assertGreaterEqual(source.count('(tab_name, id(parent))'), 3)

    def test_imc_uses_the_balanced_three_column_section_order(self):
        from rochviewer.ui.main import IMC_SECTION_ORDER

        self.assertEqual(
            IMC_SECTION_ORDER,
            ("VREF", "Command", "Refresh", "Power Down",
             "DATA", "CMD", "CLK", "CTL", "SComp",
             "ODTL", "PHY Control", "Features",
             "MR0 / MR1", "MR2 / MR3", "MR4", "MR5 / MR6"),
        )

    def test_tab_height_grows_below_a_fixed_top_edge(self):
        resize = inspect.getsource(TimingGUI._resize_for_tab)
        self.assertIn("x = self.root.winfo_x()", resize)
        self.assertIn("y = self.root.winfo_y()", resize)
        self.assertNotIn("centre_x", resize)
        self.assertNotIn("centre_y", resize)

    def test_module_selector_is_built_for_summary_and_module_aware_tabs(self):
        source = inspect.getsource(TimingGUI.create_widgets)
        self.assertNotIn("bottom_part_number_frame", source)
        summary_branch = source[
            source.index('if name == "Summary"'):
            source.index("# Timings uses three columns")
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
        self.assertIn("_finish_tab_shading", source)
        self.assertIn("after_cancel", source)
        self.assertIn("after_idle", source)
        finish = inspect.getsource(TimingGUI._finish_tab_shading)
        self.assertIn("_extend_tab_shading_to_viewport", finish)
        extension = inspect.getsource(TimingGUI._extend_tab_shading_to_viewport)
        self.assertIn("_shading_viewports", extension)
        self.assertIn("self.tabview.tab(tab_name)", extension)
        self.assertIn("table_top", extension)
        self.assertIn("(bottom - table_top) / float(pitch)", extension)

    def test_system_info_columns_are_independent(self):
        # Four left sections face two right sections. Pairing them would insert
        # blank rows between Processor and Motherboard instead of stacking each
        # column naturally from its own top.
        self.assertNotIn("System Info", PAIRED_SECTION_TABS)

    def test_system_info_uses_two_side_by_side_columns(self):
        source = inspect.getsource(TimingGUI.create_widgets)
        self.assertNotIn("left_info_frame", source)
        self.assertIn('column_keys = ("Left", "Right")', source)
        self.assertEqual(TimingGUI.SYSTEM_INFO_TEXT_INSET, 0)
        self.assertIn("padx=0", source)

    def test_system_info_spacing_preserves_continuous_row_shading(self):
        source = inspect.getsource(TimingGUI.create_section)
        self.assertIn("SYSTEM_INFO_TEXT_INSET", source)
        self.assertIn('parent is system_info_frames.get("Right")', source)
        self.assertGreaterEqual(source.count("padx=name_padx"), 5)

    def test_system_info_section_names_are_shaded_rows(self):
        from rochviewer.ui.main import CONTINUOUS_SECTION_TABS

        self.assertIn("System Info", CONTINUOUS_SECTION_TABS)
        source = inspect.getsource(TimingGUI.create_section)
        self.assertIn(
            "self.VALUE_COLOR if uniform_header else self.SUBTITLE_COLOR",
            source,
        )

    def test_hidden_columns_are_grouped_by_widget_for_shading(self):
        source = inspect.getsource(TimingGUI._extend_column_shading)
        self.assertIn('section["body"].master.master', source)
        self.assertNotIn("winfo_rootx()", source)
        self.assertIn("tab_name in CONTINUOUS_SECTION_TABS", source)
        self.assertIn('sum(1 + section["drawn"]', source)
        self.assertIn("(delta + pitch - 1) // pitch", source)

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
