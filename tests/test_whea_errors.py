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

import whea_errors


class QueryTest(unittest.TestCase):
    """The XPath asks for WHEA's own events, and only recent ones."""

    def test_it_selects_the_whea_provider(self):
        self.assertIn("Microsoft-Windows-WHEA-Logger", whea_errors._query())

    def test_without_a_boot_time_it_asks_for_everything(self):
        self.assertNotIn("timediff", whea_errors._query(None))

    def test_with_a_boot_time_it_asks_only_since_then(self):
        # A corrected error from three BIOS versions ago says nothing about
        # the settings in front of you.
        import time

        query = whea_errors._query(time.time() - 60)
        self.assertIn("timediff", query)


class ErrorTextTest(unittest.TestCase):
    def test_a_count_reads_as_a_number(self):
        self.assertEqual(whea_errors.error_text(0), "0")
        self.assertEqual(whea_errors.error_text(3), "3")

    def test_an_unreadable_log_is_not_zero(self):
        # Zero is a claim that the machine is clean. A log that would not
        # open has not said that.
        self.assertEqual(whea_errors.error_text(None), "\u2014")


class CacheTest(unittest.TestCase):
    def setUp(self):
        whea_errors._CACHE[:] = []

    def test_the_log_is_not_walked_once_a_second(self):
        with mock.patch.object(whea_errors, "count_errors",
                               return_value=0) as counted:
            whea_errors.cached_count(now=100.0)
            whea_errors.cached_count(now=101.0)
            whea_errors.cached_count(now=102.0)
        self.assertEqual(counted.call_count, 1)

    def test_it_is_re_read_once_the_cache_is_old(self):
        with mock.patch.object(whea_errors, "count_errors",
                               return_value=0) as counted:
            whea_errors.cached_count(now=100.0)
            whea_errors.cached_count(now=100.0 + whea_errors.CACHE_SECONDS + 1)
        self.assertEqual(counted.call_count, 2)

    def test_a_failed_read_is_cached_as_a_failure_not_a_zero(self):
        with mock.patch.object(whea_errors, "count_errors", return_value=None):
            self.assertIsNone(whea_errors.cached_count(now=100.0))


if __name__ == "__main__":
    unittest.main()
