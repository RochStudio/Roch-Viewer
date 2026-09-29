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

"""The X870 AORUS TACHYON ICE's supplies, fans and slot probe."""

import unittest
from unittest.mock import Mock, patch

from rochviewer.sensors import am5_board_rails as board

TACHYON = ("GIGABYTE TECHNOLOGY CO., LTD.", "X870 AORUS TACHYON ICE")

# The bench's IT8696E, as read beside a reference tool: raw counts per register.
BENCH_REGISTERS = {
    0x20: 102, 0x21: 167, 0x22: 168, 0x23: 161, 0x27: 139, 0x28: 127,
    0x2F: 128, 0x2A: 37, 0x2C: 34,
    0x0D: 225, 0x18: 1,          # CPU Fan: 481 counts
    0x82: 185, 0x83: 0,          # CPU_OPT: 185 counts
    0x0E: 0xFF, 0x19: 0xFF, 0x0F: 0xFF, 0x1A: 0xFF,
    0x80: 0xFF, 0x81: 0xFF, 0x84: 0xFF, 0x85: 0xFF,
}


def bench_reader(registers=BENCH_REGISTERS):
    reader = Mock()
    reader.read_registers.side_effect = lambda chip, wanted: {
        register: registers.get(register) for register in wanted}
    reader.voltage_step.return_value = 0.012
    return reader


class FanRpmTest(unittest.TestCase):
    def test_counts_decode_to_rpm(self):
        self.assertEqual(board.fan_rpm(225, 1), 1403)
        self.assertEqual(board.fan_rpm(185, 0), 3649)

    def test_an_empty_header_has_no_rpm(self):
        for low, high in ((0xFF, 0xFF), (0, 0), (None, 1), (1, None)):
            with self.subTest(low=low, high=high):
                self.assertIsNone(board.fan_rpm(low, high))


class BoardMonitorTest(unittest.TestCase):
    def test_the_bench_decodes_to_the_reference_readings(self):
        found = board.read_board_monitor(TACHYON, bench_reader())
        self.assertAlmostEqual(found["plus12v"], 12.096)
        self.assertAlmostEqual(found["plus3v3"], 3.3066)
        self.assertAlmostEqual(found["plus5v"], 4.830)
        self.assertAlmostEqual(found["v3vsb"], 3.336)
        self.assertAlmostEqual(found["vbat"], 3.048)
        self.assertAlmostEqual(found["avcc3"], 3.072)
        self.assertAlmostEqual(found["board_vcore"], 1.224)
        self.assertEqual((found["cpu_fan"], found["cpu_opt"]), (1403, 3649))
        self.assertEqual((found["pch"], found["pciex16"]), (37.0, 34.0))

    def test_empty_headers_are_left_out(self):
        found = board.read_board_monitor(TACHYON, bench_reader())
        for key in ("sys_fan1", "sys_fan2", "sys_fan3", "fan6"):
            with self.subTest(key=key):
                self.assertNotIn(key, found)

    def test_a_reading_out_of_band_is_dropped(self):
        registers = dict(BENCH_REGISTERS)
        registers[0x22] = 0          # +12V reading zero is no reading
        found = board.read_board_monitor(TACHYON, bench_reader(registers))
        self.assertNotIn("plus12v", found)
        self.assertIn("plus5v", found)

    def test_another_board_reads_nothing(self):
        reader = bench_reader()
        other = ("GIGABYTE TECHNOLOGY CO., LTD.", "X870E AORUS MASTER")
        self.assertEqual(board.read_board_monitor(other, reader), {})
        reader.read_registers.assert_not_called()

    def test_the_whole_block_is_one_read(self):
        reader = bench_reader()
        board.read_board_monitor(TACHYON, reader)
        self.assertEqual(reader.read_registers.call_count, 1)

    def test_a_busy_bus_reads_nothing_rather_than_zeros(self):
        reader = bench_reader()
        reader.read_registers.side_effect = None
        reader.read_registers.return_value = {}
        self.assertEqual(board.read_board_monitor(TACHYON, reader), {})

    def test_the_two_intel_named_channels_are_not_claimed(self):
        registers = {register for _key, _label, register, _div, _band
                     in board.BOARD_VOLTAGES}
        self.assertNotIn(0x24, registers)
        self.assertNotIn(0x25, registers)


class HiddenWhenBlankTest(unittest.TestCase):
    def rows(self, tab):
        return [
            {"Tab": tab, "Category": "Fans", "name": "CPU Fan",
             "hide_when_blank": True},
            {"Tab": tab, "Category": "Fans", "name": "System Fan 1",
             "display_name": "Sys Fan 1", "hide_when_blank": True},
            # Not marked: a blank row stays, as it always has.
            {"Tab": tab, "Category": "Voltages", "name": "VTT"},
        ]

    def test_only_marked_rows_wait_for_a_reading(self):
        from rochviewer.ui import main
        from rochviewer.ui.main import TimingGUI

        tab = next(iter(main.WINDOWED_TABS))
        with patch.object(main, "TIMINGS", self.rows(tab)):
            keys = TimingGUI.sensor_reveal_keys()
        self.assertEqual(keys, {("sensor", "Fans", "CPU Fan"),
                                ("sensor", "Fans", "Sys Fan 1")})

    def test_every_row_is_still_offered_to_the_window(self):
        # Whether a marked row shows is the window's call, made from its own
        # readings; the groups hand it every row and read nothing to decide.
        from rochviewer.ui import main
        from rochviewer.ui.main import TimingGUI

        tab = next(iter(main.WINDOWED_TABS))
        gui = Mock()
        with patch.object(main, "TIMINGS", self.rows(tab)):
            groups = dict(TimingGUI.sensor_groups(gui))
        self.assertEqual([label for label, _r, _p in groups["Fans"]],
                         ["CPU Fan", "Sys Fan 1"])
        gui._read_compact_value.assert_not_called()


class RevealOnReadTest(unittest.TestCase):
    """A held-back row appears on its first real reading, not before."""

    def apply(self, texts):
        from unittest import mock
        from rochviewer.ui import dimm_telemetry_window as w

        key = ("sensor", "Fans", "System Fan 1")
        cells = {(key, column): mock.Mock() for column in range(1, 5)}
        window = mock.Mock(_stats={}, _fast_keys=set(), _sensor_cells=cells,
                           _pending_rows={key: ("record", [], "panel")})
        for text in texts:
            w.DimmTelemetryWindow._apply_sensors(window, [(key, text)])
        return window, key

    def test_a_blank_reading_keeps_it_hidden(self):
        window, _key = self.apply(["", "\u2014", "N/A"])
        window._reveal_row.assert_not_called()

    def test_the_first_reading_reveals_it(self):
        window, key = self.apply(["\u2014", "1403 RPM"])
        window._reveal_row.assert_called_once_with(key)


if __name__ == "__main__":
    unittest.main()
