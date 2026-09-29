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

import unittest
from unittest import mock

from rochviewer.ui.main import TimingGUI


class AppearanceDefaultTest(unittest.TestCase):
    def mode_with(self, settings):
        gui = TimingGUI.__new__(TimingGUI)
        with mock.patch.object(TimingGUI, "load_settings",
                               return_value=settings):
            return gui.load_appearance_mode()

    def test_a_first_run_opens_in_light_mode(self):
        self.assertEqual(self.mode_with({}), "Light")

    def test_a_saved_choice_still_wins(self):
        self.assertEqual(self.mode_with({"appearance_mode": "Dark"}), "Dark")
        self.assertEqual(self.mode_with({"appearance_mode": "light"}), "Light")

    def test_an_unreadable_setting_falls_back_to_light(self):
        self.assertEqual(self.mode_with({"appearance_mode": "Sepia"}), "Light")


if __name__ == "__main__":
    unittest.main()
