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


"""The window has to say why every reading is empty.

The driver is not distributed with this project, so a first run has none and
cannot read a register. What it can still read is WMI -- the CPU, the board,
the BIOS, the module identity -- so the window comes up looking healthy while
every register row shows N/A and two show "Error". Nothing on screen said
why, which leaves a user two honest conclusions, "my hardware is not
supported" and "this is broken", neither of them true.

read.py had already worked out the reason and left it in DRIVER_ERROR "for
anything that wants to explain itself". Nothing did, and it shipped that way
in the 1.0.0 asset.

The text is checked here rather than the widget: CI has no display.
"""

import unittest

from rochviewer.hardware.driver_path import missing_message
from rochviewer.ui.main import TimingGUI


class DriverNoticeTest(unittest.TestCase):
    def notice(self, problem, missing=True):
        return TimingGUI.driver_notice_text(problem, missing)

    def test_a_working_driver_gets_no_notice(self):
        self.assertIsNone(self.notice(None))
        self.assertIsNone(self.notice(""))

    def test_the_real_missing_message_keeps_its_filename(self):
        # The bug this guards: the first sentence ends in "inpoutx64.dll", so
        # splitting on "." cut the headline down to "inpoutx64".
        text = self.notice(missing_message())
        self.assertIn("inpoutx64.dll", text)

    def test_it_says_the_readings_are_the_thing_affected(self):
        # A notice that names a missing DLL without connecting it to the
        # empty rows leaves the two looking unrelated.
        text = self.notice(missing_message())
        self.assertIn("readings below", text)

    def test_a_missing_driver_is_told_where_to_put_it(self):
        self.assertIn("beside this program", self.notice(missing_message()))

    def test_a_driver_that_will_not_load_gets_different_advice(self):
        # It is already beside the program, so telling them to put it there
        # would be the one instruction that cannot help.
        text = self.notice(
            "Found C:/app/inpoutx64.dll but could not load it. It may be the "
            "wrong architecture, or the process may not be running as "
            "administrator (126).", missing=False)
        self.assertNotIn("beside this program", text)
        self.assertIn("administrator", text)

    def test_the_directory_list_is_left_out(self):
        # missing_message names every directory searched, over several lines.
        # A one-line strip cannot hold them and should not try.
        text = self.notice(missing_message())
        self.assertNotIn("\n", text)

    def test_it_is_one_line_of_reasonable_length(self):
        text = self.notice(missing_message())
        self.assertLess(len(text), 200, text)


if __name__ == "__main__":
    unittest.main()
