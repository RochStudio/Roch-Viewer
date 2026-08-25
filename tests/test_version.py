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

"""One version, written once and agreed everywhere it appears.

It appears in three places that cannot import each other: the module, the
Windows file-version resource the EXE is stamped with, and the changelog. A
release where those disagree is one where the binary cannot be identified
from its own properties.
"""

import io
import os
import re
import unittest

from rochviewer import version

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def read(name):
    return io.open(os.path.join(ROOT, name), encoding="utf-8").read()


class VersionTest(unittest.TestCase):
    def test_it_is_three_numbers(self):
        self.assertRegex(version.__version__, r"^\d+\.\d+\.\d+$")

    def test_the_windows_tuple_is_the_version_plus_a_build_field(self):
        expected = tuple(
            int(part) for part in version.__version__.split(".")
        ) + (0,)
        self.assertEqual(version.VERSION_TUPLE, expected)
        self.assertEqual(len(version.VERSION_TUPLE), 4)

    def test_the_exe_resource_carries_the_same_version(self):
        resource = read("file_version_info.txt")
        dotted = version.__version__ + ".0"
        for field in ("FileVersion", "ProductVersion"):
            with self.subTest(field=field):
                self.assertIn("StringStruct('%s', '%s')" % (field, dotted),
                              resource)
        numbers = "(%s)" % ", ".join(str(n) for n in version.VERSION_TUPLE)
        self.assertIn("filevers=%s" % numbers, resource)
        self.assertIn("prodvers=%s" % numbers, resource)

    def test_the_resource_names_the_product(self):
        resource = read("file_version_info.txt")
        self.assertIn("StringStruct('ProductName', '%s')" % version.APP_NAME,
                      resource)

    def test_the_spec_stamps_the_resource_onto_the_exe(self):
        # Without this the EXE builds fine and carries no version at all.
        self.assertIn("version='file_version_info.txt'", read("RochViewer.spec"))

    def test_the_changelog_leads_with_this_version(self):
        first = re.search(r"^## (\S+)", read("CHANGELOG.md"), re.MULTILINE)
        self.assertIsNotNone(first, "no version heading in CHANGELOG.md")
        self.assertEqual(first.group(1), version.__version__)

    def test_the_readme_names_this_version(self):
        self.assertIn("# %s %s" % (version.APP_NAME, version.__version__),
                      read("README.md"))


if __name__ == "__main__":
    unittest.main()
