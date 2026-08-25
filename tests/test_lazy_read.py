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

import importlib
import sys
import unittest


class LazyReadImportTest(unittest.TestCase):
    def test_import_does_not_load_hardware_driver_module(self):
        sys.modules.pop("read", None)
        sys.modules.pop("lazy_read", None)
        importlib.import_module("lazy_read")
        self.assertNotIn("read", sys.modules)


if __name__ == "__main__":
    unittest.main()
