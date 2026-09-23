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

"""Every tab either fits the compact window or provides scrolling.

This builds the real window and measures, because the height depends on drawn
row pitch and font metrics rather than on anything the table knows.

Skipped wherever a window cannot be opened, which is every headless machine.
"""

import unittest


def build():
    """The real window, or None when this machine cannot open one."""
    try:
        import customtkinter as ctk

        from rochviewer.ui import main
    except Exception:
        return None
    try:
        root = ctk.CTk()
    except Exception:
        return None
    try:
        app = main.TimingGUI(root)
        root.update_idletasks()
        root.update()
        return root, app
    except Exception:
        root.destroy()
        return None


class UnscrolledTabFitTest(unittest.TestCase):
    # Built once for the class. Two roots in one process is a second full
    # startup, hardware reads included, and CustomTkinter does not always
    # survive being torn down and stood up again inside one interpreter.
    root = None
    app = None

    @classmethod
    def setUpClass(cls):
        built = build()
        if built is not None:
            cls.root, cls.app = built

    @classmethod
    def tearDownClass(cls):
        if cls.root is not None:
            # Plain destroy. Cancelling the pending callbacks first looks
            # tidier and takes CustomTkinter's own scaling tracker with them,
            # after which destroy raises "can't delete Tcl command".
            try:
                cls.root.destroy()
            except Exception:
                pass
            cls.root = cls.app = None

    def setUp(self):
        if self.root is None:
            self.skipTest("no display to draw into")

    def show_tab_at_its_requested_size(self, name):
        """Draw a tab at the exact custom size this test is validating."""
        self.app.tabview.set(name)
        width, height = self.app.TAB_WINDOW_SIZES[name]
        self.root.geometry("%dx%d" % (width, height))
        self.root.update_idletasks()
        self.root.update()

    def test_every_unscrolled_tab_fits_the_window(self):
        for name in self.app.UNSCROLLED_TABS:
            if name not in self.app.tabview._name_list:
                continue
            with self.subTest(tab=name):
                self.show_tab_at_its_requested_size(name)
                holder = self.app.tab_frames[name]
                self.assertFalse(
                    hasattr(holder, "_parent_canvas"),
                    "%s is listed as unscrolled but drawn scrollable" % name)
                needed = holder.winfo_reqheight()
                available = holder.winfo_height()
                self.assertLessEqual(
                    needed, available,
                    "%s needs %dpx and has %dpx: %dpx of it is cut off with "
                    "no scrollbar to reach it. Raise the tab height or take "
                    "rows off the tab." % (name, needed, available,
                                           needed - available))

    def test_utility_buttons_align_with_the_tab_strip(self):
        tabs = self.app.tabview._segmented_button
        tools = self.app.appearance_toolbar
        self.assertLessEqual(
            abs(tools.winfo_rooty() - tabs.winfo_rooty()), 1,
            "Telemetry and Advanced are not aligned with the tabs",
        )
        self.assertGreaterEqual(
            tools.winfo_rootx(), tabs.winfo_rootx() + tabs.winfo_width(),
            "utility buttons overlap the main tabs",
        )

    def test_system_info_row_shading_matches_across_columns(self):
        self.show_tab_at_its_requested_size("System Info")
        self.app._normalize_continuous_tab_shading("System Info")
        rows = {}
        for section in self.app._section_bodies.get("System Info", []):
            body = section["body"]
            if not body.winfo_ismapped():
                continue
            for grid_row in range(section["first_row"], body.grid_size()[1]):
                bbox = body.grid_bbox(0, grid_row)
                if not bbox or not bbox[3]:
                    continue
                colours = {
                    str(child.cget("fg_color"))
                    for child in body.grid_slaves(row=grid_row)
                    if hasattr(child, "cget")
                }
                rows.setdefault(body.winfo_rooty() + bbox[1], set()).update(
                    colours
                )
        for y, colours in rows.items():
            self.assertEqual(
                len(colours), 1,
                "System Info row at y=%d has mismatched shading: %r"
                % (y, colours),
            )

    def test_training_values_align_within_each_detail_column(self):
        if "Training" not in self.app.tabview._name_list:
            self.skipTest("Training is not available for this profile")
        self.show_tab_at_its_requested_size("Training")
        groups = [
            frames for (tab_name, _parent_id), frames
            in self.app._dual_content_frames.items()
            if tab_name == "Training"
        ]
        self.assertEqual(
            len(groups), len(self.app.grid_frames["Training"]),
            "each Training column should own one alignment group",
        )
        for frames in groups:
            starts = []
            for frame in frames:
                for child in frame.grid_slaves():
                    if int(child.grid_info().get("column", -1)) != 1:
                        continue
                    try:
                        text = child.cget("text")
                    except Exception:
                        continue
                    if text:
                        starts.append(child.winfo_rootx())
            self.assertTrue(starts)
            self.assertEqual(
                len(set(starts)), 1,
                "Training values do not share one x-position in a column",
            )

    def test_imc_columns_have_equal_widths(self):
        if "IMC" not in self.app.tabview._name_list:
            self.skipTest("IMC is not available for this profile")
        self.show_tab_at_its_requested_size("IMC")
        widths = [
            frame.winfo_width()
            for frame in self.app.grid_frames["IMC"].values()
            if frame.winfo_manager()
        ]
        self.assertEqual(len(widths), 2)
        self.assertLessEqual(
            max(widths) - min(widths), 1,
            "IMC columns do not divide the viewport evenly",
        )

    def test_tab_height_changes_only_at_the_bottom_edge(self):
        self.root.geometry("750x750+100+20")
        self.root.update_idletasks()
        self.root.update()
        x, y = self.root.winfo_x(), self.root.winfo_y()

        self.app._resize_for_tab("Timings")
        self.root.update_idletasks()
        self.root.update()

        self.assertEqual((self.root.winfo_x(), self.root.winfo_y()), (x, y))
        expected_width, expected_height = self.app.window_size_for_tab(
            "Timings", self.root.winfo_screenheight()
        )
        self.assertEqual(self.root.winfo_width(), expected_width)
        self.assertEqual(self.root.winfo_height(), expected_height)

    def test_imc_has_no_scrollbar(self):
        if "IMC" not in self.app.tabview._name_list:
            self.skipTest("IMC is not available for this profile")
        holder = self.app.tab_frames["IMC"]
        self.assertFalse(
            hasattr(holder, "_parent_canvas"),
            "IMC should fit without a scrollbar at 750x1100",
        )

    def test_nothing_on_any_tab_is_clipped(self):
        clipped = []

        def walk(widget, tab):
            for child in widget.winfo_children():
                try:
                    text = child.cget("text")
                except Exception:
                    text = None
                if (text and child.winfo_manager() and child.winfo_ismapped()
                        and child.winfo_width() > 1
                        and child.winfo_reqwidth() > child.winfo_width()):
                    clipped.append("%s: %r needs %d has %d" % (
                        tab, str(text)[:24], child.winfo_reqwidth(),
                        child.winfo_width()))
                walk(child, tab)

        for name in self.app.tabview._name_list:
            self.show_tab_at_its_requested_size(name)
            walk(self.app.tabview.tab(name), name)
        self.assertEqual(clipped, [], "labels cut off at this window width")

    def test_summary_static_rows_are_not_on_the_live_refresh(self):
        # The Telemetry copy of DRAM Frequency shares the row's name, and the
        # Summary picked it up by name, so the Summary flickered between 8000
        # and 7998 with the measured BCLK.
        from rochviewer.ui.main import SUMMARY_STATIC_ROWS

        summary = self.app.tab_frames["Summary"]

        def on_summary(widget):
            while widget is not None:
                if widget is summary:
                    return True
                widget = widget.master
            return False

        live = [timing.get("name") for timing, label
                in self.app.live_value_labels if on_summary(label)]
        for name in SUMMARY_STATIC_ROWS:
            with self.subTest(name=name):
                self.assertNotIn(name, live)

    def test_every_detail_column_stays_inside_its_tab(self):
        overflow = []
        for name, frames in self.app.grid_frames.items():
            if not hasattr(frames, "values") or name == "Summary":
                continue
            holder = self.app.tab_frames.get(name)
            if holder is None:
                continue
            self.show_tab_at_its_requested_size(name)
            right_edge = holder.winfo_rootx() + holder.winfo_width()
            for key, frame in frames.items():
                if not hasattr(frame, "winfo_manager") or not frame.winfo_manager():
                    continue
                frame_edge = frame.winfo_rootx() + frame.winfo_width()
                if frame_edge > right_edge:
                    overflow.append((name, key, frame_edge - right_edge))
        self.assertEqual(overflow, [], "detail columns outside their viewport")


if __name__ == "__main__":
    unittest.main()
