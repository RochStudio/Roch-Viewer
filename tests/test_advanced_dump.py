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

"""The Advanced tab's text dump.

Laid out to the same column as the reference tool's own dump so the two read
side by side. The formatter is pure, so the layout is checked here without a
window or a machine to read.
"""

import unittest

from rochviewer.ui.advanced_window import (
    DUMP_NAME,
    DUMP_SEPARATOR_COLUMN,
    format_dump,
)


def entry(name, value):
    return ("Timings", "Primary", name, lambda: value)


class DumpLayoutTest(unittest.TestCase):
    def test_the_separator_sits_where_the_reference_dump_puts_it(self):
        lines = format_dump([
            entry("tCL", "38"),
            entry("MOBO", "Micro-Star International Co., Ltd."),
        ]).splitlines()
        for line in lines:
            with self.subTest(line=line):
                self.assertEqual(line.find(" : "), DUMP_SEPARATOR_COLUMN)

    def test_a_long_name_pushes_its_own_separator(self):
        # Truncating would lose which register a row is, which is the one
        # thing a dump has to keep.
        name = "RXDQSCOMP_RXCODE_dllcomp_cmn_picoderxdqsnref"
        line = format_dump([entry(name, "0")]).splitlines()[0]
        self.assertIn(name, line)
        self.assertGreater(line.find(" : "), DUMP_SEPARATOR_COLUMN)

    def test_both_channels_are_written(self):
        line = format_dump([entry("tRCD", ("50", "50"))]).splitlines()[0]
        self.assertTrue(line.endswith("50 | 50"), line)

    def test_a_row_that_cannot_be_read_is_kept_as_na(self):
        # Dropping it would make the dump quietly incomplete, and a reader
        # comparing two dumps would see a row appear and disappear rather
        # than a value they could not read.
        def boom():
            raise RuntimeError("no driver")

        text = format_dump([("Misc", "Features", "Broken", boom),
                            entry("Blank", None),
                            entry("Empty", "")])
        for name in ("Broken", "Blank", "Empty"):
            with self.subTest(name=name):
                self.assertRegex(text, r"  %s +: N/A" % name)

    def test_every_row_is_written_once(self):
        rows = [entry("t%d" % i, str(i)) for i in range(40)]
        self.assertEqual(len(format_dump(rows).splitlines()), 40)

    def test_the_text_ends_with_a_newline(self):
        self.assertTrue(format_dump([entry("tCL", "38")]).endswith("\n"))

    def test_the_file_is_named_for_the_app(self):
        self.assertEqual(DUMP_NAME, "RochViewer.txt")


if __name__ == "__main__":
    unittest.main()
