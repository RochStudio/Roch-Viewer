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

    def test_every_unscrolled_tab_fits_the_window(self):
        for name in self.app.UNSCROLLED_TABS:
            if name not in self.app.tabview._name_list:
                continue
            with self.subTest(tab=name):
                self.app.tabview.set(name)
                self.root.update_idletasks()
                self.root.update()
                holder = self.app.tab_frames[name]
                self.assertFalse(
                    hasattr(holder, "_parent_canvas"),
                    "%s is listed as unscrolled but drawn scrollable" % name)
                needed = holder.winfo_reqheight()
                available = holder.winfo_height()
                self.assertLessEqual(
                    needed, available,
                    "%s needs %dpx and has %dpx: %dpx of it is cut off with "
                    "no scrollbar to reach it. Raise WINDOW_HEIGHT or take "
                    "rows off the tab." % (name, needed, available,
                                           needed - available))

    def test_utility_buttons_align_with_the_tab_strip(self):
        tabs = self.app.tabview._segmented_button
        tools = self.app.appearance_toolbar
        self.assertLessEqual(
            abs(tools.winfo_rooty() - tabs.winfo_rooty()), 1,
            "Telemetry, Advanced and Light are not aligned with the tabs",
        )
        self.assertGreaterEqual(
            tools.winfo_rootx(), tabs.winfo_rootx() + tabs.winfo_width(),
            "utility buttons overlap the main tabs",
        )

    def test_long_tabs_scroll_at_the_compact_window_height(self):
        for name in ("System Info", "Timings", "Training"):
            if name not in self.app.tabview._name_list:
                continue
            with self.subTest(tab=name):
                holder = self.app.tab_frames[name]
                self.assertTrue(
                    hasattr(holder, "_parent_canvas"),
                    "%s must remain reachable at 750x800" % name,
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
            self.app.tabview.set(name)
            self.root.update_idletasks()
            self.root.update()
            walk(self.app.tabview.tab(name), name)
        self.assertEqual(clipped, [], "labels cut off at this window width")

    def test_every_detail_column_stays_inside_its_tab(self):
        overflow = []
        for name, frames in self.app.grid_frames.items():
            if not hasattr(frames, "values") or name == "Summary":
                continue
            holder = self.app.tab_frames.get(name)
            if holder is None:
                continue
            self.app.tabview.set(name)
            self.root.update_idletasks()
            self.root.update()
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
