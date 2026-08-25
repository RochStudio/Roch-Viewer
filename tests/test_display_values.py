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

from rochviewer.ui.display_values import resolve_display_value, select_tab_names


class DisplayValueTest(unittest.TestCase):
    def test_calls_lazy_value(self):
        self.assertEqual(resolve_display_value(lambda: 36), "36")

    def test_static_value_is_preserved(self):
        self.assertEqual(resolve_display_value("AMD SMN READ-ONLY"), "AMD SMN READ-ONLY")

    def test_failure_is_neutral(self):
        self.assertEqual(resolve_display_value(lambda: 1 / 0), "—")

    def test_am5_profile_hides_empty_intel_tabs(self):
        rows = [
            {"Tab": "System Info"},
            {"Tab": "Timings"},
        ]
        self.assertEqual(
            select_tab_names(rows), ["Summary", "System Info", "Timings"]
        )


if __name__ == "__main__":
    unittest.main()
