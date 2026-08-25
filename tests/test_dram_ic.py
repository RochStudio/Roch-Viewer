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
from dram_ic import identify_dram_ic

class DramIcTest(unittest.TestCase):
    def test_gskill_f5_6000_user_kit_is_hynix_a_die(self):
        self.assertEqual(identify_dram_ic("F5-6000J2636G16G"), "SK hynix A-die")
        self.assertEqual(identify_dram_ic("F5-6000J2636G16G", "G.Skill"), "SK hynix A-die")

    def test_unknown_stays_unknown(self):
        self.assertEqual(identify_dram_ic("COMPLETELY-FAKE-PART"), "Unknown IC")

if __name__ == "__main__":
    unittest.main()
