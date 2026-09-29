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

import os
import struct
import sys
import tempfile
import unittest
import zlib
from datetime import datetime
from unittest import mock

from rochviewer.ui import screenshot


def decode_png(data):
    """The (width, height, RGB rows) of a PNG this module wrote."""
    assert data[:8] == b"\x89PNG\r\n\x1a\n"
    offset, chunks = 8, {}
    while offset < len(data):
        (length,) = struct.unpack(">I", data[offset:offset + 4])
        kind = data[offset + 4:offset + 8]
        body = data[offset + 8:offset + 8 + length]
        (crc,) = struct.unpack(">I", data[offset + 8 + length:
                                          offset + 12 + length])
        assert crc == zlib.crc32(kind + body) & 0xFFFFFFFF, kind
        chunks.setdefault(kind, b"")
        chunks[kind] += body
        offset += 12 + length
    width, height, depth, colour, _c, _f, _i = struct.unpack(
        ">IIBBBBB", chunks[b"IHDR"])
    assert (depth, colour) == (8, 2)
    raw = zlib.decompress(chunks[b"IDAT"])
    stride = width * 3 + 1
    rows = [raw[r * stride:(r + 1) * stride] for r in range(height)]
    assert all(row[0] == 0 for row in rows)
    return width, height, [row[1:] for row in rows]


class PngTest(unittest.TestCase):
    def test_pixels_survive_the_round_trip_as_rgb(self):
        # Two by two, BGRA: red, green / blue, white.
        bgra = bytes([0, 0, 255, 255, 0, 255, 0, 255,
                      255, 0, 0, 255, 255, 255, 255, 255])
        width, height, rows = decode_png(screenshot.encode_png(2, 2, bgra))
        self.assertEqual((width, height), (2, 2))
        self.assertEqual(rows[0], bytes([255, 0, 0, 0, 255, 0]))
        self.assertEqual(rows[1], bytes([0, 0, 255, 255, 255, 255]))

    def test_a_short_buffer_is_refused(self):
        with self.assertRaises(ValueError):
            screenshot.encode_png(2, 2, b"\x00" * 15)

    def test_the_clipboard_copy_is_bottom_up_behind_its_header(self):
        top, bottom = b"\x01" * 4, b"\x02" * 4
        dib = screenshot.dib_bytes(1, 2, top + bottom)
        header_size = struct.unpack("<I", dib[:4])[0]
        self.assertEqual(header_size, 40)
        # A positive height is what makes a DIB bottom-up.
        self.assertEqual(struct.unpack("<i", dib[8:12])[0], 2)
        self.assertEqual(dib[header_size:], bottom + top)

    def test_the_file_name_carries_the_tab_and_the_time(self):
        when = datetime(2026, 9, 28, 15, 30, 12)
        self.assertEqual(screenshot.screenshot_filename("System Info", when),
                         "RochViewer-SystemInfo-20260928-153012.png")
        self.assertEqual(screenshot.screenshot_filename("", when),
                         "RochViewer-Window-20260928-153012.png")

    def test_the_default_folder_is_named_for_the_app(self):
        self.assertEqual(os.path.basename(screenshot.default_folder()),
                         "Roch Viewer")


@unittest.skipUnless(sys.platform == "win32", "captures through GDI")
class LiveScreenshotTest(unittest.TestCase):
    def test_the_button_saves_the_whole_window(self):
        try:
            import customtkinter as ctk

            from rochviewer.ui import main
            root = ctk.CTk()
        except Exception:
            self.skipTest("no display to draw into")
        try:
            app = main.TimingGUI(root)
            root.geometry("775x775+40+40")
            for _ in range(10):
                root.update()
            with tempfile.TemporaryDirectory() as folder, \
                    mock.patch.object(screenshot, "default_folder",
                                      return_value=folder), \
                    mock.patch.object(screenshot, "copy_to_clipboard"):
                app._capture_screenshot()
                saved = os.listdir(folder)
                self.assertEqual(len(saved), 1)
                self.assertTrue(saved[0].startswith("RochViewer-Summary-"))
                with open(os.path.join(folder, saved[0]), "rb") as image:
                    width, height, rows = decode_png(image.read())
            self.assertEqual((width, height),
                             (root.winfo_width(), root.winfo_height()))
            # A real picture, not a blank bitmap: text, bands and borders
            # come to far more than a handful of colours.
            colours = {row[x:x + 3] for row in rows
                       for x in range(0, len(row), 3)}
            self.assertGreater(len(colours), 20)
            self.assertEqual(app.screenshot_status.cget("text"),
                             "Saved · copied")
        finally:
            root.destroy()


if __name__ == "__main__":
    unittest.main()
