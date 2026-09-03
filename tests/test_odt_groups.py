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

"""The CA/CS/CK ODT ladder has no gap in it.

RZQ divided by 0.5, 1, 2, 3, 4, 5 and 6. Code 6 is the 48 ohm rung and the
table carried "RFU" there, so all three Group B rows -- which sit on that code
on this bench -- reported a reserved setting where a real termination was
programmed. Group A was unaffected only because none of its codes lands on 6.
"""

import unittest

from tests.intel_stub import install, restore

intel_timings = None

GROUP_ROWS = ("CA ODT GROUP A", "CS ODT GROUP A", "CK ODT GROUP A",
              "CA ODT GROUP B", "CS ODT GROUP B", "CK ODT GROUP B")
TABLES = ("CA_ODT_FORMULA", "CS_ODT_FORMULA", "CK_ODT_FORMULA")

# What each code means, as a resistance in ohms. RZQ is 240.
LADDER = {0: None, 1: 480, 2: 240, 3: 120, 4: 80, 5: 60, 6: 48, 7: 40}


def setUpModule():
    global intel_timings
    intel_timings = install()


def tearDownModule():
    restore()


def rows():
    return [t for t in intel_timings.TIMINGS if t.get("name") in GROUP_ROWS]


class OdtTableTest(unittest.TestCase):
    def test_every_table_is_the_three_bit_field_and_no_more(self):
        for name in TABLES:
            with self.subTest(table=name):
                self.assertEqual(sorted(getattr(intel_timings, name)),
                                 list(range(8)))

    def test_the_ladder_has_no_gap(self):
        # The defect: 480, 240, 120, 80, 60, RFU, 40 -- every rung present but
        # the 48. A reserved code sitting between two defined ones is the shape
        # of a missing entry, not of a reserved code.
        for name in TABLES:
            table = getattr(intel_timings, name)
            for code, ohms in LADDER.items():
                with self.subTest(table=name, code=code):
                    if ohms is None:
                        self.assertEqual(table[code], "RTT_OFF")
                    else:
                        self.assertIn("(%d)" % ohms, table[code])
                        self.assertNotIn("RFU", table[code])

    def test_the_ladder_descends(self):
        # Each code divides RZQ by a larger number, so the resistance falls.
        # A transposed pair would still pass the test above.
        ohms = [LADDER[c] for c in range(1, 8)]
        self.assertEqual(ohms, sorted(ohms, reverse=True))


class OdtRowTest(unittest.TestCase):
    def setUp(self):
        # DDR5 rows. A DDR4 table does not build them at all, so there is
        # nothing to assert rather than something wrong.
        if not rows():
            self.skipTest("ODT group rows are DDR5 only")

    def test_all_six_rows_are_present(self):
        found = {t.get("name") for t in rows()}
        self.assertEqual(found, set(GROUP_ROWS))

    def test_every_side_reads_three_bits(self):
        # Four bits reaches a neighbouring field, whose bit made Group B's
        # code 6 look like a 14 and sent the first diagnosis the wrong way.
        for row in rows():
            for side in ("a", "b"):
                params = row.get("dynamic_params_%s" % side)
                with self.subTest(row=row.get("name"), side=side):
                    self.assertIsNotNone(params)
                    self.assertEqual(params.get("bit_length_dynamic"), 3)
                    self.assertEqual(params.get("bit_start_dynamic"), 0)

    def test_the_two_groups_look_up_different_entries(self):
        # Group A and Group B differ only by which table entry they follow, so
        # a copied key would silently make one group report the other's value.
        keys = {}
        for row in rows():
            keys[row["name"]] = row["dynamic_params_a"]["value_to_find"]
        self.assertEqual(len(set(keys.values())), len(GROUP_ROWS), keys)


if __name__ == "__main__":
    unittest.main()
