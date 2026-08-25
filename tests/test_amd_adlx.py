"""ADLX reads a limit atiadlxx has no answer for, and fails closed without it."""

from __future__ import annotations

import ctypes
import unittest
from unittest import mock

from rochviewer.amd import adlx as amd_adlx


class LimitTextTest(unittest.TestCase):
    def setUp(self):
        amd_adlx._CACHE[:] = []

    def test_whole_watts(self):
        self.assertEqual(amd_adlx.limit_text(363), "363 W")

    def test_an_unreadable_limit_says_nothing(self):
        # An em dash rather than a zero: zero is a claim about the card, and
        # a library that would not answer has not made one.
        self.assertEqual(amd_adlx.limit_text(None), "—")

    def test_not_supplied_reads_rather_than_reporting_nothing(self):
        # The sentinel is the whole point: None means unreadable, and a
        # missing argument means go and look.
        with mock.patch.object(amd_adlx, "cached_limit", return_value=363):
            self.assertEqual(amd_adlx.limit_text(), "363 W")


class CacheTest(unittest.TestCase):
    def setUp(self):
        amd_adlx._CACHE[:] = []

    def test_the_limit_is_not_re_read_every_tick(self):
        with mock.patch.object(amd_adlx, "board_power_limit",
                               return_value=363) as read:
            self.assertEqual(amd_adlx.cached_limit(now=100.0), 363)
            self.assertEqual(amd_adlx.cached_limit(now=110.0), 363)
            self.assertEqual(read.call_count, 1)

    def test_a_stale_limit_is_re_read(self):
        with mock.patch.object(amd_adlx, "board_power_limit",
                               side_effect=[363, 300]) as read:
            self.assertEqual(amd_adlx.cached_limit(now=100.0), 363)
            later = 100.0 + amd_adlx.CACHE_SECONDS + 1
            self.assertEqual(amd_adlx.cached_limit(now=later), 300)
            self.assertEqual(read.call_count, 2)

    def test_an_unreadable_limit_is_cached_too(self):
        # Otherwise a machine without the library pays a failed load every
        # tick, which is the cost the cache exists to avoid.
        with mock.patch.object(amd_adlx, "board_power_limit",
                               return_value=None) as read:
            self.assertIsNone(amd_adlx.cached_limit(now=100.0))
            self.assertIsNone(amd_adlx.cached_limit(now=101.0))
            self.assertEqual(read.call_count, 1)


class NoLibraryTest(unittest.TestCase):
    def test_a_machine_without_adlx_reports_no_limit(self):
        with mock.patch.object(amd_adlx.ctypes, "CDLL",
                               side_effect=OSError("not found")):
            self.assertIsNone(amd_adlx.board_power_limit())


class SlotConstantsTest(unittest.TestCase):
    """The slots are the part that cannot be wrong quietly.

    A wrong index calls whatever function sits at it, so these are pinned to
    what the SDK headers declare rather than left to drift.
    """

    def test_release_is_the_second_slot_of_every_counted_interface(self):
        self.assertEqual(amd_adlx.RELEASE, 1)

    def test_system_slots_skip_no_reference_counting(self):
        # IADLXSystem is the one interface that is not reference-counted, so
        # its methods start at slot 0 rather than after Acquire/Release/QI.
        self.assertEqual(amd_adlx.SYSTEM_GET_GPUS, 1)
        self.assertEqual(amd_adlx.SYSTEM_GET_PERF_SERVICES, 9)

    def test_the_counted_interfaces_start_after_the_first_three(self):
        for slot in (amd_adlx.LIST_SIZE, amd_adlx.LIST_BEGIN,
                     amd_adlx.LIST_AT_GPULIST,
                     amd_adlx.PERF_GET_SUPPORTED_GPU_METRICS,
                     amd_adlx.SUPPORT_TOTAL_BOARD_POWER_RANGE):
            self.assertGreaterEqual(slot, 3)

    def test_the_board_power_range_slot_is_where_the_header_puts_it(self):
        self.assertEqual(amd_adlx.SUPPORT_TOTAL_BOARD_POWER_RANGE, 22)


class ReadOnlyTest(unittest.TestCase):
    def test_no_tuning_slot_is_defined(self):
        # GetGPUTuningServices is slot 8 of IADLXSystem and is the door to
        # changing a limit rather than reporting it. Not naming the slot is
        # what keeps that door shut: the module cannot call what it has no
        # constant for.
        named = [name for name in dir(amd_adlx) if "TUNING" in name.upper()]
        self.assertEqual(named, [])

    def test_the_only_system_slots_named_are_the_two_readers(self):
        slots = {name: getattr(amd_adlx, name) for name in dir(amd_adlx)
                 if name.startswith("SYSTEM_")}
        self.assertEqual(sorted(slots), ["SYSTEM_GET_GPUS",
                                         "SYSTEM_GET_PERF_SERVICES"])


if __name__ == "__main__":
    unittest.main()
