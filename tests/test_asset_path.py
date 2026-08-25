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


"""Where the application icon is looked for.

This is not cosmetic-only. The icon feeds the window and taskbar icon, and
the EXE's own file icon is baked in by PyInstaller at build time -- so when
the lookup fails, Explorer still shows the right icon and only the running
window is wrong, which is a bug that hides from the obvious way of checking.

The lookup used to be "beside my own module", which was the project root
until the modules moved into packages. Nothing failed, nothing logged: the
icon simply stopped being found. The first test below is the one that would
have caught it.
"""

import os
import sys
import unittest
from unittest import mock

from rochviewer.ui import asset_path


class TheRealIconTest(unittest.TestCase):
    def test_the_icon_shipped_in_this_tree_is_found(self):
        # Wherever icon.ico is moved to, this has to keep passing. It is the
        # check the package reorganisation needed and did not have.
        self.assertIsNotNone(
            asset_path.find_icon(),
            "icon.ico is not in any searched directory: %s"
            % "\n  ".join(asset_path.search_directories()),
        )

    def test_it_is_the_icon_and_not_just_some_file(self):
        path = asset_path.find_icon()
        if path is None:
            self.skipTest("no icon in this tree")
        with open(path, "rb") as handle:
            header = handle.read(4)
        # An .ico starts with reserved 0, type 1.
        self.assertEqual(header[:4], b"\x00\x00\x01\x00")


class SearchOrderTest(unittest.TestCase):
    def test_it_walks_up_to_the_project_root(self):
        # The asset sits at the root while the module sits two levels down.
        with mock.patch.object(sys, "frozen", False, create=True):
            directories = asset_path.search_directories()
        here = os.path.dirname(os.path.realpath(asset_path.__file__))
        root = os.path.dirname(os.path.dirname(here))
        self.assertIn(here, directories)
        self.assertIn(root, directories)

    def test_frozen_it_looks_beside_the_executable_first(self):
        with mock.patch.object(sys, "frozen", True, create=True), \
                mock.patch.object(sys, "executable", r"C:\apps\RochViewer.exe"):
            directories = asset_path.search_directories()
        self.assertEqual(directories[0], r"C:\apps")

    def test_frozen_the_unpacked_bundle_is_searched(self):
        # Where the spec's ('icon.ico', '.') entry lands.
        with mock.patch.object(sys, "frozen", True, create=True), \
                mock.patch.object(sys, "executable", r"C:\apps\RochViewer.exe"), \
                mock.patch.object(sys, "_MEIPASS", r"C:\temp\_MEI1234",
                                  create=True):
            directories = asset_path.search_directories()
        self.assertIn(r"C:\temp\_MEI1234", directories)

    def test_no_directory_is_searched_twice(self):
        directories = asset_path.search_directories()
        self.assertEqual(len(directories), len(set(directories)))

    def test_a_missing_asset_is_none_not_a_raise(self):
        self.assertIsNone(asset_path.find_asset("no-such-asset.bin"))


if __name__ == "__main__":
    unittest.main()
