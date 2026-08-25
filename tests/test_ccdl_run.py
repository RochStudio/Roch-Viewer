"""The tCCD_L run, found by marker rather than by trusting the registers.

ZenTimings moved all three tCCD_L readings out of the UMC registers because
some boards never program 0x50198 from the BIOS setting at all -- so a
register that disagrees with what was asked for is the firmware's doing
rather than a misread, and no amount of re-reading it helps.

The marker the run is found from is not something a BIOS can change, which is
the whole point: the run is located without needing the registers to be
right. On this bench the marker appears twice, once per channel, and both
runs read (21, 83, 42) -- the same values the registers give here, which is
what says adopting it changes nothing where the registers are already
correct.
"""

from __future__ import annotations

import struct
import unittest

from rochviewer.amd.apob import (
    CCDL_RUN_MARKER,
    CCDL_RUN_OFFSET,
    find_ccdl_run,
)

# The values this bench reads, from both the registers and both APOB copies.
BENCH_RUN = (21, 83, 42)


def _table(run=BENCH_RUN, copies=2, marker=None):
    """A synthetic APOB carrying ``copies`` marked runs, with padding around."""
    marker = CCDL_RUN_MARKER if marker is None else marker
    padding = bytes(64)
    table = b""
    for _ in range(copies):
        chunk = bytearray(b"\x11" * (CCDL_RUN_OFFSET * 2 + 8))
        struct.pack_into("<2H", chunk, 0, *marker)
        struct.pack_into("<3H", chunk, CCDL_RUN_OFFSET * 2, *run)
        table += padding + bytes(chunk)
    return table + padding


class FindRunTest(unittest.TestCase):
    def test_the_bench_run_is_found(self):
        self.assertEqual(find_ccdl_run(_table()), BENCH_RUN)

    def test_one_copy_is_enough(self):
        self.assertEqual(find_ccdl_run(_table(copies=1)), BENCH_RUN)

    def test_channels_that_disagree_are_refused(self):
        # The marker sits once per channel. Two different runs means it
        # matched somewhere it does not belong, and picking one of them is
        # worse than saying nothing.
        table = bytearray(_table())
        second = table.rindex(struct.pack("<2H", *CCDL_RUN_MARKER))
        struct.pack_into("<3H", table, second + CCDL_RUN_OFFSET * 2,
                         22, 84, 43)
        self.assertIsNone(find_ccdl_run(bytes(table)))

    def test_a_table_without_the_marker_says_nothing(self):
        self.assertIsNone(find_ccdl_run(bytes(512)))
        self.assertIsNone(find_ccdl_run(b""))
        self.assertIsNone(find_ccdl_run(None))

    def test_a_run_running_past_the_table_is_refused(self):
        # The marker can legitimately be the last thing in the blob.
        table = struct.pack("<2H", *CCDL_RUN_MARKER)
        self.assertIsNone(find_ccdl_run(table))


class PlausibilityTest(unittest.TestCase):
    """The encoding was derived empirically and may not hold on every AGESA."""

    def test_a_tccdl_outside_its_range_is_dropped(self):
        self.assertIsNone(find_ccdl_run(_table(run=(2, 83, 42))))
        self.assertIsNone(find_ccdl_run(_table(run=(99, 83, 42))))

    def test_a_wr2_outside_its_range_is_dropped(self):
        self.assertIsNone(find_ccdl_run(_table(run=(21, 83, 900))))

    def test_wr_below_tccdl_or_above_four_times_it_is_dropped(self):
        self.assertIsNone(find_ccdl_run(_table(run=(21, 20, 42))))
        self.assertIsNone(find_ccdl_run(_table(run=(21, 200, 42))))

    def test_the_boundaries_themselves_are_accepted(self):
        # tCCD_L_WR exactly equal to tCCD_L, and exactly four times it.
        self.assertEqual(find_ccdl_run(_table(run=(21, 21, 42))),
                         (21, 21, 42))
        self.assertEqual(find_ccdl_run(_table(run=(21, 84, 42))),
                         (21, 84, 42))


class RegisterFallbackTest(unittest.TestCase):
    """Where the marker fails, the old register anchor still answers."""

    def test_the_runtime_prefers_the_run_and_falls_back(self):
        from rochviewer.amd.profile import Am5Runtime

        runtime = Am5Runtime()
        runtime._ccdl_run_attempted = True
        runtime._ccdl_run = BENCH_RUN
        self.assertEqual(runtime.ccdl_value("tCCD_L"), 21)
        self.assertEqual(runtime.ccdl_value("tCCD_L_WR"), 83)
        self.assertEqual(runtime.ccdl_value("tCCD_L_WR2"), 42)

    def test_without_a_run_the_registers_answer_the_two_they_hold(self):
        from rochviewer.amd.profile import Am5Runtime

        runtime = Am5Runtime()
        runtime._ccdl_run_attempted = True
        runtime._ccdl_run = None
        runtime._decoded = {"tCCD_L": 20, "tCCD_L_WR2": 40}
        self.assertEqual(runtime.ccdl_value("tCCD_L"), 20)
        self.assertEqual(runtime.ccdl_value("tCCD_L_WR2"), 40)
        # The one no register holds stays unanswered rather than inventing.
        self.assertIsNone(runtime.ccdl_value("tCCD_L_WR"))


if __name__ == "__main__":
    unittest.main()
