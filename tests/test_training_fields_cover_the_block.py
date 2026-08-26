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

"""Every field the APOB decodes has to be routed to the APOB.

Am5Runtime.value sends a name to the training block when TRAINING_FIELDS
lists it and to the UMC register decode otherwise. The two are different
sources, and a name the block owns that is missing from the list goes to the
registers, finds nothing there, and comes back as an em dash.

proc_dq_ds did exactly that. It was decoded correctly the whole time -- the
block held "34.3 Omega", matching ZenTimings -- while every one of its
siblings was listed and it was not. Nothing looked wrong on screen, because
both tabs that show it read the block per channel and never went through
value() at all. Only a caller joining on the timings table would have seen
the blank, and there was no test over any of it.

Comparing the two sets is the guard: the decoder's own output decides what
the list has to contain, so adding a field to the APOB cannot silently fail
to reach the row that shows it.
"""

from __future__ import annotations

import unittest

from rochviewer.amd import apob
from rochviewer.amd.profile import TRAINING_FIELDS

# The block is 0x30 bytes on Granite Ridge. Its content does not matter here:
# the decoder returns the same keys whatever the bytes say, and this test is
# about the keys.
EMPTY_BLOCK = bytes(0x30)


def _decoded_field_names():
    return set(apob.decode_granite_ridge_training_block(EMPTY_BLOCK))


class TrainingFieldsCoverTheBlockTest(unittest.TestCase):
    def test_every_decoded_field_is_routed_to_the_block(self):
        missing = sorted(_decoded_field_names() - set(TRAINING_FIELDS))
        self.assertEqual(
            missing, [],
            "the APOB decodes %s, but TRAINING_FIELDS does not list %s, so "
            "value() sends %s to the UMC register decode instead -- which "
            "has no such field, and answers with an em dash."
            % (", ".join(sorted(_decoded_field_names())),
               ", ".join(missing),
               "it" if len(missing) == 1 else "them"))

    def test_the_list_names_nothing_the_block_does_not_hold(self):
        # The other direction: a name listed here but never decoded is routed
        # away from the registers to a block that cannot answer either, which
        # is the same blank arrived at from the opposite side.
        stray = sorted(set(TRAINING_FIELDS) - _decoded_field_names())
        self.assertEqual(
            stray, [],
            "TRAINING_FIELDS lists %s, which the APOB decoder never produces."
            % ", ".join(stray))

    def test_proc_dq_ds_is_among_them(self):
        # Named on its own because it is the one that was missing, and a set
        # comparison that happened to be rewritten would stop covering it.
        self.assertIn("proc_dq_ds", TRAINING_FIELDS)
        self.assertIn("proc_dq_ds", _decoded_field_names())


if __name__ == "__main__":
    unittest.main()
