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

"""The MSI Z790MPOWER's NCT6687D: every input HWiNFO lists, and only there."""

import unittest
from unittest import mock

from rochviewer.sensors import board_sensors
from rochviewer.sensors.board_sensors import decode_temperature, validate_rail
from rochviewer.sensors.superio_lpc import decode_sensor_volts

# Raw words read off the MS-7E01 window, with what HWiNFO showed for each.
MS_7E01_WINDOW = {
    "plus12v": (0x3EA0, 12.024),
    "plus5v": (0x3F40, 5.060),
    "vcore": (0x4FA0, 1.274),
    "vin3": (0x3480, 0.840),
    "vdd2": (0x2B20, 1.380),
    "cpu_sa": (0x4A00, 1.184),
    "cpu_aux": (0x3820, 1.796),
    "vin7": (0x5EE0, 1.518),
    "plus3v3": (0xD100, 3.344),
}
MS_7E01_TEMPERATURES = {"t0": (0x1080, 16.5), "t1": (0x2000, 32.0)}


def on_board(model):
    return mock.patch.object(board_sensors, "_board_model_tag",
                             return_value=model)


class Ms7e01RailsTest(unittest.TestCase):
    def test_every_rail_decodes_to_the_hwinfo_reading(self):
        with on_board("Z790MPOWER (MS-7E01)"):
            rails = board_sensors.nct668x_rails()
        self.assertEqual(set(rails), set(MS_7E01_WINDOW))
        for key, (raw, expected) in MS_7E01_WINDOW.items():
            with self.subTest(rail=key):
                volts = decode_sensor_volts(raw, rails[key][1])
                self.assertAlmostEqual(volts, expected, places=3)
                self.assertIsNotNone(validate_rail(key, volts))

    def test_t1_is_shown_and_the_empty_t0_is_not(self):
        # T0 reads 16.5 C, under the room it is in: an empty header.
        with on_board("Z790MPOWER (MS-7E01)"):
            sensors = board_sensors.nct668x_temperatures()
        self.assertNotIn("t0", sensors)
        self.assertEqual(sensors["t1"], 0x10C)
        for key, (raw, expected) in MS_7E01_TEMPERATURES.items():
            with self.subTest(sensor=key):
                self.assertEqual(decode_temperature(raw), expected)


class Ms7e01FansTest(unittest.TestCase):
    def test_the_three_headers_hwinfo_lists(self):
        with on_board("Z790MPOWER (MS-7E01)"):
            fans = board_sensors.nct668x_fans()
        self.assertEqual(fans, {"cpu_fan": 0x140, "pump1": 0x142,
                                "system1": 0x144})

    def test_rpm_is_read_whole_and_written_without_a_separator(self):
        # The telemetry statistics take the first plain number in the text;
        # "1,010 RPM" would have read as 1.
        from rochviewer.ui.dimm_telemetry_window import parse_reading

        reader = mock.Mock()
        reader.read_word.side_effect = lambda address: {
            0x140: 0x03F2, 0x142: 0x0B36, 0x144: 0x05BC}[address]
        profile = {"reader": reader, "fans": {"cpu_fan": 0x140,
                                             "pump1": 0x142,
                                             "system1": 0x144}}
        with mock.patch.object(board_sensors, "board_sensor_profile",
                               return_value=profile):
            self.assertEqual(board_sensors.read_board_fans(),
                             {"cpu_fan": 1010, "pump1": 2870, "system1": 1468})
            self.assertEqual(board_sensors.fan_text("cpu_fan"), "1010 RPM")
        self.assertEqual(parse_reading("1010 RPM"), (1010.0, "RPM", 0))


class Ms7e01FanDutyTest(unittest.TestCase):
    def test_the_three_connected_headers(self):
        with on_board("Z790MPOWER (MS-7E01)"):
            duties = board_sensors.nct668x_fan_duties()
        self.assertEqual(duties, {"cpu_fan": 0x160, "pump1": 0x161,
                                  "system1": 0x162})

    def test_the_bench_bytes_decode_to_whole_percentages(self):
        from rochviewer.ui.dimm_telemetry_window import parse_reading

        reader = mock.Mock()
        reader.read_bytes.side_effect = lambda address, count: [
            {0x160: 0x66, 0x161: 0xFF, 0x162: 0x99}[address]]
        profile = {"reader": reader, "fan_duties": {"cpu_fan": 0x160,
                                                    "pump1": 0x161,
                                                    "system1": 0x162}}
        with mock.patch.object(board_sensors, "board_sensor_profile",
                               return_value=profile):
            self.assertEqual(board_sensors.read_board_fan_duties(),
                             {"cpu_fan": 40, "pump1": 100, "system1": 60})
            self.assertEqual(board_sensors.fan_duty_text("pump1"), "100 %")
        self.assertEqual(parse_reading("40 %"), (40.0, "%", 0))

    def test_one_read_serves_the_rows_of_a_tick(self):
        # The first duty row refreshes every header; the rest take that read.
        reader = mock.Mock()
        reader.read_bytes.side_effect = lambda address, count: [
            {0x160: 0x66, 0x161: 0xFF, 0x162: 0x99}[address]]
        profile = {"reader": reader, "fan_duties": {"cpu_fan": 0x160,
                                                    "pump1": 0x161,
                                                    "system1": 0x162}}
        with mock.patch.object(board_sensors, "board_sensor_profile",
                               return_value=profile):
            shown = [board_sensors.fan_duty_text("cpu_fan", True),
                     board_sensors.fan_duty_text("pump1", False),
                     board_sensors.fan_duty_text("system1", False)]
        self.assertEqual(shown, ["40 %", "100 %", "60 %"])
        self.assertEqual(reader.read_bytes.call_count, 3)

    def test_off_and_full_scale(self):
        self.assertEqual(board_sensors.decode_fan_duty(0x00), 0)
        self.assertEqual(board_sensors.decode_fan_duty(0xFF), 100)

    def test_each_duty_row_follows_its_fan(self):
        from rochviewer.intel import intel_timings

        with on_board("Z790MPOWER (MS-7E01)"):
            rows = [row[0] for row in intel_timings._nct6687d_rows()
                    if row[1] == "Fans"]
        self.assertEqual(rows, ["CPU Fan", "CPU Fan Duty", "PUMP1",
                                "PUMP1 Duty", "System 1", "System 1 Duty"])


class OtherBoardTest(unittest.TestCase):
    def test_ms_7e06_keeps_its_own_map(self):
        # Same chip, different wiring: T0 there is an unconnected header.
        with on_board("PRO Z790-P WIFI DDR4 (MS-7E06)"):
            self.assertNotIn("t0", board_sensors.nct668x_temperatures())
            self.assertNotIn("plus12v", board_sensors.nct668x_rails())
            self.assertEqual(board_sensors.nct668x_fans(), {})
            self.assertEqual(board_sensors.nct668x_fan_duties(), {})

    def test_rows_follow_the_board_map(self):
        from rochviewer.intel import intel_timings

        with on_board("Z790MPOWER (MS-7E01)"):
            names = [row[0] for row in intel_timings._nct6687d_rows()]
        self.assertEqual(names, ["T1", "+12V", "+5V", "VIN3", "VIN7",
                                 "+3.3V", "CPU Fan", "CPU Fan Duty", "PUMP1",
                                 "PUMP1 Duty", "System 1", "System 1 Duty"])
        with on_board("PRO Z790-P WIFI DDR4 (MS-7E06)"):
            self.assertEqual(intel_timings._nct6687d_rows(), ())


if __name__ == "__main__":
    unittest.main()
